//! Lifecycle orchestration: thin async sequencing of Docker adapter calls
//! with all decisions taken by the pure `compose` layer.

use std::collections::BTreeMap;
use std::net::IpAddr;

use chrono::Utc;

use crate::auth::ControlToken;
use crate::compose::{
    self, DockerHardening, build_env, build_host_spec, build_labels, resolve_resource_limits,
    validate_create_request,
};
use crate::docker::{self, DockerAdapter, DockerError, ExecdArtifacts};
use crate::model::{
    CreateSandboxRequest, CreateSandboxResponse, Endpoint, Sandbox, SandboxStatus, error_codes,
};
use crate::policy::{LimitPolicy, NetworkPolicy, effective_limits};

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error("{code}: {message}")]
    Contract { code: &'static str, message: String },
    #[error(transparent)]
    Docker(#[from] DockerError),
}

impl ServiceError {
    pub fn code(&self) -> &'static str {
        match self {
            ServiceError::Contract { code, .. } => code,
            ServiceError::Docker(DockerError::Contract { code, .. }) => code,
            ServiceError::Docker(DockerError::Operation { .. }) => {
                error_codes::CONTAINER_START_FAILED
            }
        }
    }
}

/// The trust boundary the server enforces: who may ask, where the
/// sandbox lives, and how much of the host it may take.
#[derive(Debug, Clone)]
pub struct TrustConfig {
    /// The credential that authorizes a client of the control plane.
    pub control_token: ControlToken,
    /// The address the sandbox ports are published on. The loopback by
    /// default: a published port is reachable from the host, and from
    /// nowhere else.
    pub publish_address: IpAddr,
    /// The origin the resolved endpoints announce. It is CONFIGURED:
    /// the origin of a request belongs to the caller, and a server that
    /// echoes it lets the caller choose where the client connects.
    pub public_origin: String,
    /// The network the sandboxes are attached to.
    pub network: NetworkPolicy,
    /// The limits every sandbox runs under. They are mandatory.
    pub limits: LimitPolicy,
}

impl Default for TrustConfig {
    fn default() -> Self {
        Self {
            control_token: ControlToken::random(),
            publish_address: IpAddr::from([127, 0, 0, 1]),
            public_origin: "127.0.0.1".to_string(),
            network: NetworkPolicy::parse(
                crate::policy::INTERNAL_EGRESS_PROXY,
                crate::policy::DEFAULT_SANDBOX_NETWORK,
                crate::policy::DEFAULT_SANDBOX_SUBNET,
            )
            .expect("the default policy is supported"),
            limits: LimitPolicy::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub hardening: DockerHardening,
    pub server_env: BTreeMap<String, String>,
    pub execd_run_as_init: bool,
    pub max_timeout_seconds: i64,
    /// Global budget of concurrent environments. `None` means no limit.
    /// Enforced in the create path, so it bounds EVERY client.
    pub max_concurrent_environments: Option<usize>,
    /// Who may ask, and what the sandbox may reach.
    pub trust: TrustConfig,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            hardening: DockerHardening::default(),
            server_env: BTreeMap::new(),
            execd_run_as_init: true,
            max_timeout_seconds: 86_400,
            max_concurrent_environments: None,
            trust: TrustConfig::default(),
        }
    }
}

/// The lifecycle service, shared by the HTTP surface.
pub type SharedService = std::sync::Arc<LifecycleService>;

pub struct LifecycleService {
    adapter: DockerAdapter,
    config: ServiceConfig,
}

impl LifecycleService {
    pub fn new(adapter: DockerAdapter, config: ServiceConfig) -> Self {
        Self { adapter, config }
    }

    /// The credential that authorizes a client of this control plane.
    pub fn control_token(&self) -> &ControlToken {
        &self.config.trust.control_token
    }

