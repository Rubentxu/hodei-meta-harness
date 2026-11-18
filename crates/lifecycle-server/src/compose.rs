//! Pure composition logic ported from the reference OpenSandbox Docker server
//! (`container_ops.py`). No I/O here — everything is testable offline.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::model::{CreateSandboxRequest, PlatformSpec, error_codes};
use crate::policy::NetworkPolicy;

// Exact label keys of the reference server (services/constants.py).
pub const RESERVED_LABEL_PREFIX: &str = "opensandbox.io/";
pub const LABEL_ID: &str = "opensandbox.io/id";
pub const LABEL_EXPIRES_AT: &str = "opensandbox.io/expires-at";
pub const LABEL_CREATED_AT: &str = "opensandbox.io/created-at";
pub const LABEL_MANUAL_CLEANUP: &str = "opensandbox.io/manual-cleanup";
pub const LABEL_PLATFORM_OS: &str = "opensandbox.io/platform-os";
pub const LABEL_PLATFORM_ARCH: &str = "opensandbox.io/platform-arch";
pub const LABEL_METADATA_KEYS: &str = "opensandbox.io/metadata-keys";
pub const LABEL_EXECD_HOST_PORT: &str = "opensandbox.io/embedding-proxy-port";
pub const LABEL_HTTP_HOST_PORT: &str = "opensandbox.io/http-port";

/// Container-internal ports exposed by every sandbox (docker_service.py).
pub const EXECD_CONTAINER_PORT: &str = "44772/tcp";
pub const HTTP_CONTAINER_PORT: &str = "8080/tcp";
pub const CONTAINER_NAME_PREFIX: &str = "sandbox-";
pub const EXECED_INSTALL_PATH: &str = "/opt/opensandbox/execd";
pub const BOOTSTRAP_PATH: &str = "/opt/opensandbox/bootstrap.sh";
pub const OPENSANDBOX_DIR: &str = "/opt/opensandbox";

/// The name of the environment variable that carries the credential of
/// the execution plane. The execd reads it from the environment of its
/// own process: the credential never travels in a command, a label or a
/// record.
pub const EXECD_ACCESS_TOKEN_NAME: &str = "EXECD_ACCESS_TOKEN";

/// The environment variable that carries the credential of the
/// execution plane, as it appears in the environment of a container.
pub const EXECD_ACCESS_TOKEN_ENV: &str = "EXECD_ACCESS_TOKEN=";

/// Hardening defaults mirroring the example `~/.sandbox.toml` (docker section).
#[derive(Debug, Clone)]
pub struct DockerHardening {
    pub no_new_privileges: bool,
    pub drop_capabilities: Vec<String>,
    pub pids_limit: Option<i64>,
}

impl Default for DockerHardening {
    fn default() -> Self {
        Self {
            no_new_privileges: true,
            drop_capabilities: [
                "AUDIT_WRITE",
                "MKNOD",
                "NET_ADMIN",
                "NET_RAW",
                "SYS_ADMIN",
                "SYS_MODULE",
                "SYS_PTRACE",
                "SYS_TIME",
                "SYS_TTY_CONFIG",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            pids_limit: Some(4096),
        }
    }
}

/// Container-level configuration produced by the pure composition layer,
/// mapped 1:1 to bollard types in the docker module.
#[derive(Debug, Clone, Default)]
pub struct HostSpec {
    /// The network the container is attached to. It is the network the
    /// policy names, never the default bridge.
    pub network_mode: String,
    /// The address the published ports are bound to. The loopback by
    /// default: a published port is reachable from the host, and from
    /// nowhere else.
    pub publish_address: String,
    pub security_opt: Vec<String>,
    pub cap_drop: Vec<String>,
    pub pids_limit: Option<i64>,
    pub memory: Option<i64>,
    pub nano_cpus: Option<i64>,
    /// Auto-assign host ports: empty HostPort lets the daemon pick, then the
    /// real mapping is read back from container inspect.
    pub auto_assign_ports: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ValidationError {
    pub code: &'static str,
    pub message: String,
}

pub fn validation_error(code: &'static str, message: impl Into<String>) -> ValidationError {
    ValidationError {
        code,
        message: message.into(),
    }
}

/// `ensure_entrypoint` + `ensure_metadata_labels` + `ensure_timeout_within_limit`
/// from the reference create path, for the request subset we implement.
pub fn validate_create_request(
    req: &CreateSandboxRequest,
    max_timeout_seconds: i64,
) -> Result<(), ValidationError> {
    let entrypoint = req.entrypoint.as_deref().unwrap_or(&[]);
    if entrypoint.is_empty() {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            "entrypoint is required when creating a sandbox from an image",
        ));
    }
    if req.image.is_none() {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            "image is required (snapshots and templates are not implemented; contract subset)",
        ));
    }
    if let Some(bad) = req.metadata.as_ref().and_then(|metadata| {
        metadata
            .keys()
            .find(|k| k.starts_with(RESERVED_LABEL_PREFIX))
    }) {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!(
                "metadata key {bad:?} collides with the reserved label prefix {RESERVED_LABEL_PREFIX:?}"
            ),
        ));
    }
    if let Some(timeout) = req.timeout {
        if timeout < 0 {
            return Err(validation_error(
                error_codes::INVALID_PARAMETER,
                format!("timeout must be a non-negative number of seconds, got {timeout}"),
            ));
        }
        if timeout > max_timeout_seconds {
            return Err(validation_error(
                error_codes::INVALID_PARAMETER,
                format!("timeout {timeout}s exceeds the server maximum of {max_timeout_seconds}s"),
            ));
        }
    }
    Ok(())
}

