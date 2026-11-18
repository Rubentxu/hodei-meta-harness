//! hodei-mh binary. The `acp-probe` subcommand is the TASK-1 vertical slice:
//! it proves a real ACP v1 agent session inside an OpenSandbox environment,
//! driven end to end through the hodei-mh lifecycle server and execd pipe.

use std::process::Command;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{Value, json};

use opensandbox::acp::AcpAssembler;
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions, PipeSession};

mod acp_dialogue;
mod autonomous_probe;
mod budget_probe;
mod custom_goal_probe;
mod digest;
mod docs;
mod elf;
mod goal_probe;
mod interact_probe;
mod multiproject_probe;
mod oneshot_debug;
mod package;
mod probe_util;
mod profile_probe;
mod profile_run;
mod promote_probe;
mod release_probe;
mod rollover_probe;
mod run_probe;
mod supervisor_cli;
mod supervisor_probe;
mod tracker_probe;
mod tui;
mod verify_probe;
mod workspace_probe;

use acp_dialogue::{AcpAction, AcpDialogue};
use probe_util::{MOCK_AGENT_JS, run_command_session_with_output, ws_url_for};

const PROBE_FILE: &str = "/workspace/hodei-probe.txt";

/// What a consumer reads when the binary is called without a command:
/// the real commands of the product, not one of the probes.
const USAGE: &str = "\
hodei-meta-harness — sandbox-first meta-harness driven by persistent goals

usage: hodei-mh <command> [options]

product:
  doctor --profile <name>              read-only checks and the release gate
  profile-run --profile <name>         run a project inside a pinned sandbox
  supervisor --socket <path> --project <name>
                                       the single controller of a project
  sup-status | sup-action | sup-events | attach-terminal | tui
                                       observe and drive the supervisor
  package                              build the installable bundle

probes (evidence about this host):
  release-probe | profile-probe | supervisor-probe | multiproject-probe

options:
  --version                            print the version and exit
  --help                               print this help and exit
";

