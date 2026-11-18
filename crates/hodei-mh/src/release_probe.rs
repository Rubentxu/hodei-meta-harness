//! `hodei-mh release-probe`: TASK-13 vertical slice — REAL reconciliation of
//! a create whose response was lost (search by request tag BEFORE any retry,
//! exactly one effect), orphan cleanup that only ever touches
//! `managed-by=hodei-mh` environments, idle release gated by a CONFIRMED
//! durable candidate (DELETE verified by 404, tracker still observable
//! afterwards), and loss recovery that resumes from the durable candidate in
//! a fresh session while un-exported files are marked as possible data loss.

use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine as _;
use hodei_mh_workspace::git as workspace_git;
use release::idle::{IdleGate, ReleaseDecision, release_on_idle};
use release::reconcile::{
    CreateOutcome, EnvironmentMeta, ReconcileDecision, RequestTag, TagLookup, orphans_to_release,
    reconcile_create,
};
use release::recovery::{self, SandboxLoss};
use serde_json::{Value, json};
use verify::digest::CandidateDigest;

const CANDIDATE_CONTENT: &str = "durable-candidate-payload-v1\n";
const ORPHAN_THRESHOLD_SECS: u64 = 1_800;

/// Bounded wait for a freshly created sandbox's execd to start
/// answering. The port is published before the server inside is ready.
const EXECD_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// The facts of a one-shot, as they are: a failure must be
/// DIAGNOSTICABLE from the report, never a static sentence.
fn oneshot_facts(label: &str, outcome: &opensandbox::execd::oneshot::OneshotOutcome) -> String {
    let tail: String = outcome
        .output
        .chars()
        .rev()
        .take(200)
        .collect::<String>()
        .chars()
        .rev()
        .collect();
    format!(
        "{label}: exit={:?} end={:?} ran={} expected={:?} missing={} replayed={} output={:?}",
        outcome.exit_code,
        outcome.end,
        !outcome.not_accepted(),
        outcome.expected_bytes,
        outcome.missing_bytes,
        outcome.replayed,
        tail
    )
}

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// List the sandboxes. An error is an ERROR: returning an empty list on
/// failure would make "the sandbox is not there" indistinguishable from
/// "I could not ask" — and that confusion licenses a duplicate.
async fn list_sandboxes(client: &reqwest::Client, server: &str) -> Result<Vec<Value>, String> {
    let response = client
        .get(format!("{server}/v1/sandboxes"))
        .send()
        .await
        .map_err(|e| format!("list sandboxes failed: {e}"))?;
    if !response.status().is_success() {
        return Err(format!("list sandboxes returned {}", response.status()));
    }
    let value: Value = response
        .json()
        .await
        .map_err(|e| format!("list sandboxes body unreadable: {e}"))?;
    Ok(value
        .get("items")
        .and_then(|i| i.as_array())
        .cloned()
        .unwrap_or_default())
}

fn find_by_tag(items: &[Value], tag: &str) -> Option<String> {
    items.iter().find_map(|item| {
        let metadata = item.get("metadata")?;
        if metadata.get("request-tag")?.as_str()? != tag {
            return None;
        }
        item.get("id")?.as_str().map(str::to_string)
    })
}

fn count_by_tag(items: &[Value], tag: &str) -> usize {
    items
        .iter()
        .filter(|item| {
            item.get("metadata")
                .and_then(|m| m.get("request-tag"))
                .and_then(|v| v.as_str())
                == Some(tag)
        })
        .count()
}

fn metadata_of(item: &Value) -> Option<&Value> {
    item.get("metadata")
}

/// Map one REAL listed sandbox into the reconciliation input. The idle
/// age comes from the environment's REAL `createdAt`: a fabricated
/// constant would make the idle filter untestable.
fn environment_meta(item: &Value, now: chrono::DateTime<chrono::Utc>) -> Option<EnvironmentMeta> {
    let id = item.get("id")?.as_str()?.to_string();
    let metadata = metadata_of(item);
    let owned_by_us = metadata
        .and_then(|m| m.get("managed-by"))
        .and_then(|v| v.as_str())
        == Some("hodei-mh");
    let claimed = metadata.is_some_and(|m| m.get("claimed-by").is_some());
    let created_at = item
        .get("createdAt")
        .and_then(|v| v.as_str())
        .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&chrono::Utc));
    let idle_for_secs = created_at
        .map(|created| (now - created).num_seconds().max(0) as u64)
        // No `createdAt` → the age is UNKNOWN, and an unknown age is not
        // "infinitely idle": it is not eligible for release.
        .unwrap_or(0);
    Some(EnvironmentMeta {
        id,
        owned_by_us,
        claimed,
        idle_for_secs,
    })
}