/// Metadata-derived labels + reserved labels (reference `_build_labels_and_env`).
pub fn build_labels(
    sandbox_id: &str,
    request: &CreateSandboxRequest,
    created_at: DateTime<Utc>,
    expires_at: Option<DateTime<Utc>>,
    platform: Option<&PlatformSpec>,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    if let Some(metadata) = &request.metadata {
        for (k, v) in metadata {
            labels.insert(k.clone(), v.clone());
        }
        let keys: Vec<&str> = metadata.keys().map(|s| s.as_str()).collect();
        labels.insert(
            LABEL_METADATA_KEYS.to_string(),
            serde_json::to_string(&keys).expect("sorted string keys serialize infallibly"),
        );
    }
    labels.insert(LABEL_ID.to_string(), sandbox_id.to_string());
    match expires_at {
        Some(exp) => {
            labels.insert(
                LABEL_EXPIRES_AT.to_string(),
                exp.to_rfc3339_opts(SecondsFormat::Micros, true),
            );
        }
        None => {
            labels.insert(LABEL_MANUAL_CLEANUP.to_string(), "true".to_string());
        }
    }
    labels.insert(
        LABEL_CREATED_AT.to_string(),
        created_at.to_rfc3339_opts(SecondsFormat::Micros, true),
    );
    if let Some(p) = platform {
        labels.insert(LABEL_PLATFORM_OS.to_string(), p.os.clone());
        labels.insert(LABEL_PLATFORM_ARCH.to_string(), p.arch.clone());
    }
    labels
}

/// The prefixes of the environment entries the server owns.
///
/// The credential and the identity of the sandbox are read back from
/// the container by prefix. A key like `EXECD_ACCESS_TOKEN=x` would
/// produce the entry `EXECD_ACCESS_TOKEN=x=...`, which carries the
/// same prefix and would be read back INSTEAD of the credential the
/// server minted: the whole prefix is reserved, not only its exact
/// name.
const RESERVED_ENV_PREFIXES: [&str; 3] = [EXECD_ACCESS_TOKEN_ENV, "OPENSANDBOX_ID=", "EXECD_INIT="];

/// The namespaces the execution plane reads its own configuration from.
///
/// The execd takes its configuration from the environment of its own
/// process. A request that may write into that namespace would steer
/// the execution plane, so it may not: the operator owns it.
const EXECUTION_PLANE_PREFIXES: [&str; 2] = ["OPENSANDBOX_", "EXECD_"];

/// True when the entry `key=value` would shadow one of the entries the
/// server owns.
fn shadows_a_reserved_entry(key: &str) -> bool {
    let entry = format!("{key}=");
    RESERVED_ENV_PREFIXES
        .iter()
        .any(|prefix| entry.starts_with(prefix))
}

