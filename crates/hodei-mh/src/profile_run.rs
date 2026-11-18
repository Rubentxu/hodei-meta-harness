//! `hodei-mh doctor` / `hodei-mh profile-run` (TASK-16): reproducible
//! language profiles. The doctor is strictly READ-ONLY; profile-run gates
//! every run through it and REFUSES (informatively, no YOLO fallback) when
//! the sandbox, the pinned image or the restricted egress is missing.
//! Credentials only ever travel as process env and never appear in
//! commands, logs or the run record.

use std::path::Path;
use std::time::Duration;

use opensandbox::control::ControlPlane;
use opensandbox::execd::endpoint::ExecdEndpoint;
use profiles::build_plan::{self, BuildPlan};
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

pub const DEFAULT_PROXY_URL: &str = "http://172.17.0.1:8888";

/// Bounded wait for the sandbox's execd to start answering. A sandbox
/// that never becomes usable is a FAILURE, not an infinite wait.
const EXECD_READY_TIMEOUT: Duration = Duration::from_secs(60);

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

/// TCP reachability of the restricted egress proxy (read-only).
fn egress_reachable(proxy_url: &str) -> bool {
    let authority = proxy_url
        .strip_prefix("http://")
        .unwrap_or(proxy_url)
        .trim_end_matches('/');
    // bash's /dev/tcp wants HOST/PORT as separate path segments.
    let Some((host, port)) = authority.split_once(':') else {
        return false;
    };
    let output = std::process::Command::new("bash")
        .args([
            "-c",
            &format!("timeout 3 bash -c 'cat < /dev/null > /dev/tcp/{host}/{port}' && echo ok"),
        ])
        .output();
    matches!(output, Ok(o) if o.status.success())
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
    let egress = egress_reachable(proxy_url);
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
    let proxy_url = flag(args, "--proxy-url", DEFAULT_PROXY_URL);
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

fn tar_project(project_dir: &Path, dest: &Path) -> bool {
    std::process::Command::new("tar")
        .args([
            "czf",
            dest.to_string_lossy().as_ref(),
            "-C",
            project_dir.to_string_lossy().as_ref(),
            ".",
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// `hodei-mh profile-run --profile rust --project DIR`: doctor → gate →
/// sandbox (pinned image + requested limits) → observe effective limits →
/// run the build plan → record SHA, commands, outputs. Refuses without a
/// sandbox when the gate says so.
pub async fn run_profile(args: &[String]) -> i32 {
    let started = std::time::Instant::now();
    let server = flag(args, "--server", "http://127.0.0.1:8081");
    let proxy_url = flag(args, "--proxy-url", DEFAULT_PROXY_URL);
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

    // 2. Sandbox with the PINNED image and REQUESTED limits.
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
    let execd: ExecdEndpoint = match client
        .get(&ControlPlane::endpoint_path(&sid, 44772))
        .send()
        .await
    {
        Ok(response) => {
            let endpoint_value: Value = response.json().await.unwrap_or_default();
            ExecdEndpoint::from_contract(&endpoint_value).unwrap_or_default()
        }
        Err(_) => ExecdEndpoint::default(),
    };

    // The sandbox publishes its execd port BEFORE the server inside can
    // answer: the first attempts are refused while the container is
    // still booting. Waiting (bounded) for it turns a boot delay into a
    // wait instead of a false "the command failed".
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

    // 3. OBSERVED limits from inside the container (cgroups) AND the
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
    let proxy_authority = proxy_url
        .strip_prefix("http://")
        .unwrap_or(&proxy_url)
        .trim_end_matches('/')
        .to_string();
    let proxy_target = match proxy_authority.split_once(':') {
        Some((host, port)) => format!("{host}/{port}"),
        None => proxy_authority.clone(),
    };
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

    // 4. Ship the project (pack host-side, upload, extract — no egress).
    // The directory name is SANITIZED: it is interpolated into a shell
    // command inside the sandbox.
    let project_name = build_plan::sanitize_dir_name(
        &project_dir
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "project".into()),
    );
    let sandbox_root = format!("/workspace/{project_name}");
    let tgz_path = std::env::temp_dir().join(format!("hodei-profile-{}.tgz", std::process::id()));
    let packed = tar_project(&project_dir, &tgz_path);
    let mut uploaded = false;
    if packed {
        // The execd connection can refuse the first attempts right after
        // the sandbox starts: bounded retries, never an unbounded wait.
        let bytes = std::fs::read(&tgz_path).unwrap_or_default();
        let _ = std::fs::remove_file(&tgz_path);
        for _ in 0..3 {
            if opensandbox::execd::api::upload_file(&execd, bytes.clone(), "/tmp/project.tgz")
                .await
                .is_ok()
            {
                uploaded = true;
                break;
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
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
    if !packed || !uploaded || !staged_ok {
        let _ = client
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        eprintln!(
            "project staging failed: packed={packed} uploaded={uploaded} staged_ok={staged_ok}"
        );
        return 1;
    }

    // 5. Run the build plan. The egress proxy is the ONLY authorized
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
    let mut command_outputs: Vec<Value> = Vec::new();
    let mut all_ok = observed.succeeded();
    for command in &plan.commands {
        let proxied = format!("HTTPS_PROXY={proxy_url} HTTP_PROXY={proxy_url} {command}");
        let outcome =
            opensandbox::execd::oneshot::run(&execd, &proxied, Duration::from_secs(600)).await;
        all_ok = all_ok && outcome.succeeded();
        command_outputs.push(json!({
            "command": proxied,
            "exit_code": outcome.exit_code,
            "succeeded": outcome.succeeded(),
            // The output is SCRUBBED before it reaches the record: a build
            // that prints a credential must not persist it.
            "output_tail": credentials::redact_output(&outcome.output.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>()),
        }));
    }

    // 6. The record: SHA, commands, limits — no secrets by construction.
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
    // over the plan's templates.
    let executed: Vec<String> = command_outputs
        .iter()
        .filter_map(|c| c["command"].as_str().map(str::to_string))
        .collect();
    let leaks = credentials::secret_leak_positions(&executed);

    // 7. Release the environment through the IDLE GATE. The gate — not
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
