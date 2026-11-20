//! `hodei-mh doctor` / `hodei-mh profile-run` (TASK-16): reproducible
//! language profiles. The doctor is strictly READ-ONLY; profile-run gates
//! every run through it and REFUSES (informatively, no YOLO fallback) when
//! the sandbox, the pinned image or the restricted egress is missing.
//! Credentials only ever travel as process env and never appear in
//! commands, logs or the run record.

use std::time::Duration;

use hodei_mh_workspace::export::{self, ExportPolicy};
use opensandbox::control::ControlPlane;
use opensandbox::execd::endpoint::ExecdEndpoint;
use profiles::build_plan::{self, BuildPlan};
use profiles::cache::{self, BuildCacheIdentity, CacheManifest, CacheSettings, RestoreVerdict};
use profiles::credentials::{self, SubscriptionNote};
use profiles::doctor::{self, DoctorCheck};
use profiles::gate::{self, GateDecision};
use profiles::profile::{self, ImagePin, ProfileSpec};
use release::idle::{IdleGate, ReleaseDecision, release_on_idle};
use release::reconcile::{
    CreateOutcome, OWNER_VALUE, OWNERSHIP_KEY, REQUEST_TAG_KEY, ReconcileDecision, RequestTag,
    TagLookup, reconcile_create,
};
use serde_json::{Value, json};

/// Docker DNS alias assigned to the trusted proxy in each sandbox network.
pub const DEFAULT_PROXY_URL: &str = "http://hodei-proxy:8888";

/// Bounded wait for the sandbox's execd to start answering. A sandbox
/// that never becomes usable is a FAILURE, not an infinite wait.
const EXECD_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// How much of a build output the record keeps. The output is REDACTED
/// before it is cut: the tail of a build that printed a credential must
/// not keep it.
const OUTPUT_TAIL_LIMIT: usize = 400;

/// Where a cache archive is staged inside the sandbox while it travels.
const CACHE_ARCHIVE_IN_SANDBOX: &str = "/tmp/hodei-build-cache.tgz";

/// Bounded wait for archiving/extracting a cache: a real build's cache is
/// large, but a wedged command must not hold the run forever.
const CACHE_IO_TIMEOUT: Duration = Duration::from_secs(300);

/// The host cache store: `$XDG_CACHE_HOME/hodei-mh/build-cache`, or
/// `$HOME/.cache/hodei-mh/build-cache`, or a temporary directory.
fn default_cache_store_root() -> String {
    let base = std::env::var("XDG_CACHE_HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            std::env::var("HOME")
                .ok()
                .filter(|value| !value.is_empty())
                .map(|home| format!("{home}/.cache"))
        })
        .unwrap_or_else(|| std::env::temp_dir().to_string_lossy().to_string());
    format!("{base}/hodei-mh/build-cache")
}

/// The dependency lock files that can change the meaning of a cache. The
/// JVM profile ships no download cache, so it has none here.
fn dependency_locks(project_dir: &std::path::Path) -> Vec<(&'static str, Vec<u8>)> {
    ["Cargo.lock"]
        .into_iter()
        .filter_map(|name| {
            std::fs::read(project_dir.join(name))
                .ok()
                .map(|bytes| (name, bytes))
        })
        .collect()
}

fn now_epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

/// Join the plan's cache directory with a scope subdirectory; an empty
/// relative name means the whole cache directory.
fn scope_path(cache_dir: &str, relative: &str) -> String {
    if relative.is_empty() {
        cache_dir.to_string()
    } else {
        format!("{cache_dir}/{relative}")
    }
}

fn read_cache_manifest(settings: &CacheSettings) -> CacheManifest {
    let Ok(raw) = std::fs::read_to_string(cache::manifest_path(&settings.store_root)) else {
        return CacheManifest::default();
    };
    match serde_json::from_str::<CacheManifest>(&raw) {
        Ok(manifest) if manifest.is_current() => manifest,
        _ => CacheManifest::default(),
    }
}

fn write_cache_manifest(settings: &CacheSettings, manifest: &CacheManifest) -> Result<(), String> {
    let path = cache::manifest_path(&settings.store_root);
    let entries = std::path::Path::new(&settings.store_root).join(cache::ENTRIES_DIR);
    std::fs::create_dir_all(&entries).map_err(|error| {
        format!(
            "the cache entries directory {} could not be created: {error}",
            entries.display()
        )
    })?;
    let bytes = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("the cache manifest could not be serialized: {error}"))?;
    let temporary = format!("{path}.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| {
        format!("the cache manifest could not be written to {temporary}: {error}")
    })?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("the cache manifest could not be moved to {path}: {error}"))
}

fn write_cache_archive(settings: &CacheSettings, key: &str, bytes: &[u8]) -> Result<(), String> {
    let entries = std::path::Path::new(&settings.store_root).join(cache::ENTRIES_DIR);
    std::fs::create_dir_all(&entries).map_err(|error| {
        format!(
            "the cache entries directory {} could not be created: {error}",
            entries.display()
        )
    })?;
    let path = cache::archive_path(&settings.store_root, key);
    let temporary = format!("{path}.tmp");
    std::fs::write(&temporary, bytes).map_err(|error| {
        format!("the cache archive could not be written to {temporary}: {error}")
    })?;
    std::fs::rename(&temporary, &path)
        .map_err(|error| format!("the cache archive could not be moved to {path}: {error}"))
}