/// True when the key belongs to the namespace of the execution plane.
fn belongs_to_the_execution_plane(key: &str) -> bool {
    EXECUTION_PLANE_PREFIXES
        .iter()
        .any(|prefix| key.starts_with(prefix))
}

/// Environment list (reference `_build_labels_and_env`): request env merged
/// UNDER the config env, then the identity of the sandbox.
///
/// The identity comes LAST: a request may not choose the identity of the
/// environment it asks for, and the credential of the execution plane
/// belongs to the server.
///
/// The request does NOT win over the operator's configuration: the
/// environment of the sandbox is read by the execd, and a client that
/// could overwrite the operator's variables would be able to steer the
/// execution plane. A request may only add the keys the operator did
/// not set, and never in the namespace the execution plane reads.
pub fn build_env(
    server_env: &BTreeMap<String, String>,
    request_env: Option<&BTreeMap<String, String>>,
    sandbox_id: &str,
    execd_run_as_init: bool,
    execd_access_token: &str,
) -> Vec<String> {
    let mut merged: BTreeMap<&String, &String> = BTreeMap::new();
    if let Some(req) = request_env {
        for (k, v) in req {
            // The namespace of the execution plane belongs to the
            // operator: a request that writes into it would steer the
            // execd, whatever the value the operator gave.
            if belongs_to_the_execution_plane(k) {
                continue;
            }
            merged.insert(k, v);
        }
    }
    for (k, v) in server_env {
        merged.insert(k, v);
    }
    merged.retain(|key, _| !shadows_a_reserved_entry(key));
    let mut env: Vec<String> = merged
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if execd_run_as_init {
        env.push("EXECD_INIT=1".to_string());
    }
    env.push(format!("OPENSANDBOX_ID={sandbox_id}"));
    env.push(format!("{EXECD_ACCESS_TOKEN_ENV}{execd_access_token}"));
    env
}

/// The credential of the execution plane, read back from the
/// environment of a container.
///
/// The entries are matched by EXACT name: an entry that merely starts
/// with the name of the variable (`EXECD_ACCESS_TOKEN=x`) belongs to
/// another variable and must not be taken as the credential. Two
/// entries with the exact name make the credential ambiguous, and an
/// ambiguous credential is no credential: the read fails closed.
pub fn token_from_env(env: Vec<String>) -> Option<String> {
    let mut found: Option<String> = None;
    for entry in env {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        if name != EXECD_ACCESS_TOKEN_NAME {
            continue;
        }
        if found.is_some() {
            return None;
        }
        found = Some(value.to_string());
    }
    found
}

/// Parse a Docker-style memory limit ("2Gi", "512Mi", "262144000", plain bytes).
pub fn parse_memory_limit(raw: &str) -> Result<i64, ValidationError> {
    let s = raw.trim();
    if s.is_empty() {
        return Err(bad_memory(raw));
    }
    let units: [(&str, i64); 6] = [
        ("Ki", 1024),
        ("Mi", 1024 * 1024),
        ("Gi", 1024 * 1024 * 1024),
        ("Ti", 1024i64 * 1024 * 1024 * 1024),
        ("K", 1000),
        ("M", 1000 * 1000),
    ];
    for (suffix, mult) in units {
        if let Some(num) = s.strip_suffix(suffix) {
            let value: f64 = num.trim().parse().map_err(|_| bad_memory(raw))?;
            let bytes = (value * mult as f64) as i64;
            if bytes <= 0 {
                return Err(bad_memory(raw));
            }
            return Ok(bytes);
        }
    }
    let bytes: i64 = s.parse().map_err(|_| bad_memory(raw))?;
    if bytes <= 0 {
        return Err(bad_memory(raw));
    }
    Ok(bytes)
}

fn bad_memory(raw: &str) -> ValidationError {
    validation_error(
        error_codes::INVALID_PARAMETER,
        format!(
            "Invalid resourceLimits.memory: {raw:?}; must resolve to a positive resource limit"
        ),
    )
}

