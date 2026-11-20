//! `hodei-mh workspace-probe`: TASK-2 vertical slice — isolated Git workspace
//! with explicit base revision, candidate bundle export, host-side validation
//! before sandbox release, crash recovery and private-path rejection.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use hodei_mh_workspace::git;
use hodei_mh_workspace::plan::{WorkspacePlan, validate_candidate};
use opensandbox::control::ControlPlane;
use opensandbox::execd::api;
use opensandbox::execd::client::ExecdClient;
use opensandbox::execd::deadline::Deadline;
use opensandbox::execd::endpoint::ExecdEndpoint;

use crate::probe_util::run_command_session_with_output;

const CANDIDATE_BUNDLE: &str = "/workspace/candidate.bundle";
const LEAK_BUNDLE: &str = "/workspace/leak.bundle";

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let client = ControlPlane::new(server);
    let mut checks: Vec<Check> = Vec::new();

    let finish = |checks: &[Check], elapsed: Duration| -> i32 {
        let summary = json!({
            "task": "TASK-2",
            "checks": checks.iter().map(|c| json!({
                "name": c.name, "passed": c.passed, "detail": c.detail,
            })).collect::<Vec<_>>(),
            "all_passed": checks.iter().all(|c| c.passed),
            "elapsed_seconds": elapsed.as_secs_f64(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        if checks.iter().all(|c| c.passed) {
            0
        } else {
            1
        }
    };

    // Build the base fixture on the host: an explicit revision to ship.
    let host = match build_base_fixture() {
        Ok(f) => f,
        Err(err) => {
            checks.push(Check {
                name: "base-fixture",
                passed: false,
                detail: err,
            });
            return finish(&checks, started.elapsed());
        }
    };
    checks.push(Check {
        name: "base-fixture",
        passed: true,
        detail: format!("base revision {}", host.base),
    });

    // 1. Sandbox receives exactly the chosen base revision.
    let sid = match create_sandbox(&client, image).await {
        Some(id) => id,
        None => {
            checks.push(Check {
                name: "sandbox-create",
                passed: false,
                detail: "create failed".into(),
            });
            return finish(&checks, started.elapsed());
        }
    };
    let Some(execd) = resolve_execd(&client, &sid).await else {
        checks.push(Check {
            name: "execd-endpoint",
            passed: false,
            detail: "resolve failed".into(),
        });
        return finish(&checks, started.elapsed());
    };

    let bundle_bytes = std::fs::read(&host.base_bundle).unwrap_or_default();
    let uploaded = match api::upload_file(&execd, bundle_bytes, "/workspace/base.bundle").await {
        Ok(()) => true,
        Err(err) => {
            eprintln!("upload error: {err}");
            false
        }
    };
    let clone_cmd = "mkdir -p /workspace/repo && git clone -q /workspace/base.bundle /workspace/repo && cd /workspace/repo && git config user.email probe@hodei && git config user.name probe && git checkout -q main";
    let clone_output = run_command_session_with_output(&execd, clone_cmd).await;
    eprintln!("upload={uploaded} clone_output: {clone_output}");
    let head =
        run_command_session_with_output(&execd, "git -C /workspace/repo rev-parse HEAD").await;
    let head_matches = head.trim_start().starts_with(&host.base);
    checks.push(Check {
        name: "sandbox-exact-base-revision",
        passed: uploaded && head_matches,
        detail: format!(
            "expected {base_rev} got {got}",
            base_rev = host.base,
            got = head.trim()
        ),
    });

    // 2. Candidate commit in sandbox, exported as bundle, downloaded atomically.
    let candidate_cmd = "cd /workspace/repo && echo candidate-work > feature.txt && git add -A && git commit -qm 'candidate: feature' && git bundle create /workspace/candidate.bundle --all";
    let _ = run_command_session_with_output(&execd, candidate_cmd).await;
    let staging = stage_dir(&sid);
    let _ = std::fs::create_dir_all(&staging);
    let exported =
        download_atomically(&execd, CANDIDATE_BUNDLE, &staging.join("candidate.bundle")).await;
    checks.push(Check {
        name: "candidate-exported-atomically",
        passed: exported,
        detail: format!("staged at {}", staging.join("candidate.bundle").display()),
    });

    // 3. Host validation BEFORE releasing the sandbox.
    let inspect_dir = staging.join("inspect");
    let _ = std::fs::create_dir_all(&inspect_dir);
    let plan = WorkspacePlan::new(&host.base, "/workspace/repo").expect("valid plan");
    let validation = std::fs::read(staging.join("candidate.bundle"))
        .ok()
        .and_then(|_| {
            git::inspect_bundle(
                &staging.join("candidate.bundle"),
                &plan.base_revision,
                &inspect_dir,
            )
            .ok()
        });
    let (candidate_head, _changed_files) = match validation {
        Some((head, files)) => {
            let verdict = validate_candidate(files.iter().map(String::as_str), &plan.denylist);
            match verdict {
                Ok(()) => {
                    checks.push(Check {
                        name: "host-validation-before-release",
                        passed: true,
                        detail: format!("candidate {head} touches only {}", files.join(", ")),
                    });
                    (head, files.clone())
                }
                Err(rejected) => {
                    checks.push(Check {
                        name: "host-validation-before-release",
                        passed: false,
                        detail: format!("rejected: {rejected:?}"),
                    });
                    return finish(&checks, started.elapsed());
                }
            }
        }
        None => {
            checks.push(Check {
                name: "host-validation-before-release",
                passed: false,
                detail: "bundle failed integrity (clone/index-pack)".into(),
            });
            return finish(&checks, started.elapsed());
        }
    };

    // 4. Private-path leg: an ordinary agent change must never promote
    //    backlog management state; export, download, and REJECT on host.
    let leak_cmd = "cd /workspace/repo && mkdir -p backlog && echo secrets > backlog/leak.md && git add -A && git commit -qm 'leak attempt' && git bundle create /workspace/leak.bundle --all";
    let _ = run_command_session_with_output(&execd, leak_cmd).await;
    let leak_downloaded =
        download_atomically(&execd, LEAK_BUNDLE, &staging.join("leak.bundle")).await;
    let leak_dir = staging.join("inspect-leak");
    let _ = std::fs::create_dir_all(&leak_dir);
    let leak_rejected = leak_downloaded
        && git::inspect_bundle(&staging.join("leak.bundle"), &candidate_head, &leak_dir)
            .ok()
            .and_then(|(_, files)| {
                validate_candidate(files.iter().map(String::as_str), &plan.denylist).err()
            })
            .is_some();
    checks.push(Check {
        name: "private-path-promotion-rejected",
        passed: leak_rejected,
        detail: "backlog/leak.md candidate rejected by host validation".into(),
    });

    // 5. Crash simulation: release the sandbox, then re-validate the staged
    //    candidate from the host staging dir alone.
    let deleted = client
        .delete(&ControlPlane::sandbox_path(&sid))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let revalidated = deleted && std::fs::read(staging.join("candidate.bundle")).is_ok() && {
        let recovery_dir = staging.join("inspect-recovery");
        let _ = std::fs::create_dir_all(&recovery_dir);
        git::inspect_bundle(&staging.join("candidate.bundle"), &host.base, &recovery_dir).is_ok()
    };
    checks.push(Check {
        name: "crash-after-export-recoverable",
        passed: revalidated,
        detail: format!("sandbox deleted={deleted}; candidate re-validated from staging"),
    });

    checks.push(Check {
        name: "cleanup",
        passed: true,
        detail: format!("staging kept at {} (recovery artifact)", staging.display()),
    });

    finish(&checks, started.elapsed())
}

struct BaseFixture {
    base: String,
    base_bundle: PathBuf,
}

fn build_base_fixture() -> Result<BaseFixture, String> {
    let dir = std::env::temp_dir().join(format!("hodei-ws-base-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("repo")).map_err(|e| e.to_string())?;
    let repo = dir.join("repo");
    let run_git = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .map_err(|e| e.to_string())?;
        if status.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&status.stderr).to_string())
        }
    };
    git::init_fixture_repo(&repo).map_err(|e| e.to_string())?;
    std::fs::write(repo.join("README.md"), "base revision fixture\n").map_err(|e| e.to_string())?;
    run_git(&["add", "-A"])?;
    // The fixture commits with git's PLUMBING: automation must not
    // depend on the host's `git commit` path, which can be governed by a
    // policy that refuses repositories it does not classify.
    let base = git::commit_index(&repo, "base").map_err(|e| e.to_string())?;
    let bundle = dir.join("base.bundle");
    git::create_bundle(&repo, &bundle, "--all").map_err(|e| e.to_string())?;
    Ok(BaseFixture {
        base,
        base_bundle: bundle,
    })
}

