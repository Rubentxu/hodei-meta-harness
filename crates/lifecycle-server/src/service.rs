//! Lifecycle orchestration: thin async sequencing of Docker adapter calls
//! with all decisions taken by the pure `compose` layer.

use std::collections::BTreeMap;
use std::net::IpAddr;

use chrono::Utc;

use crate::admission::{AdmissionAuthority, AdmissionBudget, AdmissionError, EnvironmentResources};
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

/// How long a network that no sandbox owns is left alone before it is
/// removed.
///
/// A creation in flight is protected by the record of the creations in
/// flight, not by this grace: the age of the network says nothing
/// about how long the creation takes. This margin is the last line for
/// the networks that no creation owns, because the process that created
/// them is gone.
const ORPHAN_NETWORK_GRACE: chrono::Duration = chrono::Duration::minutes(2);

/// The creations that are in flight, by sandbox.
///
/// A sandbox has its network before it has a container, so a sweep that
/// ignored the creations in flight could take the network of a creation
/// that is still working. The record is what protects them: the age of
/// the network is not a fact about the creation.
type InFlight = std::sync::Arc<std::sync::Mutex<std::collections::HashSet<String>>>;

/// The network of a sandbox that is still being created.
///
/// The network is created BEFORE the container, so a creation that
/// fails — or that is abandoned because its client gave up — would
/// leave a bridge on the host that no sandbox owns. This guard owns
/// that network until the creation finishes: while it lives, the
/// network goes away with it, whatever the reason the creation did
/// not complete.
struct CreationGuard {
    adapter: DockerAdapter,
    sandbox_id: String,
    /// The creations in flight: the sweep leaves their networks alone
    /// however long they take.
    in_flight: InFlight,
    /// Whether the creation completed and the network now belongs to
    /// a sandbox that exists.
    completed: bool,
}

impl CreationGuard {
    fn new(adapter: DockerAdapter, sandbox_id: &str, in_flight: InFlight) -> Self {
        lock_in_flight(&in_flight).insert(sandbox_id.to_string());
        Self {
            adapter,
            sandbox_id: sandbox_id.to_string(),
            in_flight,
            completed: false,
        }
    }

    /// The creation finished: the network belongs to the sandbox.
    fn complete(&mut self) {
        self.completed = true;
        lock_in_flight(&self.in_flight).remove(&self.sandbox_id);
    }
}

/// The creations in flight.
///
/// The lock is held for a hash operation and never across an await: a
/// poisoned lock is recovered, because the record it protects is a
/// convenience and not a truth that a panic may destroy.
fn lock_in_flight(
    in_flight: &InFlight,
) -> std::sync::MutexGuard<'_, std::collections::HashSet<String>> {
    in_flight
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Drop for CreationGuard {
    fn drop(&mut self) {
        if self.completed {
            return;
        }
        // The creation did not finish. The removal runs on its own
        // task: a guard is dropped inside a future that is going away,
        // and there is no place to await it there. A removal that
        // fails is reported: a bridge nobody owns has to be visible.
        let adapter = self.adapter.clone();
        let sandbox_id = self.sandbox_id.clone();
        let in_flight = self.in_flight.clone();
        tokio::spawn(async move {
            match adapter.remove_sandbox_network(&sandbox_id).await {
                Ok(true) => tracing::info!(
                    "the network of the sandbox {sandbox_id} was removed: the creation did not finish"
                ),
                Ok(false) => {}
                Err(error) => tracing::error!(
                    "the network of the sandbox {sandbox_id} could not be removed after a failed creation: {error}"
                ),
            }
            // The creation is over: the sweep may take the network from
            // here on, and a removal that failed is its business.
            lock_in_flight(&in_flight).remove(&sandbox_id);
        });
    }
}

/// Whether a network that no sandbox owns may be removed.
///
/// The answer is NO whenever there is a doubt: a network of a live
/// sandbox is never removed, a network that is younger than the grace
/// period may belong to a creation in flight, and a network whose age
/// is unknown is left alone rather than destroyed on a guess.
fn is_reapable_orphan(
    id: &str,
    live: &[String],
    in_flight: &[String],
    created: Option<chrono::DateTime<Utc>>,
    now: chrono::DateTime<Utc>,
) -> bool {
    if live.iter().any(|owned| owned == id) {
        return false;
    }
    // A creation in flight has its network before it has a container.
    // Its age says nothing about how long the creation takes, so the
    // record of the creations in flight is what protects it.
    if in_flight.iter().any(|creating| creating == id) {
        return false;
    }
    match created {
        Some(created) => now.signed_duration_since(created) >= ORPHAN_NETWORK_GRACE,
        None => false,
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
    /// Operator-selected trusted proxy container, never supplied by a client.
    pub proxy_container: Option<String>,
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
                crate::policy::DEFAULT_SANDBOX_ADDRESS_POOL,
            )
            .expect("the default policy is supported"),
            proxy_container: None,
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
    /// Global budget of memory the environments may take. `None` means no
    /// limit.
    pub max_total_memory_bytes: Option<i64>,
    /// Global budget of CPU the environments may take. `None` means no
    /// limit.
    pub max_total_nano_cpus: Option<i64>,
    /// How many times the release of an environment is attempted.
    pub release_attempts: usize,
    /// The wait between two attempts of the release of an environment.
    pub release_delay: std::time::Duration,
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
            max_total_memory_bytes: None,
            max_total_nano_cpus: None,
            release_attempts: DEFAULT_RELEASE_ATTEMPTS,
            release_delay: DEFAULT_RELEASE_DELAY,
            trust: TrustConfig::default(),
        }
    }
}

/// How many times the release of an environment is attempted.
///
/// The attempt is BOUNDED: a daemon that refuses the removal is a real
/// failure, and the operator has to see it.
const DEFAULT_RELEASE_ATTEMPTS: usize = 3;

/// The wait between two attempts of the release of an environment.
const DEFAULT_RELEASE_DELAY: std::time::Duration = std::time::Duration::from_millis(200);