/// Parse a CPU request ("1", "0.5") into Docker nanoscpus.
pub fn parse_cpu_nano_cpus(raw: &str) -> Result<i64, ValidationError> {
    let cpus: f64 = raw.trim().parse().map_err(|_| {
        validation_error(
            error_codes::INVALID_PARAMETER,
            format!("Invalid resourceLimits.cpu: {raw:?}; must be a positive CPU count"),
        )
    })?;
    if !cpus.is_finite() || cpus < 0.000_000_001 || cpus > i64::MAX as f64 / 1_000_000_000.0 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!("Invalid resourceLimits.cpu: {raw:?}; must be a positive CPU count"),
        ));
    }
    Ok((cpus * 1_000_000_000.0) as i64)
}

/// Resolve resource limits from the request, mirroring
/// `_resolve_resource_limits` for the cpu/memory subset.
pub fn resolve_resource_limits(
    req: &CreateSandboxRequest,
) -> Result<(Option<i64>, Option<i64>), ValidationError> {
    let Some(limits) = &req.resource_limits else {
        return Ok((None, None));
    };
    let memory = match &limits.memory {
        Some(raw) => Some(parse_memory_limit(raw)?),
        None => None,
    };
    let nano_cpus = match &limits.cpu {
        Some(raw) => Some(parse_cpu_nano_cpus(raw)?),
        None => None,
    };
    Ok((memory, nano_cpus))
}

/// Base HostConfig spec. The isolated relay does not publish sandbox ports.
pub fn build_host_spec(
    hardening: &DockerHardening,
    memory: Option<i64>,
    nano_cpus: Option<i64>,
    network: &NetworkPolicy,
    publish_address: &str,
) -> HostSpec {
    let mut security_opt = Vec::new();
    if hardening.no_new_privileges {
        security_opt.push("no-new-privileges=true".to_string());
    }
    HostSpec {
        network_mode: network.network_name().to_string(),
        publish_address: publish_address.to_string(),
        security_opt,
        cap_drop: hardening.drop_capabilities.clone(),
        pids_limit: hardening.pids_limit,
        memory,
        nano_cpus,
        auto_assign_ports: false,
    }
}

pub fn container_name(sandbox_id: &str) -> String {
    format!("{CONTAINER_NAME_PREFIX}{sandbox_id}")
}

