//! `hodei-mh run-probe`: TASK-5 vertical slice — one exclusive ACP session
//! driven from START (explicit grant + coordinator claim) to STOP
//! (invalidation, cancellation, honest irreversible report).

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use hodei_mh_coordinator::actor::CoordinatorActor;
use hodei_mh_coordinator::core::ClaimResult;
use hodei_mh_run::grant::{EffectKind, Grant};
use hodei_mh_run::run::{EventIdentity, RunPlan, TaggedEvent};
use opensandbox::acp::{self, AcpAssembler};
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions, PipeSession};

use crate::probe_util::{MOCK_AGENT_JS, ws_url_for};

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let client = reqwest::Client::new();
    let mut checks: Vec<Check> = Vec::new();
    let finish = |checks: &[Check], elapsed: Duration| -> i32 {
        let summary = json!({
            "task": "TASK-5",
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

    // START: explicit grant bound to project + coordinator claim (single
    // writer). A second claim attempt for the same ticket is denied.
    let db = std::env::temp_dir().join(format!("hodei-run-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let actor = match CoordinatorActor::start(&db) {
        Ok(actor) => actor,
        Err(err) => {
            checks.push(Check {
                name: "coordinator-start",
                passed: false,
                detail: err.to_string(),
            });
            return finish(&checks, started.elapsed());
        }
    };
    let claim = actor.claim("ticket-run-5", "hodei-mh").await;
    let claim = match claim {
        Ok(claim) => claim,
        Err(err) => {
            checks.push(Check {
                name: "coordinator-claim",
                passed: false,
                detail: err.to_string(),
            });
            return finish(&checks, started.elapsed());
        }
    };
    let epoch = match claim {
        ClaimResult::Granted(claim) => claim.epoch,
        ClaimResult::Denied { holder } => {
            checks.push(Check {
                name: "coordinator-claim",
                passed: false,
                detail: format!("denied by {holder}"),
            });
            return finish(&checks, started.elapsed());
        }
    };
    let second = actor.claim("ticket-run-5", "impostor").await;
    let exclusive = matches!(second, Ok(ClaimResult::Denied { .. }));
    checks.push(Check {
        name: "exclusive-session-via-coordinator",
        passed: exclusive,
        detail: "second claimant denied by coordinator".into(),
    });

    // Sandbox + mock agent staged.
    let sid = create_sandbox(&client, server, image).await;
    let Some(sid) = sid else {
        checks.push(Check {
            name: "sandbox-create",
            passed: false,
            detail: "create failed".into(),
        });
        let _ = actor.stop().await;
        return finish(&checks, started.elapsed());
    };
    let Some(execd_base) = resolve_execd(&client, server, &sid).await else {
        checks.push(Check {
            name: "execd-endpoint",
            passed: false,
            detail: "resolve failed".into(),
        });
        let _ = actor.stop().await;
        let _ = client
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(MOCK_AGENT_JS);
    let _ = crate::probe_util::run_command_session_with_output(
        &execd_base,
        &format!(
            "mkdir -p /workspace && echo {encoded} | base64 -d > /workspace/mock-acp-agent.js"
        ),
    )
    .await;
    let agent_session = api::create_session(
        &execd_base,
        None,
        CreateSessionRequest {
            cwd: Some("/workspace"),
            command: Some("node /workspace/mock-acp-agent.js"),
        },
    )
    .await;
    let Ok(agent_session) = agent_session else {
        checks.push(Check {
            name: "agent-session",
            passed: false,
            detail: "spawn failed".into(),
        });
        let _ = actor.stop().await;
        let _ = client
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };

    // START the run: grant authorizes exactly the prompt session effect.
    let grant = Grant::new(
        "hodei-meta-harness",
        format!("run-{epoch}"),
        epoch,
        vec![EffectKind::AgentPromptSession],
    );
    assert!(grant.authorize(&EffectKind::AgentPromptSession).is_ok());
    assert!(grant.authorize(&EffectKind::WorkspaceGitCommit).is_err());
    checks.push(Check {
        name: "start-explicit-grant",
        passed: true,
        detail: format!(
            "grant run={} epoch={epoch} effects=[AgentPromptSession]; WorkspaceGitCommit denied",
            grant.run_id
        ),
    });

    let ws_url = ws_url_for(&execd_base, &agent_session.session_id);
    let mut pipe_session: PipeSession = match pipe::attach(&ws_url, AttachOptions::default()).await
    {
        Ok(session) => session,
        Err(err) => {
            checks.push(Check {
                name: "pipe-attach",
                passed: false,
                detail: err.to_string(),
            });
            let _ = actor.stop().await;
            let _ = client
                .delete(format!("{server}/v1/sandboxes/{sid}"))
                .send()
                .await;
            return finish(&checks, started.elapsed());
        }
    };

    let mut run_state = hodei_mh_run::run::RunState::start(grant, agent_session.session_id.clone());
    run_state.track_process(agent_session.session_id.clone());
    // Goal-less mock agent: finite prompt turns (no goal simulation).
    let plan = RunPlan::for_agent(
        false,
        "write the probe file",
        vec!["Create /workspace/hodei-run.txt with content 'run-5-ok'".to_string()],
    );
    let prompt_turns = plan.prompt_turns();

    // Drive: ACP handshake + finite prompts, tagging every wire message with
    // run/session/epoch identity and rejecting foreign events.
    let identity = EventIdentity {
        run_id: run_state.grant.run_id.clone(),
        session_id: run_state.session_id.clone(),
        epoch: run_state.grant.epoch,
    };
    let mut assembler = AcpAssembler::new();
    let mut next_id: u64 = 1;
    let init_id = next_id;
    next_id += 1;
    let _ = pipe_session
        .send_stdin(
            acp::encode_request(init_id, "initialize", json!({"protocolVersion": 1})).as_bytes(),
        )
        .await;
    let mut turn_index = 0usize;
    let mut prompt_answered = false;
    let mut updates = 0usize;
    let mut foreign_rejected = false;
    let deadline = Instant::now() + Duration::from_secs(30);

    // Send the first prompt turn once initialize responds.
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), pipe_session.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        match event {
            ServerEvent::Stdout(bytes) => {
                for message in assembler.push(&bytes).unwrap_or_default() {
                    let tagged = TaggedEvent {
                        identity: identity.clone(),
                        kind: message["method"].as_str().unwrap_or("response").to_string(),
                        payload: message.clone(),
                    };
                    if run_state.receive(tagged).is_err() {
                        foreign_rejected = true;
                    }
                    if message.get("method").and_then(|m| m.as_str()) == Some("session/update") {
                        updates += 1;
                    }
                    let id = message.get("id").and_then(|v| v.as_u64());
                    if id == Some(init_id) && message.get("result").is_some() {
                        let new_session = next_id;
                        next_id += 1;
                        let _ = pipe_session
                            .send_stdin(
                                acp::encode_request(
                                    new_session,
                                    "session/new",
                                    json!({"cwd": "/workspace"}),
                                )
                                .as_bytes(),
                            )
                            .await;
                    } else if message.get("result").is_some()
                        && message["result"].get("sessionId").is_some()
                    {
                        let prompt_id = next_id;
                        next_id += 1;
                        let _ = pipe_session
                            .send_stdin(
                                acp::encode_request(
                                    prompt_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": message["result"]["sessionId"],
                                        "prompt": [{"type": "text", "text": prompt_turns[turn_index]}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        turn_index += 1;
                    } else if message.get("result").is_some()
                        && message["result"].get("stopReason").is_some()
                    {
                        prompt_answered = true;
                    }
                }
            }
            ServerEvent::Exit { .. } => break,
            _ => {}
        }
        if prompt_answered {
            break;
        }
    }

    // Identity enforcement on the wire level: a fabricated event from a
    // different session must be rejected by the run state machine.
    let foreign = EventIdentity {
        run_id: "run-999".into(),
        session_id: "other".into(),
        epoch,
    };
    let rejection = run_state.receive(hodei_mh_run::run::TaggedEvent {
        identity: foreign,
        kind: "session/update".into(),
        payload: json!({}),
    });
    foreign_rejected = foreign_rejected || rejection.is_err();
    checks.push(Check {
        name: "identity-tagged-events-exclusive-session",
        passed: foreign_rejected && !run_state.pending.is_empty(),
        detail: format!(
            "tagged events accepted={} foreign rejected={foreign_rejected} updates={updates}",
            run_state.pending.len()
        ),
    });
    checks.push(Check {
        name: "finite-prompt-turns-completed",
        passed: prompt_answered,
        detail: format!(
            "{turn_index}/{} turns, answered={prompt_answered}",
            prompt_turns.len()
        ),
    });

    // STOP: invalidate pending, cancel the controlled process, report
    // initiated irreversible actions honestly.
    run_state.mark_initiated("prompt turn sent to agent");
    let pending_before = run_state.pending.len();
    let cancelled = run_state.stop();
    let _ = pipe_session.send_signal("SIGTERM").await;
    let mut released = None;
    let grace = Instant::now() + Duration::from_secs(5);
    while Instant::now() < grace {
        let Some(event) = tokio::time::timeout(Duration::from_secs(2), pipe_session.next_event())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let ServerEvent::Exit { exit_code } = event {
            released = exit_code;
            break;
        }
    }
    checks.push(Check {
        name: "stop-invalidates-cancels-reports",
        passed: run_state.pending.is_empty() && released.is_some(),
        detail: format!(
            "pending invalidated (was {pending_before}), process cancelled={cancelled:?}, exit={released:?}, irreversible reported={}",
            run_state.initiated_irreversible.len()
        ),
    });

    // Cleanup.
    let _ = api::delete_session(&execd_base, None, &agent_session.session_id).await;
    let deleted = client
        .delete(format!("{server}/v1/sandboxes/{sid}"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    let released_ticket = actor
        .release("ticket-run-5", "hodei-mh")
        .await
        .unwrap_or(false);
    let _ = actor.stop().await;
    checks.push(Check {
        name: "cleanup-and-release",
        passed: deleted && released_ticket,
        detail: format!("sandbox deleted={deleted}, claim released={released_ticket}"),
    });

    finish(&checks, started.elapsed())
}

async fn create_sandbox(client: &reqwest::Client, server: &str, image: &str) -> Option<String> {
    client
        .post(format!("{server}/v1/sandboxes"))
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 900,
            "metadata": {"probe": "task-5-run"},
        }))
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?["id"]
        .as_str()
        .map(str::to_string)
}

async fn resolve_execd(client: &reqwest::Client, server: &str, sid: &str) -> Option<String> {
    let ep = client
        .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
        .send()
        .await
        .ok()?
        .json::<Value>()
        .await
        .ok()?["endpoint"]
        .as_str()?
        .to_string();
    let execd = format!("http://{ep}");
    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it.
    crate::probe_util::wait_for_execd(&execd).await;
    Some(execd)
}
