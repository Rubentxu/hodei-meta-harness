//! One-shot command execution over the execd pipe: ATTACH-FIRST is a
//! requirement, not an optimization — the execd drops unheld sessions
//! before their process can even start, so long commands (extraction of
//! large bundles, installs) never run without an immediate holder. Short
//! commands survive the polling-first style, which is why the bug stayed
//! hidden until a slow one-shot needed it.

use std::time::{Duration, Instant};

use super::api::{self, CreateSessionRequest};
use super::frames::ServerEvent;
use super::pipe::{self, AttachOptions};

/// What a one-shot produced. `exit_code: None` means no terminal status was
/// observed (timeout, signal or lost session) — never a success.
#[derive(Debug, Clone, PartialEq)]
pub struct OneshotOutcome {
    pub exit_code: Option<i32>,
    pub output: String,
}

impl OneshotOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }
}

fn ws_url_for(execd_base: &str, session_id: &str) -> String {
    let ws_base = execd_base
        .replacen("http://", "ws://", 1)
        .replacen("https://", "wss://", 1);
    format!("{ws_base}/pty/{session_id}/ws?pty=0")
}

/// Run one command to its terminal state or to the timeout, whichever comes
/// first. The pipe attaches IMMEDIATELY after session creation.
pub async fn run(execd_base: &str, command: &str, timeout: Duration) -> OneshotOutcome {
    let session = match api::create_session(
        execd_base,
        None,
        CreateSessionRequest {
            cwd: None,
            command: Some(command),
        },
    )
    .await
    {
        Ok(session) => session,
        Err(err) => {
            return OneshotOutcome {
                exit_code: None,
                output: format!("[create failed: {err}]"),
            };
        }
    };

    let ws_url = ws_url_for(execd_base, &session.session_id);
    let mut output: Vec<u8> = Vec::new();
    let mut terminal_code: Option<i32> = None;

    let attach_outcome = pipe::attach(&ws_url, AttachOptions::default()).await;
    let attach_error = attach_outcome.as_ref().err().map(|e| e.to_string());
    if let Ok(mut pipe_session) = attach_outcome {
        let started = Instant::now();
        let deadline = started + timeout;
        while Instant::now() < deadline {
            let wait = deadline.saturating_duration_since(Instant::now());
            match tokio::time::timeout(wait, pipe_session.next_event()).await {
                Ok(Some(ServerEvent::Stdout(bytes)))
                | Ok(Some(ServerEvent::Replay { data: bytes, .. })) => output.extend(bytes),
                Ok(Some(ServerEvent::Stderr(bytes))) => output.extend(bytes),
                Ok(Some(ServerEvent::Exit { exit_code })) => {
                    terminal_code = exit_code;
                    break;
                }
                Ok(Some(_)) => {}
                // A silent tick is NOT the deadline: long quiet commands
                // (large extractions, installs) keep running. Only the
                // while condition ends the wait.
                Ok(None) | Err(_) => continue,
            }
        }
        if terminal_code.is_none() {
            // Still running at the deadline: kill and record absence. A
            // negative code means death by signal — no terminal status.
            let _ = pipe_session.send_signal("SIGKILL").await;
            let grace = Instant::now() + Duration::from_secs(2);
            while Instant::now() < grace && terminal_code.is_none() {
                match tokio::time::timeout(Duration::from_millis(800), pipe_session.next_event())
                    .await
                {
                    Ok(Some(ServerEvent::Exit { exit_code })) => terminal_code = exit_code,
                    Ok(Some(_)) => {}
                    Ok(None) | Err(_) => break,
                }
            }
        }
        let _ = pipe_session.close().await;
    }
    let _ = api::delete_session(execd_base, None, &session.session_id).await;

    let mut text = String::from_utf8_lossy(&output).to_string();
    if let Some(err) = attach_error {
        text.push_str(&format!("[attach failed: {err}]"));
    }
    match terminal_code {
        Some(code) if code >= 0 => OneshotOutcome {
            exit_code: Some(code),
            output: text,
        },
        _ => OneshotOutcome {
            exit_code: None,
            output: text,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn succeeded_requires_zero_exit() {
        let ok = OneshotOutcome {
            exit_code: Some(0),
            output: "ok".into(),
        };
        assert!(ok.succeeded());
        let absent = OneshotOutcome {
            exit_code: None,
            output: String::new(),
        };
        assert!(!absent.succeeded());
        let failed = OneshotOutcome {
            exit_code: Some(2),
            output: String::new(),
        };
        assert!(!failed.succeeded());
    }
}
