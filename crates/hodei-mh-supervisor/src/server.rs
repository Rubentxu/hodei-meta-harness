//! Socket server adapter: NDJSON request/response over a Unix socket.
//! One supervisor, many clients; the socket IS the control surface.
//!
//! Ownership is ENFORCED, not documented:
//! - A LIVE supervisor owns its socket: a second supervisor NEVER steals
//!   it. The decision is an EXCLUSIVE LOCK taken by the kernel, not a
//!   probe that races with the binding.
//! - A LIVE supervisor owns its PROJECT identity: a second controller for
//!   the same project is refused even if it uses another socket.
//! - A STALE socket file (left by a crashed supervisor) is removed and
//!   reused, so a crash never blocks a restart. A file at the socket
//!   path that is NOT a socket is never removed: the controller refuses
//!   to start instead of deleting what the operator put there.
//! - The socket file is removed on shutdown ONLY if it is still the file
//!   this process bound (device+inode identity), so a supervisor that
//!   exits never deletes a successor's socket.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use run::run::Phase;
use run::runtime::{ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RenewOutcome, RunId};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::OwnedReadHalf;
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Semaphore;

use crate::engine::{self, SupervisorCore};
use crate::lock::{IdentityLock, LockError};
use crate::paths;
use crate::protocol::{PROTOCOL_VERSION, Request, Response, SupervisorAction};

/// How long a runtime has to CONFIRM an objective it accepted. A runtime
/// that never answers would leave the run in `Starting` forever; the
/// deadline turns that into an explicit, visible failure.
pub const CONFIRMATION_DEADLINE: Duration = Duration::from_secs(120);

/// How long a stopped attempt has to wind down before it is aborted.
///
/// The stop is a cooperative signal first: the attempt returns, closes its
/// session and releases its sandbox. Only an attempt that ignores the
/// signal is aborted, and the durable journal is still the safety net for
/// whatever it left behind.
pub const CANCEL_GRACE: Duration = Duration::from_secs(30);

/// How many bytes one request line may weigh. A client that never ends
/// its line is cut off here instead of filling the memory of the host.
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;

/// How many bytes one answer may weigh. An answer that does not fit is
/// cut down to what fits, and the cursor says where to continue.
pub const MAX_RESPONSE_BYTES: usize = 1024 * 1024;

/// How long the supervisor waits for a client to read its answer. A
/// client that never reads is dropped: it must not hold the others.
pub const WRITE_DEADLINE: Duration = Duration::from_secs(10);

/// How long a connection may stay silent. A client that connects and
/// says nothing does not keep a task alive forever.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How many clients may be connected at the same time.
pub const MAX_CONNECTIONS: usize = 64;

/// How many times the request limit the supervisor is willing to
/// discard before it gives up on a client that does not stop sending.
const DRAIN_FACTOR: usize = 4;

/// How many attempts one run may chain after confirmed objectives.
///
/// A confirmed objective keeps the session alive and the run selects the
/// next work; the bound keeps that chain from becoming an unbounded loop
/// nobody observes. Reaching it ends the run with a visible verdict.
pub const MAX_CHAINED_ATTEMPTS: usize = 32;

/// How long the supervisor waits for the rest of an abusive request.
///
/// The refusal has ALREADY been written; this window only exists so the
/// client can read it. A socket closed while data is still unread in
/// its queue is RESET, and the client would lose the answer that says
/// why it was refused.
const DRAIN_GRACE: Duration = Duration::from_millis(100);

/// The bounds of the control surface.
///
/// They are explicit so a caller (and a test) can see what it is
/// accepting: the product uses [`Limits::default`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// How many bytes one request line may weigh.
    pub max_request_bytes: usize,
    /// How many bytes one answer may weigh.
    pub max_response_bytes: usize,
    /// How long the supervisor waits for a client to read its answer.
    pub write_deadline: Duration,
    /// How long a connection may stay silent.
    pub idle_timeout: Duration,
    /// How many clients may be connected at the same time.
    pub max_connections: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_request_bytes: MAX_REQUEST_BYTES,
            max_response_bytes: MAX_RESPONSE_BYTES,
            write_deadline: WRITE_DEADLINE,
            idle_timeout: IDLE_TIMEOUT,
            max_connections: MAX_CONNECTIONS,
        }
    }
}

/// The in-flight execution and the run it belongs to, so a Stop cancels
/// exactly the execution of the run it names.
type Pending = Arc<Mutex<Option<(RunId, tokio::task::JoinHandle<()>)>>>;

/// What one request asks the server to do next.
#[derive(Debug, Clone, PartialEq)]
enum FollowUp {
    Nothing,
    /// Hand the objective to the runtime under the run's identity.
    Execute {
        request: ObjectiveRequest,
        run: RunId,
    },
    /// The run named by `run` was stopped: its execution must be
    /// cancelled, and no other run's execution may be touched.
    Cancel {
        run: RunId,
    },
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
    #[error(
        "a supervisor is already listening on {path}: refusing to start a second controller{holder}"
    )]
    AlreadyRunning { path: String, holder: String },
    #[error(
        "another controller already serves the project '{project}': refusing to start a second controller{detail}"
    )]
    ProjectBusy { project: String, detail: String },
    #[error("{path} is not a socket: refusing to remove it and to start")]
    SocketPathOccupied { path: String },
    #[error("cannot lock {path}: {detail}")]
    Lock { path: String, detail: String },
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