    pub async fn create_sandbox(
        &self,
        req: CreateSandboxRequest,
    ) -> Result<CreateSandboxResponse, ServiceError> {
        validate_create_request(&req, self.config.max_timeout_seconds).map_err(|e| {
            ServiceError::Contract {
                code: e.code,
                message: e.message,
            }
        })?;
        let (requested_memory, requested_nano_cpus) =
            resolve_resource_limits(&req).map_err(|e| ServiceError::Contract {
                code: e.code,
                message: e.message,
            })?;
        // The limits are decided BEFORE anything exists: a request that
        // exceeds the policy never reaches the daemon.
        let (memory, nano_cpus) = effective_limits(
            requested_memory,
            requested_nano_cpus,
            &self.config.trust.limits,
        )
        .map_err(|e| ServiceError::Contract {
            code: e.code,
            message: e.message,
        })?;

        // Admission BEFORE anything is created: the global budget is a
        // property of the server, not of the client that happens to ask.
        if self.config.max_concurrent_environments.is_some() {
            let live = self.adapter.list_sandbox_containers().await?.len();
            compose::admit_creation(live, self.config.max_concurrent_environments).map_err(
                |message| ServiceError::Contract {
                    code: crate::model::error_codes::BUDGET_EXHAUSTED,
                    message,
                },
            )?;
        }

        let image = req
            .image
            .as_ref()
            .expect("validated: image present")
            .clone();
        let entrypoint = req.entrypoint.clone().unwrap_or_default();
        let sandbox_id = uuid::Uuid::new_v4().to_string();
        let created_at = Utc::now();
        let expires_at = req
            .timeout
            .map(|t| created_at + chrono::Duration::seconds(t));
        let platform = req.platform.clone();
        let docker_platform = platform.as_ref().map(|p| format!("{}/{}", p.os, p.arch));

        let credentials = image
            .auth
            .as_ref()
            .map(|a| bollard::auth::DockerCredentials {
                username: Some(a.username.clone()),
                password: Some(a.password.clone()),
                ..Default::default()
            });
        // The image must be present before the container is created. Its
        // id is NOT resolved from the tag: a tag is a name, and an image
        // pinned by digest has none. The id comes from the container
        // itself, below.
        self.adapter
            .ensure_image_available(&image.uri, credentials, docker_platform.as_deref())
            .await?;

        let labels = build_labels(&sandbox_id, &req, created_at, expires_at, platform.as_ref());
        // The credential of the sandbox: it authorizes the execution
        // plane (the execd inside the container) and nobody else. It
        // reaches the execd through the environment of the process that
        // needs it, and it is handed to the authorized client through
        // the endpoint contract.
        let execd_access_token = format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        );
        let env = build_env(
            &self.config.server_env,
            req.env.as_ref(),
            &sandbox_id,
            self.config.execd_run_as_init,
            &execd_access_token,
        );
        let host = build_host_spec(
            &self.config.hardening,
            Some(memory),
            Some(nano_cpus),
            &self.config.trust.network,
            &self.config.trust.publish_address.to_string(),
        );

        // The network policy is ENFORCED, not assumed: the network is
        // created if it is missing and verified if it already exists.
        // A network that cannot carry the policy stops the creation.
        self.adapter
            .ensure_network(&self.config.trust.network)
            .await?;

        let labels_map: std::collections::HashMap<String, String> =
            labels.clone().into_iter().collect();
        let container_id = self
            .adapter
            .create_sandbox_container(
                &sandbox_id,
                &image.uri,
                docker_platform.as_deref(),
                &entrypoint,
                &env,
                &labels_map,
                &host,
            )
            .await?;

        // The id of the image the container RUNS is what identifies the
        // environment: it is read from the container, never guessed from
        // the tag that was requested.
        let image_id = match self.adapter.container_image_id(&container_id).await {
            Ok(image_id) => image_id,
            Err(e) => {
                let _ = self.adapter.remove_container(&container_id).await;
                return Err(e.into());
            }
        };

        let prepared = self
            .inject_execd_runtime(&container_id, docker_platform.as_deref())
            .await;
        if let Err(e) = prepared {
            let _ = self.adapter.remove_container(&container_id).await;
            return Err(e);
        }
        if let Err(e) = self.adapter.start_container(&container_id).await {
            let _ = self.adapter.remove_container(&container_id).await;
            return Err(e.into());
        }

