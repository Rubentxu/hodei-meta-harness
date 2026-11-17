//! `hodei-mh doctor` / `hodei-mh profile-run` (TASK-16): reproducible
//! language profiles. The doctor is strictly READ-ONLY; profile-run gates
//! every run through it and REFUSES (informatively, no YOLO fallback) when
//! the sandbox, the pinned image or the restricted egress is missing.
//! Credentials only ever travel as process env and never appear in
//! commands, logs or the run record.

use std::path::Path;
use std::time::Duration;

use profiles::build_plan::{self, BuildPlan};
use profiles::credentials::{self, SubscriptionNote};
use profiles::doctor::{self, DoctorCheck};
use profiles::gate::{self, GateDecision};
use profiles::profile::{self, ImagePin, ProfileSpec};
use serde_json::{Value, json};

pub const DEFAULT_PROXY_URL: &str = "http://172.17.0.1:8888";

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
        let (spec, _note) =
            profile::user_profile(name, ImagePin::user(user_image), "user-provided");
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
        println!(
            "[{}] {}: {}",
            if check.ok { "ok" } else { "MISS" },
            check.name,
            check.detail
        );
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
            "yolo_fallback": false,
            "note": subscription.limitation,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&record).unwrap_or_default()
        );
        return 1;
    }

    // 2. Sandbox with the PINNED image and REQUESTED limits.
    let client = reqwest::Client::new();
    let created: Value = match client
        .post(format!("{server}/v1/sandboxes"))
        .json(&json!({
            "image": {"uri": spec.image.full_reference()},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 1200,
            "resourceLimits": {"cpu": requested_cpu, "memory": requested_memory},
            "metadata": {"managed-by": "hodei-mh", "purpose": "hodei-profile-run"},
        }))
        .send()
        .await
    {
        Ok(response) => response.json().await.unwrap_or_default(),
        Err(_) => Value::Null,
    };
    let sid = created["id"].as_str().unwrap_or_default().to_string();
    if sid.is_empty() {
        eprintln!("sandbox create failed (gate said proceed — infra changed?)");
        return 1;
    }
    let execd: String = match client
        .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
        .send()
        .await
    {
        Ok(response) => {
            let endpoint_value: Value = response.json().await.unwrap_or_default();
            endpoint_value["endpoint"]
                .as_str()
                .map(|host| format!("http://{host}"))
                .unwrap_or_default()
        }
        Err(_) => String::new(),
    };

    // 3. OBSERVED limits from inside the container (cgroups).
    let observed = opensandbox::execd::oneshot::run(
        &execd,
        "echo cpu=$(cat /sys/fs/cgroup/cpu.max) mem=$(cat /sys/fs/cgroup/memory.max)",
        Duration::from_secs(30),
    )
    .await;

    // 4. Ship the project (pack host-side, upload, extract — no egress).
    let project_name = project_dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "project".into());
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
            if opensandbox::execd::api::upload_file(&execd, None, bytes.clone(), "/tmp/project.tgz")
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
        opensandbox::execd::oneshot::run(
            &execd,
            &format!("mkdir -p {sandbox_root} && tar xzf /tmp/project.tgz -C {sandbox_root} && rm /tmp/project.tgz"),
            Duration::from_secs(60),
        )
        .await
        .succeeded()
    } else {
        false
    };
    if !packed || !uploaded || !staged_ok {
        let _ = client
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        eprintln!(
            "project staging failed: packed={packed} uploaded={uploaded} staged_ok={staged_ok}"
        );
        return 1;
    }

    // 5. Run the build plan. The egress proxy is the ONLY authorized
    // network path; credentials NEVER enter commands (builds need none).
    let plan: BuildPlan = build_plan::plan(&spec, &project_dir.to_string_lossy(), &sandbox_root);
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
            "output_tail": outcome.output.chars().rev().take(400).collect::<String>().chars().rev().collect::<String>(),
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
    let leaks = credentials::secret_leak_positions(&plan.commands);
    let _ = client
        .delete(format!("{server}/v1/sandboxes/{sid}"))
        .send()
        .await;
    let record = json!({
        "gate": "PROCEED",
        "profile": spec.name,
        "image_sha": spec.image.full_reference(),
        "toolchain": spec.toolchain.primary,
        "requested_limits": {"cpu": requested_cpu, "memory": requested_memory},
        "observed_limits": observed.output.trim(),
        "cache_dir": plan.cache_dir,
        "commands": command_outputs,
        "all_succeeded": all_ok,
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
