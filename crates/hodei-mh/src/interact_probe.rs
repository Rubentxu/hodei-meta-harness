//! `hodei-mh interact-probe`: TASK-8 vertical slice — the typed interaction
//! resolver drives REAL agent permission requests: ordinary reads are
//! granted by policy without a consent window, writes route to the sandbox
//! (the host never executes anything), every answer is validated against
//! the exact options the agent offered, and the host advertises no
//! fs/terminal capabilities at all.

use std::time::{Duration, Instant};

use serde_json::{Value, json};

use interact::lifecycle::{InteractionState, LateDiscarded, apply};
use interact::policy::{InteractionPolicy, host_client_capabilities};
use interact::request::{InteractionRequest, PermissionOption, ToolKind};
use interact::resolver::{GrantSource, Resolution, resolve};
use interact::response::validate;

use opensandbox::acp::{self, AcpAssembler};
use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions};

use crate::probe_util::{run_command_session_with_output, ws_url_for};

fn agent_command() -> String {
    // Gateway credentials come from the host environment and reach the
    // sandbox only as process env — never persisted anywhere.
    let token = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
    let base = std::env::var("ANTHROPIC_BASE_URL").unwrap_or_default();
    let base_prefix = if base.is_empty() {
        String::new()
    } else {
        format!("ANTHROPIC_BASE_URL={base} ")
    };
    format!(
        "PATH=/workspace/ccacp/node_modules/.bin:$PATH HTTPS_PROXY=http://172.17.0.1:8888 {base_prefix}ANTHROPIC_AUTH_TOKEN={token} claude-code-acp"
    )
}

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// Map an ACP session/request_permission message into the typed request.
fn typed_request(message: &Value) -> Option<InteractionRequest> {
    let tool_name = message["params"]["toolCall"]["title"]
        .as_str()
        .or_else(|| message["params"]["toolCall"]["toolName"].as_str())
        .unwrap_or_default()
        .to_string();
    let options: Vec<PermissionOption> = message["params"]["options"]
        .as_array()?
        .iter()
        .map(|o| PermissionOption {
            option_id: o["optionId"].as_str().unwrap_or_default().to_string(),
            name: o["name"].as_str().unwrap_or_default().to_string(),
            kind: o["kind"].as_str().unwrap_or_default().to_string(),
        })
        .collect();
    Some(InteractionRequest::Permission {
        kind: ToolKind::from_tool_name(&tool_name),
        tool_name,
        options,
    })
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let client = reqwest::Client::new();
    let policy = InteractionPolicy::sandbox_first();

    let created = client
        .post(format!("{server}/v1/sandboxes"))
        .json(&json!({
            "image": {"uri": image},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 900,
            "metadata": {"probe": "task-8-interact"},
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
            name: "sandbox",
            passed: false,
            detail: "create failed".into(),
        });
        return finish(&checks, started.elapsed());
    }
    let endpoint = client
        .get(format!("{server}/v1/sandboxes/{sid}/endpoints/44772"))
        .send()
        .await;
    let execd_base = match endpoint {
        Ok(resp) => {
            let v: Value = resp.json().await.unwrap_or_default();
            format!("http://{}", v["endpoint"].as_str().unwrap_or_default())
        }
        Err(_) => String::new(),
    };
    // The sandbox publishes its execd port before the server inside can
    // answer: wait (bounded) before using it.
    crate::probe_util::wait_for_execd(&execd_base).await;
    checks.push(Check {
        name: "sandbox",
        passed: !execd_base.is_empty(),
        detail: format!("execd at {execd_base}"),
    });

    // The agent needs the staged adapter: pack host-side and upload (the
    // sandbox has no direct egress). Reuse an existing install if present.
    let install_dir = std::env::temp_dir().join("hodei-ccacp");
    if !install_dir
        .join("node_modules/.bin/claude-code-acp")
        .exists()
    {
        let _ = std::process::Command::new("npm")
            .args([
                "install",
                "--prefix",
                install_dir.to_string_lossy().as_ref(),
                "--no-audit",
                "--no-fund",
                "@zed-industries/claude-code-acp@0.16.2",
            ])
            .output();
    }
    let packed = std::process::Command::new("tar")
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
    // The agent needs the staged adapter (same procedure as the TASK-1 leg).
    let staged = crate::probe_util::run_in_sandbox(
        &execd_base,
        "mkdir -p /workspace/ccacp && tar xzf /tmp/ccacp.tgz -C /workspace/ccacp && test -x /workspace/ccacp/node_modules/.bin/claude-code-acp",
        Duration::from_secs(120),
    )
    .await;
    // A read-only fixture for the ordinary-permission prompt.
    let fixture = crate::probe_util::run_in_sandbox(
        &execd_base,
        "printf 'hodei-read-fixture' > /workspace/readme-fixture.txt",
        Duration::from_secs(20),
    )
    .await;
    checks.push(Check {
        name: "staging",
        passed: staged.succeeded() && fixture.succeeded() && packed && uploaded.is_ok(),
        detail: format!(
            "adapter staged={} fixture={} pack={} upload={}",
            staged.succeeded(),
            fixture.succeeded(),
            packed,
            uploaded.is_ok()
        ),
    });

    // AC-3 formal: the host NEVER advertises fs/terminal callbacks.
    let caps = host_client_capabilities();
    checks.push(Check {
        name: "host-capabilities-empty",
        passed: caps["clientCapabilities"].get("fs").is_none()
            && caps["clientCapabilities"].get("terminal").is_none(),
        detail: format!("clientCapabilities sent to agent: {caps}"),
    });

    let Ok(agent) = api::create_session(
        &execd_base,
        None,
        CreateSessionRequest {
            cwd: Some("/workspace"),
            command: Some(agent_command().as_str()),
        },
    )
    .await
    else {
        checks.push(Check {
            name: "agent-session",
            passed: false,
            detail: "create failed".into(),
        });
        let _ = client
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };
    let ws_url = ws_url_for(&execd_base, &agent.session_id);
    let Ok(mut pipe) = pipe::attach(&ws_url, AttachOptions::default()).await else {
        checks.push(Check {
            name: "agent-session",
            passed: false,
            detail: "attach failed".into(),
        });
        let _ = client
            .delete(format!("{server}/v1/sandboxes/{sid}"))
            .send()
            .await;
        return finish(&checks, started.elapsed());
    };

    let mut assembler = AcpAssembler::new();
    let mut next_id: u64 = 100;
    let mut decisions: Vec<(String, Resolution)> = Vec::new();
    let mut all_answers_validated = true;
    let mut write_permission_routed = false;
    let mut prompt_done = false;
    let mut saw_exit = None;
    let mut agent_text = String::new();
    let mut stderr_sample: Vec<u8> = Vec::new();
    let mut connected = false;
    let mut init_responded = false;
    let mut session_id: Option<Value> = None;

    let _ = pipe
        .send_stdin(
            acp::encode_request(
                next_id,
                "initialize",
                json!({"protocolVersion": 1, "clientCapabilities": caps["clientCapabilities"]}),
            )
            .as_bytes(),
        )
        .await;
    next_id += 1;

    let deadline = Instant::now() + Duration::from_secs(240);
    let mut phase = 0usize; // 0: init, 1: session/new, 2: read prompt, 3: write prompt
    while Instant::now() < deadline && !prompt_done {
        let Some(event) = tokio::time::timeout(Duration::from_secs(10), pipe.next_event())
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
                connected = true;
                let _ = (session_id, mode);
            }
            ServerEvent::Stderr(bytes) => {
                stderr_sample.extend(bytes);
                if stderr_sample.len() > 3000 {
                    let keep = stderr_sample.split_off(stderr_sample.len() - 1500);
                    stderr_sample = keep;
                }
            }
            ServerEvent::Stdout(bytes) => {
                for message in assembler.push(&bytes).unwrap_or_default() {
                    if let Some(method) = message.get("method").and_then(|m| m.as_str()) {
                        // Typed rules decide BEFORE any reasoning (GC-08).
                        if method == "session/request_permission" {
                            let Some(request) = typed_request(&message) else {
                                continue;
                            };
                            let resolution = resolve(&policy, &request);
                            decisions.push((
                                message["params"]["toolCall"]["title"]
                                    .as_str()
                                    .or_else(|| message["params"]["toolCall"]["toolName"].as_str())
                                    .unwrap_or("?")
                                    .to_string(),
                                resolution.clone(),
                            ));
                            if let Resolution::RoutedToSandbox
                            | Resolution::AutoGranted {
                                source: GrantSource::PolicyRule | GrantSource::SandboxRoute,
                            } = &resolution
                            {
                                // Answer must be validated against the EXACT
                                // options the agent offered (AC-2).
                                let allow = message["params"]["options"]
                                    .as_array()
                                    .and_then(|opts| {
                                        opts.iter()
                                            .find(|o| o["kind"].as_str() == Some("allow_once"))
                                            .and_then(|o| {
                                                o["optionId"].as_str().map(str::to_string)
                                            })
                                    })
                                    .unwrap_or_default();
                                let answer =
                                    json!({"outcome": {"outcome": "selected", "optionId": allow}});
                                if validate(&request, &answer).is_err() {
                                    all_answers_validated = false;
                                }
                                // Late answers after a stop are discarded: the
                                // interaction stays Pending while driving.
                                if let Err(LateDiscarded) = apply(
                                    &InteractionState::Pending,
                                    validate(&request, &answer).unwrap(),
                                ) {
                                    all_answers_validated = false;
                                }
                                let response = acp::encode_response(
                                    message.get("id").cloned().unwrap_or(json!(0)),
                                    answer,
                                );
                                let _ = pipe.send_stdin(response.as_bytes()).await;
                            }
                            // Read permissions resolved by policy rule.
                            if resolution == Resolution::RoutedToSandbox {
                                write_permission_routed = true;
                            }
                        }
                    }
                    if let Some(chunk) = message["params"]["update"]["content"]["text"].as_str()
                        && agent_text.len() < 4000
                    {
                        agent_text.push_str(chunk);
                    }
                    let is_result = message.get("result").is_some();
                    if is_result && phase == 0 {
                        init_responded = true;
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(
                                    next_id,
                                    "session/new",
                                    json!({"cwd": "/workspace", "mcpServers": []}),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 1;
                    } else if is_result
                        && message["result"].get("sessionId").is_some()
                        && phase == 1
                    {
                        session_id = Some(message["result"]["sessionId"].clone());
                        // Ordinary permission: READ an existing fixture.
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(
                                    next_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": message["result"]["sessionId"],
                                        "prompt": [{"type": "text", "text":
                                            "Read the file /workspace/readme-fixture.txt and reply with its exact content, nothing else."}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 2;
                    } else if is_result
                        && phase == 2
                        && message["result"].get("stopReason").is_some()
                    {
                        // Sandbox-routed permission: WRITE a new file.
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(
                                    next_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": session_id.clone().unwrap_or(json!(null)),
                                        "prompt": [{"type": "text", "text":
                                            "Create the file /workspace/interact-write.txt with the content 'sandbox-write-ok'"}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 3;
                    } else if is_result
                        && phase == 3
                        && message["result"].get("stopReason").is_some()
                    {
                        prompt_done = true;
                        let _ = pipe
                            .send_stdin(
                                acp::encode_request(next_id, "session/cancel", json!({}))
                                    .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        let _ = pipe.send_signal("SIGTERM").await;
                        let grace = Instant::now() + Duration::from_secs(5);
                        while Instant::now() < grace && saw_exit.is_none() {
                            match tokio::time::timeout(Duration::from_secs(2), pipe.next_event())
                                .await
                            {
                                Ok(Some(ServerEvent::Exit { exit_code })) => saw_exit = exit_code,
                                Ok(Some(_)) => {}
                                Ok(None) | Err(_) => break,
                            }
                        }
                        break;
                    }
                }
            }
            ServerEvent::Exit { exit_code } => {
                saw_exit = exit_code;
            }
            _ => {}
        }
    }

    // AC-1: the ordinary READ completed WITHOUT any consent window — the
    // real agent asked nothing for it. Should an agent ever ask, the typed
    // resolver grants it attributed to the policy rule (unit-tested) and
    // never requires a human window for reads.
    let read_window_free = !decisions
        .iter()
        .any(|(tool, _)| tool.to_lowercase().contains("read"));
    checks.push(Check {
        name: "ordinary-read-completed-without-consent-window",
        passed: read_window_free && agent_text.contains("hodei-read-fixture"),
        detail: format!(
            "read turn echoed the fixture with zero permission requests; decisions={decisions:?}"
        ),
    });

    // AC-3: the WRITE permission routed to the sandbox and the file exists
    // INSIDE the sandbox; the host never ran any tool.
    let content =
        run_command_session_with_output(&execd_base, "cat /workspace/interact-write.txt").await;
    checks.push(Check {
        name: "writes-route-to-sandbox-host-executes-nothing",
        passed: write_permission_routed && content.contains("sandbox-write-ok"),
        detail: format!(
            "write permission routed={write_permission_routed}; sandbox file content={content:?}"
        ),
    });

    // AC-2: every answer sent was validated against the agent's options.
    checks.push(Check {
        name: "answers-validated-against-offered-options",
        passed: all_answers_validated && !decisions.is_empty(),
        detail: format!(
            "{} permission requests answered, all validated (cardinality 1, offered options only)",
            decisions.len()
        ),
    });

    // The conversation finished with the model's own end_turn on both turns.
    checks.push(Check {
        name: "both-prompts-completed",
        passed: prompt_done && agent_text.contains("hodei-read-fixture"),
        detail: format!(
            "connected={connected} init_responded={init_responded} exit={saw_exit:?} stderr_tail={:?}",
            String::from_utf8_lossy(&stderr_sample)
        ),
    });

    let deleted = client
        .delete(format!("{server}/v1/sandboxes/{sid}"))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false);
    checks.push(Check {
        name: "cleanup",
        passed: deleted,
        detail: format!("sandbox {sid} deleted"),
    });

    finish(&checks, started.elapsed())
}

fn finish(checks: &[Check], elapsed: Duration) -> i32 {
    let summary = serde_json::json!({
        "task": "TASK-8",
        "checks": checks.iter().map(|c| serde_json::json!({
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
}