fn archive_names(store_root: &str) -> Vec<String> {
    let entries = std::path::Path::new(store_root).join(cache::ENTRIES_DIR);
    match std::fs::read_dir(entries) {
        Ok(read) => read
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_type()
                    .map(|kind| kind.is_file())
                    .unwrap_or(false)
            })
            .filter_map(|entry| entry.file_name().into_string().ok())
            .collect(),
        Err(_) => Vec::new(),
    }
}

/// Restore a digest-verified archive into the sandbox. A restore that
/// cannot be completed leaves the scope directory EMPTY: a half-extracted
/// archive is not a cache, and a build must not inherit it.
async fn restore_cache_into_sandbox(
    execd: &ExecdEndpoint,
    bytes: Vec<u8>,
    scope_dir: &str,
) -> Value {
    let uploaded = opensandbox::execd::api::upload_file(execd, bytes, CACHE_ARCHIVE_IN_SANDBOX)
        .await
        .is_ok();
    if !uploaded {
        return json!({"status": "cold", "reason": "uploading the cache archive failed"});
    }
    let extracted = opensandbox::execd::oneshot::run(
        execd,
        &format!(
            "mkdir -p '{scope_dir}' && tar xzf {CACHE_ARCHIVE_IN_SANDBOX} -C '{scope_dir}' && rm -f {CACHE_ARCHIVE_IN_SANDBOX}"
        ),
        CACHE_IO_TIMEOUT,
    )
    .await;
    if extracted.succeeded() {
        return json!({"status": "warm", "scope_dir": scope_dir});
    }
    let cleaned = opensandbox::execd::oneshot::run(
        execd,
        &format!("rm -rf '{scope_dir}'"),
        Duration::from_secs(30),
    )
    .await
    .succeeded();
    json!({
        "status": "cold",
        "reason": "the cache archive could not be extracted",
        "scope_cleaned": cleaned,
    })
}

/// Archive the scope directory, store it under its key, record it in the
/// manifest, evict the oldest entries until the quota fits and remove
/// archives no entry describes.
async fn store_build_cache(
    execd: &ExecdEndpoint,
    settings: &CacheSettings,
    key: &str,
    scope_dir: &str,
    manifest: &mut CacheManifest,
) -> Value {
    let archived = opensandbox::execd::oneshot::run(
        execd,
        &format!("tar czf {CACHE_ARCHIVE_IN_SANDBOX} -C '{scope_dir}' ."),
        CACHE_IO_TIMEOUT,
    )
    .await;
    if !archived.succeeded() {
        return json!({
            "status": "skipped",
            "reason": "the cache directory could not be archived inside the sandbox",
        });
    }
    let bytes = match opensandbox::execd::api::download_file(execd, CACHE_ARCHIVE_IN_SANDBOX).await
    {
        Ok(bytes) => bytes,
        Err(error) => {
            let _ = opensandbox::execd::oneshot::run(
                execd,
                &format!("rm -f {CACHE_ARCHIVE_IN_SANDBOX}"),
                Duration::from_secs(30),
            )
            .await;
            return json!({
                "status": "skipped",
                "reason": format!("the cache archive could not be downloaded: {error}"),
            });
        }
    };
    let _ = opensandbox::execd::oneshot::run(
        execd,
        &format!("rm -f {CACHE_ARCHIVE_IN_SANDBOX}"),
        Duration::from_secs(30),
    )
    .await;
    let size_bytes = bytes.len() as u64;
    if !cache::fits_quota(size_bytes, settings.quota_bytes) {
        return json!({
            "status": "refused",
            "reason": "the archive is larger than the whole quota",
            "size_bytes": size_bytes,
            "quota_bytes": settings.quota_bytes,
        });
    }
    let digest = cache::archive_digest(&bytes);
    manifest.record(cache::CacheEntry {
        key: key.to_string(),
        digest: digest.clone(),
        size_bytes,
        last_used_epoch_secs: now_epoch_secs(),
    });
    let evicted = cache::plan_eviction(&manifest.entries, settings.quota_bytes, key);
    for evicted_key in &evicted {
        let _ = std::fs::remove_file(cache::archive_path(&settings.store_root, evicted_key));
        manifest.remove(evicted_key);
    }
    if let Err(reason) = write_cache_archive(settings, key, &bytes) {
        return json!({"status": "failed", "reason": reason});
    }
    let referenced: Vec<String> = manifest
        .entries
        .iter()
        .map(|entry| entry.key.clone())
        .collect();
    let orphans = cache::unreferenced_archives(&referenced, &archive_names(&settings.store_root));
    for orphan in &orphans {
        let _ = std::fs::remove_file(
            std::path::Path::new(&settings.store_root)
                .join(cache::ENTRIES_DIR)
                .join(orphan),
        );
    }
    if let Err(reason) = write_cache_manifest(settings, manifest) {
        return json!({"status": "failed", "reason": reason});
    }
    json!({
        "status": "stored",
        "digest": digest,
        "size_bytes": size_bytes,
        "evicted": evicted,
        "pruned_orphans": orphans.len(),
        "store_total_bytes": manifest.total_bytes(),
        "quota_bytes": settings.quota_bytes,
    })
}

/// The last `limit` characters of a text, cut on a character boundary.
fn tail(text: &str, limit: usize) -> String {
    let count = text.chars().count();
    if count <= limit {
        return text.to_string();
    }
    text.chars().skip(count - limit).collect()
}

