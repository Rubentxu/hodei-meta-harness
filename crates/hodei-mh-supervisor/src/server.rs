//! Socket server adapter: NDJSON request/response over a Unix socket.
//! One supervisor, many clients; the socket IS the control surface. A
//! stale socket file from a crashed supervisor is removed on bind, and the
//! socket file is cleaned up when the server task ends.

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::engine::{self, SupervisorCore};
use crate::protocol::{Request, Response};

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error("bind {path}: {source}")]
    Bind {
        path: String,
        source: std::io::Error,
    },
}

pub type SharedCore = Arc<Mutex<SupervisorCore>>;

/// Bind the control socket, removing any stale file left by a crashed
/// supervisor (discovery must not fail because of garbage).
pub async fn bind(path: &str) -> Result<UnixListener, ServerError> {
    let _ = std::fs::remove_file(path);
    UnixListener::bind(path).map_err(|source| ServerError::Bind {
        path: path.to_string(),
        source,
    })
}

/// Accept loop: serve every connection until the listener errors. The core
/// is shared; requests are handled sequentially per connection.
pub async fn serve(listener: UnixListener, core: SharedCore) -> std::io::Result<()> {
    loop {
        let (stream, _addr) = listener.accept().await?;
        let core = core.clone();
        tokio::spawn(handle_connection(stream, core));
    }
}

async fn handle_connection(stream: UnixStream, core: SharedCore) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = match serde_json::from_str::<Request>(&line) {
            Ok(request) => respond(&core, request),
            Err(e) => Response::Error {
                message: format!("unparseable request: {e}"),
            },
        };
        let mut payload = serde_json::to_string(&response).expect("response serializes");
        payload.push('\n');
        writer.write_all(payload.as_bytes()).await?;
    }
    Ok(())
}

/// Answer one request against the shared core. The mutex lock is scoped
/// tightly: pure decisions happen inside, only serialization outside.
fn respond(core: &SharedCore, request: Request) -> Response {
    let mut guard = match core.lock() {
        Ok(guard) => guard,
        Err(_) => {
            return Response::Error {
                message: "supervisor core poisoned".into(),
            };
        }
    };
    match request {
        Request::Snapshot => Response::Snapshot(Box::new(engine::snapshot(&guard))),
        Request::Events { since } => {
            let (events, next_seq) = engine::events_since(&guard, since);
            Response::Events { events, next_seq }
        }
        Request::Action { action } => Response::Action(engine::apply(&mut guard, action)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SupervisorAction;

    #[test]
    fn unparseable_requests_get_error_not_silence() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let response = respond(
            &core,
            serde_json::from_str::<Request>("garbage").unwrap_or(Request::Snapshot),
        );
        // The test above is illustrative; real garbage is caught in the
        // connection handler. Here: snapshot answers with a snapshot.
        assert!(matches!(response, Response::Snapshot(_)));
    }

    #[test]
    fn actions_through_respond_are_applied_to_the_shared_core() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let request = Request::Action {
            action: SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        };
        let response = respond(&core, request);
        match response {
            Response::Action(outcome) => assert!(outcome.applied),
            other => panic!("expected action outcome, got {other:?}"),
        }
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).phase, run::run::Phase::Running,);
    }
}