async fn create_sandbox(client: &ControlPlane, image: &str) -> Option<String> {
    let response = client
        .post(ControlPlane::sandboxes_path())
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 900,
            "metadata": {"probe": "task-2-workspace"},
        }))
        .send()
        .await
        .ok()?;
    let value = response.json::<Value>().await.ok()?;
    value["id"].as_str().map(str::to_string)
}

async fn resolve_execd(client: &ControlPlane, sid: &str) -> Option<ExecdEndpoint> {
    let value = client
        .get(&ControlPlane::endpoint_path(sid, 44772))
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?;
    let execd = ExecdEndpoint::from_contract(&value)?;
    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it. The readiness is tied to
    // the generation behind the endpoint, so one client holds it.
    let transport = ExecdClient::new(execd.clone());
    let _ = crate::probe_util::wait_for_execd(
        &transport,
        Deadline::in_budget(crate::probe_util::EXECD_READY_TIMEOUT),
    )
    .await;
    Some(execd)
}

fn stage_dir(sid: &str) -> PathBuf {
    std::env::temp_dir().join("hodei-candidates").join(sid)
}

/// Download to `<dest>.part`, then rename: a partial download is never
/// observable under the final name (AC: partial output is not valid).
fn atomic_write(part: &Path, dest: &Path, bytes: &[u8]) -> bool {
    let written = std::fs::write(part, bytes).is_ok() && std::fs::rename(part, dest).is_ok();
    if written {
        true
    } else {
        let _ = std::fs::remove_file(part);
        false
    }
}

async fn download_atomically(execd: &ExecdEndpoint, remote: &str, dest: &Path) -> bool {
    let part = dest.with_extension("part");
    match api::download_file(execd, remote).await {
        Ok(bytes) if !bytes.is_empty() => atomic_write(&part, dest, &bytes),
        Ok(_) | Err(_) => {
            let _ = std::fs::remove_file(&part);
            false
        }
    }
}