/// How long a request waits for the creation another request owns.
const CREATION_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// The lifecycle service, shared by the HTTP surface.
pub type SharedService = std::sync::Arc<LifecycleService>;

/// The outcome of a creation, published to whoever waits for it.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CreationOutcome {
    /// The creation is still working.
    InFlight,
    /// The creation failed: the key is free again.
    Failed,
    /// The environment exists: this is its id.
    Succeeded(String),
}

/// One creation intent, by creation key.
struct CreationEntry {
    /// The fingerprint of the body the key was used with.
    fingerprint: String,
    /// Publishes the outcome of the creation to whoever waits for it.
    outcome: tokio::sync::watch::Sender<CreationOutcome>,
}

/// The claim of a creation key.
enum CreationClaim<'a> {
    /// The request owns the creation and has to perform it.
    Owned(CreationTicket<'a>),
    /// The creation already happened: this is the environment it produced.
    Existing(String),
    /// The key was already used with another body.
    Conflict,
    /// Another request owns the creation and it is still working.
    InFlight(tokio::sync::watch::Receiver<CreationOutcome>),
}

/// What the claim of a creation key decided.
enum CreationClaimResult<'a> {
    /// The request creates the environment. The ticket is `None` when the
    /// request carries no creation key.
    Create(Option<CreationTicket<'a>>),
    /// The creation already happened: this is the environment it produced.
    Existing(String),
}

/// The ownership of a creation key.
///
/// The ownership ends with the creation: when the owner finishes it
/// publishes the outcome, and when the owner goes away WITHOUT finishing
/// (the request was cancelled, the future was dropped) the ticket frees the
/// key by itself. A key that stayed in flight forever would turn every
/// retry into a wait that never ends.
struct CreationTicket<'a> {
    registry: &'a std::sync::Mutex<std::collections::HashMap<String, CreationEntry>>,
    key: String,
    outcome: tokio::sync::watch::Sender<CreationOutcome>,
    finished: bool,
}

impl CreationTicket<'_> {
    /// Publish the outcome of the creation.
    ///
    /// The publication and the removal of a failed key happen under the
    /// SAME lock: a request that claims the key in between must not see an
    /// entry that is about to disappear, and the owner must not remove the
    /// entry of the one that claimed it after.
    fn finish(&mut self, outcome: CreationOutcome) {
        self.finished = true;
        let failed = matches!(outcome, CreationOutcome::Failed);
        let mut registry = lock_creations(self.registry);
        self.outcome.send_replace(outcome);
        if failed {
            registry.remove(&self.key);
        }
    }
}

impl Drop for CreationTicket<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // The creation did not finish: the key is free again and the
            // ones that wait for it are released.
            self.finish(CreationOutcome::Failed);
        }
    }
}

/// Wait for the outcome of a creation another request owns.
///
/// The outcome is published by its owner: `None` means the owner is gone
/// without publishing anything, and the creation did not happen.
async fn await_creation_outcome(
    outcome: &mut tokio::sync::watch::Receiver<CreationOutcome>,
) -> Option<CreationOutcome> {
    loop {
        let current = outcome.borrow().clone();
        if current != CreationOutcome::InFlight {
            return Some(current);
        }
        if outcome.changed().await.is_err() {
            return None;
        }
    }
}

pub struct LifecycleService {
    adapter: DockerAdapter,
    config: ServiceConfig,
    relays: tokio::sync::Mutex<std::collections::HashMap<String, crate::relay::RelayHandle>>,
    /// The credentials this process minted, by sandbox.
    ///
    /// The credential is read back from the container, and the value
    /// that comes back is the one that is handed to the client. A
    /// credential that is NOT the one this server minted is a
    /// credential somebody else chose, and it is refused rather than
    /// delivered. The copy lives in memory only: it is never written to
    /// a label, a log or a record.
    credentials: tokio::sync::Mutex<std::collections::HashMap<String, String>>,
    /// The creations that are in flight: their networks exist before
    /// their containers do, and the sweep must not take them away.
    in_flight: InFlight,
    /// The admission authority: the single owner of the slots and of the
    /// aggregate budget of environments.
    admission: std::sync::Mutex<AdmissionAuthority>,
    /// The creation intents, by creation key.
    creations: std::sync::Mutex<std::collections::HashMap<String, CreationEntry>>,
}

/// The admission authority, locked.
///
/// The lock is held for a decision and never across an await: the
/// admission of a creation must not wait for a slow operation, and the
/// decision must not be taken twice.
fn lock_admission(
    admission: &std::sync::Mutex<AdmissionAuthority>,
) -> std::sync::MutexGuard<'_, AdmissionAuthority> {
    admission
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The creation intents, locked.
fn lock_creations(
    creations: &std::sync::Mutex<std::collections::HashMap<String, CreationEntry>>,
) -> std::sync::MutexGuard<'_, std::collections::HashMap<String, CreationEntry>> {
    creations
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The reservation of the budget for a creation in flight.
///
/// The reservation is taken BEFORE any effect and released when the
/// creation does not finish: a creation that fails, that is abandoned or
/// that is cancelled does not keep the host's slot.
struct BudgetReservation<'a> {
    authority: &'a std::sync::Mutex<AdmissionAuthority>,
    sandbox_id: String,
    committed: bool,
}

impl BudgetReservation<'_> {
    /// The environment exists: the reservation becomes a commitment.
    fn commit(&mut self) {
        self.committed = true;
        lock_admission(self.authority).commit(&self.sandbox_id);
    }
}

impl Drop for BudgetReservation<'_> {
    fn drop(&mut self) {
        if !self.committed {
            lock_admission(self.authority).release(&self.sandbox_id);
        }
    }
}