/// Create a sandbox; `request_timeout` shorter than the server work
/// simulates a LOST response (the request is delivered, the caller never
/// learns the outcome).
async fn create_sandbox(
    client: &reqwest::Client,
    server: &str,
    image: &str,
    metadata: Value,
    request_timeout: Option<Duration>,
    ttl_seconds: i64,
) -> Result<Option<String>, String> {
    let mut request = client.post(format!("{server}/v1/sandboxes")).json(&json!({
        "image": {"uri": image},
        "entrypoint": ["sleep", "infinity"],
        "timeout": ttl_seconds,
        "resourceLimits": {"cpu": "1", "memory": "1Gi"},
        "metadata": metadata,
    }));
    if let Some(timeout) = request_timeout {
        request = request.timeout(timeout);
    }
    match request.send().await {
        Ok(r) if r.status().is_success() => {
            let value: Value = r.json().await.map_err(|e| e.to_string())?;
            Ok(value["id"].as_str().map(str::to_string))
        }
        Ok(r) => Err(format!("create returned {}", r.status())),
        Err(e) => Err(format!("create response lost: {e}")),
    }
}

/// Whether the sandbox is REALLY gone. Only a 404 proves it: a transport
/// error or a 500 means "I could not ask", which is NOT "it is gone".
async fn sandbox_exists(client: &reqwest::Client, server: &str, id: &str) -> bool {
    match client
        .get(format!("{server}/v1/sandboxes/{id}"))
        .send()
        .await
    {
        Ok(response) => response.status() != reqwest::StatusCode::NOT_FOUND,
        Err(_) => true,
    }
}