/// The record of one executed command.
///
/// The command and the output are REDACTED before they reach the record,
/// and the output is redacted BEFORE it is truncated: a build that printed
/// a credential must not keep it in the tail it persists.
fn command_record(
    raw_command: &str,
    exit_code: Option<i32>,
    succeeded: bool,
    raw_output: &str,
) -> Value {
    json!({
        "command": credentials::redact_output(raw_command),
        "exit_code": exit_code,
        "succeeded": succeeded,
        "output_tail": tail(&credentials::redact_output(raw_output), OUTPUT_TAIL_LIMIT),
    })
}

fn flag(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn opt_flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// The pinned product profiles: images and digests observed from the
/// actual registry, toolchains as shipped by those images.
pub fn spec_for(args: &[String]) -> Result<(ProfileSpec, SubscriptionNote), String> {
    let subscription = SubscriptionNote {
        tier: flag(args, "--subscription-tier", "none"),
        limitation: flag(
            args,
            "--subscription-limitation",
            "no credentials configured; networked builds will fail to fetch dependencies",
        ),
    };
    if let Some(user_image) = opt_flag(args, "--image") {
        // The reference travels to the daemon and to the record: a
        // credential inside it would travel with it.
        if credentials::reference_carries_credentials(&user_image) {
            return Err("the image reference must not carry credentials".to_string());
        }
        let name = flag(args, "--profile", "user");
        let (spec, note) = profile::user_profile(name, ImagePin::user(user_image), "user-provided");
        // The honest note about the missing pin travels with the spec.
        let mut subscription = subscription;
        if !note.contains("pinned by digest") {
            subscription.limitation = format!("{}; {note}", subscription.limitation);
        }
        return Ok((spec, subscription));
    }
    let which = flag(args, "--profile", "rust");
    match which.as_str() {
        "rust" => Ok((
            profile::rust_profile(ImagePin::pinned(
                "rust:1-bookworm",
                "sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea",
            )),
            subscription,
        )),
        "jvm" => Ok((
            profile::jvm_profile(ImagePin::pinned(
                "opensandbox/code-interpreter:v1.1.0",
                "sha256:133a3c1720dd52291a019740c2987e7164ea6de79e23d8198798e58950ae2e6e",
            )),
            subscription,
        )),
        other => Err(format!(
            "unknown profile '{other}'; supported: rust, jvm (or pass --image for a user image)"
        )),
    }
}

/// Local image digest via the docker CLI (integration with the chosen
/// backend; strictly read-only). Returns the first sha256 repo digest.
fn local_image_digest(uri: &str) -> Option<String> {
    let output = std::process::Command::new("docker")
        .args(["inspect", uri, "--format", "{{json .RepoDigests}}"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value: Value = serde_json::from_slice(&output.stdout).ok()?;
    let digests = value.as_array()?;
    digests
        .iter()
        .find_map(|d| d.as_str())
        .and_then(|d| d.split("@").nth(1).map(str::to_string))
}

/// Parse a proxy URL before using its address in a sandbox command.
fn proxy_address(proxy_url: &str) -> Result<(String, u16), &'static str> {
    let url = reqwest::Url::parse(proxy_url).map_err(|_| "invalid proxy URL")?;
    if url.scheme() != "http"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("proxy URL must be an HTTP origin without credentials");
    }
    let host = url.host_str().ok_or("proxy URL lacks a host")?;
    if !host
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b':' | b'[' | b']'))
    {
        return Err("invalid proxy host");
    }
    Ok((
        host.to_string(),
        url.port().ok_or("proxy URL lacks a port")?,
    ))
}

/// TCP reachability of the restricted egress proxy (read-only).
fn egress_reachable(proxy_url: &str) -> bool {
    use std::net::ToSocketAddrs;
    let Ok((host, port)) = proxy_address(proxy_url) else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    (host, port)
        .to_socket_addrs()
        .ok()
        .is_some_and(|addresses| {
            addresses.into_iter().any(|address| {
                std::net::TcpStream::connect_timeout(&address, Duration::from_secs(3)).is_ok()
            })
        })
}

/// The digest of the image as it is RESOLVED locally: what the daemon
/// would really run. `None` when the image is not present locally (the
/// daemon would fetch it, and the resolved digest is then unknown here).
fn observed_image_digest(uri: &str) -> Option<String> {
    local_image_digest(uri)
}

async fn lifecycle_ready(server: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{server}/ready"))
        .timeout(Duration::from_secs(3))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

fn run_doctor(
    spec: &ProfileSpec,
    server: &str,
    proxy_url: &str,
    subscription: &SubscriptionNote,
) -> (Vec<DoctorCheck>, GateDecision) {
    let local_digest = local_image_digest(&spec.image.uri);
    let check_url =
        std::env::var("HODEI_MH_PROXY_CHECK_URL").unwrap_or_else(|_| proxy_url.to_string());
    let egress = egress_reachable(&check_url);
    let ready = {
        // block on the tiny readiness probe from sync context.
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(lifecycle_ready(server))
        })
    };
    let (checks, input) =
        doctor::checks_for(spec, local_digest.as_deref(), egress, ready, subscription);
    let decision = gate::decide(&input);
    (checks, decision)
}