impl LifecycleService {
    pub fn new(adapter: DockerAdapter, config: ServiceConfig) -> Self {
        let budget = AdmissionBudget {
            slots: config.max_concurrent_environments,
            memory_bytes: config.max_total_memory_bytes,
            nano_cpus: config.max_total_nano_cpus,
        };
        Self {
            adapter,
            config,
            relays: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            credentials: tokio::sync::Mutex::new(std::collections::HashMap::new()),
            in_flight: std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new())),
            admission: std::sync::Mutex::new(AdmissionAuthority::new(budget)),
            creations: std::sync::Mutex::new(std::collections::HashMap::new()),
        }
    }

    /// The credential that authorizes a client of this control plane.
    pub fn control_token(&self) -> &ControlToken {
        &self.config.trust.control_token
    }

    /// Create an environment.
    ///
    /// The creation key decides whether this request is a NEW creation or
    /// a repeat of one that was already served: the same key with the same
    /// body is ONE environment, and the same key with another body is a
    /// conflict. Without a key the request creates, as it always did.
    pub async fn create_sandbox(
        &self,
        req: CreateSandboxRequest,
    ) -> Result<CreateSandboxResponse, ServiceError> {
        match self.claim_creation(&req).await? {
            CreationClaimResult::Existing(sandbox_id) => self.describe_creation(&sandbox_id).await,
            CreationClaimResult::Create(ticket) => {
                // The ticket frees the key by itself if this future is
                // dropped before the creation finishes.
                let created = self.create_environment(req).await;
                match (created, ticket) {
                    (Ok(created), Some(mut ticket)) => {
                        ticket.finish(CreationOutcome::Succeeded(created.id.clone()));
                        Ok(created)
                    }
                    (Err(error), Some(mut ticket)) => {
                        // The creation did not happen: the key is free
                        // again and a retry may take it.
                        ticket.finish(CreationOutcome::Failed);
                        Err(error)
                    }
                    (created, None) => created,
                }
            }
        }
    }

    /// Claim the creation key of a request.
    ///
    /// The claim is taken BEFORE any effect: two requests with the same
    /// key are one creation, and the second one waits for the first
    /// instead of creating a second environment.
    async fn claim_creation(
        &self,
        req: &CreateSandboxRequest,
    ) -> Result<CreationClaimResult<'_>, ServiceError> {
        let Some(raw_key) = req.creation_key.as_deref() else {
            return Ok(CreationClaimResult::Create(None));
        };
        let key = raw_key.trim();
        if key.is_empty() {
            return Err(ServiceError::Contract {
                code: error_codes::INVALID_PARAMETER,
                message: "the creation key must not be empty".to_string(),
            });
        }
        let fingerprint = compose::creation_fingerprint(req);
        loop {
            match self.claim_creation_key(key, &fingerprint) {
                CreationClaim::Owned(ticket) => {
                    return Ok(CreationClaimResult::Create(Some(ticket)));
                }
                CreationClaim::Existing(sandbox_id) => {
                    return Ok(CreationClaimResult::Existing(sandbox_id));
                }
                CreationClaim::Conflict => {
                    return Err(ServiceError::Contract {
                        code: error_codes::CREATION_CONFLICT,
                        message: format!(
                            "the creation key {key:?} was already used with another body; \
                             no environment was created and the environment of the first \
                             request was not touched"
                        ),
                    });
                }
                CreationClaim::InFlight(mut outcome) => {
                    // The owner is still working: the answer is its
                    // answer, not a second environment.
                    match tokio::time::timeout(CREATION_WAIT, await_creation_outcome(&mut outcome))
                        .await
                    {
                        Ok(Some(CreationOutcome::Succeeded(sandbox_id))) => {
                            return Ok(CreationClaimResult::Existing(sandbox_id));
                        }
                        Ok(_) => continue,
                        Err(_) => {
                            return Err(ServiceError::Contract {
                                code: error_codes::CREATION_IN_PROGRESS,
                                message: format!(
                                    "the creation with the key {key:?} is still in flight; \
                                     no environment was created"
                                ),
                            });
                        }
                    }
                }
            }
        }
    }

    /// Claim a creation key in the registry.
    fn claim_creation_key(&self, key: &str, fingerprint: &str) -> CreationClaim<'_> {
        let mut registry = lock_creations(&self.creations);
        let known = registry.get(key).map(|entry| {
            (
                entry.fingerprint.clone(),
                entry.outcome.borrow().clone(),
                entry.outcome.clone(),
            )
        });
        if let Some((known_fingerprint, current, outcome)) = known {
            if known_fingerprint != fingerprint {
                return CreationClaim::Conflict;
            }
            match current {
                CreationOutcome::Succeeded(sandbox_id) => {
                    return CreationClaim::Existing(sandbox_id);
                }
                CreationOutcome::InFlight => {
                    return CreationClaim::InFlight(outcome.subscribe());
                }
                CreationOutcome::Failed => {
                    // The owner failed: the key is free again.
                    registry.remove(key);
                }
            }
        }
        let (outcome, _receiver) = tokio::sync::watch::channel(CreationOutcome::InFlight);
        registry.insert(
            key.to_string(),
            CreationEntry {
                fingerprint: fingerprint.to_string(),
                outcome: outcome.clone(),
            },
        );
        CreationClaim::Owned(CreationTicket {
            registry: &self.creations,
            key: key.to_string(),
            outcome,
            finished: false,
        })
    }

    /// The environment a creation key produced, read back from the daemon.
    async fn describe_creation(
        &self,
        sandbox_id: &str,
    ) -> Result<CreateSandboxResponse, ServiceError> {
        let name = compose::container_name(sandbox_id);
        let inspect = self.adapter.inspect(&name).await?;
        creation_response_from_inspect(sandbox_id, &inspect).ok_or_else(|| ServiceError::Contract {
            code: error_codes::SANDBOX_NOT_FOUND,
            message: format!("the sandbox {sandbox_id} of this creation key is not there"),
        })
    }

    /// Create the environment: the effects, in order.
    async fn create_environment(
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

        let proxy = self
            .config
            .trust
            .proxy_container
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| ServiceError::Contract {
                code: error_codes::NETWORK_POLICY_VIOLATION,
                message: "trusted egress proxy container is not configured".to_string(),
            })?;

        // The identity of the environment is decided BEFORE the
        // reservation: the reservation is keyed by it.
        let sandbox_id = uuid::Uuid::new_v4().to_string();
        // The reservation is taken BEFORE any effect: an environment that
        // is being created already occupies its slot, and two
        // simultaneous creations cannot both take the last one. The
        // reservation is released if the creation does not finish.
        let mut reservation = self.reserve_budget(&sandbox_id, memory, nano_cpus)?;
        let image = req
            .image
            .as_ref()
            .expect("validated: image present")
            .clone();
        let entrypoint = req.entrypoint.clone().unwrap_or_default();
        let created_at = Utc::now();
        // The expiry is computed with a duration that FITS: a timeout that
        // cannot be represented is refused by the validation, and this is
        // the place where that promise is kept.
        let expires_at = match req.timeout {
            Some(seconds) => Some(
                created_at
                    + chrono::Duration::try_seconds(seconds).ok_or_else(|| {
                        ServiceError::Contract {
                            code: error_codes::INVALID_PARAMETER,
                            message: format!("timeout {seconds}s is out of range"),
                        }
                    })?,
            ),
            None => None,
        };
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
        // The request chooses the image, and the daemon merges the
        // environment of the image with the one of the container: an
        // image that declares the namespace of the execution plane
        // would write into it by a door the composition does not
        // cover.
        self.adapter
            .assert_image_leaves_the_execution_plane_alone(&image.uri)
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
        // The network of THIS sandbox is created first, and the host
        // specification names it: there is no shared network any more,
        // and a container that is not attached to its own bridge never
        // gets created.
        let network_name = self
            .adapter
            .create_sandbox_network(&self.config.trust.network, &sandbox_id, proxy)
            .await?;
        // From here on the network belongs to a creation that may not
        // finish. The guard takes it away with the creation, and the
        // sweep leaves it alone while it works.
        let mut guard =
            CreationGuard::new(self.adapter.clone(), &sandbox_id, self.in_flight.clone());
        let mut host = build_host_spec(
            &self.config.hardening,
            Some(memory),
            Some(nano_cpus),
            &network_name,
            &self.config.trust.publish_address.to_string(),
        );
        host.pids_limit = Some(self.config.trust.limits.pids_limit);

        let labels_map: std::collections::HashMap<String, String> =
            labels.clone().into_iter().collect();
        let container_id = match self
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
            .await
        {
            Ok(id) => id,
            Err(e) => {
                // The network of this creation goes away with the
                // guard: the creation did not finish.
                return Err(e.into());
            }
        };

        // The id of the image the container RUNS is what identifies the
        // environment: it is read from the container, never guessed from
        // the tag that was requested.
        let image_id = match self.adapter.container_image_id(&container_id).await {
            Ok(image_id) => image_id,
            Err(e) => {
                self.cleanup_failed_creation(&container_id, &sandbox_id)
                    .await;
                return Err(e.into());
            }
        };

        let prepared = self
            .inject_execd_runtime(&container_id, docker_platform.as_deref())
            .await;
        if let Err(e) = prepared {
            self.cleanup_failed_creation(&container_id, &sandbox_id)
                .await;
            return Err(e);
        }
        if let Err(e) = self.adapter.start_container(&container_id).await {
            self.cleanup_failed_creation(&container_id, &sandbox_id)
                .await;
            return Err(e.into());
        }

        // The isolation is VERIFIED, not assumed. The network was
        // checked when it was created, but the ATTACHMENT of the
        // container to it, and the limits it actually runs under, are
        // facts about the container: they are read back, and a
        // sandbox that does not match them is not created.
        if let Err(e) = self
            .adapter
            .verify_sandbox_attachment(&container_id, &network_name)
            .await
        {
            self.cleanup_failed_creation(&container_id, &sandbox_id)
                .await;
            return Err(e.into());
        }
        if let Err(e) = self
            .adapter
            .verify_applied_limits(
                &container_id,
                memory,
                nano_cpus,
                self.config.trust.limits.pids_limit,
            )
            .await
        {
            self.cleanup_failed_creation(&container_id, &sandbox_id)
                .await;
            return Err(e.into());
        }
        // The state is read back too: a start call that succeeded does
        // not mean the container is running, and an environment that is
        // not running cannot be used.
        let state = match self.running_state(&container_id).await {
            Ok(state) => state,
            Err(error) => {
                self.cleanup_failed_creation(&container_id, &sandbox_id)
                    .await;
                return Err(error);
            }
        };

        // The credential this server minted is remembered so that the
        // value read back from the container can be checked against it.
        self.remember_credential(&sandbox_id, &execd_access_token)
            .await;

        // The sandbox exists: its network belongs to it now.
        guard.complete();

        // The environment exists: the reservation becomes a commitment.
        reservation.commit();

        Ok(CreateSandboxResponse {
            id: sandbox_id,
            image_id: Some(image_id),
            status: SandboxStatus {
                state: state.clone(),
                reason: Some("CONTAINER_RUNNING".to_string()),
                message: Some("Sandbox container started successfully.".to_string()),
            },
            metadata: req.metadata,
            expires_at,
            created_at,
            entrypoint: Some(entrypoint),
        })
    }

    /// Take the resources of an environment that is about to be created.
    ///
    /// The decision and the reservation are the SAME operation, under the
    /// same lock: a creation that is admitted owns the resources, and a
    /// creation that is refused owns nothing.
    fn reserve_budget(
        &self,
        sandbox_id: &str,
        memory_bytes: i64,
        nano_cpus: i64,
    ) -> Result<BudgetReservation<'_>, ServiceError> {
        let resources = EnvironmentResources {
            slots: 1,
            memory_bytes,
            nano_cpus,
        };
        let mut ledger = lock_admission(&self.admission);
        match ledger.reserve(sandbox_id, resources) {
            Ok(()) => Ok(BudgetReservation {
                authority: &self.admission,
                sandbox_id: sandbox_id.to_string(),
                committed: false,
            }),
            Err(AdmissionError::Exhausted(refusal)) => Err(ServiceError::Contract {
                code: error_codes::BUDGET_EXHAUSTED,
                message: refusal.message,
            }),
            Err(AdmissionError::AlreadyAccounted(id)) => Err(ServiceError::Contract {
                code: error_codes::INTERNAL,
                message: format!("the environment {id} is already accounted for"),
            }),
        }
    }

    /// Remember the credential this server minted for a sandbox.
    ///
    /// The copy is the one that lets the server tell its own credential
    /// from a foreign one when it reads the container back.
    async fn remember_credential(&self, sandbox_id: &str, token: &str) {
        self.credentials
            .lock()
            .await
            .insert(sandbox_id.to_string(), token.to_string());
    }

    /// Forget the credential of a sandbox that is gone.
    async fn forget_credential(&self, sandbox_id: &str) {
        self.credentials.lock().await.remove(sandbox_id);
    }

    /// Whether the credential read back from the container is the one
    /// this server minted for that sandbox.
    ///
    /// A sandbox this process did not create has no record to compare
    /// against: the shape of the credential is then the only thing that
    /// can be checked, and a credential that does not have it is
    /// refused anyway.
    async fn credential_is_ours(&self, sandbox_id: &str, token: &str) -> bool {
        if !token.chars().all(|c| c.is_ascii_hexdigit()) || token.len() != 64 {
            return false;
        }
        let minted = self.credentials.lock().await.get(sandbox_id).cloned();
        match minted {
            Some(minted) => minted == token,
            None => true,
        }
    }

    /// The state the daemon reports for a container, once it has
    /// settled.
    ///
    /// The daemon updates the state right after the start, and a read
    /// that races it would report a container that is not running yet.
    async fn running_state(&self, container: &str) -> Result<String, ServiceError> {
        let mut state = String::new();
        for _ in 0..10 {
            state = self.adapter.container_state(container).await?;
            if state == "running" {
                return Ok(state);
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        Err(ServiceError::Contract {
            code: error_codes::EXECD_START_FAILED,
            message: format!("the sandbox container is {state:?} instead of running"),
        })
    }

    async fn cleanup_failed_creation(&self, container: &str, _id: &str) {
        if let Err(error) = self.adapter.remove_container(container).await {
            tracing::error!("sandbox container cleanup failed: {error}");
        }
        // The network is not removed here: the creation guard owns it
        // until the creation completes, and it goes away with it.
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
        let loader = self.adapter.container_loader_name(container_id).await?;
        if loader != artifacts.loader_name {
            return Err(ServiceError::Contract {
                code: error_codes::EXECD_DISTRIBUTION_FAILED,
                message: "sandbox and execd image architectures do not match".to_string(),
            });
        }
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
        self.adapter
            .upload_archive(
                &name,
                compose::OPENSANDBOX_DIR,
                (*artifacts.busybox_archive).clone(),
            )
            .await?;
        self.adapter
            .upload_archive(
                &name,
                compose::OPENSANDBOX_DIR,
                (*artifacts.loader_archive).clone(),
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
        Ok(self
            .observe_sandboxes()
            .await?
            .into_iter()
            .map(|observed| observed.sandbox)
            .collect())
    }

    /// The environments the daemon reports, with what each one takes of
    /// the host and the creation intent it came from.
    async fn observe_sandboxes(&self) -> Result<Vec<ObservedSandbox>, ServiceError> {
        let summaries = self.adapter.list_sandbox_containers().await?;
        let mut items = Vec::new();
        for summary in summaries {
            let Some(id) = summary.labels.get(compose::LABEL_ID) else {
                continue;
            };
            let inspect = self.adapter.inspect(&summary.id).await?;
            let Some(sandbox) = sandbox_from_inspect(id, &inspect) else {
                continue;
            };
            let creation = match (
                summary.labels.get(compose::LABEL_CREATION_KEY),
                summary.labels.get(compose::LABEL_CREATION_FINGERPRINT),
            ) {
                (Some(key), Some(fingerprint)) => Some(CreationRecord {
                    key: key.clone(),
                    fingerprint: fingerprint.clone(),
                }),
                _ => None,
            };
            items.push(ObservedSandbox {
                sandbox,
                resources: resources_from_inspect(&inspect),
                creation,
            });
        }
        Ok(items)
    }

    /// Reconcile the authority with what the daemon reports.
    ///
    /// A server that restarts finds the environments it left behind, and
    /// an environment that was removed by somebody else stops counting.
    /// The creations that are in flight are not affected: they are not
    /// visible to the daemon yet.
    pub async fn reconcile(&self) -> Result<(), ServiceError> {
        let horizon = lock_admission(&self.admission).sequence();
        let observed = self.observe_sandboxes().await?;
        self.reconcile_observed(&observed, horizon);
        Ok(())
    }

    /// Apply an observation to the authority and to the creation registry.
    fn reconcile_observed(&self, observed: &[ObservedSandbox], horizon: u64) {
        let environments: Vec<(String, EnvironmentResources)> = observed
            .iter()
            .map(|item| (item.sandbox.id.clone(), item.resources))
            .collect();
        lock_admission(&self.admission).reconcile(&environments, horizon);
        let mut registry = lock_creations(&self.creations);
        for item in observed {
            let Some(creation) = &item.creation else {
                continue;
            };
            // The intents that are in flight are kept: the daemon does not
            // know about them yet.
            registry.entry(creation.key.clone()).or_insert_with(|| {
                let (outcome, _receiver) = tokio::sync::watch::channel(CreationOutcome::Succeeded(
                    item.sandbox.id.clone(),
                ));
                CreationEntry {
                    fingerprint: creation.fingerprint.clone(),
                    outcome,
                }
            });
        }
    }

    pub async fn delete_sandbox(&self, sandbox_id: &str) -> Result<(), ServiceError> {
        // The relay registry is not a lock held across slow operations:
        // the relay is taken out of it, and the Docker calls happen
        // without holding anything.
        let relay = self.relays.lock().await.remove(sandbox_id);
        if let Some(relay) = relay {
            relay.shutdown().await;
        }
        let name = compose::container_name(sandbox_id);
        let had_container = match self.adapter.inspect(&name).await {
            Ok(inspect) => {
                if sandbox_from_inspect(sandbox_id, &inspect).is_none() {
                    return Err(ServiceError::Contract {
                        code: error_codes::SANDBOX_NOT_FOUND,
                        message: "sandbox identity does not match".to_string(),
                    });
                }
                // Use the inspected immutable ID, never a name that can be reused.
                let id = inspect
                    .id
                    .as_deref()
                    .filter(|id| !id.is_empty())
                    .ok_or_else(|| ServiceError::Contract {
                        code: error_codes::INTERNAL,
                        message: "sandbox has no container identity".to_string(),
                    })?;
                // The removal is attempted a BOUNDED number of times: a
                // daemon that refuses it once is a failure, not a lost
                // environment. A removal that fails is NEVER reported as
                // a removal.
                self.remove_container_with_retry(id).await?;
                true
            }
            Err(DockerError::Contract {
                code: error_codes::SANDBOX_NOT_FOUND,
                ..
            }) => false,
            Err(error) => return Err(error.into()),
        };
        // The environment is gone: its slot is free again. The network may
        // still be there —the reaper of orphans takes it— but the
        // ENVIRONMENT is released.
        lock_admission(&self.admission).release(sandbox_id);
        let had_network = self.adapter.remove_sandbox_network(sandbox_id).await?;
        // The release is visible: a sandbox that is gone and a network
        // that is gone are facts the operator can check.
        tracing::info!(
            "the sandbox {sandbox_id} was released: container={had_container} network={had_network}"
        );
        // The credential of a sandbox that is gone is forgotten with it.
        self.forget_credential(sandbox_id).await;
        if !had_container && !had_network {
            return Err(DockerError::not_found(format!("sandbox {sandbox_id} not found")).into());
        }
        Ok(())
    }

    /// Remove the container of an environment, with a bounded number of
    /// attempts.
    ///
    /// The attempt is bounded: a daemon that refuses the removal is a real
    /// failure, and the caller has to see it. The wait between attempts
    /// belongs to the configuration, so a test does not have to sleep.
    async fn remove_container_with_retry(&self, container: &str) -> Result<(), ServiceError> {
        let attempts = self.config.release_attempts.max(1);
        let mut last_error: Option<ServiceError> = None;
        for attempt in 1..=attempts {
            match self.adapter.remove_container(container).await {
                Ok(()) => return Ok(()),
                Err(error) => {
                    tracing::warn!(
                        "the removal of the container {container} failed \
                         (attempt {attempt} of {attempts}): {error}"
                    );
                    last_error = Some(ServiceError::from(error));
                    if attempt < attempts {
                        tokio::time::sleep(self.config.release_delay).await;
                    }
                }
            }
        }
        Err(last_error.unwrap_or_else(|| ServiceError::Contract {
            code: error_codes::INTERNAL,
            message: "the container was not removed".to_string(),
        }))
    }

    /// TTL sweep: release every sandbox whose recorded expiry is due.
    ///
    /// Returns the ids this sweep RELEASED. An environment whose release
    /// failed is not in the list: it is still there, and saying otherwise
    /// would be a lie the operator can check.
    pub async fn expire_due_sandboxes(&self) -> Vec<String> {
        let horizon = lock_admission(&self.admission).sequence();
        let observed = self.observe_sandboxes().await.unwrap_or_default();
        // The authority is reconciled BEFORE the releases: an
        // environment that somebody else removed stops counting, and the
        // creations in flight keep their reservation.
        self.reconcile_observed(&observed, horizon);
        let entries: Vec<(String, Option<chrono::DateTime<chrono::Utc>>)> = observed
            .iter()
            .map(|item| (item.sandbox.id.clone(), item.sandbox.expires_at))
            .collect();
        let due = compose::expired_sandboxes(&entries, Utc::now());
        let mut released = Vec::new();
        for id in &due {
            match self.delete_sandbox(id).await {
                Ok(()) => released.push(id.clone()),
                Err(error) if error.code() == error_codes::SANDBOX_NOT_FOUND => {
                    // The environment was already gone: a racing release
                    // is not an error for the reaper.
                }
                Err(error) => {
                    // A failure to release a sandbox is reported, not
                    // swallowed: the environment is still there, and the
                    // operator has to know that it is.
                    tracing::error!("the sandbox {id} could not be released: {error}");
                }
            }
        }
        // A sandbox whose container is gone but whose network is not
        // would leave an isolated bridge on the host forever: nothing
        // else looks for it.
        self.reap_orphan_networks().await;
        // The helper that reads the execd artifacts is only meant to
        // live for the extraction; an interrupted run leaves it behind.
        let helpers = self
            .adapter
            .reap_artifact_helpers(std::time::Duration::from_secs(600))
            .await;
        if helpers > 0 {
            tracing::info!("removed {helpers} abandoned execd artifact helpers");
        }
        released
    }

    /// Remove the networks of sandboxes that no longer exist.
    ///
    /// A failed deletion (a foreign endpoint still attached, a daemon
    /// error) leaves the network behind, and the container that owned it
    /// is already gone: the TTL sweep only enumerates containers, so
    /// without this the network would outlive every reaper.
    pub async fn reap_orphan_networks(&self) {
        let live: Vec<String> = self
            .list_sandboxes()
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|sandbox| sandbox.id)
            .collect();
        let in_flight: Vec<String> = lock_in_flight(&self.in_flight).iter().cloned().collect();
        let now = Utc::now();
        for (id, created) in self
            .adapter
            .list_sandbox_networks()
            .await
            .unwrap_or_default()
        {
            // A network that is younger than the grace period is NEVER
            // removed, and neither is the network of a creation that is
            // still in flight: a sandbox that is being created has its
            // network before it has a container, and removing that
            // network would make the creation fail for a reason that has
            // nothing to do with the candidate.
            if !is_reapable_orphan(&id, &live, &in_flight, created, now) {
                continue;
            }
            match self.adapter.remove_sandbox_network(&id).await {
                Ok(true) => tracing::info!("removed the orphan network of sandbox {id}"),
                Ok(false) => {}
                Err(error) => tracing::error!(
                    "the orphan network of sandbox {id} could not be removed: {error}"
                ),
            }
        }
    }

    /// Resolve the loopback endpoint for execd (port 44772). Other ports
    /// are refused because the isolated relay does not expose them.
    ///
    /// The announced origin is the CONFIGURED one: the origin of the
    /// request belongs to the caller, and a server that echoes it lets
    /// the caller decide where the client connects next.
    pub async fn resolve_endpoint(
        &self,
        sandbox_id: &str,
        port: i32,
    ) -> Result<Endpoint, ServiceError> {
        if port != 44772 {
            return Err(ServiceError::Contract {
                code: error_codes::INVALID_PARAMETER,
                message: format!("port {port} is not available through the isolated control relay"),
            });
        }
        // The relay is taken out of the registry before any Docker call:
        // the registry is not a lock held across slow operations. A
        // relay whose listener died is replaced instead of advertised.
        let reusable = {
            let mut relays = self.relays.lock().await;
            relays.remove(sandbox_id).filter(|relay| relay.is_running())
        };
        let name = compose::container_name(sandbox_id);
        let inspect = self.adapter.inspect(&name).await?;
        if sandbox_from_inspect(sandbox_id, &inspect).is_none() {
            return Err(ServiceError::Contract {
                code: error_codes::SANDBOX_NOT_FOUND,
                message: "sandbox identity does not match".to_string(),
            });
        }
        let token = self
            .adapter
            .execd_access_token(&name)
            .await?
            .ok_or_else(|| ServiceError::Contract {
                code: error_codes::EXECD_START_FAILED,
                message: "sandbox execd credential is missing".to_string(),
            })?;
        // The credential that is read back has to be the one this
        // server minted for THIS sandbox. A credential somebody else
        // chose is refused: handing it over would give that somebody
        // the authorization of the execution plane.
        if !self.credential_is_ours(sandbox_id, &token).await {
            return Err(ServiceError::Contract {
                code: error_codes::EXECD_START_FAILED,
                message: "the credential of the sandbox is not the one this server minted"
                    .to_string(),
            });
        }
        let relay = match reusable {
            Some(relay) => relay,
            None => {
                let loader_name = self.adapter.container_loader_name(&name).await?;
                crate::relay::start(
                    self.adapter.client(),
                    name,
                    vec![
                        format!("{}/{}", compose::OPENSANDBOX_DIR, loader_name),
                        format!("{}/busybox", compose::OPENSANDBOX_DIR),
                        "nc".into(),
                    ],
                )
                .await
                .map_err(|_| ServiceError::Contract {
                    code: error_codes::EXECD_START_FAILED,
                    message: "could not start the isolated control relay".to_string(),
                })?
            }
        };
        let host_port = {
            // The relay that stays in the registry is the one the
            // client is sent to. Two concurrent resolutions can both
            // start a listener, and the one that loses the registry
            // would be dropped: its port would answer nobody.
            let mut relays = self.relays.lock().await;
            match relays.get(sandbox_id) {
                Some(existing) if existing.is_running() => existing.local_addr().port(),
                _ => {
                    let port = relay.local_addr().port();
                    relays.insert(sandbox_id.to_string(), relay);
                    port
                }
            }
        };
        let headers = Some(BTreeMap::from([(
            "X-EXECD-ACCESS-TOKEN".to_string(),
            token,
        )]));
        Ok(Endpoint {
            endpoint: format!("{}:{host_port}", self.config.trust.public_origin),
            headers,
        })
    }
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

/// One environment observed in the daemon, with what it takes of the host.
struct ObservedSandbox {
    sandbox: Sandbox,
    resources: EnvironmentResources,
    /// The creation intent it came from, when it carries one.
    creation: Option<CreationRecord>,
}

/// The creation intent an environment carries in its labels.
struct CreationRecord {
    key: String,
    fingerprint: String,
}

/// The resources a container takes of the host, read back from it.
///
/// The limits are the ones the container REALLY runs under: the ones the
/// daemon applied, not the ones the request asked for.
fn resources_from_inspect(
    inspect: &bollard::models::ContainerInspectResponse,
) -> EnvironmentResources {
    let host = inspect.host_config.as_ref();
    EnvironmentResources {
        slots: 1,
        memory_bytes: host.and_then(|host| host.memory).unwrap_or(0),
        nano_cpus: host.and_then(|host| host.nano_cpus).unwrap_or(0),
    }
}

/// The answer of a creation that already happened, read back from the
/// daemon.
///
/// It is the same shape the creation answered with: the environment that
/// the key produced, described as it is now.
fn creation_response_from_inspect(
    sandbox_id: &str,
    inspect: &bollard::models::ContainerInspectResponse,
) -> Option<CreateSandboxResponse> {
    let sandbox = sandbox_from_inspect(sandbox_id, inspect)?;
    Some(CreateSandboxResponse {
        id: sandbox.id,
        image_id: inspect.image.clone().filter(|image| !image.is_empty()),
        status: sandbox.status,
        metadata: sandbox.metadata,
        expires_at: sandbox.expires_at,
        created_at: sandbox.created_at,
        entrypoint: sandbox.entrypoint,
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
    fn a_network_of_a_live_sandbox_is_never_reaped() {
        let now = Utc::now();
        let old = now - chrono::Duration::hours(2);
        assert!(!is_reapable_orphan(
            "sbx-1",
            &["sbx-1".to_string()],
            &[],
            Some(old),
            now
        ));
    }

    #[test]
    fn a_network_of_a_creation_in_flight_is_never_reaped() {
        // The sandbox has its network before it has a container: a
        // reaper that ignored the age would delete the network of a
        // creation that is in flight and make it fail.
        let now = Utc::now();
        let fresh = now - chrono::Duration::seconds(30);
        assert!(!is_reapable_orphan("sbx-2", &[], &[], Some(fresh), now));
    }

    #[test]
    fn a_creation_in_flight_is_never_reaped_however_old_its_network_is() {
        // The age of the network is not a fact about the creation: a
        // creation that takes long (a slow host, a busy daemon) must not
        // lose its network because the margin elapsed.
        let now = Utc::now();
        let old = now - chrono::Duration::hours(2);
        assert!(!is_reapable_orphan(
            "sbx-5",
            &[],
            &["sbx-5".to_string()],
            Some(old),
            now
        ));
    }

    #[test]
    fn an_old_network_without_a_sandbox_is_reaped() {
        let now = Utc::now();
        let old = now - chrono::Duration::hours(2);
        assert!(is_reapable_orphan("sbx-3", &[], &[], Some(old), now));
    }

    #[test]
    fn a_network_of_an_unknown_age_is_left_alone() {
        let now = Utc::now();
        assert!(!is_reapable_orphan("sbx-4", &[], &[], None, now));
    }

    #[tokio::test]
    #[ignore = "requires Docker and a locally available opensandbox/execd:v1.1.0 image"]
    async fn orphan_network_cleanup_uses_original_proxy_after_reconfiguration() {
        struct Cleanup {
            proxy: String,
            other: String,
            network: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &self.other])
                    .output();
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &self.proxy])
                    .output();
                let _ = std::process::Command::new("docker")
                    .args(["network", "rm", &self.network])
                    .output();
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let proxy = format!("hodei-cleanup-test-{id}");
        let other = format!("hodei-cleanup-other-{id}");
        let network = format!("hodei-sandbox-{id}");
        let _cleanup = Cleanup {
            proxy: proxy.clone(),
            other: other.clone(),
            network: network.clone(),
        };
        let run = std::process::Command::new("docker")
            .args([
                "run",
                "-d",
                "--network",
                "bridge",
                "--name",
                &proxy,
                "opensandbox/execd:v1.1.0",
                "tail",
                "-f",
                "/dev/null",
            ])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let docker = bollard::Docker::connect_with_unix_defaults().unwrap();
        let adapter = DockerAdapter::connect(docker, "opensandbox/execd:v1.1.0");
        let config = ServiceConfig::default();
        adapter
            .create_sandbox_network(&config.trust.network, &id, &proxy)
            .await
            .unwrap();
        // An unrelated endpoint causes removal to fail; it must not be detached.
        let run_other = std::process::Command::new("docker")
            .args([
                "run",
                "-d",
                "--network",
                &network,
                "--name",
                &other,
                "opensandbox/execd:v1.1.0",
                "tail",
                "-f",
                "/dev/null",
            ])
            .output()
            .unwrap();
        assert!(
            run_other.status.success(),
            "{}",
            String::from_utf8_lossy(&run_other.stderr)
        );
        // A new service with no proxy configured simulates restart/reconfiguration.
        let service = LifecycleService::new(adapter.clone(), config);
        // The removal is RETIRED before it is reported as a failure:
        // Docker releases the endpoint of a disconnected container
        // asynchronously, and a removal that gives up at the first
        // refusal leaves the network on the host.
        let started = std::time::Instant::now();
        assert!(service.delete_sandbox(&id).await.is_err());
        assert!(
            started.elapsed()
                >= crate::docker::NETWORK_REMOVAL_DELAY
                    * (crate::docker::NETWORK_REMOVAL_ATTEMPTS as u32 - 1),
            "the removal gave up without retrying: {:?}",
            started.elapsed()
        );
        assert!(adapter.inspect(&other).await.is_ok());
        let removed = std::process::Command::new("docker")
            .args(["rm", "-f", &other])
            .output()
            .unwrap();
        assert!(removed.status.success());
        service.delete_sandbox(&id).await.unwrap();
        assert!(adapter.inspect_network(&network).await.is_err());
        assert!(
            adapter.inspect(&proxy).await.is_ok(),
            "proxy itself must survive cleanup"
        );
        assert_eq!(
            service.delete_sandbox(&id).await.unwrap_err().code(),
            error_codes::SANDBOX_NOT_FOUND
        );
    }

    /// Regression: a creation that does not finish must not leave its
    /// network on the host. The network is created BEFORE the
    /// container, so a creation that fails — or that is abandoned
    /// because the client gave up — used to leave a bridge nobody
    /// owned, invisible to the operator until the reaper removed it.
    #[tokio::test]
    #[ignore = "needs Docker"]
    async fn an_abandoned_creation_leaves_no_network() {
        struct Cleanup {
            proxy: String,
            network: String,
        }
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::process::Command::new("docker")
                    .args(["rm", "-f", &self.proxy])
                    .output();
                let _ = std::process::Command::new("docker")
                    .args(["network", "rm", &self.network])
                    .output();
            }
        }
        let id = uuid::Uuid::new_v4().to_string();
        let proxy = format!("hodei-guard-test-{id}");
        let network = format!("hodei-sandbox-{id}");
        let _cleanup = Cleanup {
            proxy: proxy.clone(),
            network: network.clone(),
        };
        let run = std::process::Command::new("docker")
            .args([
                "run",
                "-d",
                "--network",
                "bridge",
                "--name",
                &proxy,
                "opensandbox/execd:v1.1.0",
                "tail",
                "-f",
                "/dev/null",
            ])
            .output()
            .unwrap();
        assert!(
            run.status.success(),
            "{}",
            String::from_utf8_lossy(&run.stderr)
        );
        let docker = bollard::Docker::connect_with_unix_defaults().unwrap();
        let adapter = DockerAdapter::connect(docker, "opensandbox/execd:v1.1.0");
        let config = ServiceConfig::default();
        adapter
            .create_sandbox_network(&config.trust.network, &id, &proxy)
            .await
            .unwrap();
        assert!(
            adapter.inspect_network(&network).await.is_ok(),
            "the network of the creation must exist before the abandonment"
        );
        // The creation is abandoned: the guard goes away with it.
        {
            let in_flight: InFlight =
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
            let _guard = CreationGuard::new(adapter.clone(), &id, in_flight);
        }
        // The removal runs on its own task, so the assertion waits for
        // it instead of assuming it already happened.
        let mut removed = false;
        for _ in 0..100 {
            if adapter.inspect_network(&network).await.is_err() {
                removed = true;
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        assert!(
            removed,
            "the network of the abandoned creation is still on the host"
        );
        assert!(
            adapter.inspect(&proxy).await.is_ok(),
            "the trusted proxy must survive the cleanup of the creation"
        );
    }

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

#[cfg(test)]
mod admission_tests;
