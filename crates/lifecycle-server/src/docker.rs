//! Bollard-backed Docker adapter: the effectful edge of the lifecycle server.
//! Pure decisions live in `compose`; this module translates them into Docker
//! API calls and adapts results back into plain data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::body_full;
use bollard::models::{ContainerCreateBody, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, DownloadFromContainerOptionsBuilder,
    InspectContainerOptionsBuilder, ListContainersOptionsBuilder, ListImagesOptionsBuilder,
    RemoveContainerOptionsBuilder, StartContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::StreamExt;

use crate::compose::{self, HostSpec};
use crate::model::error_codes;

#[derive(Debug, thiserror::Error)]
pub enum DockerError {
    #[error("docker operation failed: {operation}: {source}")]
    Operation {
        operation: &'static str,
        #[source]
        source: bollard::errors::Error,
    },
    #[error("{code}: {message}")]
    Contract { code: &'static str, message: String },
}

impl DockerError {
    fn op(operation: &'static str) -> impl Fn(bollard::errors::Error) -> DockerError {
        move |source| DockerError::Operation { operation, source }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::Contract {
            code: error_codes::SANDBOX_NOT_FOUND,
            message: message.into(),
        }
    }
}

/// execd distribution artifacts extracted once per platform from the execd
/// image (reference `_fetch_execd_archive`).
#[derive(Debug, Clone)]
pub struct ExecdArtifacts {
    /// tar bytes whose root entry is `execd` (the daemon binary)
    pub execd_archive: Arc<Vec<u8>>,
    /// tar bytes whose root entry is `bootstrap.sh`
    pub bootstrap_archive: Arc<Vec<u8>>,
}

#[derive(Clone)]
pub struct DockerAdapter {
    docker: Docker,
    execd_image: String,
    artifacts: Arc<Mutex<HashMap<String, Option<ExecdArtifacts>>>>,
}

impl DockerAdapter {
    pub fn connect(docker: Docker, execd_image: impl Into<String>) -> Self {
        Self {
            docker,
            execd_image: execd_image.into(),
            artifacts: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Make sure the image is available locally: present, or pulled.
    ///
    /// The id of the image is NOT resolved here. A tag is a name, and an
    /// image pinned by digest has no tag to match; the id that identifies
    /// the environment is the one the container actually runs, and it is
    /// read from the container (`container_image_id`).
    pub async fn ensure_image_available(
        &self,
        image: &str,
        credentials: Option<DockerCredentials>,
        platform: Option<&str>,
    ) -> Result<(), DockerError> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("reference".to_string(), vec![image.to_string()])]);
        let options = ListImagesOptionsBuilder::new().filters(&filters).build();
        let existing = self
            .docker
            .list_images(Some(options))
            .await
            .map_err(DockerError::op("list images"))?;
        let present = existing
            .iter()
            .any(|summary| summary.repo_tags.iter().any(|tag| tag == image));
        if present {
            return Ok(());
        }
        let create = CreateImageOptionsBuilder::new()
            .from_image(image)
            .platform(platform.unwrap_or_default())
            .build();
        let mut stream = self.docker.create_image(Some(create), None, credentials);
        while let Some(item) = stream.next().await {
            item.map_err(|e| DockerError::Operation {
                operation: "pull image",
                source: e,
            })?;
        }
        Ok(())
    }

    /// The id of the image the container runs: the environment a check ran
    /// in is identified by that id, not by the tag it was requested with.
    /// It is read from the container, so it holds for an image pinned by
    /// digest and for an untagged image alike.
    pub async fn container_image_id(&self, container: &str) -> Result<String, DockerError> {
        let inspected = self.inspect(container).await?;
        inspected
            .image
            .filter(|id| !id.is_empty())
            .ok_or_else(|| DockerError::Contract {
                code: error_codes::INTERNAL,
                message: format!("the container {container} does not report the image it runs"),
            })
    }

    /// Extract execd + bootstrap.sh archives from the execd image, caching per
    /// platform key. Creates and removes a temporary `tail -f /dev/null`
    /// container exactly like the reference server does.
    pub async fn execd_artifacts(&self, platform_key: &str) -> Result<ExecdArtifacts, DockerError> {
        {
            let cache = self
                .artifacts
                .lock()
                .map_err(|_| poisoned("execd artifact cache"))?;
            if let Some(Some(hit)) = cache.get(platform_key) {
                return Ok(hit.clone());
            }
        }
        let fresh = self.fetch_artifacts_uncached().await;
        let mut cache = self
            .artifacts
            .lock()
            .map_err(|_| poisoned("execd artifact cache"))?;
        match fresh {
            Ok(a) => {
                cache.insert(platform_key.to_string(), Some(a.clone()));
                Ok(a)
            }
            Err(e) => {
                cache.insert(platform_key.to_string(), None);
                Err(e)
            }
        }
    }

    async fn fetch_artifacts_uncached(&self) -> Result<ExecdArtifacts, DockerError> {
        self.ensure_image_available(&self.execd_image, None, None)
            .await?;
        let name = format!("hodei-execd-cache-{}", uuid::Uuid::new_v4());
        let options = CreateContainerOptionsBuilder::new().name(&name).build();
        let config: ContainerCreateBody = ContainerCreateBody {
            image: Some(self.execd_image.clone()),
            cmd: Some(vec!["tail".into(), "-f".into(), "/dev/null".into()]),
            ..Default::default()
        };
        self.docker
            .create_container(Some(options), config)
            .await
            .map_err(DockerError::op("create execd cache container"))?;
        let start = StartContainerOptionsBuilder::new().build();
        if let Err(e) = self.docker.start_container(&name, Some(start)).await {
            let _ = self.remove_container(&name).await;
            return Err(DockerError::op("start execd cache container")(e));
        }
        let execd = self.download_archive(&name, "/execd").await;
        let bootstrap = self.download_archive(&name, "/bootstrap.sh").await;
        let _ = self.remove_container(&name).await;
        Ok(ExecdArtifacts {
            execd_archive: Arc::new(execd?),
            bootstrap_archive: Arc::new(bootstrap?),
        })
    }

    async fn download_archive(&self, container: &str, path: &str) -> Result<Vec<u8>, DockerError> {
        let options = DownloadFromContainerOptionsBuilder::new()
            .path(path)
            .build();
        let mut stream = self
            .docker
            .download_from_container(container, Some(options));
        let mut bytes = Vec::new();
        while let Some(chunk) = stream.next().await {
            match chunk {
                Ok(b) => bytes.extend_from_slice(&b),
                Err(e) => {
                    return Err(DockerError::Operation {
                        operation: "download archive",
                        source: e,
                    });
                }
            }
        }
        if bytes.is_empty() {
            return Err(DockerError::Contract {
                code: error_codes::EXECD_DISTRIBUTION_FAILED,
                message: format!("archive at {path} not found in execd image"),
            });
        }
        Ok(bytes)
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_sandbox_container(
        &self,
        sandbox_id: &str,
        image: &str,
        platform: Option<&str>,
        entrypoint: &[String],
        env: &[String],
        labels: &HashMap<String, String>,
        host: &HostSpec,
    ) -> Result<String, DockerError> {
        let name = compose::container_name(sandbox_id);
        let bindings: PortMap = if host.auto_assign_ports {
            [compose::EXECD_CONTAINER_PORT, compose::HTTP_CONTAINER_PORT]
                .into_iter()
                .map(|port| {
                    (
                        port.to_string(),
                        Some(vec![PortBinding {
                            host_ip: None,
                            host_port: None,
                        }]),
                    )
                })
                .collect()
        } else {
            PortMap::new()
        };
        let host_config = bollard::models::HostConfig {
            network_mode: Some(host.network_mode.clone()),
            security_opt: (!host.security_opt.is_empty()).then(|| host.security_opt.clone()),
            cap_drop: (!host.cap_drop.is_empty()).then(|| host.cap_drop.clone()),
            pids_limit: host.pids_limit,
            memory: host.memory,
            nano_cpus: host.nano_cpus,
            port_bindings: Some(bindings),
            ..Default::default()
        };
        let config: ContainerCreateBody = ContainerCreateBody {
            image: Some(image.to_string()),
            entrypoint: Some(vec![compose::BOOTSTRAP_PATH.to_string()]),
            cmd: Some(entrypoint.to_vec()),
            env: Some(env.to_vec()),
            labels: Some(labels.clone()),
            exposed_ports: Some(vec![
                compose::EXECD_CONTAINER_PORT.to_string(),
                compose::HTTP_CONTAINER_PORT.to_string(),
            ]),
            host_config: Some(host_config),
            ..Default::default()
        };
        let options = CreateContainerOptionsBuilder::new()
            .name(&name)
            .platform(platform.unwrap_or_default())
            .build();
        let created = self
            .docker
            .create_container(Some(options), config)
            .await
            .map_err(DockerError::op("create sandbox container"))?;
        Ok(created.id)
    }

    /// `put_archive` equivalent: upload tar bytes into the container path.
    pub async fn upload_archive(
        &self,
        container: &str,
        path: &str,
        tar_bytes: Vec<u8>,
    ) -> Result<(), DockerError> {
        let options = UploadToContainerOptionsBuilder::new().path(path).build();
        self.docker
            .upload_to_container(container, Some(options), body_full(tar_bytes.into()))
            .await
            .map_err(DockerError::op("upload archive"))
    }

    /// Create a directory entry inside the container (reference `_ensure_directory`).
    pub async fn ensure_directory(&self, container: &str, dir: &str) -> Result<(), DockerError> {
        let normalized = dir.trim_end_matches('/');
        if normalized.is_empty() || normalized == "/" {
            return Ok(());
        }
        let name = normalized.trim_start_matches('/');
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_data(&mut header, name, std::io::empty())
            .map_err(|e| DockerError::Contract {
                code: error_codes::EXECD_DISTRIBUTION_FAILED,
                message: format!("failed to build directory tar for {dir}: {e}"),
            })?;
        let bytes = builder.into_inner().map_err(|e| DockerError::Contract {
            code: error_codes::EXECD_DISTRIBUTION_FAILED,
            message: format!("failed to finish directory tar: {e}"),
        })?;
        self.upload_archive(container, "/", bytes).await
    }

    pub async fn start_container(&self, container: &str) -> Result<(), DockerError> {
        let options = StartContainerOptionsBuilder::new().build();
        self.docker
            .start_container(container, Some(options))
            .await
            .map_err(DockerError::op("start sandbox container"))
    }

    pub async fn remove_container(&self, container: &str) -> Result<(), DockerError> {
        let options = RemoveContainerOptionsBuilder::new()
            .force(true)
            .v(true)
            .build();
        self.docker
            .remove_container(container, Some(options))
            .await
            .map_err(DockerError::op("remove container"))
    }

    pub async fn inspect(
        &self,
        container: &str,
    ) -> Result<bollard::models::ContainerInspectResponse, DockerError> {
        let options = InspectContainerOptionsBuilder::new().build();
        self.docker
            .inspect_container(container, Some(options))
            .await
            .map_err(|e| match &e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => DockerError::not_found(format!("sandbox container {container:?}")),
                _ => DockerError::op("inspect container")(e),
            })
    }

    pub async fn list_sandbox_containers(&self) -> Result<Vec<ContainerSummaryData>, DockerError> {
        let filters: HashMap<String, Vec<String>> =
            HashMap::from([("label".to_string(), vec![compose::LABEL_ID.to_string()])]);
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        let list = self
            .docker
            .list_containers(Some(options))
            .await
            .map_err(DockerError::op("list containers"))?;
        Ok(list
            .into_iter()
            .map(|c| ContainerSummaryData {
                id: c.id.clone().unwrap_or_default(),
                labels: c.labels.clone().unwrap_or_default(),
                state: container_state_string(c.state.as_ref()),
            })
            .collect())
    }
}

#[derive(Debug, Clone)]
pub struct ContainerSummaryData {
    pub id: String,
    pub labels: HashMap<String, String>,
    pub state: String,
}

/// Serialize a Docker state enum into its lowercase contract string
/// (`"running"`, `"exited"`, ...). Container summaries already carry strings;
/// inspect responses carry enums.
pub fn container_state_string(state: Option<&impl ToString>) -> String {
    state.map(|s| s.to_string()).unwrap_or_default()
}

/// Pure mapping from Docker container state to contract lifecycle state.
pub fn map_state(docker_state: &str) -> &'static str {
    match docker_state {
        "RUNNING" | "running" => "Running",
        "PAUSED" | "paused" => "Paused",
        "CREATED" | "created" | "RESTARTING" | "restarting" => "Pending",
        "REMOVING" | "removing" => "Stopping",
        "EXITED" | "exited" => "Terminated",
        "DEAD" | "dead" => "Failed",
        _ => "Failed",
    }
}

