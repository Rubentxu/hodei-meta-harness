//! Pure composition logic ported from the reference OpenSandbox Docker server
//! (`container_ops.py`). No I/O here — everything is testable offline.

use std::collections::BTreeMap;

use chrono::{DateTime, SecondsFormat, Utc};

use crate::model::{CreateSandboxRequest, PlatformSpec, error_codes};

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

/// Hardening defaults mirroring the example `~/.sandbox.toml` (docker section).
#[derive(Debug, Clone)]
pub struct DockerHardening {
    pub network_mode: String,
    pub no_new_privileges: bool,
    pub drop_capabilities: Vec<String>,
    pub pids_limit: Option<i64>,
}

impl Default for DockerHardening {
    fn default() -> Self {
        Self {
            network_mode: "bridge".into(),
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
    pub network_mode: String,
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

/// Environment list (reference `_build_labels_and_env`): config env merged
/// under request env, then `EXECD_INIT=1` and `OPENSANDBOX_ID`.
pub fn build_env(
    server_env: &BTreeMap<String, String>,
    request_env: Option<&BTreeMap<String, String>>,
    sandbox_id: &str,
    execd_run_as_init: bool,
) -> Vec<String> {
    let mut merged: BTreeMap<&String, &String> = BTreeMap::new();
    for (k, v) in server_env {
        merged.insert(k, v);
    }
    if let Some(req) = request_env {
        for (k, v) in req {
            merged.insert(k, v);
        }
    }
    let mut env: Vec<String> = merged
        .into_iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect();
    if execd_run_as_init {
        env.push("EXECD_INIT=1".to_string());
    }
    env.push(format!("OPENSANDBOX_ID={sandbox_id}"));
    env
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
    if cpus <= 0.0 {
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

/// Base HostConfig spec (reference `_base_host_config_kwargs`) with
/// auto-assigned host ports for the two exposed container ports.
pub fn build_host_spec(
    hardening: &DockerHardening,
    memory: Option<i64>,
    nano_cpus: Option<i64>,
) -> HostSpec {
    let mut security_opt = Vec::new();
    if hardening.no_new_privileges {
        security_opt.push("no-new-privileges=true".to_string());
    }
    HostSpec {
        network_mode: hardening.network_mode.clone(),
        security_opt,
        cap_drop: hardening.drop_capabilities.clone(),
        pids_limit: hardening.pids_limit,
        memory,
        nano_cpus,
        auto_assign_ports: true,
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
    fn env_merges_request_over_server_and_appends_identity() {
        let server = BTreeMap::from([
            ("A".to_string(), "1".to_string()),
            ("B".to_string(), "server".to_string()),
        ]);
        let request = BTreeMap::from([("B".to_string(), "req".to_string())]);
        let env = build_env(&server, Some(&request), "sbx3", true);
        assert_eq!(
            env,
            vec!["A=1", "B=req", "EXECD_INIT=1", "OPENSANDBOX_ID=sbx3"]
        );
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
    }

    #[test]
    fn host_spec_applies_reference_hardening() {
        let hardening = DockerHardening::default();
        let spec = build_host_spec(&hardening, Some(1024), Some(1_000_000_000));
        assert_eq!(spec.network_mode, "bridge");
        assert!(
            spec.security_opt
                .contains(&"no-new-privileges=true".to_string())
        );
        assert!(spec.cap_drop.contains(&"NET_RAW".to_string()));
        assert_eq!(spec.cap_drop.len(), 9);
        assert_eq!(spec.pids_limit, Some(4096));
        assert_eq!(spec.memory, Some(1024));
        assert!(spec.auto_assign_ports);
    }

    #[test]
    fn container_name_uses_reference_prefix() {
        assert_eq!(container_name("abc"), "sandbox-abc");
    }
}
