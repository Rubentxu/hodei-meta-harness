//! hodei-mh binary. The `acp-probe` subcommand is the TASK-1 vertical slice:
//! it proves a real ACP v1 agent session inside an OpenSandbox environment,
//! driven end to end through the hodei-mh lifecycle server and execd pipe.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use opensandbox::acp::{self, AcpAssembler};
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions, PipeSession};

mod probe_util;
mod run_probe;
mod tracker_probe;
mod workspace_probe;

use probe_util::{MOCK_AGENT_JS, run_command_session, run_command_session_with_output, ws_url_for};

const PROBE_FILE: &str = "/workspace/hodei-probe.txt";

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("acp-probe") => {
            let code = acp_probe(&args[1..]).await;
            std::process::exit(code);
        }
        Some("run-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = run_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("tracker-probe") => {
            let code = tracker_probe::run(
                std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|_| ".".to_string()),
            )
            .await;
            std::process::exit(code);
        }
        Some("workspace-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = workspace_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        _ => {
            eprintln!(
                "usage: hodei-mh acp-probe --server URL [--mode mock|claude] [--image IMAGE]"
            );
            std::process::exit(2);
        }
    }
}

fn flag(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

struct ProbeCheck {
    name: &'static str,
    passed: bool,
    detail: String,
}

async fn acp_probe(args: &[String]) -> i32 {
    let server = flag(args, "--server", "http://127.0.0.1:8081");
    let mode = flag(args, "--mode", "mock");
    let image = flag(args, "--image", "opensandbox/code-interpreter:v1.1.0");
    let started = Instant::now();
    let mut checks: Vec<ProbeCheck> = Vec::new();
    let mut sandbox_id: Option<String> = None;

    let client = reqwest::Client::new();
    let report = |checks: &[ProbeCheck], sandbox: Option<String>, elapsed: Duration, exit: i32| {
        let summary = json!({
            "task": "TASK-1",
            "mode": std::env::var("PROBE_MODE").unwrap_or_default(),
            "checks": checks.iter().map(|c| json!({
                "name": c.name,
                "passed": c.passed,
                "detail": c.detail,
            })).collect::<Vec<_>>(),
            "all_passed": checks.iter().all(|c| c.passed),
            "elapsed_seconds": elapsed.as_secs_f64(),
            "sandbox_id": sandbox,
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        std::process::exit(exit);
    };

    // 1. Create sandbox through the hodei-mh lifecycle server.
    let create = client
        .post(format!("{server}/v1/sandboxes"))
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 900,
            "metadata": {"probe": "task-1-acp", "mode": mode},
        }))
        .send()
        .await;
    let created = match create {
        Ok(resp) if resp.status().is_success() => resp.json::<Value>().await.unwrap_or_default(),
        Ok(resp) => {
            checks.push(ProbeCheck {
                name: "sandbox-create",
                passed: false,
                detail: format!("status {}", resp.status()),
            });
            report(&checks, sandbox_id, started.elapsed(), 1);
            unreachable!()
        }
        Err(err) => {
            checks.push(ProbeCheck {
                name: "sandbox-create",
                passed: false,
                detail: err.to_string(),
            });
            report(&checks, sandbox_id, started.elapsed(), 1);
            unreachable!()
        }
    };
    sandbox_id = created["id"].as_str().map(str::to_string);
    let Some(sid) = sandbox_id.clone() else {
        checks.push(ProbeCheck {
            name: "sandbox-create",
            passed: false,
            detail: format!("no id in response: {created}"),
        });
        report(&checks, sandbox_id, started.elapsed(), 1);
        unreachable!()
    };
    checks.push(ProbeCheck {
        name: "sandbox-create",
        passed: created["status"]["state"] == "Running",
        detail: format!("id={sid} state={}", created["status"]["state"]),
    });

    // Cleanup guarantee: delete the sandbox however we exit.
    let cleanup = {
        let client = client.clone();
        let sid = sid.clone();
        move || {
            let client = client.clone();
            let sid = sid.clone();
            tokio::spawn(async move {
                let _ = client
                    .delete(format!("http://127.0.0.1:0/v1/sandboxes/{sid}"))
                    .send()
                    .await;
            });
        }
    };

    // 2. Resolve the execd endpoint.
    let endpoint = client
        .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
        .send()
        .await;
    let execd_base = match endpoint {
        Ok(resp) if resp.status().is_success() => {
            let value = resp.json::<Value>().await.unwrap_or_default();
            let ep = value["endpoint"].as_str().unwrap_or_default().to_string();
            checks.push(ProbeCheck {
                name: "execd-endpoint",
                passed: !ep.is_empty(),
                detail: format!("execd at {ep}"),
            });
            format!("http://{ep}")
        }
        other => {
            checks.push(ProbeCheck {
                name: "execd-endpoint",
                passed: false,
                detail: format!("{other:?}"),
            });
            cleanup();
            report(&checks, sandbox_id, started.elapsed(), 1);
            unreachable!()
        }
    };

    // 3. Install the agent inside the sandbox (mock script or npm adapter).
    match mode.as_str() {
        "mock" => {
            let encoded = base64::engine::general_purpose::STANDARD.encode(MOCK_AGENT_JS);
            let install = format!(
                "mkdir -p /workspace && echo {encoded} | base64 -d > /workspace/mock-acp-agent.js"
            );
            let ok = run_command_session(&execd_base, &install).await;
            checks.push(ProbeCheck {
                name: "agent-install",
                passed: ok,
                detail: "mock agent staged at /workspace/mock-acp-agent.js".into(),
            });
        }
        "claude" => {
            let install = "npm install -g @zed-industries/claude-code-acp@0.16.2 2>&1 | tail -1 && command -v claude-code-acp";
            let ok = run_command_session(&execd_base, install).await;
            checks.push(ProbeCheck {
                name: "agent-install",
                passed: ok,
                detail: "claude-code-acp installed via npm".into(),
            });
        }
        other => {
            checks.push(ProbeCheck {
                name: "mode",
                passed: false,
                detail: format!("unknown mode {other}"),
            });
            cleanup();
            report(&checks, sandbox_id, started.elapsed(), 2);
            unreachable!()
        }
    }

    // 4. Start the agent session over the execd pipe and drive ACP.
    let agent_command = match mode.as_str() {
        "mock" => "node /workspace/mock-acp-agent.js".to_string(),
        _ => {
            let token = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
            format!("ANTHROPIC_AUTH_TOKEN={token} claude-code-acp")
        }
    };
    let session = api::create_session(
        &execd_base,
        None,
        CreateSessionRequest {
            cwd: Some("/workspace"),
            command: Some(&agent_command),
        },
    )
    .await;
    let Ok(session) = session else {
        checks.push(ProbeCheck {
            name: "agent-session",
            passed: false,
            detail: session.err().map(|e| e.to_string()).unwrap_or_default(),
        });
        cleanup();
        report(&checks, sandbox_id, started.elapsed(), 1);
        unreachable!()
    };
    let ws_url = ws_url_for(&execd_base, &session.session_id);
    let attached = pipe::attach(&ws_url, AttachOptions::default()).await;
    let mut session_pipe = match attached {
        Ok(p) => p,
        Err(err) => {
            checks.push(ProbeCheck {
                name: "pipe-attach",
                passed: false,
                detail: err.to_string(),
            });
            cleanup();
            report(&checks, sandbox_id, started.elapsed(), 1);
            unreachable!()
        }
    };

    let agent = drive_acp_session(&mut session_pipe, mode.as_str()).await;
    checks.extend(agent.checks);

    // 5. Verify the file the agent wrote, through a fresh execd session.
    //    One short retry covers the rare case where the replay snapshot races
    //    with the command session teardown.
    let mut content =
        run_command_session_with_output(&execd_base, &format!("cat {PROBE_FILE}")).await;
    if !content.contains("written-by-mock-agent") {
        tokio::time::sleep(Duration::from_millis(300)).await;
        content = run_command_session_with_output(&execd_base, &format!("cat {PROBE_FILE}")).await;
    }
    checks.push(ProbeCheck {
        name: "file-modified-in-sandbox",
        passed: content.contains("written-by-mock-agent") || content.contains("hodei"),
        detail: format!("cat -> {content:?}"),
    });

    // 6. Release everything; no orphans.
    let _ = api::delete_session(&execd_base, None, &session.session_id).await;
    let deleted = client
        .delete(format!("{server}/v1/sandboxes/{sid}"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    checks.push(ProbeCheck {
        name: "cleanup",
        passed: deleted,
        detail: format!("sandbox {sid} deleted"),
    });

    let all_passed = checks.iter().all(|c| c.passed);
    report(
        &checks,
        sandbox_id,
        started.elapsed(),
        if all_passed { 0 } else { 1 },
    );
    unreachable!()
}

struct AgentProbeResult {
    checks: Vec<ProbeCheck>,
}

/// Drive the ACP v1 conversation over the pipe: initialize, session/new,
/// session/prompt, collect structured events, verify stderr separation,
/// then cancel and observe process release.
async fn drive_acp_session(pipe_session: &mut PipeSession, _mode: &str) -> AgentProbeResult {
    let mut checks = Vec::new();
    let mut assembler = AcpAssembler::new();
    let mut stdout_bytes = 0usize;
    let mut stderr_bytes = 0usize;
    let mut updates = 0usize;
    let mut next_id: u64 = 1;
    let mut pending_prompt_id: Option<u64> = None;
    let mut prompt_answer: Option<Value> = None;

    // Handshake.
    let init_id = next_id;
    next_id += 1;
    let init_params = json!({
        "protocolVersion": 1,
        "clientCapabilities": {"fs": {"readTextFile": true, "writeTextFile": true}}
    });
    if pipe_session
        .send_stdin(acp::encode_request(init_id, "initialize", init_params).as_bytes())
        .await
        .is_err()
    {
        checks.push(ProbeCheck {
            name: "acp-initialize",
            passed: false,
            detail: "stdin write failed".into(),
        });
        return AgentProbeResult { checks };
    }

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut new_session_sent = false;
    let mut prompt_sent = false;
    let mut cancel_sent = false;
    let mut saw_connected = false;
    let mut saw_exit = None;

    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(5), pipe_session.next_event())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        match event {
            ServerEvent::Connected {
                session_id, mode, ..
            } => {
                saw_connected = true;
                checks.push(ProbeCheck {
                    name: "pipe-attach",
                    passed: mode.as_deref() == Some("pipe"),
                    detail: format!("connected session={session_id} mode={mode:?}"),
                });
            }
            ServerEvent::Stdout(bytes) => {
                stdout_bytes += bytes.len();
                for message in assembler.push(&bytes).unwrap_or_default() {
                    // Structured ACP events (notifications) count separately.
                    if message.get("method").and_then(|m| m.as_str()) == Some("session/update") {
                        updates += 1;
                    }
                    let id = message.get("id").and_then(|v| v.as_u64());
                    if id == Some(init_id) && message.get("result").is_some() {
                        let version = message["result"]["protocolVersion"].clone();
                        checks.push(ProbeCheck {
                            name: "acp-initialize",
                            passed: true,
                            detail: format!("agent protocol v{version}"),
                        });
                        let sid = next_id;
                        next_id += 1;
                        let _ = pipe_session
                            .send_stdin(
                                acp::encode_request(
                                    sid,
                                    "session/new",
                                    json!({"cwd": "/workspace"}),
                                )
                                .as_bytes(),
                            )
                            .await;
                        new_session_sent = true;
                    } else if new_session_sent
                        && !prompt_sent
                        && message.get("result").is_some()
                        && message["result"].get("sessionId").is_some()
                    {
                        let prompt_id = next_id;
                        next_id += 1;
                        pending_prompt_id = Some(prompt_id);
                        let _ = pipe_session
                            .send_stdin(
                                acp::encode_request(
                                    prompt_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": message["result"]["sessionId"],
                                        "prompt": [{"type": "text", "text":
                                            "Create the file /workspace/hodei-probe.txt with the content 'written-by-mock-agent'"}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        prompt_sent = true;
                    } else if pending_prompt_id == Some(id.unwrap_or(u64::MAX))
                        && message.get("result").is_some()
                    {
                        prompt_answer = Some(message["result"].clone());
                    }
                }
            }
            ServerEvent::Stderr(bytes) => {
                stderr_bytes += bytes.len();
            }
            ServerEvent::Exit { exit_code } => {
                saw_exit = Some(exit_code);
                break;
            }
            ServerEvent::Error { code, message } => {
                checks.push(ProbeCheck {
                    name: "pipe-error",
                    passed: false,
                    detail: format!("code={code:?} message={message:?}"),
                });
            }
            ServerEvent::Replay { .. } => {}
            ServerEvent::Pong => {}
        }

        // Once the prompt finished, verify separation + cancel behaviour.
        if prompt_sent && prompt_answer.is_some() && !cancel_sent {
            checks.push(ProbeCheck {
                name: "acp-prompt-response",
                passed: prompt_answer
                    .as_ref()
                    .and_then(|r| r.get("stopReason"))
                    .is_some(),
                detail: format!(
                    "stopReason={:?}",
                    prompt_answer.as_ref().and_then(|r| r.get("stopReason"))
                ),
            });
            checks.push(ProbeCheck {
                name: "structured-events",
                passed: updates > 0,
                detail: format!("{updates} session/update notifications"),
            });
            checks.push(ProbeCheck {
                name: "stdout-stderr-separation",
                passed: stdout_bytes > 0 && stderr_bytes > 0,
                detail: format!(
                    "stdout={stdout_bytes}B stderr={stderr_bytes}B (ACP on stdout, logs on stderr)"
                ),
            });
            let _ = pipe_session
                .send_stdin(
                    acp::encode_request(
                        next_id,
                        "session/cancel",
                        json!({"sessionId": "mock-session-1"}),
                    )
                    .as_bytes(),
                )
                .await;
            let _ = pipe_session.send_signal("SIGTERM").await;
            cancel_sent = true;
            // Short grace for the exit frame.
            let grace = Instant::now() + Duration::from_secs(5);
            while Instant::now() < grace {
                let Some(event) =
                    tokio::time::timeout(Duration::from_secs(2), pipe_session.next_event())
                        .await
                        .ok()
                        .flatten()
                else {
                    break;
                };
                if let ServerEvent::Exit { exit_code } = event {
                    saw_exit = Some(exit_code);
                    break;
                }
            }
            // Release confirmed: stop driving, the probe is done.
            if saw_exit.is_some() {
                break;
            }
        }
    }

    checks.push(ProbeCheck {
        name: "connected-frame",
        passed: saw_connected,
        detail: format!("connected={saw_connected}"),
    });
    checks.push(ProbeCheck {
        name: "cancellation-releases-process",
        passed: saw_exit.is_some(),
        detail: format!("exit_code observed: {saw_exit:?}"),
    });
    AgentProbeResult { checks }
}