/// TTL decision (pure): ids whose recorded `expires_at` is due at `now`.
/// Sandboxes without an expiry (manual cleanup) are NEVER selected — the
/// reaper only enforces the contract the create request asked for.
pub fn expired_sandboxes(
    entries: &[(String, Option<chrono::DateTime<chrono::Utc>>)],
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<String> {
    entries
        .iter()
        .filter(|(_, expires_at)| expires_at.is_some_and(|exp| exp <= now))
        .map(|(id, _)| id.clone())
        .collect()
}

/// Admission decision (pure): whether a NEW environment may be created
/// given the live environments and the global slot budget.
///
/// The budget is enforced in the CREATE PATH of the server, not in a
/// client: a client-side ledger only bounds the clients that know
/// about it. `None` means "no limit configured".
///
/// The rule itself (the free slots are `budget - live`) lives in
/// `hodei-mh-budget`, so the server and the clients that predict its
/// behaviour cannot drift apart.
pub fn admit_creation(live_environments: usize, slot_budget: Option<usize>) -> Result<(), String> {
    match slot_budget {
        None => Ok(()),
        Some(budget) if budget::ledger::admissible_creates(live_environments, budget) > 0 => Ok(()),
        Some(budget) => Err(format!(
            "global environment budget exhausted: {live_environments} of {budget} slots in use; \
             no environment is created and the caller must WAIT (no silent admission)"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn req() -> CreateSandboxRequest {
        serde_json::from_value(serde_json::json!({
            "image": {"uri": "img:1"},
            "entrypoint": ["sleep", "infinity"]
        }))
        .unwrap()
    }

    #[test]
    fn validate_rejects_missing_entrypoint() {
        let mut r: CreateSandboxRequest =
            serde_json::from_value(serde_json::json!({"image": {"uri": "img:1"}})).unwrap();
        let err = validate_create_request(&r, 86_400).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_PARAMETER);
        assert!(err.message.contains("entrypoint"));
        r.entrypoint = Some(vec!["sh".into()]);
        assert!(validate_create_request(&r, 86_400).is_ok());
    }

    #[test]
    fn validate_rejects_missing_image_and_reserved_metadata() {
        let r: CreateSandboxRequest =
            serde_json::from_value(serde_json::json!({"entrypoint": ["x"]})).unwrap();
        assert_eq!(
            validate_create_request(&r, 86_400).unwrap_err().code,
            error_codes::INVALID_PARAMETER
        );
        let mut r2 = req();
        r2.metadata = Some(BTreeMap::from([(
            "opensandbox.io/id".to_string(),
            "x".to_string(),
        )]));
        let err = validate_create_request(&r2, 86_400).unwrap_err();
        assert!(err.message.contains("reserved label prefix"));
    }

    #[test]
    fn validate_enforces_timeout_bounds() {
        let mut r = req();
        r.timeout = Some(90_000);
        let err = validate_create_request(&r, 86_400).unwrap_err();
        assert!(err.message.contains("exceeds"));
        r.timeout = Some(-1);
        assert!(validate_create_request(&r, 86_400).is_err());
        r.timeout = Some(86_400);
        assert!(validate_create_request(&r, 86_400).is_ok());
        r.timeout = None; // manual cleanup
        assert!(validate_create_request(&r, 86_400).is_ok());
    }

    #[test]
    fn labels_carry_reference_keys_and_metadata_keys_json() {
        let mut r = req();
        r.metadata = Some(BTreeMap::from([
            ("team".to_string(), "gc".to_string()),
            ("a".to_string(), "1".to_string()),
        ]));
        let created = Utc.with_ymd_and_hms(2026, 11, 17, 10, 0, 0).unwrap();
        let labels = build_labels("sbx1", &r, created, None, None);
        assert_eq!(labels[LABEL_ID], "sbx1");
        assert_eq!(labels[LABEL_MANUAL_CLEANUP], "true");
        assert_eq!(labels[LABEL_CREATED_AT], "2026-11-17T10:00:00.000000Z");
        assert_eq!(labels[LABEL_METADATA_KEYS], r#"["a","team"]"#);
        assert_eq!(labels["team"], "gc");
    }

    #[test]
    fn labels_use_expires_at_when_timeout_present() {
        let r = req();
        let created = Utc.with_ymd_and_hms(2026, 11, 17, 10, 0, 0).unwrap();
        let exp = created + chrono::Duration::seconds(1800);
        let labels = build_labels("sbx2", &r, created, Some(exp), None);
        assert!(!labels.contains_key(LABEL_MANUAL_CLEANUP));
        assert_eq!(labels[LABEL_EXPIRES_AT], "2026-11-17T10:30:00.000000Z");
    }

    #[test]
    fn expiry_selects_only_due_sandboxes_and_never_manual_cleanup_ones() {
        let now = Utc.with_ymd_and_hms(2026, 11, 17, 12, 0, 0).unwrap();
        let entries = vec![
            (
                "due-past".to_string(),
                Some(now - chrono::Duration::seconds(1)),
            ),
            ("due-exactly".to_string(), Some(now)),
            (
                "future".to_string(),
                Some(now + chrono::Duration::seconds(60)),
            ),
            ("manual".to_string(), None),
        ];
        let targets = expired_sandboxes(&entries, now);
        assert_eq!(
            targets,
            vec!["due-past".to_string(), "due-exactly".to_string()]
        );
    }

    #[test]
    fn the_slot_budget_is_enforced_before_the_environment_exists() {
        assert!(admit_creation(0, Some(2)).is_ok());
        assert!(admit_creation(1, Some(2)).is_ok());
        let refused = admit_creation(2, Some(2)).expect_err("the budget is exhausted");
        assert!(refused.contains("budget exhausted"), "{refused}");
        assert!(refused.contains("WAIT"), "{refused}");
        // No budget configured: every creation is admitted.
        assert!(admit_creation(999, None).is_ok());
    }

    #[test]
    fn env_merges_the_request_under_the_server_and_appends_identity() {
        let server = BTreeMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "server".to_string()),
        ]);
        let request = BTreeMap::from([("B".to_string(), "req".to_string())]);
        let env = build_env(&server, Some(&request), "sbx3", true, "token-1");
        assert_eq!(
            env,
            vec![
                "A=1",
                "B=server",
                "EXECD_INIT=1",
                "OPENSANDBOX_ID=sbx3",
                "EXECD_ACCESS_TOKEN=token-1"
            ]
        );
    }

    #[test]
    fn a_request_cannot_steer_the_execution_plane_through_the_environment() {
        // The environment of the sandbox is read by the execd. A client
        // that could overwrite the operator's variables would steer the
        // execution plane, so the operator's configuration wins.
        let server = BTreeMap::from([
            (
                "OPENSANDBOX_LISTEN".to_string(),
                "127.0.0.1:44772".to_string(),
            ),
            ("SANDBOX_ENV".to_string(), "operator".to_string()),
        ]);
        let request = BTreeMap::from([
            (
                "OPENSANDBOX_LISTEN".to_string(),
                "0.0.0.0:44772".to_string(),
            ),
            ("SANDBOX_ENV".to_string(), "caller".to_string()),
            ("EXTRA".to_string(), "caller".to_string()),
        ]);
        let env = build_env(&server, Some(&request), "sbx5", true, "token-3");
        assert!(
            env.contains(&"OPENSANDBOX_LISTEN=127.0.0.1:44772".to_string()),
            "{env:?}"
        );
        assert!(env.contains(&"SANDBOX_ENV=operator".to_string()), "{env:?}");
        assert!(env.contains(&"EXTRA=caller".to_string()), "{env:?}");
        assert!(
            !env.contains(&"OPENSANDBOX_LISTEN=0.0.0.0:44772".to_string()),
            "{env:?}"
        );
    }

    #[test]
    fn the_identity_of_the_sandbox_cannot_be_chosen_by_the_request() {
        // A request that asks for its own credential does not get it:
        // the identity of the environment belongs to the server.
        let request = BTreeMap::from([
            ("OPENSANDBOX_ID".to_string(), "someone-else".to_string()),
            (
                "EXECD_ACCESS_TOKEN".to_string(),
                "a-token-of-the-caller".to_string(),
            ),
        ]);
        let env = build_env(&BTreeMap::new(), Some(&request), "sbx4", true, "token-2");
        assert!(env.contains(&"OPENSANDBOX_ID=sbx4".to_string()), "{env:?}");
        assert!(
            env.contains(&"EXECD_ACCESS_TOKEN=token-2".to_string()),
            "{env:?}"
        );
        assert_eq!(
            env.iter()
                .filter(|entry| entry.starts_with("EXECD_ACCESS_TOKEN="))
                .count(),
            1
        );
        assert_eq!(
            env.iter()
                .filter(|entry| entry.starts_with("OPENSANDBOX_ID="))
                .count(),
            1
        );
        assert!(
            !env.iter()
                .any(|entry| entry.contains("a-token-of-the-caller"))
        );
    }

    #[test]
    fn a_request_cannot_shadow_the_credential_with_a_prefixed_key() {
        // The credential is read back from the container by prefix. A
        // request that writes the name of the variable inside another
        // key produces an entry with the same prefix, and that entry
        // is the one the server would read back and hand out: the
        // caller would choose the credential of its own execution
        // plane.
        let request = BTreeMap::from([
            ("EXECD_ACCESS_TOKEN=x".to_string(), "y".to_string()),
            ("OPENSANDBOX_ID=z".to_string(), "w".to_string()),
        ]);
        let env = build_env(&BTreeMap::new(), Some(&request), "sbx6", true, "token-4");
        assert_eq!(
            env.iter()
                .filter(|entry| entry.starts_with("EXECD_ACCESS_TOKEN="))
                .count(),
            1,
            "{env:?}"
        );
        assert_eq!(
            env.iter()
                .filter(|entry| entry.starts_with("OPENSANDBOX_ID="))
                .count(),
            1,
            "{env:?}"
        );
        assert!(!env.iter().any(|entry| entry.contains("x=y")), "{env:?}");
        assert!(!env.iter().any(|entry| entry.contains("z=w")), "{env:?}");
    }

    #[test]
    fn a_request_cannot_write_into_the_namespace_of_the_execution_plane() {
        // The execd takes its configuration from the environment of its
        // own process. A request that may write into that namespace
        // would steer the execution plane, whatever the operator set.
        let request = BTreeMap::from([
            ("EXECD_LISTEN".to_string(), "0.0.0.0:44772".to_string()),
            (
                "OPENSANDBOX_LISTEN".to_string(),
                "0.0.0.0:44772".to_string(),
            ),
            ("APP_PORT".to_string(), "8080".to_string()),
        ]);
        let env = build_env(&BTreeMap::new(), Some(&request), "sbx7", true, "token-5");
        assert!(
            !env.iter().any(|entry| entry.starts_with("EXECD_")
                && !entry.starts_with("EXECD_INIT=")
                && !entry.starts_with("EXECD_ACCESS_TOKEN=")),
            "{env:?}"
        );
        assert!(
            !env.iter()
                .any(|entry| entry.starts_with("OPENSANDBOX_")
                    && !entry.starts_with("OPENSANDBOX_ID=")),
            "{env:?}"
        );
        assert!(env.contains(&"APP_PORT=8080".to_string()), "{env:?}");
    }

    #[test]
    fn the_credential_is_read_back_by_exact_name() {
        // A prefixed entry belongs to another variable, and an
        // ambiguous credential is no credential at all.
        assert_eq!(
            token_from_env(vec![
                "EXECD_ACCESS_TOKEN=the-real-one".to_string(),
                "APP=1".to_string(),
            ]),
            Some("the-real-one".to_string())
        );
        assert_eq!(
            token_from_env(vec![
                "EXECD_ACCESS_TOKEN=x=y".to_string(),
                "EXECD_ACCESS_TOKEN=the-real-one".to_string(),
            ]),
            None,
            "an ambiguous credential is refused, not guessed"
        );
        assert_eq!(
            token_from_env(vec![
                "EXECD_ACCESS_TOKEN=one".to_string(),
                "EXECD_ACCESS_TOKEN=two".to_string(),
            ]),
            None
        );
        assert_eq!(token_from_env(vec!["APP=1".to_string()]), None);
    }

    #[test]
    fn memory_parser_understands_suffixes_and_rejects_non_positive() {
        assert_eq!(parse_memory_limit("2Gi").unwrap(), 2 * 1024 * 1024 * 1024);
        assert_eq!(parse_memory_limit("512Mi").unwrap(), 512 * 1024 * 1024);
        assert_eq!(parse_memory_limit("1000").unwrap(), 1000);
        assert_eq!(parse_memory_limit("0.5Gi").unwrap(), 512 * 1024 * 1024);
        assert!(parse_memory_limit("-1").is_err());
        assert!(parse_memory_limit("0").is_err());
        assert!(parse_memory_limit("abc").is_err());
        assert!(parse_memory_limit("").is_err());
    }

    #[test]
    fn cpu_parser_converts_to_nano_cpus() {
        assert_eq!(parse_cpu_nano_cpus("1").unwrap(), 1_000_000_000);
        assert_eq!(parse_cpu_nano_cpus("0.5").unwrap(), 500_000_000);
        assert!(parse_cpu_nano_cpus("0").is_err());
        assert!(parse_cpu_nano_cpus("x").is_err());
        assert!(parse_cpu_nano_cpus("NaN").is_err());
        assert!(parse_cpu_nano_cpus("0.00000000001").is_err());
        assert!(parse_cpu_nano_cpus("inf").is_err());
    }

    #[test]
    fn host_spec_applies_reference_hardening() {
        let hardening = DockerHardening::default();
        let network = NetworkPolicy::parse(
            crate::policy::INTERNAL_EGRESS_PROXY,
            crate::policy::DEFAULT_SANDBOX_NETWORK,
            crate::policy::DEFAULT_SANDBOX_SUBNET,
        )
        .expect("the supported policy");
        let spec = build_host_spec(
            &hardening,
            Some(1024),
            Some(1_000_000_000),
            &network,
            "127.0.0.1",
        );
        assert_eq!(spec.network_mode, "hodei-mh-sandboxes");
        assert_eq!(spec.publish_address, "127.0.0.1");
        assert!(
            spec.security_opt
                .contains(&"no-new-privileges=true".to_string())
        );
        assert!(spec.cap_drop.contains(&"NET_RAW".to_string()));
        assert_eq!(spec.cap_drop.len(), 9);
        assert_eq!(spec.pids_limit, Some(4096));
        assert_eq!(spec.memory, Some(1024));
        assert!(!spec.auto_assign_ports);
    }

    #[test]
    fn container_name_uses_reference_prefix() {
        assert_eq!(container_name("abc"), "sandbox-abc");
    }
}