        Ok(CreateSandboxResponse {
            id: sandbox_id,
            image_id: Some(image_id),
            status: SandboxStatus {
                state: "Running".to_string(),
                reason: Some("CONTAINER_RUNNING".to_string()),
                message: Some("Sandbox container started successfully.".to_string()),
            },
            metadata: req.metadata,
            expires_at,
            created_at,
            entrypoint: Some(entrypoint),
        })
    }

    /// Inject execd + bootstrap.sh into the created (stopped) container.
    async fn inject_execd_runtime(
        &self,
        container_id: &str,
        platform: Option<&str>,
    ) -> Result<ExecdArtifacts, ServiceError> {
        let artifacts = self
            .adapter
            .execd_artifacts(platform.unwrap_or("default"))
            .await?;
        let name = container_id.to_string();
        self.adapter
            .ensure_directory(&name, compose::OPENSANDBOX_DIR)
            .await?;
        // Uploads expect the archive placed under the parent of the final path.
        self.adapter
            .upload_archive(
                &name,
                compose::OPENSANDBOX_DIR,
                (*artifacts.execd_archive).clone(),
            )
            .await?;
        self.adapter
            .upload_archive(
                &name,
                compose::OPENSANDBOX_DIR,
                (*artifacts.bootstrap_archive).clone(),
            )
            .await?;
        Ok(artifacts)
    }

    pub async fn get_sandbox(&self, sandbox_id: &str) -> Result<Sandbox, ServiceError> {
        let inspect = self
            .adapter
            .inspect(&compose::container_name(sandbox_id))
            .await?;
        sandbox_from_inspect(sandbox_id, &inspect).ok_or_else(|| ServiceError::Contract {
            code: error_codes::SANDBOX_NOT_FOUND,
            message: format!("sandbox {sandbox_id} not found"),
        })
    }

    pub async fn list_sandboxes(&self) -> Result<Vec<Sandbox>, ServiceError> {
        let summaries = self.adapter.list_sandbox_containers().await?;
        let mut items = Vec::new();
        for summary in summaries {
            let Some(id) = summary.labels.get(compose::LABEL_ID) else {
                continue;
            };
            let inspect = self.adapter.inspect(&summary.id).await?;
            if let Some(sandbox) = sandbox_from_inspect(id, &inspect) {
                items.push(sandbox);
            }
        }
        Ok(items)
    }

    pub async fn delete_sandbox(&self, sandbox_id: &str) -> Result<(), ServiceError> {
        let name = compose::container_name(sandbox_id);
        // 404 when absent, mirroring the reference contract.
        self.adapter.inspect(&name).await?;
        self.adapter.remove_container(&name).await?;
        Ok(())
    }

    /// TTL sweep: release every sandbox whose recorded expiry is due.
    /// Returns the ids this sweep removed (racing removes are fine: an
    /// already-gone sandbox is not an error for the reaper).
    pub async fn expire_due_sandboxes(&self) -> Vec<String> {
        let entries: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = self
            .list_sandboxes()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|sandbox| (sandbox.id, sandbox.expires_at))
            .collect();
        let due = compose::expired_sandboxes(&entries, Utc::now());
        for id in &due {
            let _ = self.delete_sandbox(id).await;
        }
        due
    }

    /// Resolve the host endpoint for a sandbox port. Only the two ports the
    /// server publishes (execd 44772, main 8080) are addressable; any other
    /// port is a contract error, not a silent fallback.
    ///
    /// The announced origin is the CONFIGURED one: the origin of the
    /// request belongs to the caller, and a server that echoes it lets
    /// the caller decide where the client connects next.
    pub async fn resolve_endpoint(
        &self,
        sandbox_id: &str,
        port: i32,
    ) -> Result<Endpoint, ServiceError> {
        let container_port = match port {
            44772 => compose::EXECD_CONTAINER_PORT,
            8080 => compose::HTTP_CONTAINER_PORT,
            _ => {
                return Err(ServiceError::Contract {
                    code: error_codes::INVALID_PARAMETER,
                    message: format!(
                        "port {port} is not published by the sandbox; only 44772 (execd) and 8080 (main) are addressable"
                    ),
                });
            }
        };
        let name = compose::container_name(sandbox_id);
        let inspect = self.adapter.inspect(&name).await?;
        let host_port =
            find_host_port(&inspect, container_port).ok_or_else(|| ServiceError::Contract {
                code: error_codes::SANDBOX_NOT_FOUND,
                message: format!(
                    "no published host port for {container_port} on sandbox {sandbox_id}"
                ),
            })?;
        // The execution plane carries its own credential: the client
        // that resolves the execd endpoint is the one allowed to drive
        // it.
        let headers = if port == 44772 {
            self.adapter
                .execd_access_token(&name)
                .await?
                .map(|token| BTreeMap::from([("X-EXECD-ACCESS-TOKEN".to_string(), token)]))
        } else {
            None
        };
        Ok(Endpoint {
            endpoint: format!("{}:{host_port}", self.config.trust.public_origin),
            headers,
        })
    }
}

fn find_host_port(
    inspect: &bollard::models::ContainerInspectResponse,
    container_port: &str,
) -> Option<String> {
    let ports = inspect.network_settings.as_ref()?.ports.as_ref()?;
    let bindings = ports.get(container_port)?;
    bindings.as_ref()?.iter().find_map(|b| b.host_port.clone())
}

