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

use run::runtime::{ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

use crate::engine::{self, SupervisorCore};
use crate::protocol::{Request, Response, SupervisorAction};

/// How long a runtime has to CONFIRM an objective it accepted. A runtime
/// that never answers would leave the run in `Starting` forever; the
/// deadline turns that into an explicit, visible failure.
pub const CONFIRMATION_DEADLINE: Duration = Duration::from_secs(120);

/// The in-flight execution, so a Stop can cancel it.
type Pending = Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>;

/// What one request asks the server to do next.
#[derive(Debug, Clone, PartialEq)]
enum FollowUp {
    Nothing,
    /// Hand the objective to the runtime.
    Execute {
        request: ObjectiveRequest,
        epoch: u64,
    },
    /// The run was stopped: the in-flight execution must be cancelled.
    Cancel,
}

/// One request and its answer.
#[derive(Debug, Clone, PartialEq)]
struct Handled {
    response: Response,
    follow_up: FollowUp,
}

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
///
/// The runtime is an EXPLICIT argument, not a second function: a caller
/// that means to execute objectives and a caller that does not must both
/// say so here, where the difference is visible. Two functions — one with
/// a runtime and one without — let a caller reach for the wrong one and
/// serve a controller that accepts a Start and never executes it.
///
/// With a runtime, an applied Start hands the objective to it, and the
/// runtime's confirmation is what moves the run to `Running`. Without one
/// the controller still serves its surface, but a Start is refused: the
/// engine refuses it, because a run that nobody can execute is not a run.
pub async fn serve(
    listener: UnixListener,
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
) -> std::io::Result<()> {
    let pending: Pending = Arc::new(Mutex::new(None));
    let mut consecutive_errors = 0u32;
    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                consecutive_errors = 0;
                let core = core.clone();
                let runtime = runtime.clone();
                let pending = pending.clone();
                tokio::spawn(handle_connection(stream, core, runtime, pending));
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

async fn handle_connection(
    stream: UnixStream,
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
    pending: Pending,
) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
        if line.trim().is_empty() {
            continue;
        }
        let handled = respond_to_line(&core, &line);
        let mut payload = serde_json::to_string(&handled.response).expect("response serializes");
        payload.push('\n');
        writer.write_all(payload.as_bytes()).await?;
        // The runtime runs OUTSIDE the lock: the core is never held
        // across an await.
        match handled.follow_up {
            FollowUp::Nothing => {}
            FollowUp::Execute { request, epoch } => {
                spawn_execution(
                    core.clone(),
                    runtime.clone(),
                    pending.clone(),
                    request,
                    epoch,
                );
            }
            FollowUp::Cancel => cancel_execution(&pending),
        }
    }
    Ok(())
}

/// Hand the objective to the runtime and apply what it reports.
///
/// The runtime NEVER runs while the core is locked: the lock is taken
/// only to read the epoch and to apply the outcome.
fn spawn_execution(
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
    pending: Pending,
    request: ObjectiveRequest,
    epoch: u64,
) {
    let Some(runtime) = runtime else {
        // The engine refuses a Start when no runtime is wired, so this
        // cannot happen; the guard keeps the failure honest anyway.
        return;
    };
    let handle = tokio::spawn(async move {
        let started = tokio::time::timeout(CONFIRMATION_DEADLINE, runtime.start(request)).await;
        match started {
            Ok(Ok(objective)) => {
                {
                    let mut guard = lock_core(&core);
                    engine::confirm_runtime(&mut guard, epoch);
                }
                // The objective ends on its own time; its verdict is
                // recorded when it does.
                let outcome = objective.completion().await;
                let mut guard = lock_core(&core);
                engine::record_outcome(&mut guard, epoch, outcome);
            }
            Ok(Err(error)) => {
                let mut guard = lock_core(&core);
                engine::record_outcome(
                    &mut guard,
                    epoch,
                    ObjectiveOutcome::Failed {
                        reason: error.to_string(),
                    },
                );
            }
            Err(_elapsed) => {
                let mut guard = lock_core(&core);
                engine::record_outcome(
                    &mut guard,
                    epoch,
                    ObjectiveOutcome::Failed {
                        reason: format!(
                            "the runtime did not confirm the objective within {}s",
                            CONFIRMATION_DEADLINE.as_secs()
                        ),
                    },
                );
            }
        }
    });
    // Keep the handle so a Stop can cancel the in-flight execution.
    let mut slot = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(previous) = slot.replace(handle) {
        previous.abort();
    }
}