/// The help text, with the documentation the consumer REALLY has.
///
/// The pointer is resolved at RUN time: a path written in the source
/// points at a file that only exists in a checkout, and the consumer
/// of an installation has the documentation next to the binary.
fn usage() -> String {
    let exe = std::env::current_exe().ok();
    let cwd = std::env::current_dir().ok();
    let source = docs::choose(&docs::candidates(exe.as_deref(), cwd.as_deref()));
    format!("{USAGE}\n{}", docs::pointer_line(source.as_ref()))
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("acp-probe") => {
            let code = acp_probe(&args[1..]).await;
            std::process::exit(code);
        }
        Some("budget-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = budget_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("release-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = release_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("autonomous-loop") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = autonomous_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("supervisor") => {
            let code = supervisor_cli::run_supervisor(&args[1..]).await;
            std::process::exit(code);
        }
        Some("sup-status") => {
            let code = supervisor_cli::run_status(&args[1..]).await;
            std::process::exit(code);
        }
        Some("sup-action") => {
            let code = supervisor_cli::run_action(&args[1..]).await;
            std::process::exit(code);
        }
        Some("sup-events") => {
            let code = supervisor_cli::run_events(&args[1..]).await;
            std::process::exit(code);
        }
        Some("attach-terminal") => {
            let code = supervisor_cli::run_attach_terminal(&args[1..]).await;
            std::process::exit(code);
        }
        Some("tui") => {
            let code = supervisor_cli::run_tui(&args[1..]).await;
            std::process::exit(code);
        }
        Some("doctor") => {
            let code = profile_run::run_doctor_cli(&args[1..]).await;
            std::process::exit(code);
        }
        Some("profile-run") => {
            let code = profile_run::run_profile(&args[1..]).await;
            std::process::exit(code);
        }
        Some("package") => {
            let code = package::run_package(&args[1..]).await;
            std::process::exit(code);
        }
        Some("multiproject-probe") => {
            let code = multiproject_probe::run(&args[1..]).await;
            std::process::exit(code);
        }
        Some("profile-probe") => {
            let code = profile_probe::run(&args[1..]).await;
            std::process::exit(code);
        }
        Some("supervisor-probe") => {
            let code = supervisor_probe::run().await;
            std::process::exit(code);
        }
        Some("custom-goal-probe") => {
            let code = custom_goal_probe::run();
            std::process::exit(code);
        }
        Some("goal-probe") => {
            let code = goal_probe::run(
                std::env::current_dir()
                    .map(|d| d.display().to_string())
                    .unwrap_or_else(|_| ".".to_string()),
            )
            .await;
            std::process::exit(code);
        }
        Some("interact-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = interact_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("oneshot-probe") => {
            let code = oneshot_debug::run(&args[1..]).await;
            std::process::exit(code);
        }
        Some("rollover-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = rollover_probe::run(&server, &image).await;
            std::process::exit(code);
        }
        Some("promote-probe") => {
            let code = promote_probe::run().await;
            std::process::exit(code);
        }
        Some("verify-probe") => {
            let server = flag(&args[1..], "--server", "http://127.0.0.1:8081");
            let image = flag(&args[1..], "--image", "opensandbox/code-interpreter:v1.1.0");
            let code = verify_probe::run(&server, &image).await;
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
        Some("--version") | Some("-V") => {
            println!("hodei-mh {}", env!("CARGO_PKG_VERSION"));
            std::process::exit(0);
        }
        Some("--help") | Some("-h") | Some("help") => {
            print!("{}", usage());
            std::process::exit(0);
        }
        _ => {
            eprint!("{}", usage());
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

    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it.
    probe_util::wait_for_execd(&execd_base).await;

    // 3. Install the agent inside the sandbox (mock script or npm adapter).
    match mode.as_str() {
        "mock" => {
            // Attach-first oneshot: unheld sessions get purged before their
            // process starts, so staging must hold the pipe immediately.
            let encoded = base64::engine::general_purpose::STANDARD.encode(MOCK_AGENT_JS);
            let install = format!(
                "mkdir -p /workspace && echo {encoded} | base64 -d > /workspace/mock-acp-agent.js && test -s /workspace/mock-acp-agent.js"
            );
            let outcome =
                probe_util::run_in_sandbox(&execd_base, &install, Duration::from_secs(20)).await;
            checks.push(ProbeCheck {
                name: "agent-install",
                passed: outcome.succeeded(),
                detail: format!(
                    "mock agent staged at /workspace/mock-acp-agent.js (exit={:?} out={:?})",
                    outcome.exit_code,
                    outcome.output.trim()
                ),
            });
        }
        "claude" => {
            // Install host-side, pack, upload, extract. The adapter is pure
            // JS (node) so it is portable; nothing is persisted beyond the
            // sandbox.
            let install_dir = std::env::temp_dir().join("hodei-ccacp");
            let host_install = Command::new("npm")
                .args([
                    "install",
                    "--prefix",
                    install_dir.to_string_lossy().as_ref(),
                    "--no-audit",
                    "--no-fund",
                    "@zed-industries/claude-code-acp@0.16.2",
                ])
                .output();
            let installed = match host_install {
                Ok(out) if out.status.success() => true,
                other => {
                    checks.push(ProbeCheck {
                        name: "agent-install",
                        passed: false,
                        detail: format!("host npm install failed: {other:?}"),
                    });
                    cleanup();
                    report(&checks, sandbox_id, started.elapsed(), 1);
                    unreachable!()
                }
            };
            let packed = Command::new("tar")
                .args([
                    "czf",
                    "/tmp/hodei-ccacp.tgz",
                    "-C",
                    install_dir.to_string_lossy().as_ref(),
                    "node_modules",
                ])
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false);
            let bundle = std::fs::read("/tmp/hodei-ccacp.tgz").unwrap_or_default();
            let uploaded = api::upload_file(&execd_base, None, bundle, "/tmp/ccacp.tgz").await;
            let probe_fs = probe_util::run_in_sandbox(
                &execd_base,
                "ls -la /tmp/ccacp.tgz",
                Duration::from_secs(15),
            )
            .await;
            let staged_outcome = probe_util::run_in_sandbox(
                &execd_base,
                "mkdir -p /workspace/ccacp && tar xzf /tmp/ccacp.tgz -C /workspace/ccacp 2>&1 | tail -2 && test -x /workspace/ccacp/node_modules/.bin/claude-code-acp",
                Duration::from_secs(120),
            )
            .await;
            let staged = staged_outcome.succeeded();
            let staged_output = staged_outcome.output;
            checks.push(ProbeCheck {
                name: "agent-install",
                passed: installed && packed && uploaded.is_ok() && staged,
                detail: format!(
                    "host install={installed} pack={packed} upload={} extract={staged} out={staged_output:?} fs={:?}",
                    uploaded.is_ok(),
                    probe_fs
                ),
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
            // Propagate an optional gateway base URL; never persist either.
            let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default();
            let base_prefix = if base.is_empty() {
                String::new()
            } else {
                format!("ANTHROPIC_BASE_URL={base} ")
            };
            // The host bridge proxy (docker gateway IP) provides the HTTPS
            // egress the agent uses. Direct internet is NOT blocked by the
            // harness: the proxy is the authorized path, not an enforced
            // boundary (see docs/profiles.md).
            format!(
                "PATH=/workspace/ccacp/node_modules/.bin:$PATH HTTPS_PROXY=http://172.17.0.1:8888 {base_prefix}ANTHROPIC_AUTH_TOKEN={token} claude-code-acp"
            )
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
    // Both modes must leave the SAME artifact: the agent (mock or real)
    // writing the requested content inside the sandbox.
    checks.push(ProbeCheck {
        name: "file-modified-in-sandbox",
        passed: content.contains("written-by-mock-agent"),
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
///
/// The policy of the conversation lives in `AcpDialogue`; this function is
/// only the transport around it: bytes in, bytes out.
async fn drive_acp_session(pipe_session: &mut PipeSession, mode: &str) -> AgentProbeResult {
    let mut checks = Vec::new();
    let mut dialogue = AcpDialogue::new();
    let mut assembler = AcpAssembler::new();
    let mut stdout_bytes = 0usize;
    let mut stderr_bytes = 0usize;
    let mut stderr_sample: Vec<u8> = Vec::new();
    let mut saw_connected = false;
    let mut saw_exit = None;

    // Handshake.
    let handshake = dialogue.handshake();
    if pipe_session.send_stdin(handshake.bytes()).await.is_err() {
        checks.push(ProbeCheck {
            name: "acp-initialize",
            passed: false,
            detail: "stdin write failed".into(),
        });
        return AgentProbeResult { checks };
    }

    let deadline = Instant::now() + Duration::from_secs(if mode == "claude" { 240 } else { 60 });
    let mut cancel_sent = false;

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
                    if let Some(AcpAction::Write(out)) = dialogue.on_message(&message) {
                        let _ = pipe_session.send_stdin(out.as_bytes()).await;
                    }
                }
            }
            ServerEvent::Stderr(bytes) => {
                stderr_bytes += bytes.len();
                stderr_sample.extend(bytes);
                if stderr_sample.len() > 4096 {
                    let keep = stderr_sample.split_off(stderr_sample.len() - 2048);
                    stderr_sample = keep;
                }
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
        if dialogue.prompt_answer().is_some() && !cancel_sent {
            let answer = dialogue.prompt_answer().cloned().unwrap_or(Value::Null);
            checks.push(ProbeCheck {
                name: "acp-prompt-response",
                passed: answer.get("stopReason").is_some(),
                detail: format!(
                    "stopReason={:?} agent_text_tail={:?}",
                    answer.get("stopReason"),
                    dialogue
                        .agent_text()
                        .chars()
                        .rev()
                        .take(400)
                        .collect::<String>()
                ),
            });
            checks.push(ProbeCheck {
                name: "structured-events",
                passed: dialogue.updates() > 0,
                detail: format!("{} session/update notifications", dialogue.updates()),
            });
            checks.push(ProbeCheck {
                name: "stdout-stderr-separation",
                passed: stdout_bytes > 0 && stderr_bytes > 0,
                detail: format!(
                    "stdout={stdout_bytes}B stderr={stderr_bytes}B (ACP on stdout, logs on stderr)"
                ),
            });
            let cancel = dialogue.cancel();
            let _ = pipe_session.send_stdin(cancel.bytes()).await;
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
        name: "acp-initialize",
        passed: dialogue.protocol_version().is_some(),
        detail: match dialogue.protocol_version() {
            Some(version) => format!("agent protocol v{version}"),
            None => "the agent never answered the handshake".to_string(),
        },
    });
    checks.push(ProbeCheck {
        name: "connected-frame",
        passed: saw_connected,
        detail: format!("connected={saw_connected}"),
    });
    checks.push(ProbeCheck {
        name: "cancellation-releases-process",
        passed: saw_exit.is_some(),
        detail: format!(
            "exit_code observed: {saw_exit:?}; methods_count={}; stderr_tail={:?}",
            dialogue.methods().len(),
            String::from_utf8_lossy(&stderr_sample)
        ),
    });
    AgentProbeResult { checks }
}