/// A socket that is BOUND and OWNED: the listener, the identity of the
/// project it serves and the two locks that make that ownership atomic.
///
/// The locks live as long as this value: dropping it releases them, and
/// the kernel releases them when the process dies.
#[derive(Debug)]
pub struct BoundSocket {
    listener: UnixListener,
    socket_path: PathBuf,
    project: String,
    /// The file this controller bound, as it was when it bound it. The
    /// identity is RECORDED, not looked up when the controller stops:
    /// by then the file at the path may be a successor's socket.
    identity: Option<FileIdentity>,
    _project_lock: IdentityLock,
    _socket_lock: IdentityLock,
}

impl Drop for BoundSocket {
    /// Remove the socket file ONLY if it is still the one this
    /// controller bound.
    ///
    /// A clean shutdown leaves no name behind. A crash leaves the file
    /// (nobody runs this code) and the next controller takes it over. A
    /// successor that already replaced the file keeps its own socket: the
    /// identity says whose file it is.
    fn drop(&mut self) {
        let Some(ours) = self.identity else {
            return;
        };
        let _ = unlink_if_owned(&self.socket_path, ours);
    }
}

impl BoundSocket {
    /// The path this controller serves.
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// The project this controller serves.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// The listener, for the rare caller that needs it.
    pub fn listener(&self) -> &UnixListener {
        &self.listener
    }
}

/// Bind the control socket for `project`.
///
/// OWNERSHIP is decided by EXCLUSIVE LOCKS, not by the presence of a
/// name:
///
/// - the PROJECT lock makes "one controller per project" an atomic fact;
/// - the SOCKET lock makes "one owner of this path" an atomic fact.
///
/// A socket file that nobody listens on is stale garbage and is reused.
/// A file at the path that is NOT a socket is refused and NEVER removed:
/// deleting what the operator put there is not this controller's
/// decision.
pub async fn bind(socket: &str, project: &str) -> Result<BoundSocket, ServerError> {
    let socket_path = PathBuf::from(socket);
    // The private runtime directory exists BEFORE any lock is taken: a
    // lock file in a directory that somebody else could write to would
    // not be an ownership at all.
    paths::ensure_runtime_dirs().map_err(|source| ServerError::Lock {
        path: paths::runtime_root().display().to_string(),
        detail: source.to_string(),
    })?;
    // The locks live in the PRIVATE runtime directory of the user, never
    // next to the socket: the socket may be in a shared directory.
    let project_lock =
        IdentityLock::acquire(&paths::project_lock(project), project).map_err(|error| {
            ServerError::ProjectBusy {
                project: project.to_string(),
                detail: holder_detail(&error),
            }
        })?;
    let socket_lock =
        IdentityLock::acquire(&paths::socket_lock(&socket_path), project).map_err(|error| {
            match error {
                LockError::Held { holder, .. } => ServerError::AlreadyRunning {
                    path: socket.to_string(),
                    holder,
                },
                other => ServerError::Lock {
                    path: socket.to_string(),
                    detail: other.to_string(),
                },
            }
        })?;
    // Only now that the ownership is ours may the path be touched.
    if socket_path.exists() {
        if !is_socket(&socket_path) {
            return Err(ServerError::SocketPathOccupied {
                path: socket.to_string(),
            });
        }
        if supervisor_is_alive(&socket_path).await {
            return Err(ServerError::AlreadyRunning {
                path: socket.to_string(),
                holder: String::new(),
            });
        }
        // Stale file from a crashed supervisor: remove it and take over.
        // Only a SOCKET is ever removed here.
        let _ = std::fs::remove_file(&socket_path);
    }
    let listener = UnixListener::bind(&socket_path).map_err(|source| ServerError::Bind {
        path: socket.to_string(),
        source,
    })?;
    // The socket is readable and writable by its owner only: the mode of
    // the file is the first layer of the trust boundary.
    let _ = std::fs::set_permissions(
        &socket_path,
        std::os::unix::fs::PermissionsExt::from_mode(0o600),
    );
    let identity = identity_of(&socket_path);
    Ok(BoundSocket {
        listener,
        socket_path,
        project: project.to_string(),
        identity,
        _project_lock: project_lock,
        _socket_lock: socket_lock,
    })
}

/// Whether the file at `path` is a socket.
fn is_socket(path: &Path) -> bool {
    use std::os::unix::fs::FileTypeExt as _;
    std::fs::metadata(path)
        .map(|meta| meta.file_type().is_socket())
        .unwrap_or(false)
}