/// Map a container inspect into the contract Sandbox using the labels the
/// server wrote at creation (restore-from-labels, no separate store).
pub fn sandbox_from_inspect(
    sandbox_id: &str,
    inspect: &bollard::models::ContainerInspectResponse,
) -> Option<Sandbox> {
    let config = inspect.config.as_ref()?;
    let labels = config.labels.as_ref()?;
    let label_id = labels.get(compose::LABEL_ID)?;
    if label_id != sandbox_id {
        return None;
    }
    let docker_state = inspect
        .state
        .as_ref()
        .and_then(|s| s.status.as_ref())
        .and_then(|status| {
            serde_json::to_value(status)
                .ok()
                .and_then(|v| v.as_str().map(str::to_string))
        })
        .unwrap_or_default();
    let created_at = labels
        .get(compose::LABEL_CREATED_AT)
        .and_then(|raw| docker::parse_label_time(raw))
        .unwrap_or_else(Utc::now);
    let expires_at = labels
        .get(compose::LABEL_EXPIRES_AT)
        .and_then(|raw| docker::parse_label_time(raw));
    let metadata_keys: Vec<String> = labels
        .get(compose::LABEL_METADATA_KEYS)
        .and_then(|raw| serde_json::from_str::<Vec<String>>(raw).ok())
        .unwrap_or_default();
    let metadata: BTreeMap<String, String> = metadata_keys
        .iter()
        .filter_map(|k| labels.get(k).map(|v| (k.clone(), v.clone())))
        .collect();
    let metadata = (!metadata.is_empty()).then_some(metadata);
    let entrypoint = config.cmd.clone().filter(|c| !c.is_empty());
    Some(Sandbox {
        id: sandbox_id.to_string(),
        status: SandboxStatus {
            state: docker::map_state(&docker_state).to_string(),
            reason: None,
            message: None,
        },
        metadata,
        expires_at,
        created_at,
        entrypoint,
    })
}

/// Parse helpers kept total: resource limit strings come from untrusted input.
pub fn resource_limits_checked(
    req: &CreateSandboxRequest,
) -> Result<(Option<i64>, Option<i64>), ServiceError> {
    resolve_resource_limits(req).map_err(|e| ServiceError::Contract {
        code: e.code,
        message: e.message,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compose::validation_error;

    #[test]
    fn missing_labels_yield_none_not_panic() {
        let inspect = bollard::models::ContainerInspectResponse::default();
        assert!(sandbox_from_inspect("x", &inspect).is_none());
    }

    /// Regression: the metadata-keys label is a JSON STRING in a label; it
    /// must be parsed with from_str. from_value over Value::String always
    /// fails and silently dropped every sandbox's metadata (TASK-13 probe).
    #[test]
    fn metadata_is_restored_from_labels_on_inspect() {
        let mut labels = std::collections::HashMap::new();
        labels.insert(crate::compose::LABEL_ID.to_string(), "sbx-meta".to_string());
        labels.insert(
            crate::compose::LABEL_METADATA_KEYS.to_string(),
            r#"["team","purpose"]"#.to_string(),
        );
        labels.insert("team".to_string(), "hodei-mh".to_string());
        labels.insert("purpose".to_string(), "probe".to_string());
        let inspect = bollard::models::ContainerInspectResponse {
            config: Some(bollard::models::ContainerConfig {
                labels: Some(labels),
                ..Default::default()
            }),
            ..Default::default()
        };
        let sandbox = sandbox_from_inspect("sbx-meta", &inspect).expect("sandbox restored");
        let metadata = sandbox.metadata.expect("metadata restored");
        assert_eq!(metadata.get("team").map(String::as_str), Some("hodei-mh"));
        assert_eq!(metadata.get("purpose").map(String::as_str), Some("probe"));
    }

    #[test]
    fn service_error_codes_map_from_both_variants() {
        let contract = ServiceError::Contract {
            code: error_codes::SANDBOX_NOT_FOUND,
            message: "m".into(),
        };
        assert_eq!(contract.code(), error_codes::SANDBOX_NOT_FOUND);
        let validation = validation_error(error_codes::INVALID_PARAMETER, "bad");
        let from_validation = ServiceError::Contract {
            code: validation.code,
            message: validation.message,
        };
        assert_eq!(from_validation.code(), error_codes::INVALID_PARAMETER);
    }
}