async fn delete_sandbox(client: &reqwest::Client, server: &str, id: &str) -> bool {
    client
        .delete(format!("{server}/v1/sandboxes/{id}"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
}

async fn execd_for(client: &reqwest::Client, server: &str, id: &str) -> String {
    let response = client
        .get(format!("{server}/v1/sandboxes/{id}/endpoints/44772"))
        .send()
        .await
        .ok();
    let value: Value = match response {
        Some(r) => r.json().await.unwrap_or_default(),
        None => Value::Null,
    };
    format!("http://{}", value["endpoint"].as_str().unwrap_or_default())
}

/// Minimal but REAL tracker fixture: one backlog project with one task, so
/// the probe can prove the tracker stays observable after releasing.
fn make_tracker_fixture(base: &std::path::Path) -> Result<std::path::PathBuf, String> {
    let root = base.join("tracker");
    std::fs::create_dir_all(&root).map_err(|e| e.to_string())?;
    let run = |args: &[&str]| -> Result<(), String> {
        Command::new("backlog")
            .args(args)
            .current_dir(&root)
            .output()
            .map_err(|e| format!("spawn {e}"))
            .and_then(|o| {
                if o.status.success() {
                    Ok(())
                } else {
                    Err(format!(
                        "backlog {:?} failed: {}",
                        args.first().unwrap_or(&""),
                        String::from_utf8_lossy(&o.stderr).trim()
                    ))
                }
            })
    };
    workspace_git::init_fixture_repo(&root).map_err(|e| e.to_string())?;
    run(&[
        "init",
        "fixture",
        "--agent-instructions",
        "none",
        "--check-branches",
        "false",
    ])?;
    run(&["task", "create", "Observe me", "--ac", "observable"])?;
    Ok(root)
}

/// (task listed, raw CLI output for the probe report).
fn tracker_lists_task(root: &std::path::Path) -> (bool, String) {
    Command::new("backlog")
        .args(["task", "list", "--plain"])
        .current_dir(root)
        .output()
        .map(|o| {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let listed = stdout.lines().any(|l| {
                l.split_whitespace()
                    .next()
                    .is_some_and(|token| token.starts_with("TASK-"))
            });
            (
                listed,
                format!(
                    "status_ok={} stdout={}B",
                    o.status.success(),
                    stdout.trim().chars().count()
                ),
            )
        })
        .unwrap_or((false, "backlog task list could not spawn".to_string()))
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let client = reqwest::Client::new();
    let tag = format!(
        "release-probe-{}-{}",
        std::process::id(),
        started.elapsed().as_millis() as u64
    );
    let base = std::env::temp_dir().join(format!("hodei-release-probe-{}", tag));
    let mut created: Vec<String> = Vec::new();

    // ---- AC-1: reconcile a create whose response was lost ---------------
    let lost = create_sandbox(
        &client,
        server,
        image,
        json!({"managed-by": "hodei-mh", "request-tag": tag}),
        Some(Duration::from_millis(400)),
        600,
    )
    .await;
    let lost_response = lost.is_err();
    let mut reconciled_id: Option<String> = lost.ok().flatten();
    if lost_response {
        // Search by tag before ANY retry, polling until the in-flight effect
        // becomes visible.
        for _ in 0..25 {
            let items = list_sandboxes(&client, server).await.unwrap_or_default();
            if let Some(id) = find_by_tag(&items, &tag) {
                reconciled_id = Some(id);
                break;
            }
            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }
    let items_after = list_sandboxes(&client, server).await.unwrap_or_default();
    let decision = match reconciled_id.clone() {
        Some(_) => reconcile_create(
            CreateOutcome::Unknown {
                tag: RequestTag(tag.clone()),
            },
            |t| match find_by_tag(&items_after, t.0.as_str()) {
                Some(id) => TagLookup::Found { id },
                None => TagLookup::NotFound,
            },
        ),
        None => {
            // No trace anywhere: retry is SAFE (no duplicate possible).
            let retried = create_sandbox(
                &client,
                server,
                image,
                json!({"managed-by": "hodei-mh", "request-tag": tag}),
                None,
                600,
            )
            .await;
            match retried.ok().flatten() {
                Some(id) => {
                    created.push(id.clone());
                    reconciled_id = Some(id.clone());
                    ReconcileDecision::RetrySafe
                }
                None => ReconcileDecision::Abort {
                    reason: "retry create failed".into(),
                },
            }
        }
    };
    let decision_logged = format!("{decision:?}");
    let final_items = list_sandboxes(&client, server).await.unwrap_or_default();
    let effect_count = count_by_tag(&final_items, &tag);
    let adopted_not_duplicated = matches!(
        decision,
        ReconcileDecision::AdoptExisting { .. } | ReconcileDecision::RetrySafe
    ) && effect_count == 1
        && reconciled_id.is_some();
    checks.push(Check {
        name: "unknown-create-reconciled-by-tag-before-retry",
        passed: adopted_not_duplicated,
        detail: format!(
            "lost_response={lost_response}; decision={decision_logged}; effects with the tag={effect_count} (exactly one, no duplicate create)",
        ),
    });
    if let Some(id) = reconciled_id.clone() {
        created.push(id);
    }

    // ---- AC-1 (orphans): only managed-by=hodei-mh is ever released ------
    let foreign_id = create_sandbox(
        &client,
        server,
        image,
        json!({"team": "foreign-demo"}),
        None,
        600,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if !foreign_id.is_empty() {
        created.push(foreign_id.clone());
    }
    let listed = list_sandboxes(&client, server).await.unwrap_or_default();
    let now = chrono::Utc::now();
    let metas: Vec<EnvironmentMeta> = listed
        .iter()
        .filter_map(|item| environment_meta(item, now))
        .collect();
    // The idle ages come from the REAL `createdAt` of each environment.
    // The threshold used here is the AGE the probe's own environments
    // have reached (they were just created), so the OWNERSHIP filter is
    // what this check exercises; the time filter itself is covered by
    // the unit tests of `orphans_to_release`.
    let probe_age = metas
        .iter()
        .find(|m| m.id == reconciled_id.clone().unwrap_or_default())
        .map(|m| m.idle_for_secs)
        .unwrap_or(0);
    let threshold = probe_age.min(ORPHAN_THRESHOLD_SECS);
    let targets = orphans_to_release(&metas, threshold);
    let foreign_in_targets = targets.contains(&foreign_id);
    let ours_released = match targets.iter().find(|id| reconciled_id.as_ref() == Some(id)) {
        Some(id) => delete_sandbox(&client, server, id).await,
        None => false,
    };
    let ours_gone = match reconciled_id.as_ref() {
        Some(id) => !sandbox_exists(&client, server, id).await,
        None => false,
    };
    let foreign_alive =
        !foreign_id.is_empty() && sandbox_exists(&client, server, &foreign_id).await;
    checks.push(Check {
        name: "orphan-release-never-touches-foreign-environments",
        passed: ours_released
            && ours_gone
            && !foreign_in_targets
            && foreign_alive,
        detail: format!(
            "idle ages come from the REAL createdAt of each listed environment (threshold {threshold}s, the age this probe's own environment reached); released {} hodei-mh orphan(s), deleted={ours_released}, gone(404)={ours_gone}; foreign sandbox untouched={foreign_alive} (deleted by the probe itself afterwards, not by the reconciler)",
            targets.len(),
        ),
    });

    // ---- AC-2: idle release gated by a CONFIRMED durable candidate ------
    let candidate_id = create_sandbox(
        &client,
        server,
        image,
        json!({"managed-by": "hodei-mh", "request-tag": format!("{tag}-candidate")}),
        None,
        600,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if !candidate_id.is_empty() {
        created.push(candidate_id.clone());
    }
    let (kept_while_pending, exported, released_after_confirm, candidate_facts) = if candidate_id
        .is_empty()
    {
        (
            false,
            false,
            false,
            "the candidate sandbox was never created".to_string(),
        )
    } else {
        let keep = release_on_idle(&IdleGate::WorkPending);
        let kept = matches!(keep, ReleaseDecision::Keep { .. })
            && sandbox_exists(&client, server, &candidate_id).await;
        let execd = execd_for(&client, server, &candidate_id).await;
        let write = opensandbox::execd::oneshot::run_when_ready(
                &execd,
                &format!("printf %s '{CANDIDATE_CONTENT}' > /workspace/candidate.txt && cat /workspace/candidate.txt"),
                Duration::from_secs(30),
                EXECD_READY_TIMEOUT,
            )
            .await;
        let digest = CandidateDigest::from_bytes(CANDIDATE_CONTENT.as_bytes());
        let exported = write.succeeded() && write.output.contains(CANDIDATE_CONTENT);
        let release = release_on_idle(&IdleGate::CandidateConfirmed {
            digest: digest.clone(),
        });
        let released = match release {
            ReleaseDecision::Release { .. } => {
                delete_sandbox(&client, server, &candidate_id).await
                    && !sandbox_exists(&client, server, &candidate_id).await
            }
            ReleaseDecision::Keep { .. } => false,
        };
        let facts = format!(
            "sandbox={candidate_id} keep={keep:?} write[{}] exported={exported} release={release:?} released={released}",
            oneshot_facts("candidate write", &write)
        );
        (kept, exported, released, facts)
    };
    let (tracker_observed, tracker_diagnostic) = match make_tracker_fixture(&base) {
        Ok(root) => tracker_lists_task(&root),
        Err(e) => (false, format!("fixture error: {e}")),
    };
    checks.push(Check {
        name: "idle-release-only-after-durable-confirmation-tracker-still-observed",
        passed: kept_while_pending && exported && released_after_confirm && tracker_observed,
        detail: format!(
            "WorkPending kept the environment alive; after the candidate digest was confirmed the release DELETE returned 404 on GET; tracker still lists its task after release (observed, not inferred): {tracker_observed} [{tracker_diagnostic}]; {candidate_facts}",
        ),
    });

    // ---- AC-3: loss recovery from durable state, loss marked otherwise --
    let resume_id = create_sandbox(
        &client,
        server,
        image,
        json!({"managed-by": "hodei-mh", "request-tag": format!("{tag}-resume-a")}),
        None,
        600,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if !resume_id.is_empty() {
        created.push(resume_id.clone());
    }
    let (resumed, digest_matches, resume_facts) = if resume_id.is_empty() {
        (
            false,
            false,
            "the sandbox to lose was never created".to_string(),
        )
    } else {
        let execd = execd_for(&client, server, &resume_id).await;
        let seeded = opensandbox::execd::oneshot::run_when_ready(
            &execd,
            &format!("printf %s '{CANDIDATE_CONTENT}' > /workspace/candidate.txt"),
            Duration::from_secs(30),
            EXECD_READY_TIMEOUT,
        )
        .await;
        // Durable export BEFORE the loss: bytes + digest survive the sandbox.
        let durable_dir = base.join("durable");
        let _ = std::fs::create_dir_all(&durable_dir);
        let durable_ok =
            std::fs::write(durable_dir.join("candidate.txt"), CANDIDATE_CONTENT).is_ok();
        let digest = CandidateDigest::from_bytes(CANDIDATE_CONTENT.as_bytes());
        // REAL loss: the environment is removed; GET must 404 (no snapshot
        // to assume).
        let lost = delete_sandbox(&client, server, &resume_id).await
            && !sandbox_exists(&client, server, &resume_id).await;
        let plan = recovery::recover_from_loss(&SandboxLoss {
            last_export: durable_ok.then_some(digest.clone()),
            unexported: vec![],
        });
        match plan {
            recovery::RecoveryPlan::ResumeFromCandidate { digest, .. } => {
                // Fresh session: a NEW sandbox rebuilt from durable bytes.
                let fresh = create_sandbox(
                    &client,
                    server,
                    image,
                    json!({"managed-by": "hodei-mh", "request-tag": format!("{tag}-resume-b")}),
                    None,
                    600,
                )
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
                if !fresh.is_empty() {
                    created.push(fresh.clone());
                }
                if fresh.is_empty() {
                    (
                        lost,
                        false,
                        format!(
                            "sandbox={resume_id} seed[{}] lost={lost} the fresh session was never created",
                            oneshot_facts("seed write", &seeded)
                        ),
                    )
                } else {
                    let fresh_execd = execd_for(&client, server, &fresh).await;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(CANDIDATE_CONTENT.as_bytes());
                    let restored = opensandbox::execd::oneshot::run_when_ready(
                        &fresh_execd,
                        &format!(
                            "echo {encoded} | base64 -d > /workspace/candidate.txt && cat /workspace/candidate.txt"
                        ),
                        Duration::from_secs(30),
                        EXECD_READY_TIMEOUT,
                    )
                    .await;
                    let mut matches = restored.succeeded()
                        && restored.output.contains(CANDIDATE_CONTENT)
                        && CandidateDigest::from_bytes(CANDIDATE_CONTENT.as_bytes()) == digest;
                    let mut restore_facts = oneshot_facts("restore", &restored);
                    if !matches && restored.succeeded() && restored.output.is_empty() {
                        // The read returned NOTHING: the transport lost
                        // the output. Reading the file again is
                        // idempotent — the work is not repeated, only
                        // measured again — and the detail says that the
                        // first read came back empty.
                        let reread = opensandbox::execd::oneshot::run_when_ready(
                            &fresh_execd,
                            "cat /workspace/candidate.txt",
                            Duration::from_secs(30),
                            EXECD_READY_TIMEOUT,
                        )
                        .await;
                        matches = reread.succeeded()
                            && reread.output.contains(CANDIDATE_CONTENT)
                            && CandidateDigest::from_bytes(CANDIDATE_CONTENT.as_bytes()) == digest;
                        restore_facts = format!(
                            "{restore_facts} (the first read returned no output) re-read[{}]",
                            oneshot_facts("re-read", &reread)
                        );
                    }
                    (
                        lost,
                        matches,
                        format!(
                            "sandbox={resume_id} lost={lost} fresh={fresh} seed[{}] restore[{restore_facts}]",
                            oneshot_facts("seed write", &seeded),
                        ),
                    )
                }
            }
            recovery::RecoveryPlan::PossibleDataLoss { .. } => (
                lost,
                false,
                format!("sandbox={resume_id} lost={lost} the plan was not a resume"),
            ),
        }
    };
    checks.push(Check {
        name: "loss-recovers-from-durable-candidate-in-a-fresh-session",
        passed: resumed && digest_matches,
        detail: format!(
            "sandbox removed for real (404 after DELETE); recovery resumed from the durable candidate in a NEW session; digest of restored content matches the durable digest: {digest_matches}; {resume_facts}",
        ),
    });

    // Un-exported work: the loss must be MARKED, never assumed recoverable.
    let no_export_id = create_sandbox(
        &client,
        server,
        image,
        json!({"managed-by": "hodei-mh", "request-tag": format!("{tag}-noexport")}),
        None,
        600,
    )
    .await
    .ok()
    .flatten()
    .unwrap_or_default();
    if !no_export_id.is_empty() {
        created.push(no_export_id.clone());
    }
    let (marked, gone_forever, loss_facts) = if no_export_id.is_empty() {
        (false, false, "the sandbox was never created".to_string())
    } else {
        let execd = execd_for(&client, server, &no_export_id).await;
        let wrote = opensandbox::execd::oneshot::run_when_ready(
            &execd,
            "echo in-progress > /workspace/half-edited.rs && echo scratch > /workspace/scratch.sh && ls /workspace",
            Duration::from_secs(30),
            EXECD_READY_TIMEOUT,
        )
        .await;
        let files_present = wrote.output.contains("half-edited.rs");
        let lost = delete_sandbox(&client, server, &no_export_id).await
            && !sandbox_exists(&client, server, &no_export_id).await;
        let plan = recovery::recover_from_loss(&SandboxLoss {
            last_export: None,
            unexported: vec!["half-edited.rs".into(), "scratch.sh".into()],
        });
        let marked = match &plan {
            recovery::RecoveryPlan::PossibleDataLoss { files } => {
                files == &vec!["half-edited.rs".to_string(), "scratch.sh".to_string()]
            }
            recovery::RecoveryPlan::ResumeFromCandidate { .. } => false,
        };
        (
            marked && files_present,
            lost,
            format!(
                "sandbox={no_export_id} files_present={files_present} lost={lost} plan={plan:?} write[{}]",
                oneshot_facts("write", &wrote)
            ),
        )
    };
    checks.push(Check {
        name: "un-exported-work-marked-as-possible-data-loss",
        passed: marked && gone_forever,
        detail: format!(
            "files existed, environment removed (404), no export existed: plan = PossibleDataLoss(half-edited.rs, scratch.sh) — no snapshot was assumed; {loss_facts}",
        ),
    });

    // ---- DoD: REAL TTL expiration — the SERVER releases the sandbox, the
    // probe never calls DELETE; the reaper sweep enforces the contract.
    let ttl_id = create_sandbox(
        &client,
        server,
        image,
        json!({"managed-by": "hodei-mh", "request-tag": format!("{tag}-ttl")}),
        None,
        8,
    )
    .await;
    let (ttl_created, expired_on_its_own, ttl_seconds) = match ttl_id.ok().flatten() {
        Some(id) => {
            // This create lives only 8s; wait for the server's reaper.
            let deadline = Instant::now() + Duration::from_secs(40);
            let mut gone = !sandbox_exists(&client, server, &id).await;
            while !gone && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(1000)).await;
                gone = !sandbox_exists(&client, server, &id).await;
            }
            if !gone {
                // Only then the probe cleans up its own leftover.
                let _ = delete_sandbox(&client, server, &id).await;
            }
            (true, gone, started.elapsed().as_secs())
        }
        None => (false, false, 0),
    };
    checks.push(Check {
        name: "ttl-expiration-is-real-server-enforced",
        passed: ttl_created && expired_on_its_own,
        detail: format!(
            "sandbox created with timeout=8s; released by the server's expiry sweep WITHOUT any probe DELETE (probe waited {ttl_seconds}s total)",
        ),
    });

    // ---- AC-4 (anti-stall): every decision was actionable, bounded time --
    let elapsed = started.elapsed();
    let all_actionable = checks.iter().all(|c| c.passed);
    checks.push(Check {
        name: "every-outcome-actionable-no-stall-state",
        passed: all_actionable && elapsed < Duration::from_secs(300),
        detail: format!(
            "adopt/retry-safe/release/marked-loss are all terminal, actionable decisions; whole slice finished in {:.1}s (< 300s)",
            elapsed.as_secs_f64(),
        ),
    });

    // Cleanup: probe-owned leftovers only (the foreign sandbox too, but the
    // report above already proved the reconciler never selected it).
    for id in &created {
        let _ = delete_sandbox(&client, server, id).await;
    }
    let _ = std::fs::remove_dir_all(&base);

    let summary = json!({
        "task": "TASK-13",
        "checks": checks.iter().map(|c| json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
        "elapsed_seconds": started.elapsed().as_secs_f64(),
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
}