fn network_observation_ok(succeeded: bool, output: &str) -> bool {
    let lines: Vec<&str> = output.lines().map(str::trim).collect();
    succeeded
        && lines.contains(&"PROXY_REACHABLE")
        && lines.contains(&"DIRECT_INTERNET_BLOCKED")
        && !lines.contains(&"PROXY_UNREACHABLE")
        && !lines.contains(&"DIRECT_INTERNET_REACHABLE")
}

/// `hodei-mh doctor --profile rust --server URL`: report + gate decision,
/// strictly read-only.
pub async fn run_doctor_cli(args: &[String]) -> i32 {
    let (spec, subscription) = match spec_for(args) {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    let server = flag(args, "--server", "http://127.0.0.1:8081");
    let proxy_default =
        std::env::var("HODEI_MH_PROXY_URL").unwrap_or_else(|_| DEFAULT_PROXY_URL.to_string());
    let proxy_url = flag(args, "--proxy-url", &proxy_default);
    let (checks, decision) = run_doctor(&spec, &server, &proxy_url, &subscription);
    for check in &checks {
        let label = if check.ok {
            "ok"
        } else if check.blocking {
            "MISS"
        } else {
            "note"
        };
        println!("[{label}] {}: {}", check.name, check.detail);
    }
    match decision {
        GateDecision::Proceed => {
            println!("gate: PROCEED");
            0
        }
        GateDecision::Refuse { missing } => {
            println!("gate: REFUSE — no native (YOLO) fallback exists");
            for missing in missing {
                println!("  missing: {missing}");
            }
            1
        }
    }
}

/// `hodei-mh profile-run --profile rust --project DIR`: doctor → gate →
/// sandbox (pinned image + requested limits) → observe effective limits →
/// run the build plan → record SHA, commands, outputs. Refuses without a
/// sandbox when the gate says so.
pub async fn run_profile(args: &[String]) -> i32 {
    let started = std::time::Instant::now();
    let server = flag(args, "--server", "http://127.0.0.1:8081");
    let proxy_default =
        std::env::var("HODEI_MH_PROXY_URL").unwrap_or_else(|_| DEFAULT_PROXY_URL.to_string());
    let proxy_url = flag(args, "--proxy-url", &proxy_default);
    let (proxy_host, proxy_port) = match proxy_address(&proxy_url) {
        Ok(address) => address,
        Err(reason) => {
            eprintln!("{reason}");
            return 2;
        }
    };
    let proxy_url = format!("http://{proxy_host}:{proxy_port}");
    let (spec, subscription) = match spec_for(args) {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("{message}");
            return 2;
        }
    };
    let project_dir = match opt_flag(args, "--project") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => {
            eprintln!("--project DIR is required");
            return 2;
        }
    };
    if !project_dir.is_dir() {
        eprintln!("project dir {} does not exist", project_dir.display());
        return 2;
    }
    // The build cache is OPT-IN and off by default. The verifier never
    // enables it: its caller always resolves the policy as `Verification`.
    let env_cache_enabled = std::env::var("HODEI_MH_BUILD_CACHE").ok();
    let env_cache_root = std::env::var("HODEI_MH_BUILD_CACHE_DIR").ok();
    let cache_settings = match cache::resolve_settings(
        args,
        cache::CacheEnv {
            enabled: env_cache_enabled.as_deref(),
            store_root: env_cache_root.as_deref(),
        },
        &default_cache_store_root(),
        cache::CacheUse::Development,
    ) {
        Ok(settings) => settings,
        Err(error) => {
            eprintln!("{error}");
            return 2;
        }
    };

    // 1. Doctor + gate (host-side, read-only).
    let (checks, decision) = run_doctor(&spec, &server, &proxy_url, &subscription);
    let requested_cpu = flag(args, "--cpu", "1");
    let requested_memory = flag(args, "--memory", "1Gi");
    if let GateDecision::Refuse { missing } = &decision {
        let record = json!({
            "gate": "REFUSE",
            "profile": spec.name,
            "image": spec.image.full_reference(),
            "checks": checks,
            "missing": missing,
            // Derived, not asserted: the build never ran on the host
            // (there is no host execution path in this code), so the
            // fallback did not happen.
            "yolo_fallback": false,
            "build_executed_on_host": false,
            "note": subscription.limitation,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&record).unwrap_or_default()
        );
        return 1;
    }

    // 2. The EXPORT: the explicit list of files that leave the host.
    //
    // It is decided BEFORE the sandbox exists: a refusal here leaves
    // nothing to clean up, and the private directory that holds the
    // packed bytes is removed when the value is dropped.
    let policy = if args.iter().any(|argument| argument == "--export-untracked") {
        ExportPolicy::WithUntracked
    } else {
        ExportPolicy::TrackedOnly
    };
    let staged = match export::stage_project(&project_dir, policy) {
        Ok(staged) => staged,
        Err(error) => {
            eprintln!("project export refused: {error}");
            return 1;
        }
    };
    let export_bytes = match staged.bytes() {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("project export failed: {error}");
            return 1;
        }
    };

    // 3. Sandbox with the PINNED image and REQUESTED limits.
    //
    // The create carries a REQUEST TAG: if the response is lost, the
    // effect is findable and is ADOPTED, never duplicated.
    let client = ControlPlane::new(server);
    let request_tag = format!(
        "profile-run-{}-{}",
        std::process::id(),
        started.elapsed().as_nanos()
    );
    let metadata = json!({
        OWNERSHIP_KEY: OWNER_VALUE,
        REQUEST_TAG_KEY: request_tag.clone(),
        "purpose": "hodei-profile-run",
    });
    let create_response = client
        .post(ControlPlane::sandboxes_path())
        .json(&json!({
            "image": {"uri": spec.image.full_reference()},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 1200,
            "resourceLimits": {"cpu": requested_cpu, "memory": requested_memory},
            // The request tag is the creation key: the server answers a
            // repeat with the same sandbox instead of creating a second one.
            "creationKey": request_tag,
            "metadata": metadata,
        }))
        .send()
        .await;
    let create_outcome = match create_response {
        Ok(response) if response.status().is_success() => {
            let body: Value = response.json().await.unwrap_or_default();
            match body["id"].as_str() {
                Some(id) => CreateOutcome::Confirmed { id: id.to_string() },
                None => CreateOutcome::Unknown {
                    tag: RequestTag(request_tag.clone()),
                },
            }
        }
        // The request may have reached the server: the effect is UNKNOWN.
        _ => CreateOutcome::Unknown {
            tag: RequestTag(request_tag.clone()),
        },
    };
    // Reconciliation: search by the request tag BEFORE any retry. A failed
    // search aborts — it never licenses a second sandbox.
    let listed = match client.get(ControlPlane::sandboxes_path()).send().await {
        Ok(response) => response.json::<Value>().await.ok(),
        Err(_) => None,
    };
    let search_failed = listed.is_none();
    let decision = reconcile_create(create_outcome, |tag| match listed.as_ref() {
        None => TagLookup::Failed {
            reason: "the sandbox list could not be read".into(),
        },
        Some(value) => {
            let items = value.get("items").and_then(|i| i.as_array());
            match items.and_then(|items| {
                items.iter().find_map(|item| {
                    let metadata = item.get("metadata")?;
                    if metadata.get(REQUEST_TAG_KEY)?.as_str()? != tag.0.as_str() {
                        return None;
                    }
                    item.get("id")?.as_str().map(str::to_string)
                })
            }) {
                Some(id) => TagLookup::Found { id },
                None => TagLookup::NotFound,
            }
        }
    });
    let sid = match &decision {
        ReconcileDecision::AdoptExisting { id } => id.clone(),
        ReconcileDecision::RetrySafe => {
            // The search worked and found NOTHING: a retry cannot
            // duplicate the effect. The retry carries the SAME request
            // tag, so even a lost retry response is reconcilable.
            eprintln!(
                "sandbox create failed and no effect is on record (request-tag {request_tag}); retrying is safe"
            );
            match client
                .post(ControlPlane::sandboxes_path())
                .json(&json!({
                    "image": {"uri": spec.image.full_reference()},
                    "entrypoint": ["sleep", "infinity"],
                    "timeout": 1200,
                    "resourceLimits": {"cpu": requested_cpu, "memory": requested_memory},
                    "creationKey": request_tag,
                    "metadata": metadata,
                }))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    let body: Value = response.json().await.unwrap_or_default();
                    match body["id"].as_str() {
                        Some(id) => id.to_string(),
                        None => {
                            eprintln!("sandbox create retry returned no id");
                            return 1;
                        }
                    }
                }
                Ok(response) => {
                    eprintln!("sandbox create retry returned {}", response.status());
                    return 1;
                }
                Err(e) => {
                    eprintln!("sandbox create retry failed: {e}");
                    return 1;
                }
            }
        }
        ReconcileDecision::Abort { reason } => {
            eprintln!("sandbox create unresolved: {reason} (request-tag {request_tag})");
            return 1;
        }
    };
    let _ = search_failed;
    let result = match client
        .get(&ControlPlane::endpoint_path(&sid, 44772))
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(value) => ExecdEndpoint::from_contract(&value)
                .ok_or_else(|| "invalid endpoint contract".to_string()),
            Err(_) => Err("invalid endpoint response".to_string()),
        },
        Ok(response) => Err(format!(
            "endpoint resolution returned {}",
            response.status()
        )),
        Err(_) => Err("endpoint resolution failed".to_string()),
    };
    let execd = match result {
        Ok(endpoint) => endpoint,
        Err(reason) => {
            let cleanup = client
                .delete(&ControlPlane::sandbox_path(&sid))
                .send()
                .await;
            if !matches!(cleanup, Ok(ref response) if response.status().is_success()) {
                eprintln!("sandbox {sid} could not be cleaned up after endpoint failure");
            }
            eprintln!("sandbox {sid}: {reason}");
            return 1;
        }
    };

    // The relay may be ready before execd has started inside the sandbox.
    // Wait with a deadline rather than treating a boot delay as a failed build.
    if !opensandbox::execd::oneshot::wait_until_ready(&execd, EXECD_READY_TIMEOUT).await {
        let _ = client
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        eprintln!(
            "sandbox {sid} never became usable: its execd did not answer within {EXECD_READY_TIMEOUT:?}"
        );
        return 1;
    }

    // 4. OBSERVED limits from inside the container (cgroups) AND the
    // sandbox's OWN network reachability (measured from inside, not from
    // the host: a host-side TCP check proves nothing about the sandbox).
    let observed = opensandbox::execd::oneshot::run(
        &execd,
        "echo cpu=$(cat /sys/fs/cgroup/cpu.max) mem=$(cat /sys/fs/cgroup/memory.max)",
        Duration::from_secs(30),
    )
    .await;
    // bash's /dev/tcp takes HOST and PORT as SEPARATE path segments
    // (/dev/tcp/host/port), never host:port.
    let proxy_target = format!(
        "{}/{proxy_port}",
        proxy_host.trim_start_matches('[').trim_end_matches(']')
    );
    let egress = opensandbox::execd::oneshot::run(
        &execd,
        &format!(
            "if timeout 5 bash -c 'exec 3<>/dev/tcp/{proxy_target}' 2>/dev/null; then echo PROXY_REACHABLE; else echo PROXY_UNREACHABLE; fi; if timeout 5 bash -c 'exec 3<>/dev/tcp/1.1.1.1/443' 2>/dev/null; then echo DIRECT_INTERNET_REACHABLE; else echo DIRECT_INTERNET_BLOCKED; fi",
            proxy_target = proxy_target
        ),
        Duration::from_secs(30),
    )
    .await;
    let proxy_reachable = egress.output.contains("PROXY_REACHABLE");
    let direct_blocked = egress.output.contains("DIRECT_INTERNET_BLOCKED");
    let direct_reachable = egress.output.contains("DIRECT_INTERNET_REACHABLE");
    if !network_observation_ok(egress.succeeded(), &egress.output) {
        let cleanup = client
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        if !matches!(cleanup, Ok(ref response) if response.status().is_success()) {
            eprintln!("sandbox {sid} could not be cleaned up after network policy failure");
        }
        eprintln!("sandbox {sid}: network policy could not be verified");
        return 1;
    }

    // 5. Ship the project (upload the packed export, extract — no egress).
    // The directory name is SANITIZED: it is interpolated into a shell
    // command inside the sandbox.
    let project_name = build_plan::sanitize_dir_name(
        &project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".into()),
    );
    let sandbox_root = format!("/workspace/{project_name}");
    let mut uploaded = false;
    // The execd connection can refuse the first attempts right after
    // the sandbox starts: bounded retries, never an unbounded wait.
    for _ in 0..3 {
        if opensandbox::execd::api::upload_file(&execd, export_bytes.clone(), "/tmp/project.tgz")
            .await
            .is_ok()
        {
            uploaded = true;
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let staged_ok = if uploaded {
        // The root is a SANITIZED name, quoted for the shell.
        opensandbox::execd::oneshot::run(
            &execd,
            &format!(
                "mkdir -p '{sandbox_root}' && tar xzf /tmp/project.tgz -C '{sandbox_root}' && rm /tmp/project.tgz"
            ),
            Duration::from_secs(60),
        )
        .await
        .succeeded()
    } else {
        false
    };
    if !uploaded || !staged_ok {
        let _ = client
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        eprintln!("project staging failed: uploaded={uploaded} staged_ok={staged_ok}");
        return 1;
    }

    // 6. Run the build plan. The egress proxy is the ONLY authorized
    // network path; credentials NEVER enter commands (builds need none).
    //
    // The cache token is derived from the CANONICAL absolute path: two
    // relative paths to the same project must not produce two caches,
    // and two different projects must never share one.
    let canonical_project = project_dir
        .canonicalize()
        .unwrap_or_else(|_| project_dir.clone());
    let plan: BuildPlan =
        build_plan::plan(&spec, &canonical_project.to_string_lossy(), &sandbox_root);

    // 6a. The OPT-IN cache: identity, restore and (after the build) store.
    let lock_bytes = dependency_locks(&project_dir);
    let lock_refs: Vec<(&str, &[u8])> = lock_bytes
        .iter()
        .map(|(name, bytes)| (*name, bytes.as_slice()))
        .collect();
    let cache_key = BuildCacheIdentity::new(
        &spec.name,
        &spec.image.full_reference(),
        &spec.toolchain.primary,
        &cache::lockfile_digest(&lock_refs),
        &build_plan::project_cache_token(&canonical_project.to_string_lossy()),
        cache_settings.scope,
    )
    .key();
    let cache_scope_dir = cache::scope_dir(&spec.name, cache_settings.scope)
        .map(|relative| scope_path(&plan.cache_dir, relative));
    let mut cache_manifest = CacheManifest::default();
    let mut cached_bytes = 0u64;
    let mut cache_restore = json!({"status": "disabled"});
    if cache_settings.enabled {
        match &cache_scope_dir {
            None => {
                cache_restore = json!({
                    "status": "unsupported",
                    "reason": "the profile has no dependency download cache",
                });
            }
            Some(scope_dir) => {
                cache_manifest = read_cache_manifest(&cache_settings);
                let archive_path = cache::archive_path(&cache_settings.store_root, &cache_key);
                match std::fs::read(&archive_path) {
                    Err(_) => {
                        cache_restore =
                            json!({"status": "cold", "reason": "no archive for this key"});
                    }
                    Ok(bytes) => match cache::restore_verdict(
                        cache_manifest.find(&cache_key),
                        &cache_key,
                        &cache::archive_digest(&bytes),
                    ) {
                        RestoreVerdict::Verified => {
                            cached_bytes = bytes.len() as u64;
                            cache_restore =
                                restore_cache_into_sandbox(&execd, bytes, scope_dir).await;
                        }
                        RestoreVerdict::RejectedCorrupted { reason } => {
                            // A cache that is not the one recorded is
                            // DISCARDED: it is never an input to a build.
                            let discarded = std::fs::remove_file(&archive_path).is_ok();
                            cache_manifest.remove(&cache_key);
                            let _ = write_cache_manifest(&cache_settings, &cache_manifest);
                            cache_restore = json!({
                                "status": "rejected",
                                "reason": reason,
                                "discarded": discarded,
                            });
                        }
                        RestoreVerdict::ColdMiss => {
                            // An archive with no manifest entry cannot be
                            // verified, so it cannot be restored.
                            let discarded = std::fs::remove_file(&archive_path).is_ok();
                            cache_restore = json!({
                                "status": "cold",
                                "reason": "no manifest entry for this key",
                                "discarded": discarded,
                            });
                        }
                    },
                }
            }
        }
    }

    let mut command_outputs: Vec<Value> = Vec::new();
    // The RAW strings are kept for the leak check ONLY: they are never
    // persisted. A detector that reads the redacted record would be
    // blinded by the very redaction it has to notice.
    let mut executed: Vec<String> = Vec::new();
    let mut all_ok = observed.succeeded();
    let build_started = std::time::Instant::now();
    for command in &plan.commands {
        let proxied = format!("HTTPS_PROXY={proxy_url} HTTP_PROXY={proxy_url} {command}");
        let outcome =
            opensandbox::execd::oneshot::run(&execd, &proxied, Duration::from_secs(600)).await;
        all_ok = all_ok && outcome.succeeded();
        executed.push(proxied.clone());
        command_outputs.push(command_record(
            &proxied,
            outcome.exit_code,
            outcome.succeeded(),
            &outcome.output,
        ));
    }
    let build_elapsed_seconds = build_started.elapsed().as_secs_f64();

    // A cache is only stored when the build that produced it succeeded: a
    // failed build leaves state no later run should inherit.
    let cache_persist = match (cache_settings.enabled, &cache_scope_dir) {
        (false, _) => json!({"status": "disabled"}),
        (true, None) => json!({
            "status": "unsupported",
            "reason": "the profile has no dependency download cache",
        }),
        (true, Some(_)) if !all_ok => {
            json!({"status": "skipped", "reason": "the build did not succeed"})
        }
        (true, Some(scope_dir)) => {
            store_build_cache(
                &execd,
                &cache_settings,
                &cache_key,
                scope_dir,
                &mut cache_manifest,
            )
            .await
        }
    };

    // 7. The record: SHA, commands, limits — no secrets by construction.
    let env_sample: Vec<(String, String)> = vec![
        ("HTTPS_PROXY".to_string(), proxy_url.clone()),
        (
            "ANTHROPIC_AUTH_TOKEN".to_string(),
            "sk-should-never-appear".to_string(),
        ),
    ];
    let scrubbed =
        credentials::scrub_for_log(env_sample.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    // The leak check runs over the EXACT strings that were executed, not
    // over the plan's templates and not over the redacted record.
    let leaks = credentials::secret_leak_positions(&executed);

    // 8. Release the environment through the IDLE GATE. The gate — not
    //    the caller — decides whether the environment may be released:
    //    only a CONFIRMED durable export permits it. The candidate is
    //    written to durable storage and READ BACK; the digest comes from
    //    the bytes that came back, not from a value in memory.
    let export_dir = std::env::temp_dir().join(format!("hodei-profile-export-{sid}"));
    let export_path = export_dir.join("candidate.json");
    let candidate = json!({
        "profile": spec.name,
        "project": canonical_project.to_string_lossy(),
        "sandbox_id": sid,
        "project_export": staged.report(),
        "commands": command_outputs,
    });
    let exported = std::fs::create_dir_all(&export_dir).is_ok()
        && std::fs::write(&export_path, candidate.to_string()).is_ok();
    let durable_digest = std::fs::read(&export_path)
        .ok()
        .map(|bytes| verify::digest::CandidateDigest::from_bytes(&bytes));
    let gate = match &durable_digest {
        Some(digest) => IdleGate::CandidateConfirmed {
            digest: digest.clone(),
        },
        None => IdleGate::WorkPending,
    };
    let release = release_on_idle(&gate);
    // The cleanup result is recorded: a failed DELETE leaves an orphan
    // the operator must be able to find.
    let cleanup = match release {
        ReleaseDecision::Release { durable } => {
            match client
                .delete(&ControlPlane::sandbox_path(&sid))
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    format!("released ({durable})")
                }
                Ok(response) => format!("release refused: delete returned {}", response.status()),
                Err(e) => format!("release refused: delete failed: {e}"),
            }
        }
        ReleaseDecision::Keep { reason } => {
            format!("kept: {reason} (the sandbox id {sid} is in this record)")
        }
    };
    // The digest ACTUALLY observed for the image the sandbox ran. The
    // requested pin is recorded next to it: the two can differ.
    let observed_digest = observed_image_digest(&spec.image.uri);
    let record = json!({
        "gate": "PROCEED",
        "profile": spec.name,
        "sandbox_id": sid,
        "request_tag": request_tag,
        "image_sha_requested": spec.image.full_reference(),
        "image_digest_observed": observed_digest,
        "toolchain": spec.toolchain.primary,
        "requested_limits": {"cpu": requested_cpu, "memory": requested_memory},
        "observed_limits": observed.output.trim(),
        "sandbox_egress": {
            "measured_from": "inside the sandbox",
            "proxy_reachable": proxy_reachable,
            "direct_internet_blocked": direct_blocked,
            "direct_internet_reachable": direct_reachable,
            "raw": egress.output.trim(),
        },
        "cache_dir": plan.cache_dir,
        "cache": {
            "enabled": cache_settings.enabled,
            "scope": cache_settings.scope.as_str(),
            "key": cache_key,
            "scope_dir": cache_scope_dir,
            "store": cache_settings.store_root,
            "quota_bytes": cache_settings.quota_bytes,
            "restore": cache_restore,
            "restored_bytes": cached_bytes,
            "persist": cache_persist,
            // Promotion is validated by a fresh checkout in a fresh
            // sandbox that never reads this cache, so a poisoned cache
            // cannot produce PASS.
            "verification_eligible": false,
        },
        "build": {
            "elapsed_seconds": build_elapsed_seconds,
        },
        "project_export": staged.report(),
        "commands": command_outputs,
        "all_succeeded": all_ok,
        "cleanup": cleanup,
        "durable_export": {
            "path": export_path.to_string_lossy(),
            "exported": exported,
            "digest_confirmed": durable_digest.is_some(),
        },
        "secret_leak_positions_in_commands": leaks,
        "credential_channel": "process env of the requiring process only",
        "scrub_sample": scrubbed,
        "elapsed_seconds": started.elapsed().as_secs_f64(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&record).unwrap_or_default()
    );
    if all_ok && leaks.is_empty() { 0 } else { 1 }
}

#[cfg(test)]
mod tests {
    use super::{command_record, network_observation_ok, proxy_address, spec_for, tail};

    /// A synthetic credential: it has the shape of a provider token and
    /// it is not one.
    const SYNTHETIC_SECRET: &str = "sk-ant-synthetic-profile-run-0123456789";

    #[test]
    fn network_policy_requires_both_observations_and_a_successful_probe() {
        let safe = "PROXY_REACHABLE\nDIRECT_INTERNET_BLOCKED\n";
        assert!(network_observation_ok(true, safe));
        assert!(!network_observation_ok(false, safe));
        assert!(!network_observation_ok(true, "PROXY_REACHABLE"));
        assert!(!network_observation_ok(
            true,
            "PROXY_UNREACHABLE\nDIRECT_INTERNET_BLOCKED"
        ));
        assert!(!network_observation_ok(
            true,
            "PROXY_REACHABLE\nDIRECT_INTERNET_REACHABLE"
        ));
    }

    #[test]
    fn proxy_address_rejects_credentials_and_shell_metacharacters() {
        assert_eq!(
            proxy_address("http://172.30.0.1:8888").unwrap(),
            ("172.30.0.1".into(), 8888)
        );
        for url in [
            "http://user:pass@proxy:8888",
            "http://proxy:8888/$(id)",
            "http://proxy:8888?x=secret",
            "https://proxy:8888",
            "http://proxy:8888/path",
        ] {
            assert!(proxy_address(url).is_err(), "accepted {url}");
        }
    }

    #[test]
    fn an_image_reference_with_credentials_is_refused() {
        // The reference travels to the daemon and to the record.
        let args = vec![
            "--image".to_string(),
            "user:pass@registry.local/team/image:tag".to_string(),
        ];
        let error = spec_for(&args).expect_err("the reference carries a credential");
        assert!(error.contains("credentials"), "{error}");
    }

    #[test]
    fn a_digest_reference_is_not_a_credential() {
        // `name@sha256:<digest>` is the pinned form, not a userinfo.
        let args = vec![
            "--image".to_string(),
            "rust@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea"
                .to_string(),
        ];
        assert!(spec_for(&args).is_ok());
    }

    #[test]
    fn the_output_is_redacted_before_it_is_truncated() {
        // The credential sits at the very end: truncating FIRST would keep
        // it in the window that is persisted.
        let output = format!("{}\nbuilding...\n{SYNTHETIC_SECRET}\n", "x".repeat(600));
        let record = command_record("cargo build", Some(0), true, &output);
        let tail = record["output_tail"].as_str().expect("the tail");
        assert!(!tail.contains(SYNTHETIC_SECRET), "{tail}");
        assert!(tail.contains("[redacted]"), "{tail}");
        assert!(tail.len() <= 400, "the tail is bounded");
    }

    #[test]
    fn the_record_keeps_its_structure_after_the_redaction() {
        let record = command_record(
            "curl https://user:pass@example.com/repo",
            Some(0),
            true,
            "downloading\n",
        );
        assert_eq!(record["exit_code"], 0);
        assert_eq!(record["succeeded"], true);
        assert!(record["command"].is_string());
        assert!(record["output_tail"].is_string());
        let encoded = serde_json::to_string(&record).expect("the record serializes");
        assert!(!encoded.contains("user:pass"), "{encoded}");
    }

    #[test]
    fn the_leak_check_reads_the_raw_command_and_not_the_redacted_record() {
        // A detector that read the record would be blinded by the very
        // redaction it has to notice.
        let raw = format!("curl -H 'Authorization: Bearer {SYNTHETIC_SECRET}'");
        let record = command_record(&raw, Some(0), true, "");
        assert!(
            !record["command"]
                .as_str()
                .expect("the command")
                .contains(SYNTHETIC_SECRET)
        );
        let executed = vec![raw];
        assert_eq!(
            super::credentials::secret_leak_positions(&executed),
            vec![0]
        );
    }

    #[test]
    fn the_tail_cuts_on_a_character_boundary() {
        assert_eq!(tail("abcdef", 3), "def");
        assert_eq!(tail("abc", 10), "abc");
        assert_eq!(tail("áéíóú", 2), "óú");
    }
}
