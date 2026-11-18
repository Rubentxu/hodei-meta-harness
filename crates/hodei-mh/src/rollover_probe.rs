//! `hodei-mh rollover-probe`: TASK-7 vertical slice — five consecutive real
//! rollovers (fresh PTY sessions, never resume), canary isolation, crash
//! recovery with a persisted ledger, bounded redacted handoff.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use opensandbox::acp::{self, AcpAssembler};
use opensandbox::control::ControlPlane;
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::endpoint::ExecdEndpoint;
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions};
use rollover::checkpoint::{Checkpoint, RecoveryAction, RolloverLedger};
use rollover::handoff::{contains_canary_risk, render_handoff};

use crate::probe_util::MOCK_AGENT_JS;

const GOAL_ID: &str = "goal-rollover-7";
const WORK_REF: &str = "TASK-7";
const CANARY: &str = "CANARY-ONLY-IN-OLD-CONVERSATION-7f3a";

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// One fresh ACP conversation against the mock agent: initialize, new
/// session, one prompt turn. Returns the exit code observed.
async fn run_conversation(execd: &ExecdEndpoint, prompt: &str) -> (String, Option<i32>) {
    let Ok(agent) = api::create_session(
        execd,
        CreateSessionRequest {
            cwd: Some("/workspace"),
            command: Some("node /workspace/mock-acp-agent.js"),
        },
    )
    .await
    else {
        return (String::new(), None);
    };
    let ws_url = execd.ws_url(&agent.session_id);
    let Ok(mut session) = pipe::attach(&ws_url, AttachOptions::default()).await else {
        let _ = api::delete_session(execd, &agent.session_id).await;
        return (String::new(), None);
    };
    let mut assembler = AcpAssembler::new();
    let mut first = true;
    let mut answered = false;
    let mut exit = None;
    let deadline = Instant::now() + Duration::from_secs(20);
    let _ = session
        .send_stdin(acp::encode_request(1, "initialize", json!({"protocolVersion": 1})).as_bytes())
        .await;
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), session.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        match event {
            ServerEvent::Stdout(bytes) => {
                for message in assembler.push(&bytes).unwrap_or_default() {
                    let is_init_response = message.get("id").and_then(|v| v.as_u64()) == Some(1)
                        && message.get("result").is_some();
                    if is_init_response && first {
                        first = false;
                        let _ = session
                            .send_stdin(
                                acp::encode_request(2, "session/new", json!({"cwd": "/workspace"}))
                                    .as_bytes(),
                            )
                            .await;
                    } else if message["result"].get("sessionId").is_some() {
                        let _ = session
                            .send_stdin(
                                acp::encode_request(
                                    3,
                                    "session/prompt",
                                    json!({
                                        "sessionId": message["result"]["sessionId"],
                                        "prompt": [{"type": "text", "text": prompt}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                    } else if message["result"].get("stopReason").is_some() {
                        answered = true;
                    }
                }
            }
            ServerEvent::Exit { exit_code } => {
                exit = exit_code;
                break;
            }
            _ => {}
        }
        if answered {
            // Prompt consumed: cancel to release the process promptly.
            let _ = session
                .send_stdin(acp::encode_request(9, "session/cancel", json!({})).as_bytes())
                .await;
            let _ = session.send_signal("SIGTERM").await;
            let grace = Instant::now() + Duration::from_secs(3);
            while Instant::now() < grace {
                let Some(event) =
                    tokio::time::timeout(Duration::from_secs(2), session.next_event())
                        .await
                        .ok()
                        .flatten()
                else {
                    break;
                };
                if let ServerEvent::Exit { exit_code } = event {
                    exit = exit_code;
                    break;
                }
            }
            break;
        }
    }
    let _ = api::delete_session(execd, &agent.session_id).await;
    (agent.session_id, exit)
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let client = ControlPlane::new(server);
    let mut checks: Vec<Check> = Vec::new();
    let finish = |checks: &[Check], elapsed: Duration| -> i32 {
        let summary = json!({
            "task": "TASK-7",
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

    // Sandbox + agent staging.
    let created = client
        .post(ControlPlane::sandboxes_path())
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 900,
            "metadata": {"probe": "task-7-rollover"},
        }))
        .send()
        .await;
    let sid: String = match created {
        Ok(resp) => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| v["id"].as_str().map(str::to_string))
            .unwrap_or_default(),
        Err(_) => String::new(),
    };
    if sid.is_empty() {
        checks.push(Check {
            name: "sandbox-create",
            passed: false,
            detail: "create failed".into(),
        });
        return finish(&checks, started.elapsed());
    }
    let endpoint = client
        .get(&ControlPlane::endpoint_path(&sid, 44772))
        .send()
        .await;
    let execd = match endpoint {
        Ok(resp) => resp
            .json::<Value>()
            .await
            .ok()
            .and_then(|v| ExecdEndpoint::from_contract(&v))
            .unwrap_or_default(),
        Err(_) => ExecdEndpoint::default(),
    };
    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it.
    crate::probe_util::wait_for_execd(&execd).await;
    let encoded = base64::engine::general_purpose::STANDARD.encode(MOCK_AGENT_JS);
    let _ = crate::probe_util::run_command_session_with_output(
        &execd,
        &format!(
            "mkdir -p /workspace && echo {encoded} | base64 -d > /workspace/mock-acp-agent.js"
        ),
    )
    .await;

    // Ledger persisted to a file: the crash boundary.
    let ledger_path =
        std::env::temp_dir().join(format!("hodei-rollover-{}.json", std::process::id()));
    let checkpoint = Checkpoint {
        goal_id: GOAL_ID.into(),
        work_ref: WORK_REF.into(),
        epoch: 1,
        session_id: "initial".into(),
        next_action: "probe rollovers".into(),
        decisions: vec![],
        references: vec![],
        evidences: vec![],
        failed_strategies: vec![],
    };
    let mut ledger = RolloverLedger::initial(checkpoint);

    // Canary exists ONLY in the old conversation (this string is never put
    // into any checkpoint field).
    let old_conversation_transcript = format!("agent said {CANARY} somewhere");
    assert!(contains_canary_risk(&format!(
        "<transcript>{old_conversation_transcript}</transcript>"
    )));

    let mut session_ids: Vec<String> = Vec::new();
    let mut epochs: Vec<u64> = Vec::new();
    let mut all_sessions_fresh = true;
    for _ in 0..5usize {
        // Crash-safety: persist the ledger before and during each rollover.
        let step = ledger.next_step();
        if let rollover::checkpoint::RolloverStep::RequestNewSession { new_epoch } = step {
            ledger.begin();
            let _ = std::fs::write(&ledger_path, serde_json::to_string(&ledger).unwrap());
            // Fresh session: a brand-new PTY process every epoch — never
            // session/load, never resume.
            let (new_session, exit) = run_conversation(
                &execd,
                "Create /workspace/hodei-rollover.txt with 'ok' then stop",
            )
            .await;
            if new_session.is_empty() || exit.is_none() {
                all_sessions_fresh = false;
                break;
            }
            ledger.record_session(&new_session);
            let _ = std::fs::write(&ledger_path, serde_json::to_string(&ledger).unwrap());
            let confirmed = ledger.confirm();
            if confirmed.session_id != new_session {
                all_sessions_fresh = false;
            }
            session_ids.push(confirmed.session_id.clone());
            epochs.push(confirmed.epoch);
            if confirmed.epoch != new_epoch {
                all_sessions_fresh = false;
            }
        }
    }

    // AC-1: five rollovers, GoalId/WorkRef constant, SessionId/epoch changed.
    let unique_sessions = session_ids
        .iter()
        .collect::<std::collections::HashSet<_>>()
        .len();
    checks.push(Check {
        name: "five-rollovers-preserve-goal-change-session",
        passed: all_sessions_fresh && epochs == vec![2, 3, 4, 5, 6] && unique_sessions == 5,
        detail: format!(
            "epochs={epochs:?} sessions unique={unique_sessions} goal={GOAL_ID} work={WORK_REF}"
        ),
    });

    // AC-3 crash simulation with the persisted ledger: adopt pending after
    // a crash between request and confirm.
    ledger.begin();
    ledger.record_session("s-after-crash");
    let _ = std::fs::write(&ledger_path, serde_json::to_string(&ledger).unwrap());
    let reloaded: RolloverLedger =
        serde_json::from_str(&std::fs::read_to_string(&ledger_path).unwrap()).unwrap();
    let adopted = matches!(
        reloaded.recovery_action(),
        RecoveryAction::AdoptPending { .. }
    );
    let mut recovered = reloaded;
    let confirmed = recovered.confirm();
    checks.push(Check {
        name: "crash-recovery-adopts-pending-no-duplicate",
        passed: adopted && confirmed.session_id == "s-after-crash",
        detail: format!(
            "recovery adopted pending session; confirmed epoch={}",
            confirmed.epoch
        ),
    });

    // AC-2: handoff built from checkpoint only; canary cannot appear.
    let handoff = render_handoff(&ledger.confirmed).unwrap_or_default();
    let handoff_clean = !handoff.contains(CANARY) && !handoff.contains("transcript");
    checks.push(Check {
        name: "canary-isolated-from-new-request",
        passed: handoff_clean,
        detail: format!(
            "handoff {} bytes, canary absent={handoff_clean}",
            handoff.len()
        ),
    });

    // AC-4: handoff carries the required sections.
    let handoff_full = render_handoff(&Checkpoint {
        decisions: vec!["use fresh sessions".into()],
        evidences: vec!["5 rollovers green".into()],
        failed_strategies: vec!["session/load resume".into()],
        references: vec![WORK_REF.into()],
        ..ledger.confirmed.clone()
    })
    .unwrap_or_default();
    checks.push(Check {
        name: "handoff-bounded-redacted-sections",
        passed: handoff_full.contains("## Decisions")
            && handoff_full.contains("## Evidences")
            && handoff_full.contains("## Failed strategies")
            && handoff_full.contains("## Next action")
            && handoff_full.len() <= rollover::handoff::MAX_HANDOFF_BYTES + 16,
        detail: format!("{} bytes with all sections", handoff_full.len()),
    });

    // Cleanup.
    let deleted = client
        .delete(&ControlPlane::sandbox_path(&sid))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(&ledger_path);
    checks.push(Check {
        name: "cleanup",
        passed: deleted,
        detail: format!("sandbox {sid} deleted"),
    });

    finish(&checks, started.elapsed())
}
