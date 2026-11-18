//! Socket server adapter: NDJSON request/response over a Unix socket.
//! One supervisor, many clients; the socket IS the control surface.
//!
//! Ownership is ENFORCED, not documented:
//! - A LIVE supervisor owns its socket: a second supervisor NEVER steals
//!   it. `bind` refuses with [`ServerError::AlreadyRunning`].
//! - A STALE socket file (left by a crashed supervisor) is removed and
//!   reused, so a crash never blocks a restart.
//! - The socket file is removed on shutdown ONLY if it is still the file
//!   this process bound (device+inode identity), so a supervisor that
//!   exits never deletes a successor's socket.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

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
    #[error("a supervisor is already listening on {path}: refusing to start a second controller")]
    AlreadyRunning { path: String },
}

pub type SharedCore = Arc<Mutex<SupervisorCore>>;

/// Identity of a bound socket file (device + inode). Used to prove the
/// file at a path is still OURS before removing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

/// Identity of the file at `path`, if it exists.
pub fn identity_of(path: &Path) -> Option<FileIdentity> {
    use std::os::unix::fs::MetadataExt as _;
    let meta = std::fs::metadata(path).ok()?;
    Some(FileIdentity {
        device: meta.dev(),
        inode: meta.ino(),
    })
}

/// Remove the socket file ONLY if it is still the one `ours` describes.
/// A successor supervisor that already replaced the file keeps its socket.
pub fn unlink_if_owned(path: &Path, ours: FileIdentity) -> bool {
    match identity_of(path) {
        Some(current) if current == ours => std::fs::remove_file(path).is_ok(),
        _ => false,
    }
}

/// Bind the control socket.
///
/// A socket file that nobody listens on is stale garbage and is removed.
/// A socket file that IS being listened on belongs to a live supervisor:
/// binding is REFUSED (a second controller would split the state).
pub async fn bind(path: &str) -> Result<UnixListener, ServerError> {
    let socket_path = PathBuf::from(path);
    if socket_path.exists() && supervisor_is_alive(&socket_path).await {
        return Err(ServerError::AlreadyRunning {
            path: path.to_string(),
        });
    }
    // Stale file from a crashed supervisor: remove it and take over.
    let _ = std::fs::remove_file(&socket_path);
    UnixListener::bind(&socket_path).map_err(|source| ServerError::Bind {
        path: path.to_string(),
        source,
    })
}

/// A supervisor is alive when its socket ACCEPTS a connection. A socket
/// file with no listener behind it is stale, not alive.
async fn supervisor_is_alive(path: &Path) -> bool {
    match tokio::time::timeout(Duration::from_millis(500), UnixStream::connect(path)).await {
        Ok(Ok(_stream)) => true,
        Ok(Err(_)) => false,
        Err(_) => false,
    }
}

/// Accept loop: serve every connection until the listener breaks for good.
/// The core is shared; requests are handled sequentially per connection.
pub async fn serve(listener: UnixListener, core: SharedCore) -> std::io::Result<()> {
    let mut consecutive_errors = 0u32;
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                consecutive_errors = 0;
                let core = core.clone();
                tokio::spawn(handle_connection(stream, core));
            }
            Err(e) => {
                // A transient accept failure (fd exhaustion, EINTR) must not
                // kill the control surface; a persistently broken listener
                // does.
                consecutive_errors += 1;
                if consecutive_errors >= 10 {
                    return Err(e);
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }
    }
}

async fn handle_connection(stream: UnixStream, core: SharedCore) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let response = respond_to_line(&core, &line);
        let mut payload = serde_json::to_string(&response).expect("response serializes");
        payload.push('\n');
        writer.write_all(payload.as_bytes()).await?;
    }
    Ok(())
}

/// Turn one NDJSON line into its response. An unparseable line is an
/// ERROR response, never a silent snapshot.
fn respond_to_line(core: &SharedCore, line: &str) -> Response {
    match serde_json::from_str::<Request>(line) {
        Ok(request) => respond(core, request),
        Err(e) => Response::Error {
            message: format!("unparseable request: {e}"),
        },
    }
}

/// Lock the core, recovering from a poisoned mutex instead of becoming a
/// permanent black hole: the core state is still usable, and the poison
/// is recorded as a degraded state (never silently).
fn lock_core(core: &SharedCore) -> MutexGuard<'_, SupervisorCore> {
    match core.lock() {
        Ok(guard) => guard,
        Err(poisoned) => {
            let mut guard: MutexGuard<'_, SupervisorCore> = PoisonError::into_inner(poisoned);
            engine::mark_degraded(
                &mut guard,
                "a request panicked and poisoned the core lock; recovered and continued",
            );
            guard
        }
    }
}

/// Answer one request against the shared core. The mutex lock is scoped
/// tightly: pure decisions happen inside, only serialization outside.
fn respond(core: &SharedCore, request: Request) -> Response {
    let mut guard = lock_core(core);
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
        let response = respond_to_line(&core, "garbage");
        match response {
            Response::Error { message } => assert!(
                message.contains("unparseable request"),
                "real error expected, got {message}"
            ),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_line_is_answered_with_its_response() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let response = respond_to_line(&core, r#"{"req":"snapshot"}"#);
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

    #[test]
    fn a_poisoned_core_is_recovered_and_marked_degraded() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        // Poison it the way a panicking request would.
        let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = core.lock().expect("lock");
            panic!("simulated panic while the lock was held");
        }));
        assert!(poisoned.is_err(), "the simulated panic must have happened");
        let response = respond(&core, Request::Snapshot);
        match response {
            Response::Snapshot(snapshot) => assert!(
                snapshot.degraded.is_some(),
                "a recovered poison must be visible in the snapshot"
            ),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }
}