fn poisoned(what: &str) -> DockerError {
    DockerError::Contract {
        code: error_codes::INTERNAL,
        message: format!("mutex poisoned: {what}"),
    }
}

/// Parse an RFC3339 label back into a timestamp (used to restore from labels).
pub fn parse_label_time(raw: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(raw)
        .ok()
        .map(|t| t.with_timezone(&Utc))
        .or_else(|| {
            chrono::NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
                .ok()
                .map(|n| n.and_utc())
        })
}

/// Render a timestamp in the same format we write to labels.
pub fn label_time(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_mapping_covers_reference_transitions() {
        assert_eq!(map_state("running"), "Running");
        assert_eq!(map_state("RUNNING"), "Running");
        assert_eq!(map_state("exited"), "Terminated");
        assert_eq!(map_state("created"), "Pending");
        assert_eq!(map_state("paused"), "Paused");
        assert_eq!(map_state("dead"), "Failed");
        assert_eq!(map_state("whatever"), "Failed");
    }

    #[test]
    fn label_time_roundtrip_and_python_isoformat_compatibility() {
        let t = chrono::Utc::now();
        let rendered = label_time(t);
        // The label format guarantees microsecond precision.
        let expected =
            chrono::DateTime::from_timestamp(t.timestamp(), t.timestamp_subsec_micros() * 1_000)
                .unwrap();
        assert_eq!(parse_label_time(&rendered), Some(expected));
        // Python datetime.isoformat() writes +00:00 instead of Z.
        let python_style = "2026-11-17T10:00:00.123456+00:00";
        assert!(parse_label_time(python_style).is_some());
    }

    #[test]
    fn directory_tar_has_single_mode_755_entry() {
        let mut header = tar::Header::new_gnu();
        header.set_size(0);
        header.set_mode(0o755);
        header.set_entry_type(tar::EntryType::Directory);
        header.set_cksum();
        let mut builder = tar::Builder::new(Vec::new());
        builder
            .append_data(&mut header, "opt/opensandbox", std::io::empty())
            .unwrap();
        let bytes = builder.into_inner().unwrap();
        let mut archive = tar::Archive::new(&bytes[..]);
        let mut entries = archive.entries().unwrap();
        let entry = entries.next().unwrap().unwrap();
        assert_eq!(entry.path().unwrap().to_str(), Some("opt/opensandbox"));
        assert_eq!(entry.header().entry_type(), tar::EntryType::Directory);
        assert_eq!(entry.header().mode().unwrap(), 0o755);
        assert!(entries.next().is_none());
    }
}
