//! Bollard-backed Docker adapter: the effectful edge of the lifecycle server.
//! Pure decisions live in `compose`; this module translates them into Docker
//! API calls and adapts results back into plain data.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bollard::Docker;
use bollard::auth::DockerCredentials;
use bollard::body_full;
use bollard::models::{ContainerCreateBody, PortBinding, PortMap};
use bollard::query_parameters::{
    CreateContainerOptionsBuilder, CreateImageOptionsBuilder, DownloadFromContainerOptionsBuilder,
    InspectContainerOptionsBuilder, InspectNetworkOptionsBuilder, ListContainersOptionsBuilder,
    ListImagesOptionsBuilder, ListNetworksOptionsBuilder, RemoveContainerOptionsBuilder,
    StartContainerOptionsBuilder, UploadToContainerOptionsBuilder,
};
use chrono::{DateTime, SecondsFormat, Utc};
use futures_util::StreamExt;

use crate::compose::{self, HostSpec};
use crate::model::error_codes;
use crate::policy::NetworkPolicy;

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
    /// BusyBox and its musl loader are injected for the loopback relay.
    pub busybox_archive: Arc<Vec<u8>>,
    pub loader_archive: Arc<Vec<u8>>,
    pub loader_name: String,
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

    pub(crate) fn client(&self) -> Docker {
        self.docker.clone()
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

    /// Refuse an image that carries the namespace of the execution
    /// plane in its OWN environment.
    ///
    /// The request chooses the image, and the daemon merges the
    /// environment of the image with the one of the container. An image
    /// that declares `EXECD_*` or `OPENSANDBOX_*` would therefore write
    /// into the namespace the server owns, by a door the composition of
    /// the environment does not cover.
    pub async fn assert_image_leaves_the_execution_plane_alone(
        &self,
        image: &str,
    ) -> Result<(), DockerError> {
        let inspected = match self.docker.inspect_image(image).await {
            Ok(inspected) => inspected,
            Err(error) => {
                return Err(DockerError::op("inspect the image the request chose")(
                    error,
                ));
            }
        };
        let env = inspected
            .config
            .as_ref()
            .and_then(|config| config.env.as_ref())
            .cloned()
            .unwrap_or_default();
        for entry in env {
            let Some((name, _)) = entry.split_once('=') else {
                continue;
            };
            if compose::belongs_to_the_execution_plane(name) {
                return Err(DockerError::Contract {
                    code: error_codes::INVALID_PARAMETER,
                    message: format!(
                        "the image {image} declares {name}, which belongs to the execution plane"
                    ),
                });
            }
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

    /// Return the relay loader compatible with the image actually used by a container.
    pub async fn container_loader_name(
        &self,
        container: &str,
    ) -> Result<&'static str, DockerError> {
        let image_id = self.container_image_id(container).await?;
        let image = self
            .docker
            .inspect_image(&image_id)
            .await
            .map_err(DockerError::op("inspect sandbox image"))?;
        loader_for_architecture(image.architecture.as_deref().unwrap_or_default())
    }

    /// The credential that authorizes the execution plane of a sandbox.
    ///
    /// It is read from the container, where the server injected it: the
    /// server keeps no second copy of it, and it is never written to a
    /// label, a log or a record.
    pub async fn execd_access_token(&self, container: &str) -> Result<Option<String>, DockerError> {
        let inspected = self.inspect(container).await?;
        Ok(inspected
            .config
            .and_then(|config| config.env)
            .and_then(compose::token_from_env)
            .filter(|token| !token.is_empty()))
    }

    /// Create a dedicated network. Docker allocates its subnet from its address pool.
    /// A pre-existing network is never adopted, even if its name matches.
    pub async fn create_sandbox_network(
        &self,
        policy: &NetworkPolicy,
        sandbox_id: &str,
        proxy: &str,
    ) -> Result<String, DockerError> {
        let name = sandbox_network_name(sandbox_id)?;
        // Resolve the configured name once. A name can later be reused for a
        // different container, so only the immutable full Docker ID is stored.
        let proxy_id = self
            .inspect(proxy)
            .await?
            .id
            .filter(|id| valid_container_id(id))
            .ok_or_else(|| network_violation("trusted proxy has no full Docker ID"))?;
        let request = sandbox_network_request(&name, sandbox_id, &proxy_id);
        let created = self
            .docker
            .create_network(request)
            .await
            .map_err(DockerError::op("create dedicated sandbox network"))?;
        let result = async {
            let network = self.inspect_network(&created.id).await?;
            validate_sandbox_network(&network, sandbox_id, policy)?;
            self.docker
                .connect_network(
                    &created.id,
                    bollard::models::NetworkConnectRequest {
                        container: proxy_id.clone(),
                        endpoint_config: Some(bollard::models::EndpointSettings {
                            aliases: Some(vec!["hodei-proxy".to_string()]),
                            ..Default::default()
                        }),
                    },
                )
                .await
                .map_err(DockerError::op("connect trusted proxy"))?;
            Ok(name)
        }
        .await;
        if result.is_err() {
            // Only the ID returned by this create operation may be rolled
            // back. The proxy may already be attached: the
            // disconnection comes first, because Docker refuses to
            // remove a network with a live endpoint.
            //
            // A rollback that fails is REPORTED: a network left on the
            // host is a bridge nobody owns, and the operator has to be
            // able to see it.
            if let Err(error) = self
                .docker
                .disconnect_network(
                    &created.id,
                    bollard::models::NetworkDisconnectRequest {
                        container: proxy_id.clone(),
                        force: Some(false),
                    },
                )
                .await
            {
                tracing::warn!("the rollback could not detach the trusted proxy: {error}");
            }
            if let Err(error) = self
                .remove_network_with_retry(&created.id, Some(&proxy_id))
                .await
            {
                tracing::error!("the rollback could not remove the sandbox network: {error}");
            }
        }
        result
    }

    /// Remove a network, retrying while Docker still holds an endpoint.
    ///
    /// The disconnection of the trusted proxy is asynchronous: the
    /// removal that follows it can be refused because the endpoint is
    /// not released yet. A network that survives would be a bridge left
    /// on the host, so the removal is attempted a bounded number of
    /// times before it is reported as a failure.
    ///
    /// Every attempt makes sure the proxy is really DETACHED: the
    /// endpoint is what the daemon refuses the removal for, and a
    /// detachment that only half took is the reason the network
    /// outlives its sandbox.
    async fn remove_network_with_retry(
        &self,
        id: &str,
        proxy_id: Option<&str>,
    ) -> Result<(), DockerError> {
        let mut attempt = 0;
        loop {
            match self.docker.remove_network(id).await {
                Ok(_) => return Ok(()),
                Err(error) => {
                    attempt += 1;
                    if attempt >= NETWORK_REMOVAL_ATTEMPTS {
                        return Err(DockerError::op("remove dedicated sandbox network")(error));
                    }
                    if let Some(proxy_id) = proxy_id {
                        let request = |force: bool| bollard::models::NetworkDisconnectRequest {
                            container: proxy_id.to_string(),
                            force: Some(force),
                        };
                        let _ = self.docker.disconnect_network(id, request(false)).await;
                        let _ = self.docker.disconnect_network(id, request(true)).await;
                    }
                    tokio::time::sleep(NETWORK_REMOVAL_DELAY).await;
                }
            }
        }
    }

    /// Remove only the network bearing this sandbox's ownership labels. Other
    /// endpoints are not forcibly removed; Docker refuses removal while in use.
    pub async fn remove_sandbox_network(&self, sandbox_id: &str) -> Result<bool, DockerError> {
        let name = sandbox_network_name(sandbox_id)?;
        let network = match self.inspect_network(&name).await {
            Ok(network) => network,
            Err(DockerError::Contract {
                code: error_codes::SANDBOX_NOT_FOUND,
                ..
            }) => {
                tracing::info!("the sandbox network of {sandbox_id} was already gone");
                return Ok(false);
            }
            Err(error) => return Err(error),
        };
        validate_network_owner(&network, sandbox_id)?;
        let id = network
            .id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| network_violation("network has no Docker ID"))?;
        let proxy_id = network_proxy_id(&network)?;
        if network
            .containers
            .as_ref()
            .is_some_and(|containers| containers.contains_key(proxy_id))
        {
            self.disconnect_proxy(id, proxy_id).await?;
        }
        self.remove_network_with_retry(id, Some(proxy_id)).await?;
        tracing::info!("the sandbox network of {sandbox_id} was removed");
        Ok(true)
    }

    /// Disconnect the trusted proxy from a network that is going away.
    ///
    /// The disconnection is attempted without force first: Docker
    /// refuses it while the container holds the network for something
    /// else. When it does, the forced disconnection is the one that
    /// unblocks the removal of the network, and the proxy keeps
    /// running on the networks it still holds.
    async fn disconnect_proxy(&self, network_id: &str, proxy_id: &str) -> Result<(), DockerError> {
        let request = |force: bool| bollard::models::NetworkDisconnectRequest {
            container: proxy_id.to_string(),
            force: Some(force),
        };
        if let Err(error) = self
            .docker
            .disconnect_network(network_id, request(false))
            .await
        {
            self.docker
                .disconnect_network(network_id, request(true))
                .await
                .map_err(|forced| DockerError::op("disconnect trusted proxy")(forced))?;
            tracing::debug!("the proxy needed a forced disconnection: {error}");
        }
        Ok(())
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
        let fresh = self.fetch_artifacts_uncached(platform_key).await;
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

    async fn fetch_artifacts_uncached(
        &self,
        platform_key: &str,
    ) -> Result<ExecdArtifacts, DockerError> {
        self.ensure_image_available(&self.execd_image, None, None)
            .await?;
        let architecture = self
            .docker
            .inspect_image(&self.execd_image)
            .await
            .map_err(DockerError::op("inspect execd image"))?
            .architecture
            .unwrap_or_default();
        if platform_key != "default" && platform_key != format!("linux/{architecture}") {
            return Err(DockerError::Contract {
                code: error_codes::EXECD_DISTRIBUTION_FAILED,
                message: format!(
                    "execd image architecture {architecture:?} does not match requested platform {platform_key:?}"
                ),
            });
        }
        let loader_name = loader_for_architecture(&architecture)?;
        let name = format!("hodei-execd-cache-{}", uuid::Uuid::new_v4());
        let options = CreateContainerOptionsBuilder::new().name(&name).build();
        // The helper only reads four files out of the image. It runs
        // with NO network and with a label of its own: a helper that
        // reached the host's networks would be an egress channel that
        // no sandbox ever has, and a helper nobody accounts for would
        // outlive a crash.
        let config: ContainerCreateBody = ContainerCreateBody {
            image: Some(self.execd_image.clone()),
            cmd: Some(vec!["tail".into(), "-f".into(), "/dev/null".into()]),
            labels: Some(HashMap::from([(
                ARTIFACT_HELPER_LABEL.to_string(),
                "lifecycle-server".to_string(),
            )])),
            host_config: Some(bollard::models::HostConfig {
                network_mode: Some("none".to_string()),
                ..Default::default()
            }),
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
        let busybox = self.download_archive(&name, "/bin/busybox").await;
        let loader = self
            .download_archive(&name, &format!("/lib/{loader_name}"))
            .await;
        let _ = self.remove_container(&name).await;
        Ok(ExecdArtifacts {
            execd_archive: Arc::new(execd?),
            bootstrap_archive: Arc::new(bootstrap?),
            busybox_archive: Arc::new(busybox?),
            loader_archive: Arc::new(loader?),
            loader_name: loader_name.to_string(),
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
                            // The published port is bound to the
                            // CONFIGURED address: the loopback by
                            // default, so the sandbox is reachable from
                            // the host and from the network the host
                            // sits in, never.
                            host_ip: Some(host.publish_address.clone()),
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
        // The container is attached to the network the policy names, and
        // to no other: the default bridge would hand it the host's
        // network.
        let networking_config = bollard::models::NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                host.network_mode.clone(),
                bollard::models::EndpointSettings::default(),
            )])),
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
            networking_config: Some(networking_config),
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

    /// Inspect a network by name.
    pub async fn inspect_network(
        &self,
        network: &str,
    ) -> Result<bollard::models::NetworkInspect, DockerError> {
        let options = InspectNetworkOptionsBuilder::new().build();
        self.docker
            .inspect_network(network, Some(options))
            .await
            .map_err(|e| match &e {
                bollard::errors::Error::DockerResponseServerError {
                    status_code: 404, ..
                } => DockerError::not_found(format!("network {network:?}")),
                _ => DockerError::op("inspect network")(e),
            })
    }

    /// Verify that the container is attached to EXACTLY the isolated
    /// network of its sandbox, and to nothing else.
    ///
    /// The network is verified when it is created, but a container that
    /// ended up somewhere else would run with another isolation than the
    /// one that was checked. The attachment is read back from the daemon
    /// and never assumed.
    pub async fn verify_sandbox_attachment(
        &self,
        container: &str,
        network: &str,
    ) -> Result<(), DockerError> {
        let inspected = self.inspect(container).await?;
        let attached = inspected
            .network_settings
            .as_ref()
            .and_then(|settings| settings.networks.as_ref())
            .ok_or_else(|| network_violation("the sandbox container has no network attachment"))?;
        let names: Vec<&str> = attached.keys().map(String::as_str).collect();
        if names.len() != 1 || names[0] != network {
            return Err(network_violation(format!(
                "the sandbox container is attached to {names:?} instead of only {network:?}"
            )));
        }
        Ok(())
    }

    /// The state the daemon reports for a container.
    ///
    /// The state of a container that was just started is NOT a fact: the
    /// start call only says the container was started, and an entrypoint
    /// that fails leaves it dead. The state is read back.
    pub async fn container_state(&self, container: &str) -> Result<String, DockerError> {
        let inspected = self.inspect(container).await?;
        Ok(inspected
            .state
            .as_ref()
            .map(|state| container_state_string(state.status.as_ref()))
            .unwrap_or_default())
    }

    /// Verify that the limits the policy mandates are the ones the
    /// container actually carries.
    ///
    /// The limits are sent to the daemon, but a daemon that applies
    /// something else would leave the sandbox with limits the policy
    /// never agreed to. The effective configuration is read back.
    pub async fn verify_applied_limits(
        &self,
        container: &str,
        memory: i64,
        nano_cpus: i64,
        pids: i64,
    ) -> Result<(), DockerError> {
        let inspected = self.inspect(container).await?;
        let host_config = inspected
            .host_config
            .as_ref()
            .ok_or_else(|| network_violation("the sandbox container has no host configuration"))?;
        let applied = (
            host_config.memory,
            host_config.nano_cpus,
            host_config.pids_limit,
        );
        if applied != (Some(memory), Some(nano_cpus), Some(pids)) {
            return Err(network_violation(format!(
                "the sandbox container runs with the limits {applied:?} instead of the mandatory \
                 (memory {memory}, cpu {nano_cpus}, pids {pids})"
            )));
        }
        Ok(())
    }

    /// The ids of the sandboxes whose dedicated networks still exist,
    /// with the moment each network was created.
    ///
    /// A network outlives its container when a removal fails, and the
    /// TTL sweep only enumerates containers: without this list, such a
    /// network would never be found again. The creation moment matters
    /// because a network that is minutes old may belong to a creation
    /// that is still in flight.
    pub async fn list_sandbox_networks(
        &self,
    ) -> Result<Vec<(String, Option<DateTime<Utc>>)>, DockerError> {
        let filters: HashMap<String, Vec<String>> = HashMap::from([(
            "label".to_string(),
            vec![format!("{NETWORK_OWNER_LABEL}={NETWORK_OWNER}")],
        )]);
        let options = ListNetworksOptionsBuilder::new().filters(&filters).build();
        let networks = self
            .docker
            .list_networks(Some(options))
            .await
            .map_err(DockerError::op("list networks"))?;
        Ok(networks
            .iter()
            .filter_map(|network| {
                let id = network.labels.as_ref()?.get(compose::LABEL_ID)?.clone();
                let created = network
                    .created
                    .as_deref()
                    .and_then(|raw| DateTime::parse_from_rfc3339(raw).ok())
                    .map(|created| created.with_timezone(&Utc));
                Some((id, created))
            })
            .collect())
    }

    /// Remove the helper containers that read the execd artifacts and
    /// outlived the run that created them.
    ///
    /// A helper is only left behind when the process that created it
    /// died between starting it and removing it. Only the helpers that
    /// are clearly abandoned are removed: a helper that is minutes old
    /// may belong to a concurrent extraction.
    pub async fn reap_artifact_helpers(&self, older_than: Duration) -> usize {
        let filters: HashMap<String, Vec<String>> = HashMap::from([(
            "label".to_string(),
            vec![format!("{ARTIFACT_HELPER_LABEL}=lifecycle-server")],
        )]);
        let options = ListContainersOptionsBuilder::new()
            .all(true)
            .filters(&filters)
            .build();
        let Ok(containers) = self.docker.list_containers(Some(options)).await else {
            return 0;
        };
        let now = Utc::now().timestamp();
        let mut removed = 0;
        for container in containers {
            let age = container
                .created
                .map(|created| now - created)
                .unwrap_or(i64::MAX);
            if age < older_than.as_secs() as i64 {
                continue;
            }
            if let Some(id) = container.id
                && self.remove_container(&id).await.is_ok()
            {
                removed += 1;
            }
        }
        removed
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

const NETWORK_OWNER_LABEL: &str = "hodei.io/network-owner";
const NETWORK_OWNER: &str = "lifecycle-server";
const NETWORK_PROXY_ID_LABEL: &str = "hodei.io/network-proxy-id";
/// How many times the removal of a network is attempted.
///
/// Docker releases the endpoint of a disconnected container
/// asynchronously: the removal that follows the disconnection can be
/// refused with "network has active endpoints". The attempt is bounded:
/// a network that survives every try is a real failure, and the reaper
/// of orphan networks will try again later.
pub(crate) const NETWORK_REMOVAL_ATTEMPTS: usize = 5;
/// The wait between two attempts of the removal of a network.
pub(crate) const NETWORK_REMOVAL_DELAY: Duration = Duration::from_millis(250);
/// The label of the helper container that reads the execd artifacts out
/// of the image. It carries no network and is accounted for by the
/// sweeper, so a crash does not leave it running.
pub const ARTIFACT_HELPER_LABEL: &str = "hodei.io/artifact-helper";

fn valid_container_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn network_proxy_id(network: &bollard::models::NetworkInspect) -> Result<&str, DockerError> {
    network
        .labels
        .as_ref()
        .and_then(|labels| labels.get(NETWORK_PROXY_ID_LABEL))
        .map(String::as_str)
        .filter(|id| valid_container_id(id))
        .ok_or_else(|| network_violation("sandbox network lacks a valid original proxy identity"))
}

fn network_violation(message: impl Into<String>) -> DockerError {
    DockerError::Contract {
        code: error_codes::NETWORK_POLICY_VIOLATION,
        message: message.into(),
    }
}

fn sandbox_network_name(sandbox_id: &str) -> Result<String, DockerError> {
    if sandbox_id.is_empty()
        || !sandbox_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err(network_violation("invalid sandbox ID for network name"));
    }
    Ok(format!("hodei-sandbox-{sandbox_id}"))
}

fn sandbox_network_request(
    name: &str,
    sandbox_id: &str,
    proxy_id: &str,
) -> bollard::models::NetworkCreateRequest {
    bollard::models::NetworkCreateRequest {
        name: name.to_string(),
        driver: Some("bridge".into()),
        internal: Some(true),
        enable_ipv6: Some(false),
        options: Some(HashMap::from([(
            "com.docker.network.bridge.gateway_mode_ipv4".into(),
            "isolated".into(),
        )])),
        labels: Some(HashMap::from([
            (NETWORK_OWNER_LABEL.into(), NETWORK_OWNER.into()),
            (compose::LABEL_ID.into(), sandbox_id.into()),
            (NETWORK_PROXY_ID_LABEL.into(), proxy_id.into()),
        ])),
        ..Default::default()
    }
}

fn validate_network_owner(
    network: &bollard::models::NetworkInspect,
    sandbox_id: &str,
) -> Result<(), DockerError> {
    let labels = network.labels.as_ref();
    if !labels.is_some_and(|labels| {
        labels.get(NETWORK_OWNER_LABEL).map(String::as_str) == Some(NETWORK_OWNER)
            && labels.get(compose::LABEL_ID).map(String::as_str) == Some(sandbox_id)
    }) {
        return Err(network_violation(
            "sandbox network ownership labels do not match",
        ));
    }
    Ok(())
}

fn validate_sandbox_network(
    network: &bollard::models::NetworkInspect,
    sandbox_id: &str,
    policy: &NetworkPolicy,
) -> Result<(), DockerError> {
    validate_network_owner(network, sandbox_id)?;
    let isolated = network
        .options
        .as_ref()
        .and_then(|o| o.get("com.docker.network.bridge.gateway_mode_ipv4"))
        .is_some_and(|v| v == "isolated");
    let allocated = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .is_some_and(|configs| {
            configs.len() == 1
                && configs[0].subnet.as_deref().is_some_and(|subnet| {
                    subnet.split_once('/').is_some_and(|(ip, prefix)| {
                        ip.parse::<std::net::Ipv4Addr>().is_ok()
                            && prefix.parse::<u8>().is_ok_and(|p| p <= 32)
                    })
                })
                && configs[0].gateway.as_deref().is_none_or(str::is_empty)
        });
    // Docker allocated the subnet: it must come from the range the
    // operator declared. A subnet from anywhere else is a boundary
    // nobody chose, and it is refused instead of adopted.
    let within_the_pool = network
        .ipam
        .as_ref()
        .and_then(|ipam| ipam.config.as_ref())
        .and_then(|configs| configs.first())
        .and_then(|config| config.subnet.as_deref())
        .is_some_and(|subnet| crate::policy::cidr_within(policy.address_pool(), subnet));
    if network.driver.as_deref() != Some("bridge")
        || network.internal != Some(true)
        || network.enable_ipv6 != Some(false)
        || !isolated
        || !allocated
        || !within_the_pool
    {
        return Err(network_violation(
            "sandbox network is not an isolated internal IPv4 bridge inside the declared address pool",
        ));
    }
    Ok(())
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

fn loader_for_architecture(architecture: &str) -> Result<&'static str, DockerError> {
    match architecture {
        "amd64" => Ok("ld-musl-x86_64.so.1"),
        "arm64" => Ok("ld-musl-aarch64.so.1"),
        _ => Err(DockerError::Contract {
            code: error_codes::EXECD_DISTRIBUTION_FAILED,
            message: format!("unsupported image architecture {architecture:?} for isolated relay"),
        }),
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
    fn dedicated_network_request_and_validation_enforce_isolation_and_ownership() {
        let name = sandbox_network_name("sbx-123").unwrap();
        let proxy_id = "a".repeat(64);
        let request = sandbox_network_request(&name, "sbx-123", &proxy_id);
        assert!(request.ipam.is_none(), "Docker must allocate the subnet");
        let policy = NetworkPolicy::parse(
            crate::policy::INTERNAL_EGRESS_PROXY,
            crate::policy::DEFAULT_SANDBOX_ADDRESS_POOL,
        )
        .unwrap();
        let mut network = bollard::models::NetworkInspect {
            driver: request.driver,
            internal: request.internal,
            enable_ipv6: request.enable_ipv6,
            options: request.options,
            labels: request.labels,
            ipam: Some(bollard::models::Ipam {
                config: Some(vec![bollard::models::IpamConfig {
                    subnet: Some("172.25.0.0/16".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        assert_eq!(network_proxy_id(&network).unwrap(), proxy_id);
        assert!(validate_sandbox_network(&network, "sbx-123", &policy).is_ok());
        assert!(validate_network_owner(&network, "other").is_err());
        network.enable_ipv6 = Some(true);
        assert!(validate_sandbox_network(&network, "sbx-123", &policy).is_err());
        network.enable_ipv6 = Some(false);
        network.ipam.as_mut().unwrap().config.as_mut().unwrap()[0].gateway =
            Some("172.25.0.1".into());
        assert!(validate_sandbox_network(&network, "sbx-123", &policy).is_err());
        assert!(sandbox_network_name("../other").is_err());
    }

    #[test]
    fn a_subnet_outside_the_declared_pool_is_refused() {
        let policy = NetworkPolicy::parse(
            crate::policy::INTERNAL_EGRESS_PROXY,
            crate::policy::DEFAULT_SANDBOX_ADDRESS_POOL,
        )
        .unwrap();
        let network = bollard::models::NetworkInspect {
            driver: Some("bridge".into()),
            internal: Some(true),
            enable_ipv6: Some(false),
            options: Some(HashMap::from([(
                "com.docker.network.bridge.gateway_mode_ipv4".into(),
                "isolated".into(),
            )])),
            labels: Some(HashMap::from([
                (NETWORK_OWNER_LABEL.into(), NETWORK_OWNER.into()),
                (compose::LABEL_ID.into(), "sbx-123".into()),
                (NETWORK_PROXY_ID_LABEL.into(), "a".repeat(64)),
            ])),
            ipam: Some(bollard::models::Ipam {
                config: Some(vec![bollard::models::IpamConfig {
                    subnet: Some("10.9.0.0/16".into()),
                    ..Default::default()
                }]),
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = validate_sandbox_network(&network, "sbx-123", &policy)
            .expect_err("a subnet from outside the pool is refused");
        let message = error.to_string();
        assert!(message.contains("address pool"), "{message}");
    }

    #[test]
    fn proxy_identity_requires_a_full_immutable_id() {
        let mut network = bollard::models::NetworkInspect::default();
        assert!(network_proxy_id(&network).is_err());
        network.labels = Some(HashMap::from([(
            NETWORK_PROXY_ID_LABEL.into(),
            "proxy-name".into(),
        )]));
        assert!(network_proxy_id(&network).is_err());
        network
            .labels
            .as_mut()
            .unwrap()
            .insert(NETWORK_PROXY_ID_LABEL.into(), "a".repeat(64));
        assert_eq!(network_proxy_id(&network).unwrap(), "a".repeat(64));
    }

    #[test]
    fn relay_loader_rejects_unknown_architectures() {
        assert_eq!(
            loader_for_architecture("amd64").unwrap(),
            "ld-musl-x86_64.so.1"
        );
        assert_eq!(
            loader_for_architecture("arm64").unwrap(),
            "ld-musl-aarch64.so.1"
        );
        assert!(loader_for_architecture("").is_err());
        assert!(loader_for_architecture("riscv64").is_err());
    }

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