/// What the lock file says about its holder, for a refusal that names it.
fn holder_detail(error: &LockError) -> String {
    match error {
        LockError::Held { holder, .. } => holder.clone(),
        other => other.to_string(),
    }
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
    bound: BoundSocket,
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
    limits: Limits,
) -> std::io::Result<()> {
    // The runtime the controller SERVES is the runtime its snapshot
    // NAMES: one writer, one fact. A core that claimed one runtime while
    // the server served another would accept a Start and never run it.
    // The PROJECT is the same kind of fact: the identity the controller
    // took the lock for is the project it serves.
    {
        let mut guard = lock_core(&core);
        guard.project = bound.project().to_string();
        guard.runtime = runtime.as_ref().map(|runtime| runtime.identity());
    }
    let pending: Pending = Arc::new(Mutex::new(None));
    let connections = Arc::new(Semaphore::new(limits.max_connections));
    let mut consecutive_errors = 0u32;
    loop {
        match bound.listener.accept().await {
            Ok((stream, _addr)) => {
                consecutive_errors = 0;
                let core = core.clone();
                let runtime = runtime.clone();
                let pending = pending.clone();
                let connections = connections.clone();
                tokio::spawn(async move {
                    // The number of clients is bounded: a client that
                    // opens thousands of connections does not make the
                    // supervisor serve thousands of tasks.
                    let permit = match connections.clone().try_acquire_owned() {
                        Ok(permit) => permit,
                        Err(_) => {
                            let _ = refuse(stream, "too many clients are connected").await;
                            return;
                        }
                    };
                    let _permit = permit;
                    let _ = handle_connection(stream, core, runtime, pending, limits).await;
                });
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

/// Tell a client that it may not stay, and close the connection.
async fn refuse(stream: UnixStream, message: &str) -> std::io::Result<()> {
    let response = Response::Error {
        protocol_version: PROTOCOL_VERSION,
        message: message.to_string(),
    };
    let mut payload = serde_json::to_string(&response).unwrap_or_default();
    payload.push('\n');
    let (_reader, mut writer) = stream.into_split();
    writer.write_all(payload.as_bytes()).await
}

/// What one read of a connection produced.
enum LineOutcome {
    /// A complete line.
    Line(String),
    /// The client closed the connection.
    Closed,
    /// The line does not fit in the limit.
    TooLong,
}

/// Read ONE line, refusing a line that does not fit in `limit`.
///
/// The buffer never grows past the limit: a client that sends an endless
/// line is cut off instead of filling the memory of the host.
async fn read_bounded_line(
    reader: &mut BufReader<OwnedReadHalf>,
    limit: usize,
) -> std::io::Result<LineOutcome> {
    let mut line: Vec<u8> = Vec::new();
    loop {
        let (chunk, consumed, eof) = {
            let available = reader.fill_buf().await?;
            if available.is_empty() {
                (Vec::new(), 0usize, true)
            } else {
                match available.iter().position(|byte| *byte == b'\n') {
                    Some(index) => (available[..index].to_vec(), index + 1, false),
                    None => (available.to_vec(), available.len(), false),
                }
            }
        };
        if eof {
            if line.is_empty() {
                return Ok(LineOutcome::Closed);
            }
            return Ok(LineOutcome::Line(
                String::from_utf8_lossy(&line).into_owned(),
            ));
        }
        if line.len() + chunk.len() > limit {
            return Ok(LineOutcome::TooLong);
        }
        let complete = consumed > chunk.len();
        line.extend_from_slice(&chunk);
        reader.consume(consumed);
        if complete {
            return Ok(LineOutcome::Line(
                String::from_utf8_lossy(&line).into_owned(),
            ));
        }
    }
}

async fn handle_connection(
    stream: UnixStream,
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
    pending: Pending,
    limits: Limits,
) -> std::io::Result<()> {
    // The peer is authenticated BEFORE the stream is split: the
    // credentials of a peer only live in the socket itself.
    if !peer_is_ours(&stream) {
        return Ok(());
    }
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    loop {
        let line = match tokio::time::timeout(
            limits.idle_timeout,
            read_bounded_line(&mut reader, limits.max_request_bytes),
        )
        .await
        {
            Ok(Ok(LineOutcome::Line(line))) => line,
            // The client said nothing for too long: the task ends.
            Ok(Ok(LineOutcome::Closed)) | Err(_) => break,
            Ok(Ok(LineOutcome::TooLong)) => {
                let response = Response::Error {
                    protocol_version: PROTOCOL_VERSION,
                    message: format!(
                        "the request exceeds the limit of {} bytes: refusing it",
                        limits.max_request_bytes
                    ),
                };
                write_answer(&mut writer, &response, limits).await?;
                // The refusal is written; what the client keeps sending is
                // discarded so the connection can be closed CLEANLY. A
                // close with data still unread in the queue resets the
                // socket, and the client would lose the answer that
                // says why it was refused.
                drain_input(
                    &mut reader,
                    limits.max_request_bytes.saturating_mul(DRAIN_FACTOR),
                    DRAIN_GRACE,
                )
                .await;
                break;
            }
            Ok(Err(error)) => return Err(error),
        };
        if line.trim().is_empty() {
            continue;
        }
        serve_line(
            &core,
            runtime.as_ref(),
            &pending,
            &line,
            limits,
            &mut writer,
        )
        .await?;
    }
    Ok(())
}

/// Serve one request line: answer it, then apply what it asked for.
///
/// The follow-up runs even when the answer could not be written: a Stop
/// that only reached a closed socket would leave its attempt running, and
/// a Start would wedge in `starting` with nobody executing it. The first
/// write failure, if any, is returned AFTER the follow-up has been
/// applied.
async fn serve_line<W>(
    core: &SharedCore,
    runtime: Option<&Arc<dyn ObjectiveRuntime>>,
    pending: &Pending,
    line: &str,
    limits: Limits,
    writer: &mut W,
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let handled = respond_line(core, runtime, line, limits).await;
    let written = write_answer(writer, &handled.response, limits).await;
    // The runtime runs OUTSIDE the lock: the core is never held across an
    // await.
    match handled.follow_up {
        FollowUp::Nothing => {}
        FollowUp::Execute { request, run } => {
            spawn_execution(
                core.clone(),
                runtime.cloned(),
                pending.clone(),
                request,
                run,
            );
        }
        FollowUp::Cancel { run } => {
            cancel_execution(runtime, pending, run, CANCEL_GRACE).await;
            // The attempt has ended, whichever way: a new one may run.
            lock_core(core).winding_down = false;
        }
    }
    written
}

/// Read and DISCARD what an abusive client keeps sending.
///
/// The answer has already been written. This only empties the pipe so
/// the connection can be closed without resetting the socket: a reset
/// takes the answer away from the client that is waiting for it.
///
/// The wait is bounded twice: by the number of bytes that are thrown
/// away and by the silence of the client. A client that never stops
/// sending is cut down; a client that stops is let go at once.
async fn drain_input(reader: &mut BufReader<OwnedReadHalf>, limit: usize, grace: Duration) {
    let mut discarded = 0usize;
    let mut buffer = vec![0u8; 8192];
    loop {
        match tokio::time::timeout(grace, reader.read(&mut buffer)).await {
            // The client closed the connection: there is nothing left.
            Ok(Ok(0)) => return,
            Ok(Ok(count)) => {
                discarded += count;
                if discarded >= limit {
                    return;
                }
            }
            Ok(Err(_)) => return,
            // The client stopped sending.
            Err(_) => return,
        }
    }
}

/// Write one answer, within the deadline and within the size limit.
async fn write_answer<W>(writer: &mut W, response: &Response, limits: Limits) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let mut payload = serde_json::to_string(response).unwrap_or_default();
    payload.push('\n');
    // A client that never reads must not hold the connection forever.
    match tokio::time::timeout(limits.write_deadline, writer.write_all(payload.as_bytes())).await {
        Ok(result) => result,
        Err(_) => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "the client did not read the answer",
        )),
    }
}

/// Whether the peer of `stream` is the user this controller runs as.
///
/// On Linux the kernel reports the credentials of the peer; a peer that
/// is somebody else is not served. On platforms without peer credentials
/// the permissions of the socket file are the only layer.
#[cfg(target_os = "linux")]
fn peer_is_ours(stream: &UnixStream) -> bool {
    match stream.peer_cred() {
        Ok(credentials) => credentials.uid() == paths::current_uid(),
        // The credentials could not be read: the peer is not served.
        Err(_) => false,
    }
}

#[cfg(not(target_os = "linux"))]
fn peer_is_ours(_stream: &UnixStream) -> bool {
    true
}

/// Hand the objective to the runtime and apply what it reports.
///
/// The runtime NEVER runs while the core is locked: the lock is taken
/// only to read the epoch and to apply the outcome.
///
/// A confirmed objective does not end the session: the core keeps the run
/// live, so the same run asks the runtime for its next selection
/// (`work_ref: None`) instead of waiting for a Start the engine would
/// refuse while the run is in flight. The chain is bounded; an idle
/// outcome, a rejected or failed one and a stop all end it.
fn spawn_execution(
    core: SharedCore,
    runtime: Option<Arc<dyn ObjectiveRuntime>>,
    pending: Pending,
    request: ObjectiveRequest,
    run: RunId,
) {
    let Some(runtime) = runtime else {
        // The engine refuses a Start when no runtime is wired, so this
        // cannot happen; the guard keeps the failure honest anyway.
        return;
    };
    let project = request.project.clone();
    let handle = tokio::spawn(async move {
        let mut attempt = request;
        for _ in 0..MAX_CHAINED_ATTEMPTS {
            if !run_attempt(&core, &runtime, run, attempt).await {
                return;
            }
            // The objective held: the next selection belongs to the SAME
            // run, and it is never a named one.
            attempt = ObjectiveRequest {
                project: project.clone(),
                work_ref: None,
            };
        }
        // The bound was reached with the run still live: it is ended with
        // a visible verdict instead of leaving a run nobody executes.
        let mut guard = lock_core(&core);
        engine::record_outcome(
            &mut guard,
            run.number(),
            ObjectiveOutcome::Failed {
                reason: format!(
                    "the objective kept holding for {MAX_CHAINED_ATTEMPTS} chained attempts: the supervisor stopped selecting work for it"
                ),
            },
        );
    });
    // Keep the handle so a Stop can cancel the in-flight execution.
    let mut slot = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // A Start that arrives this late belongs to a run that was already
    // superseded: it must not abort the execution of the newer one.
    if slot.as_ref().is_some_and(|(current, _)| *current > run) {
        drop(slot);
        handle.abort();
        return;
    }
    if let Some((_, previous)) = slot.replace((run, handle)) {
        previous.abort();
    }
}

/// Run ONE attempt of `run` and record what the runtime reports.
///
/// Returns whether the run may chain another attempt: the outcome was a
/// confirmed objective and the core still holds this same run, live and
/// unpaused, with no stop raised against it. The check happens under the
/// same lock that records the outcome, so a stop that lands first always
/// wins.
async fn run_attempt(
    core: &SharedCore,
    runtime: &Arc<dyn ObjectiveRuntime>,
    run: RunId,
    request: ObjectiveRequest,
) -> bool {
    let started = tokio::time::timeout(CONFIRMATION_DEADLINE, runtime.start(run, request)).await;
    let outcome = match started {
        Ok(Ok(objective)) => {
            {
                let mut guard = lock_core(core);
                // The first attempt moves the run from `starting` to
                // `running`. A chained attempt runs inside a run that is
                // already running: its own confirmation is its outcome.
                if guard
                    .run
                    .as_ref()
                    .is_some_and(|state| *state.phase() == Phase::Starting)
                {
                    engine::confirm_runtime(&mut guard, run.number(), &runtime.identity());
                }
            }
            // The objective ends on its own time; its verdict is
            // recorded when it does.
            objective.completion().await
        }
        Ok(Err(error)) => ObjectiveOutcome::Failed {
            reason: error.to_string(),
        },
        Err(_elapsed) => ObjectiveOutcome::Failed {
            reason: format!(
                "the runtime did not confirm the objective within {}s",
                CONFIRMATION_DEADLINE.as_secs()
            ),
        },
    };
    let mut guard = lock_core(core);
    engine::record_outcome(&mut guard, run.number(), outcome.clone());
    matches!(outcome, ObjectiveOutcome::Confirmed { .. })
        && guard.epoch == run.number()
        && !guard.paused
        && guard.stop_reason.is_none()
        && guard
            .run
            .as_ref()
            .is_some_and(|state| state.is_live() && *state.phase() == Phase::Running)
}

/// Stop the execution of `run`: the user stopped the run.
///
/// The stop is a SIGNAL first, through the runtime, and only then a
/// deadline. The signal goes out even when no execution handle exists: the
/// Start's follow-up may still be waiting to spawn, and the runtime keeps
/// the stop so that attempt begins already cancelled. Aborting straight
/// away drops the attempt mid-flight, so the agent never reaches its
/// `session.close()` and the sandbox outlives the stop; a cooperative
/// wind-down lets the attempt release what it owns. An attempt that
/// ignores the signal is aborted after `grace`, and the journal still
/// names the sandbox to reconcile.
async fn cancel_execution(
    runtime: Option<&Arc<dyn ObjectiveRuntime>>,
    pending: &Pending,
    run: RunId,
    grace: Duration,
) {
    if let Some(runtime) = runtime {
        // The stop names its run: a stop raised for a run that already
        // ended never reaches the attempt of the one that followed it.
        runtime.cancel(run);
    }
    // Only the execution of the run this stop names is taken: a stop never
    // cancels the execution of another run.
    let taken = {
        let mut slot = pending
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match slot.as_ref() {
            Some((held_run, _)) if *held_run == run => slot.take(),
            _ => None,
        }
    };
    let Some((_, mut handle)) = taken else {
        return;
    };
    if tokio::time::timeout(grace, &mut handle).await.is_err() {
        // The last resort: the attempt did not wind down in time.
        handle.abort();
        let _ = handle.await;
    }
}

/// Turn one NDJSON line into its response.
///
/// A renewal is the ONE action whose answer the runtime owns: it is asked
/// OUTSIDE the lock (the lock is never held across an await), and its
/// answer is what the engine records. Everything else is a pure decision
/// over the core.
async fn respond_line(
    core: &SharedCore,
    runtime: Option<&Arc<dyn ObjectiveRuntime>>,
    line: &str,
    limits: Limits,
) -> Handled {
    match serde_json::from_str::<Request>(line) {
        Ok(Request::Action {
            action: SupervisorAction::Renew { added_seconds },
        }) => {
            let renewal = match runtime {
                Some(runtime) => runtime.renew(added_seconds).await,
                None => RenewOutcome::Unsupported {
                    reason: "no runtime is wired: there is no environment to renew".into(),
                },
            };
            let mut guard = lock_core(core);
            Handled {
                response: Response::Action {
                    protocol_version: PROTOCOL_VERSION,
                    outcome: engine::apply_renewal(&mut guard, added_seconds, renewal),
                },
                follow_up: FollowUp::Nothing,
            }
        }
        _ => respond_to_line(core, line, limits),
    }
}

/// Turn one NDJSON line into its response. An unparseable line is an
/// ERROR response, never a silent snapshot.
fn respond_to_line(core: &SharedCore, line: &str, limits: Limits) -> Handled {
    match serde_json::from_str::<Request>(line) {
        Ok(request) => respond(core, request, limits),
        Err(e) => Handled {
            response: Response::Error {
                protocol_version: PROTOCOL_VERSION,
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
fn respond(core: &SharedCore, request: Request, limits: Limits) -> Handled {
    let mut guard = lock_core(core);
    match request {
        Request::Snapshot => Handled {
            response: Response::Snapshot(Box::new(engine::snapshot(&guard))),
            follow_up: FollowUp::Nothing,
        },
        Request::Events { since, limit } => Handled {
            response: events_response(&guard, since, limit, limits),
            follow_up: FollowUp::Nothing,
        },
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
                    // The epoch IS the identity of the run: the stop that
                    // arrives later names the same run.
                    run: RunId(guard.epoch),
                }
            } else if stops && outcome.applied {
                FollowUp::Cancel {
                    run: RunId(guard.epoch),
                }
            } else {
                FollowUp::Nothing
            };
            Handled {
                response: Response::Action {
                    protocol_version: PROTOCOL_VERSION,
                    outcome,
                },
                follow_up,
            }
        }
    }
}

/// The page of events, cut down to what one answer may carry.
///
/// The page keeps its CURSOR: when events are dropped to fit, the cursor
/// points at the first one that did NOT fit, so the client continues
/// where the answer stopped instead of losing the middle of the log.
fn events_response(
    core: &SupervisorCore,
    since: u64,
    limit: Option<u32>,
    limits: Limits,
) -> Response {
    let mut page = engine::events_since(core, since, limit);
    loop {
        let response = Response::Events {
            protocol_version: PROTOCOL_VERSION,
            events: page.events.clone(),
            next_seq: page.next_seq,
            oldest_seq: page.oldest_seq,
            history_lost: page.history_lost,
        };
        let encoded = serde_json::to_string(&response).unwrap_or_default();
        if encoded.len() <= limits.max_response_bytes {
            return response;
        }
        if page.events.is_empty() {
            // Even an empty page does not fit: the answer is an explicit
            // refusal, never a silent truncation.
            return Response::Error {
                protocol_version: PROTOCOL_VERSION,
                message: format!(
                    "the answer does not fit in the limit of {} bytes",
                    limits.max_response_bytes
                ),
            };
        }
        // Cut the page in half and try again: the cursor moves with it.
        let keep = page.events.len() / 2;
        page.events.truncate(keep);
        page.next_seq = page
            .events
            .last()
            .map(|event| event.seq + 1)
            .unwrap_or(page.next_seq);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::SupervisorAction;
    use run::runtime::{ObjectiveHandle, RenewOutcome, RuntimeError, RuntimeIdentity};
    use std::pin::Pin;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::task::{Context, Poll};

    /// A runtime whose renewal answer is scripted: the server must pass it
    /// through, not invent one.
    struct RenewingRuntime {
        outcome: RenewOutcome,
    }

    #[async_trait::async_trait]
    impl ObjectiveRuntime for RenewingRuntime {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity::live("renewing")
        }

        async fn start(
            &self,
            _run: RunId,
            _request: ObjectiveRequest,
        ) -> Result<ObjectiveHandle, RuntimeError> {
            Ok(ObjectiveHandle::finished(ObjectiveOutcome::Idle {
                reason: "nothing to do".into(),
            }))
        }

        async fn renew(&self, _added_seconds: u64) -> RenewOutcome {
            self.outcome.clone()
        }
    }

    fn renewal_line() -> &'static str {
        r#"{"req":"action","action":{"action":"renew","added_seconds":300}}"#
    }

    #[tokio::test]
    async fn a_renewal_applies_the_confirmation_the_runtime_answered() {
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::live("renewing"),
        )));
        let runtime: Arc<dyn ObjectiveRuntime> = Arc::new(RenewingRuntime {
            outcome: RenewOutcome::Confirmed { ttl_seconds: 420 },
        });
        let handled = respond_line(&core, Some(&runtime), renewal_line(), Limits::default()).await;
        match handled.response {
            Response::Action { outcome, .. } => {
                assert!(outcome.applied, "{}", outcome.detail);
                assert!(outcome.detail.contains("420"), "{}", outcome.detail);
            }
            other => panic!("expected action outcome, got {other:?}"),
        }
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).ttl_remaining_seconds, Some(420));
    }

    #[tokio::test]
    async fn an_unsupported_renewal_is_not_applied_and_says_why() {
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::live("renewing"),
        )));
        let runtime: Arc<dyn ObjectiveRuntime> = Arc::new(RenewingRuntime {
            outcome: RenewOutcome::Unsupported {
                reason: "the lifecycle backend has no ttl renewal".into(),
            },
        });
        let handled = respond_line(&core, Some(&runtime), renewal_line(), Limits::default()).await;
        match handled.response {
            Response::Action { outcome, .. } => {
                assert!(!outcome.applied, "never a simulated success");
                assert!(
                    outcome.detail.contains("no ttl renewal"),
                    "the reason the runtime gave is reported: {}",
                    outcome.detail
                );
            }
            other => panic!("expected action outcome, got {other:?}"),
        }
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).ttl_remaining_seconds, None);
    }

    #[tokio::test]
    async fn a_renewal_without_a_runtime_is_refused_instead_of_invented() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let handled = respond_line(&core, None, renewal_line(), Limits::default()).await;
        match handled.response {
            Response::Action { outcome, .. } => {
                assert!(!outcome.applied);
                assert!(
                    outcome.detail.contains("no runtime is wired"),
                    "{}",
                    outcome.detail
                );
            }
            other => panic!("expected action outcome, got {other:?}"),
        }
    }

    /// A writer whose peer is gone: every write fails.
    struct ClosedWriter;

    impl tokio::io::AsyncWrite for ClosedWriter {
        fn poll_write(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
            _buffer: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "the client closed the socket",
            )))
        }

        fn poll_flush(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(
            self: Pin<&mut Self>,
            _context: &mut Context<'_>,
        ) -> Poll<std::io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn the_follow_up_of_a_line_runs_even_when_the_answer_cannot_be_written() {
        // A Start whose answer is lost must still reach the runtime: the
        // run would wedge in `starting` otherwise.
        let recorded = Arc::new(RecordingRuntime::default());
        let runtime: Arc<dyn ObjectiveRuntime> = recorded.clone();
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::live("recording"),
        )));
        let pending: Pending = Arc::new(Mutex::new(None));
        let line =
            r#"{"req":"action","action":{"action":"start","project":"gc","ticket":"TASK-9"}}"#;

        let written = serve_line(
            &core,
            Some(&runtime),
            &pending,
            line,
            Limits::default(),
            &mut ClosedWriter,
        )
        .await;

        assert!(
            written.is_err(),
            "the write failure is returned to the connection"
        );
        let (held_run, handle) = pending
            .lock()
            .expect("lock")
            .take()
            .expect("the start spawned its execution");
        assert_eq!(held_run, RunId(1));
        handle.await.expect("the execution ends");
        assert_eq!(
            recorded.started(),
            vec![RunId(1)],
            "the objective reached the runtime although the answer was lost"
        );
        assert!(
            recorded.cancelled().is_empty(),
            "nothing was stopped: the follow-up of the start is not a stop"
        );
    }

    #[test]
    fn unparseable_requests_get_error_not_silence() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let handled = respond_to_line(&core, "garbage", Limits::default());
        match handled.response {
            Response::Error { message, .. } => assert!(
                message.contains("unparseable request"),
                "real error expected, got {message}"
            ),
            other => panic!("expected an error response, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_line_is_answered_with_its_response() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        let handled = respond_to_line(&core, r#"{"req":"snapshot"}"#, Limits::default());
        assert!(matches!(handled.response, Response::Snapshot(_)));
    }

    #[test]
    fn every_answer_states_the_version_of_the_contract() {
        let core: SharedCore = Arc::new(Mutex::new(engine::default_core("gc")));
        for line in [
            r#"{"req":"snapshot"}"#,
            r#"{"req":"events","since":0}"#,
            r#"{"req":"action","action":{"action":"pause"}}"#,
        ] {
            let handled = respond_to_line(&core, line, Limits::default());
            assert_eq!(
                handled.response.protocol_version(),
                PROTOCOL_VERSION,
                "the answer of {line} states its version"
            );
        }
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
        let handled = respond(&core, request, Limits::default());
        match handled.response {
            Response::Action { outcome, .. } => {
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
        let handled = respond(&core, request, Limits::default());
        match &handled.response {
            Response::Action { outcome, .. } => assert!(outcome.applied),
            other => panic!("expected action outcome, got {other:?}"),
        }
        // The run is STARTING: nothing has confirmed it yet.
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).phase, run::run::Phase::Starting);
        drop(guard);
        match handled.follow_up {
            FollowUp::Execute { request, run } => {
                assert_eq!(request.work_ref.as_deref(), Some("TASK-9"));
                assert_eq!(run, RunId(1));
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
        let handled = respond(&core, Request::Snapshot, Limits::default());
        match handled.response {
            Response::Snapshot(snapshot) => assert!(
                snapshot.degraded.is_some(),
                "a recovered poison must be visible in the snapshot"
            ),
            other => panic!("expected a snapshot, got {other:?}"),
        }
    }

    /// A runtime that delivers its stop to the attempt it owns, the way the
    /// live runtime does: the attempt winds down by itself.
    struct SignallingRuntime {
        cancelled: Arc<AtomicBool>,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait::async_trait]
    impl ObjectiveRuntime for SignallingRuntime {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity::live("signalling")
        }

        async fn start(
            &self,
            _run: RunId,
            _request: ObjectiveRequest,
        ) -> Result<ObjectiveHandle, RuntimeError> {
            Ok(ObjectiveHandle::finished(ObjectiveOutcome::Idle {
                reason: "nothing to do".into(),
            }))
        }

        fn cancel(&self, _run: RunId) {
            self.cancelled.store(true, Ordering::SeqCst);
            self.gate.notify_one();
        }
    }

    fn signalling(
        cancelled: &Arc<AtomicBool>,
        gate: &Arc<tokio::sync::Notify>,
    ) -> Arc<dyn ObjectiveRuntime> {
        Arc::new(SignallingRuntime {
            cancelled: cancelled.clone(),
            gate: gate.clone(),
        })
    }

    #[tokio::test(start_paused = true)]
    async fn a_stop_signals_the_runtime_and_lets_the_attempt_wind_down() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let gate = Arc::new(tokio::sync::Notify::new());
        let runtime = signalling(&cancelled, &gate);
        // The attempt: it ends when the stop reaches it, the way an ACP
        // session closes its own conversation and sandbox.
        let wound_down = Arc::new(AtomicBool::new(false));
        let flag = wound_down.clone();
        let waiter = gate.clone();
        let attempt = tokio::spawn(async move {
            waiter.notified().await;
            flag.store(true, Ordering::SeqCst);
        });
        let pending: Pending = Arc::new(Mutex::new(Some((RunId(1), attempt))));

        let started = tokio::time::Instant::now();
        cancel_execution(Some(&runtime), &pending, RunId(1), CANCEL_GRACE).await;

        assert!(
            cancelled.load(Ordering::SeqCst),
            "the runtime was told to stop"
        );
        assert!(
            wound_down.load(Ordering::SeqCst),
            "the attempt wound down on its own"
        );
        assert_eq!(
            tokio::time::Instant::now() - started,
            Duration::ZERO,
            "a cooperative attempt is never aborted"
        );
        assert!(
            pending.lock().expect("lock").is_none(),
            "the handle was consumed"
        );
    }

    /// Marks that the attempt's future was DROPPED: an abort is the only
    /// thing that drops an attempt that never ends on its own.
    struct DropSignal(Arc<AtomicBool>);

    impl Drop for DropSignal {
        fn drop(&mut self) {
            self.0.store(true, Ordering::SeqCst);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn an_attempt_that_ignores_the_stop_is_aborted_only_after_the_grace() {
        let cancelled = Arc::new(AtomicBool::new(false));
        let runtime = signalling(&cancelled, &Arc::new(tokio::sync::Notify::new()));
        let dropped = Arc::new(AtomicBool::new(false));
        let signal = DropSignal(dropped.clone());
        let attempt = tokio::spawn(async move {
            let _signal = signal;
            std::future::pending::<()>().await;
        });
        let pending: Pending = Arc::new(Mutex::new(Some((RunId(1), attempt))));

        let started = tokio::time::Instant::now();
        cancel_execution(Some(&runtime), &pending, RunId(1), CANCEL_GRACE).await;

        assert!(
            cancelled.load(Ordering::SeqCst),
            "the signal was sent before the abort"
        );
        assert!(
            dropped.load(Ordering::SeqCst),
            "an attempt that ignores the stop is aborted"
        );
        assert_eq!(
            tokio::time::Instant::now() - started,
            CANCEL_GRACE,
            "the abort waited for the whole grace"
        );
        assert!(pending.lock().expect("lock").is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn a_delayed_start_of_an_older_run_does_not_abort_the_newer_execution() {
        // A Start whose follow-up was delayed until after a newer run took
        // the slot must not abort the execution of that newer run.
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::rehearsal("trial"),
        )));
        let runtime = signalling(
            &Arc::new(AtomicBool::new(false)),
            &Arc::new(tokio::sync::Notify::new()),
        );
        let newer_alive = Arc::new(AtomicBool::new(true));
        let signal = DropSignal(newer_alive.clone());
        let newer = tokio::spawn(async move {
            let _signal = signal;
            std::future::pending::<()>().await;
        });
        let pending: Pending = Arc::new(Mutex::new(Some((RunId(2), newer))));

        spawn_execution(
            core,
            Some(runtime),
            pending.clone(),
            ObjectiveRequest {
                project: "gc".into(),
                work_ref: None,
            },
            RunId(1),
        );
        // The stale execution is aborted before it is polled.
        tokio::task::yield_now().await;

        assert!(
            newer_alive.load(Ordering::SeqCst),
            "the newer run keeps its execution"
        );
        assert_eq!(
            pending.lock().expect("lock").as_ref().map(|(run, _)| *run),
            Some(RunId(2)),
            "the slot still belongs to the newer run"
        );
    }

    /// A runtime that records the run each answer belongs to, so a test can
    /// see WHICH run reached it and in which order.
    #[derive(Default)]
    struct RecordingRuntime {
        started: Mutex<Vec<RunId>>,
        cancelled: Mutex<Vec<RunId>>,
    }

    impl RecordingRuntime {
        fn started(&self) -> Vec<RunId> {
            self.started
                .lock()
                .map(|started| started.clone())
                .unwrap_or_default()
        }

        fn cancelled(&self) -> Vec<RunId> {
            self.cancelled
                .lock()
                .map(|cancelled| cancelled.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl ObjectiveRuntime for RecordingRuntime {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity::live("recording")
        }

        async fn start(
            &self,
            run: RunId,
            _request: ObjectiveRequest,
        ) -> Result<ObjectiveHandle, RuntimeError> {
            if let Ok(mut started) = self.started.lock() {
                started.push(run);
            }
            Ok(ObjectiveHandle::finished(ObjectiveOutcome::Failed {
                reason: "the run was stopped before it was published".into(),
            }))
        }

        fn cancel(&self, run: RunId) {
            if let Ok(mut cancelled) = self.cancelled.lock() {
                cancelled.push(run);
            }
        }
    }

    #[tokio::test]
    async fn a_stop_raised_before_the_execution_is_spawned_reaches_the_runtime_by_run_id() {
        // The window TASK-34 closes: the engine applied the Start, the stop
        // lands before the execution was spawned, so no handle exists to
        // cancel. The stop must still reach the runtime — named for the
        // same run — and the delayed Start must arrive after it.
        let core: SharedCore = Arc::new(Mutex::new(engine::core_with_runtime(
            "gc",
            RuntimeIdentity::live("recording"),
        )));
        let request = Request::Action {
            action: SupervisorAction::Start {
                project: "gc".into(),
                ticket: Some("TASK-9".into()),
                revision: None,
            },
        };
        let started = respond(&core, request, Limits::default());
        let FollowUp::Execute {
            request,
            run: starting,
        } = started.follow_up
        else {
            panic!("the start must hand the objective to the runtime");
        };

        let stopped = respond(
            &core,
            Request::Action {
                action: SupervisorAction::Stop,
            },
            Limits::default(),
        );
        let FollowUp::Cancel { run: stopping } = stopped.follow_up else {
            panic!("the stop must cancel the run it stops");
        };
        assert_eq!(
            stopping, starting,
            "the stop names the run the start created"
        );

        let recorded = Arc::new(RecordingRuntime::default());
        let runtime: Arc<dyn ObjectiveRuntime> = recorded.clone();
        let pending: Pending = Arc::new(Mutex::new(None));
        cancel_execution(Some(&runtime), &pending, stopping, CANCEL_GRACE).await;
        // The delayed execution of the start still runs: it must arrive
        // after the stop, under the same identity.
        spawn_execution(
            core.clone(),
            Some(runtime),
            pending.clone(),
            request,
            starting,
        );
        let (held_run, handle) = pending
            .lock()
            .expect("lock")
            .take()
            .expect("the execution handle");
        assert_eq!(held_run, starting);
        handle.await.expect("the execution ends");

        assert_eq!(recorded.cancelled(), vec![starting]);
        assert_eq!(
            recorded.started(),
            vec![starting],
            "the stop reached the runtime before the start of the same run"
        );
        let guard = core.lock().expect("lock");
        assert_eq!(engine::snapshot(&guard).phase, run::run::Phase::Stopped);
        assert!(
            guard.objective.is_none(),
            "the stopped run gets no verdict of its own"
        );
        assert!(!guard.winding_down, "the delayed attempt has ended");
    }
}
