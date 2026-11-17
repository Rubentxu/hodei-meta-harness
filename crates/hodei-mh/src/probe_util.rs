//! Shared helpers for probe subcommands.

pub(crate) const MOCK_AGENT_JS: &str = r#"
let buffer = '';
process.stdin.setEncoding('utf8');
console.error('mock-acp-agent: stderr transport marker');
function reply(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', id, result }) + '\n');
}
function notify(method, params) {
  process.stdout.write(JSON.stringify({ jsonrpc: '2.0', method, params }) + '\n');
}
process.stdin.on('data', (chunk) => {
  buffer += chunk;
  let idx;
  while ((idx = buffer.indexOf('\n')) >= 0) {
    const line = buffer.slice(0, idx).trim();
    buffer = buffer.slice(idx + 1);
    if (!line) continue;
    const msg = JSON.parse(line);
    if (msg.method === 'initialize') {
      reply(msg.id, { protocolVersion: 1, agentCapabilities: { loadSession: false } });
    } else if (msg.method === 'session/new') {
      reply(msg.id, { sessionId: 'mock-session-1' });
    } else if (msg.method === 'session/prompt') {
      require('fs').writeFileSync('/workspace/hodei-probe.txt', 'written-by-mock-agent\n');
      notify('session/update', { sessionId: 'mock-session-1', update: {
        sessionUpdate: 'agent_message_chunk',
        content: { type: 'text', text: 'wrote /workspace/hodei-probe.txt' } } });
      reply(msg.id, { stopReason: 'end_turn' });
    } else if (msg.method === 'session/cancel') {
      process.exit(0);
    }
  }
});
"#;

use std::time::{Duration, Instant};

use opensandbox::execd::api::{self, CreateSessionRequest};
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{self, AttachOptions};

/// execd endpoints resolve as `http://host:port`; the pipe WebSocket reuses
/// the same authority under the `ws` scheme.
pub fn ws_url_for(execd_base: &str, session_id: &str) -> String {
    let authority = execd_base
        .strip_prefix("https://")
        .map(|rest| format!("wss://{rest}"))
        .unwrap_or_else(|| {
            execd_base
                .strip_prefix("http://")
                .map(|rest| format!("ws://{rest}"))
                .unwrap_or_else(|| execd_base.to_string())
        });
    format!("{authority}/pty/{session_id}/ws?pty=0")
}

/// Run a one-shot shell command in the sandbox via a dedicated PTY session,
/// returning whether it exited successfully.
pub async fn run_command_session(execd_base: &str, command: &str) -> bool {
    run_command_session_with_output(execd_base, command)
        .await
        .ends_with("EXIT:0")
}

/// Same, returning captured output plus a trailing `EXIT:<code>` marker.
/// One-shot commands may finish before the pipe attaches, so completion is
/// tracked through the status endpoint and output is read via replay.
pub async fn run_command_session_with_output(execd_base: &str, command: &str) -> String {
    let Ok(session) = api::create_session(
        execd_base,
        None,
        CreateSessionRequest {
            cwd: None,
            command: Some(command),
        },
    )
    .await
    else {
        return String::new();
    };
    let deadline = Instant::now() + Duration::from_secs(120);
    let mut running = true;
    while Instant::now() < deadline {
        match api::session_status(execd_base, None, &session.session_id).await {
            Ok(status) if !status.running => {
                running = false;
                break;
            }
            Ok(_) => {}
            Err(_) => break,
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let ws_url = ws_url_for(execd_base, &session.session_id);
    let Ok(mut pipe_session) = pipe::attach(&ws_url, AttachOptions::default()).await else {
        return String::new();
    };
    let mut output = Vec::new();
    let read_deadline = Instant::now() + Duration::from_secs(5);
    let mut quiet = false;
    while !quiet && Instant::now() < read_deadline {
        match tokio::time::timeout(Duration::from_millis(1500), pipe_session.next_event()).await {
            Ok(Some(ServerEvent::Stdout(bytes)))
            | Ok(Some(ServerEvent::Replay { data: bytes, .. })) => {
                output.extend(bytes);
            }
            Ok(Some(ServerEvent::Stderr(bytes))) => output.extend(bytes),
            Ok(Some(ServerEvent::Exit {
                exit_code: Some(code),
            })) => {
                output.extend(format!("\nEXIT:{code}").into_bytes());
                break;
            }
            Ok(Some(_)) => {}
            Ok(None) => quiet = true,
            Err(_) => quiet = true,
        }
    }
    if !output.windows(5).any(|w| w == b"EXIT:") {
        output.extend(format!("\nEXIT:{}", if !running { 0 } else { -1 }).into_bytes());
    }
    let _ = api::delete_session(execd_base, None, &session.session_id).await;
    String::from_utf8_lossy(&output).to_string()
}