/// Cancel the in-flight execution: the user stopped the run.
fn cancel_execution(pending: &Pending) {
    let mut slot = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(handle) = slot.take() {
        handle.abort();
    }
}

/// Turn one NDJSON line into its response. An unparseable line is an
/// ERROR response, never a silent snapshot.
fn respond_to_line(core: &SharedCore, line: &str) -> Handled {
    match serde_json::from_str::<Request>(line) {
        Ok(request) => respond(core, request),
        Err(e) => Handled {
            response: Response::Error {
                message: format!("unparseable request: {e}"),
            },
            follow_up: FollowUp::Nothing,
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
fn respond(core: &SharedCore, request: Request) -> Handled {
    let mut guard = lock_core(core);
    match request {
        Request::Snapshot => Handled {
            response: Response::Snapshot(Box::new(engine::snapshot(&guard))),
            follow_up: FollowUp::Nothing,
        },
        Request::Events { since } => {
            let (events, next_seq) = engine::events_since(&guard, since);
            Handled {
                response: Response::Events { events, next_seq },
                follow_up: FollowUp::Nothing,
            }
        }
        Request::Action { action } => {
            let starts = matches!(&action, SupervisorAction::Start { .. });
            let stops = matches!(&action, SupervisorAction::Stop);
            let outcome = engine::apply(&mut guard, action);
            let follow_up = if starts && outcome.applied {
                // The objective the client named, or none: the runtime
                // selects the next eligible work when it was not named.
                FollowUp::Execute {
                    request: ObjectiveRequest {
                        project: guard.project.clone(),
                        work_ref: guard.ticket.clone(),
                    },
                    epoch: guard.epoch,
                }
            } else if stops && outcome.applied {
                FollowUp::Cancel
            } else {
                FollowUp::Nothing
            };
            Handled {
                response: Response::Action(outcome),
                follow_up,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SupervisorAction;
    use run::runtime::RuntimeIdentity;

    #[test]
    fn unparseable_requests_get_error_not_silence() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let handled = respond_to_line(&core, "garbage");
        match handled.response {
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
        let handled = respond_to_line(&core, r#"{"req":"snapshot"}"#);
        assert!(matches!(handled.response, Response::Snapshot(_)));
    }

    #[test]
    fn a_start_without_a_runtime_is_refused_instead_of_pretended() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let request = Request::Action {
            action: SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        };
        let handled = respond(&core, request);
        match handled.response {
            Response::Action(outcome) => {
                assert!(!outcome.applied, "a run nobody can execute is not a run");
                assert!(
                    outcome.detail.contains("no runtime is wired"),
                    "{}",
                    outcome.detail
                );
            }
            other => panic!("expected action outcome, got {other:?}"),
        }
        assert_eq!(handled.follow_up, FollowUp::Nothing);
    }

    #[test]
    fn an_applied_start_starts_the_run_and_hands_the_objective_to_the_runtime() {
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::rehearsal("trial"),
        )));
        let request = Request::Action {
            action: SupervisorAction::Start {
                project: "gc".into(),
                ticket: Some("TASK-9".into()),
                revision: None,
            },
        };
        let handled = respond(&core, request);
        match &handled.response {
            Response::Action(outcome) => assert!(outcome.applied),
            other => panic!("expected action outcome, got {other:?}"),
        }
        // The run is STARTING: nothing has confirmed it yet.
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).phase, run::run::Phase::Starting);
        drop(guard);
        match handled.follow_up {
            FollowUp::Execute { request, epoch } => {
                assert_eq!(request.work_ref.as_deref(), Some("TASK-9"));
                assert_eq!(epoch, 1);
            }
            other => panic!("the objective must be handed to the runtime, got {other:?}"),
        }
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
        let handled = respond(&core, Request::Snapshot);
        match handled.response {
            Response::Snapshot(snapshot) => assert!(
                snapshot.degraded.is_some(),
                "a recovered poison must be visible in the snapshot"
            ),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }
}
