//! `hodei-mh run-probe`: TASK-5 vertical slice — one exclusive ACP session
//! driven from START (explicit grant + coordinator claim) to STOP
//! (invalidation, cancellation, honest irreversible report).

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use hodei_mh_coordinator::actor::CoordinatorActor;
use hodei_mh_coordinator::core::ClaimResult;
use hodei_mh_run::grant::{EffectKind, Grant};
use hodei_mh_run::run::{EventIdentity, Phase, RunPlan, TaggedEvent};
use opensandbox::acp::{self, AcpAssembler};
use opensandbox::control::ControlPlane;
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::endpoint::ExecdEndpoint;
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, PipeError, PipeSession};

use crate::probe_util::MOCK_AGENT_JS;

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// The verdict of a probe: every check it reported passed.
///
/// A probe that reported NO check did not pass: an omitted check is a
/// failure, never a green probe. The verdict is a pure function of the
/// checks, so the rule cannot drift from the report.
fn all_passed(checks: &[Check]) -> bool {
    !checks.is_empty() && checks.iter().all(|check| check.passed)
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let client = ControlPlane::new(server);
    let mut checks: Vec<Check> = Vec::new();
    let finish = |checks: &[Check], elapsed: Duration| -> i32 {
        let summary = json!({
            "task": "TASK-5",
            "checks": checks.iter().map(|c| json!({
                "name": c.name, "passed": c.passed, "detail": c.detail,
            })).collect::<Vec<_>>(),
            "all_passed": all_passed(checks),
            "elapsed_seconds": elapsed.as_secs_f64(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        if all_passed(checks) { 0 } else { 1 }
    };

    // START: explicit grant bound to project + coordinator claim (single
    // writer). A second claim attempt for the same ticket is denied.
    let db = std::env::temp_dir().join(format!("hodei-run-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&db);
    let actor = match CoordinatorActor::start("run-probe", &db) {
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
    let sid = create_sandbox(&client, image).await;
    let Some(sid) = sid else {
        checks.push(Check {
            name: "sandbox-create",
            passed: false,
            detail: "create failed".into(),
        });
        let _ = actor.stop().await;
        return finish(&checks, started.elapsed());
    };
    checks.push(Check {
        name: "sandbox-create",
        passed: true,
        detail: format!("the sandbox {sid} is created"),
    });
    let Some(execd) = resolve_execd(&client, &sid).await else {
        checks.push(Check {
            name: "execd-endpoint",
            passed: false,
            detail: "resolve failed".into(),
        });
        let _ = actor.stop().await;
        let _ = client
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };
    checks.push(Check {
        name: "execd-endpoint",
        passed: true,
        detail: format!("the endpoint of the sandbox is {}", execd),
    });
    // The credential of the sandbox is part of the endpoint contract:
    // the execd refuses a handshake that does not present it. The probe
    // says so as a check of its own, so a refusal is never confused
    // with a missing credential.
    checks.push(credential_check(&execd));
    let encoded = base64::engine::general_purpose::STANDARD.encode(MOCK_AGENT_JS);
    let _ = crate::probe_util::run_command_session_with_output(
        &execd,
        &format!(
            "mkdir -p /workspace && echo {encoded} | base64 -d > /workspace/mock-acp-agent.js"
        ),
    )
    .await;
    let agent_session = api::create_session(
        &execd,
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
            .delete(&ControlPlane::sandbox_path(&sid))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };
    checks.push(Check {
        name: "agent-session",
        passed: true,
        detail: format!(
            "the mock agent runs in session {}",
            agent_session.session_id
        ),
    });

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

    // The attach is a check of the probe, not only a failure: a pipe
    // that was attached and a pipe that was never reported are two
    // different stories, and the report must tell them apart.
    let mut pipe_session: PipeSession = match attach_pipe(&execd, &agent_session.session_id).await {
        Ok(session) => {
            checks.push(attached_check(&agent_session.session_id));
            session
        }
        Err(err) => {
            checks.push(refused_check(&err));
            let _ = actor.stop().await;
            let _ = client
                .delete(&ControlPlane::sandbox_path(&sid))
                .send()
                .await;
            return finish(&checks, started.elapsed());
        }
    };

    // The run is REQUESTED: the session is bound to the grant, but nothing
    // has answered yet that the work started. It becomes `Running` only
    // when the agent answers the handshake.
    let mut run_state =
        hodei_mh_run::run::RunState::pending(grant, agent_session.session_id.clone());
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
                        // The agent answered the handshake: THAT is the
                        // runtime's confirmation. Before it, the run was
                        // only STARTING.
                        run_state.confirm();
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

    checks.push(Check {
        name: "the-run-is-running-only-after-the-agent-answered",
        passed: run_state.phase() == &Phase::Running,
        detail: format!(
            "the run was STARTING until the agent answered the handshake; the phase after the real prompt turn is {:?}",
            run_state.phase()
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
    let _ = api::delete_session(&execd, &agent_session.session_id).await;
    let deleted = client
        .delete(&ControlPlane::sandbox_path(&sid))
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

/// The check that the endpoint carried the credential of the sandbox.
///
/// The execd is the execution plane of a sandbox and it authorizes the
/// calls that reach it with the credential the endpoint contract
/// returned. A contract without that credential is a failed check, not
/// a detail: the handshake would be refused.
fn credential_check(execd: &ExecdEndpoint) -> Check {
    Check {
        name: "execd-credential-delivered",
        passed: execd.access_token().is_some(),
        detail: match execd.access_token() {
            Some(_) => "the endpoint contract carried the credential of the sandbox".into(),
            None => {
                "the endpoint contract carried NO credential: the execd would refuse the handshake"
                    .into()
            }
        },
    }
}

/// The check that reports an attached pipe.
fn attached_check(session_id: &str) -> Check {
    Check {
        name: "pipe-attach",
        passed: true,
        detail: format!(
            "the pipe of session {session_id} is attached with the credential of the sandbox"
        ),
    }
}

/// The check that reports a refused handshake.
///
/// The refusal of the execd is a FAILURE of the probe, reported as a
/// check of its own: a check that is omitted is a check nobody read.
fn refused_check(error: &PipeError) -> Check {
    Check {
        name: "pipe-attach",
        passed: false,
        detail: error.to_string(),
    }
}

/// Attach to the pipe of a session, delivering the credential of the
/// sandbox.
///
/// The credential travels in the header the execd expects, never in the
/// URL: a URL is read, logged and forwarded, and a credential that
/// travels in one is a credential that leaks.
async fn attach_pipe(execd: &ExecdEndpoint, session_id: &str) -> Result<PipeSession, PipeError> {
    pipe::attach(&execd.ws_url(session_id), execd.attach_options()).await
}

async fn create_sandbox(client: &ControlPlane, image: &str) -> Option<String> {
    client
        .post(ControlPlane::sandboxes_path())
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
    // answer: wait (bounded) before using it.
    crate::probe_util::wait_for_execd(&execd).await;
    Some(execd)
}

#[cfg(test)]
mod tests {
    use super::*;
    use opensandbox::execd::endpoint::EXECD_ACCESS_TOKEN_HEADER;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;

    /// A fake execd that REFUSES every handshake and records the
    /// request it received. The refusal is what an execd answers to a
    /// handshake it does not authorize.
    async fn spawn_refusing_execd() -> (String, oneshot::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the listener");
        let addr = listener.local_addr().expect("the address");
        let (tx, rx) = oneshot::channel();
        tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("the connection");
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let read = socket.read(&mut buffer).await.unwrap_or(0);
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let _ = tx.send(String::from_utf8_lossy(&request).to_lowercase());
            let _ = socket
                .write_all(b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\n\r\n")
                .await;
            let _ = socket.shutdown().await;
        });
        (format!("http://{addr}"), rx)
    }

    /// The endpoint a contract with this credential describes.
    fn endpoint_of_contract(origin: &str, token: Option<&str>) -> ExecdEndpoint {
        let mut contract = json!({"endpoint": origin.trim_start_matches("http://")});
        if let Some(token) = token {
            contract["headers"] = json!({EXECD_ACCESS_TOKEN_HEADER: token});
        }
        ExecdEndpoint::from_contract(&contract).expect("the endpoint of the contract")
    }

    #[tokio::test]
    async fn the_handshake_carries_the_credential_the_endpoint_contract_returned() {
        let (origin, request) = spawn_refusing_execd().await;
        let execd = endpoint_of_contract(&origin, Some("token-1"));
        let _ = attach_pipe(&execd, "session-1").await;
        let request = request.await.expect("the recorded handshake");
        assert!(
            request.contains(&EXECD_ACCESS_TOKEN_HEADER.to_lowercase()),
            "{request}"
        );
        assert!(request.contains("token-1"), "{request}");
    }

    #[tokio::test]
    async fn a_handshake_without_the_credential_is_refused() {
        let (origin, request) = spawn_refusing_execd().await;
        let execd = endpoint_of_contract(&origin, None);
        let error = match attach_pipe(&execd, "session-1").await {
            Ok(_) => panic!("a handshake without the credential must be refused"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("401"), "{error}");
        let request = request.await.expect("the recorded handshake");
        assert!(
            !request.contains(&EXECD_ACCESS_TOKEN_HEADER.to_lowercase()),
            "{request}"
        );
    }

    #[test]
    fn a_contract_without_a_credential_is_a_failed_check() {
        let execd = endpoint_of_contract("http://127.0.0.1:44772", None);
        let check = credential_check(&execd);
        assert_eq!(check.name, "execd-credential-delivered");
        assert!(!check.passed, "{}", check.detail);
    }

    #[test]
    fn a_contract_with_a_credential_is_a_passed_check() {
        let execd = endpoint_of_contract("http://127.0.0.1:44772", Some("token-1"));
        let check = credential_check(&execd);
        assert!(check.passed, "{}", check.detail);
    }

    #[test]
    fn the_pipe_attach_check_is_reported_passed_or_failed() {
        let attached = attached_check("session-1");
        assert_eq!(attached.name, "pipe-attach");
        assert!(attached.passed, "{}", attached.detail);
        let refused = refused_check(&PipeError::WriteClosed);
        assert_eq!(refused.name, "pipe-attach");
        assert!(!refused.passed);
        assert!(!refused.detail.is_empty());
    }

    #[test]
    fn an_omitted_check_is_never_a_pass() {
        assert!(!all_passed(&[]));
        assert!(!all_passed(&[Check {
            name: "pipe-attach",
            passed: false,
            detail: "refused".into(),
        }]));
        assert!(all_passed(&[Check {
            name: "pipe-attach",
            passed: true,
            detail: "attached".into(),
        }]));
    }
}
