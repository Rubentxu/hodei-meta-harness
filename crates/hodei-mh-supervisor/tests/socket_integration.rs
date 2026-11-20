//! Integration: REAL Unix-socket server + client (AC-1/AC-2). Discovery,
//! exclusive controller, action confirmation, event replay after
//! reconnection, and honest not-reachable when the socket is gone.
//!
//! Every test uses its OWN project identity: the product serves one
//! controller per project, and the tests run concurrently.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use run::runtime::{
    ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RunId, RuntimeError,
    RuntimeIdentity,
};
use supervisor::client::{self, ClientError};
use supervisor::engine;
use supervisor::protocol::{Response, Snapshot, SupervisorAction};
use supervisor::server;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// A runtime that confirms immediately and reports a confirmed outcome.
/// The supervisor's own test needs no agent: what it proves is the
/// contract between the controller and a runtime.
///
/// A start that names no work asks for the NEXT selection of the same
/// run; the stub answers with an attempt that is still in flight, so the
/// session stays running after the confirmed objective.
struct StubRuntime;

#[async_trait::async_trait]
impl ObjectiveRuntime for StubRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity::rehearsal("stub")
    }

    async fn start(
        &self,
        _run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError> {
        let Some(work_ref) = request.work_ref else {
            return Ok(ObjectiveHandle::new(std::future::pending()));
        };
        Ok(ObjectiveHandle::finished(ObjectiveOutcome::Confirmed {
            work_ref,
            detail: "the stub confirmed the objective".into(),
        }))
    }
}

/// Wait for the run to reach a phase. The confirmation arrives
/// asynchronously; a deadline that expires FAILS the test instead of
/// passing quietly.
async fn wait_for_phase(
    client: &mut supervisor::client::SupervisorClient,
    expected: run::run::Phase,
) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = client.snapshot().await.expect("snapshot");
        if snapshot.phase == expected {
            return;
        }
        if Instant::now() >= deadline {
            panic!(
                "the run never reached {expected:?}; it is {:?}",
                snapshot.phase
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn temp_socket_path(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "hodei-supervisor-test-{tag}-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis())
                .unwrap_or_default()
        ))
        .to_string_lossy()
        .to_string()
}

#[tokio::test]
async fn clients_discover_apply_and_reconnect_without_a_second_controller() {
    let path = temp_socket_path("main");
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(engine::core_with_runtime(
        "gc-fixture-main",
        RuntimeIdentity::rehearsal("stub"),
    )));
    let bound = server::bind(&path, "gc-fixture-main").await.expect("bind");
    let server_task = tokio::spawn(server::serve(
        bound,
        core.clone(),
        Some(Arc::new(StubRuntime)),
        server::Limits::default(),
    ));

    // AC-2: snapshot before anything — telemetry unknown is honest.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture-main");
    assert_eq!(snapshot.phase, run::run::Phase::Idle);
    assert!(matches!(
        snapshot.resources,
        supervisor::protocol::ResourceObservation::Unknown { .. }
    ));
    // The runtime's kind is visible before anything runs.
    let runtime = snapshot.runtime.expect("the wired runtime is visible");
    assert_eq!(runtime.kind, run::runtime::RuntimeKind::Rehearsal);

    // AC-2: an action applied for real is confirmed; a refused one is a
    // real answer too.
    let pause_idle = client.action(SupervisorAction::Pause).await.expect("pause");
    assert!(!pause_idle.applied, "pausing with no run must be refused");
    let start = client
        .action(SupervisorAction::Start {
            project: "gc-fixture-main".into(),
            ticket: Some("TASK-9".into()),
            revision: Some("deadbeef".into()),
        })
        .await
        .expect("start");
    assert!(start.applied);
    // The run is running because the runtime CONFIRMED it, not because
    // the client asked for it.
    wait_for_phase(&mut client, run::run::Phase::Running).await;
    let after = client.snapshot().await.expect("snapshot after");
    assert_eq!(after.ticket.as_deref(), Some("TASK-9"));

    // The second-controller Start is refused over the wire.
    let second = client
        .action(SupervisorAction::Start {
            project: "other".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("second start");
    assert!(!second.applied);

    // AC-1: drop the client and reconnect — same supervisor, no new
    // controller, event history replays by sequence.
    drop(client);
    let mut client2 = client::connect(&path).await.expect("reconnect");
    let page = client2.events(1, None).await.expect("events");
    assert!(
        page.events.len() >= 2,
        "history replays: {}",
        page.events.len()
    );
    assert_eq!(
        page.next_seq,
        page.events.last().map(|e| e.seq + 1).unwrap_or(1)
    );
    let phases = client2.snapshot().await.expect("snapshot again");
    assert_eq!(phases.phase, run::run::Phase::Running);
    // The objective's verdict is recorded, not just the phase.
    let objective = phases.objective.expect("the objective reported");
    assert!(matches!(
        objective.outcome,
        ObjectiveOutcome::Confirmed { .. }
    ));

    drop(client2);
    server_task.abort();
    let _ = std::fs::remove_file(&path);

    // AC-1: with the supervisor gone, clients see NOT REACHABLE — they do
    // not start a second controller.
    let error = client::connect(&path).await;
    match error {
        Err(ClientError::NotReachable { path: p, .. }) => assert_eq!(p, path),
        Err(other) => panic!("expected NotReachable, got {other}"),
        Ok(_) => panic!("connect must fail with the supervisor gone"),
    }
}

#[tokio::test]
async fn a_controller_without_a_runtime_refuses_to_start() {
    let path = temp_socket_path("no-runtime");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-no-runtime")));
    let bound = server::bind(&path, "gc-fixture-no-runtime")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(
        bound,
        core.clone(),
        None,
        server::Limits::default(),
    ));

    let mut client = client::connect(&path).await.expect("connect");
    let start = client
        .action(SupervisorAction::Start {
            project: "gc-fixture-no-runtime".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("start");
    assert!(!start.applied, "a run nobody can execute is not a run");
    assert!(
        start.detail.contains("no runtime is wired"),
        "{}",
        start.detail
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_stale_socket_file_is_reused_but_a_file_that_is_not_a_socket_is_never_removed() {
    // A CRASHED supervisor leaves a SOCKET file with nobody listening:
    // nobody ran the cleanup, because nobody could. The new controller
    // takes it over: a crash never blocks a restart.
    let path = temp_socket_path("stale-socket");
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind a socket");
    drop(listener);
    // The socket file is still there; nobody listens on it.
    assert!(std::path::Path::new(&path).exists());
    let rebound = server::bind(&path, "gc-fixture-stale-socket")
        .await
        .expect("bind over a stale socket");
    drop(rebound);
    let _ = std::fs::remove_file(&path);

    // A file that is NOT a socket is the operator's file: it is never
    // removed, and the controller refuses to start.
    let path = temp_socket_path("occupied");
    std::fs::write(&path, b"not a socket").expect("create the file");
    match server::bind(&path, "gc-fixture-occupied").await {
        Err(server::ServerError::SocketPathOccupied { path: p }) => assert_eq!(p, path),
        Err(other) => panic!("expected SocketPathOccupied, got {other}"),
        Ok(_) => panic!("a file that is not a socket must not be taken over"),
    }
    assert!(
        std::path::Path::new(&path).exists(),
        "the operator's file must still be there"
    );
    assert_eq!(
        std::fs::read(&path).expect("read the file"),
        b"not a socket".to_vec()
    );
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_second_supervisor_cannot_steal_a_live_socket() {
    let path = temp_socket_path("live");
    let bound = server::bind(&path, "gc-fixture-live").await.expect("bind");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-live")));
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));

    // A second supervisor on the SAME socket must be refused: two live
    // cores would split the state (the clients would disagree).
    match server::bind(&path, "gc-fixture-live-other").await {
        Err(server::ServerError::AlreadyRunning { path: p, .. }) => assert_eq!(p, path),
        Err(other) => panic!("expected AlreadyRunning, got {other}"),
        Ok(_) => panic!("a second supervisor must not take a live socket"),
    }

    // The original supervisor is still the one serving.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture-live");

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn two_supervisors_cannot_serve_the_same_project() {
    // The project is the IDENTITY of a controller: a second one, even on
    // another socket, is refused. The decision is an exclusive lock, so
    // two controllers that start at the same time cannot both win.
    let first_path = temp_socket_path("identity-a");
    let second_path = temp_socket_path("identity-b");
    let project = "gc-fixture-identity";
    let _first = server::bind(&first_path, project)
        .await
        .expect("bind the first");

    match server::bind(&second_path, project).await {
        Err(server::ServerError::ProjectBusy { project: p, .. }) => assert_eq!(p, project),
        Err(other) => panic!("expected ProjectBusy, got {other}"),
        Ok(_) => panic!("a second controller must not serve the same project"),
    }

    let _ = std::fs::remove_file(&first_path);
}

#[tokio::test]
async fn a_supervisor_may_restart_after_the_previous_one_crashed() {
    let path = temp_socket_path("restart");
    // A crashed supervisor leaves a socket file with nobody listening:
    // the process died and nobody ran the cleanup.
    let listener = std::os::unix::net::UnixListener::bind(&path).expect("bind a socket");
    drop(listener);
    assert!(std::path::Path::new(&path).exists());

    // A new supervisor takes it over (the old one is not alive).
    let bound = server::bind(&path, "gc-fixture-restart")
        .await
        .expect("restart binds");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("first")));
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot = client.snapshot().await.expect("snapshot");
    // The controller serves the identity it took the lock for.
    assert_eq!(snapshot.project, "gc-fixture-restart");
    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_supervisor_only_removes_the_socket_it_owns() {
    let path = temp_socket_path("owned");
    let bound = server::bind(&path, "gc-fixture-owned").await.expect("bind");
    let ours = server::identity_of(std::path::Path::new(&path)).expect("identity");

    // Somebody else replaced the file at the same path (a successor
    // supervisor that already took the socket over).
    std::fs::remove_file(&path).expect("unlink");
    std::fs::write(&path, b"not our socket").expect("replace");
    assert!(
        !server::unlink_if_owned(std::path::Path::new(&path), ours),
        "a file that is not ours must not be removed"
    );
    assert!(std::path::Path::new(&path).exists());

    // The controller stops: it does NOT remove the file that is not
    // its socket.
    drop(bound);
    assert!(
        std::path::Path::new(&path).exists(),
        "the successor's file must survive the controller that stopped"
    );
    assert_eq!(
        std::fs::read(&path).expect("read the file"),
        b"not our socket".to_vec()
    );
    let _ = std::fs::remove_file(&path);

    // Its OWN socket file IS removed when it stops.
    let path = temp_socket_path("owned-clean");
    let bound = server::bind(&path, "gc-fixture-owned-clean")
        .await
        .expect("bind");
    assert!(std::path::Path::new(&path).exists());
    drop(bound);
    assert!(
        !std::path::Path::new(&path).exists(),
        "a clean shutdown leaves no socket file behind"
    );
}

#[tokio::test]
async fn an_oversized_request_is_refused_and_the_others_keep_the_service() {
    let path = temp_socket_path("oversized");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-oversized")));
    let bound = server::bind(&path, "gc-fixture-oversized")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));

    // A client that sends an endless line is cut off with an explicit
    // answer, and it does not take the service down.
    let mut abusive = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let flood = vec![b'x'; server::MAX_REQUEST_BYTES * 2];
    abusive.write_all(&flood).await.expect("write the flood");
    let mut answer = String::new();
    let read = abusive
        .read_to_string(&mut answer)
        .await
        .expect("read the answer");
    assert!(read > 0, "the abusive client gets an answer, not silence");
    assert!(
        answer.contains("exceeds the limit"),
        "the refusal names the limit: {answer}"
    );

    // A normal client is served while the abusive one is being cut down.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture-oversized");

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn the_number_of_connected_clients_is_bounded() {
    let path = temp_socket_path("connections");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-connections")));
    let bound = server::bind(&path, "gc-fixture-connections")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));

    // Fill the room: every connection holds a permit while it lives.
    let mut held = Vec::new();
    for _ in 0..server::MAX_CONNECTIONS {
        held.push(
            tokio::net::UnixStream::connect(&path)
                .await
                .expect("connect"),
        );
    }
    // The next client is refused, and it is told why.
    let mut extra = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let mut answer = String::new();
    extra
        .read_to_string(&mut answer)
        .await
        .expect("read the answer");
    assert!(
        answer.contains("too many clients"),
        "the extra client is told why: {answer}"
    );

    drop(held);
    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_lost_history_is_announced_to_the_client() {
    let path = temp_socket_path("history");
    let mut core = engine::default_core("gc-fixture-history");
    // More events than the ring keeps: the oldest ones fall off.
    for index in 0..(engine::MAX_RETAINED_EVENTS + 10) {
        engine::emit_event(
            &mut core,
            "test/event",
            serde_json::json!({ "index": index }),
            supervisor::protocol::Provenance::Human,
        );
    }
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(core));
    let bound = server::bind(&path, "gc-fixture-history")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));

    let mut client = client::connect(&path).await.expect("connect");
    let page = client.events(1, None).await.expect("events");
    assert!(
        page.history_lost,
        "the client is told that its history has a hole"
    );
    assert!(page.oldest_seq > 1, "the oldest retained event is named");
    // The page is BOUNDED and the cursor says where to continue: it
    // continues where the RETAINED history starts, not where the
    // client asked from.
    assert_eq!(page.events.len(), engine::MAX_EVENTS_PER_PAGE);
    assert_eq!(
        page.events.first().map(|event| event.seq),
        Some(page.oldest_seq)
    );
    assert_eq!(
        page.next_seq,
        page.oldest_seq + engine::MAX_EVENTS_PER_PAGE as u64
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn an_event_keeps_its_structure_over_the_wire() {
    let path = temp_socket_path("structure");
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(engine::core_with_runtime(
        "gc-fixture-structure",
        RuntimeIdentity::rehearsal("stub"),
    )));
    let bound = server::bind(&path, "gc-fixture-structure")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(
        bound,
        core.clone(),
        Some(Arc::new(StubRuntime)),
        server::Limits::default(),
    ));

    let mut client = client::connect(&path).await.expect("connect");
    client
        .action(SupervisorAction::Start {
            project: "gc-fixture-structure".into(),
            ticket: Some("TASK-9".into()),
            revision: None,
        })
        .await
        .expect("start");
    let page = client.events(1, None).await.expect("events");
    let action = page
        .events
        .iter()
        .find(|event| event.kind == "supervisor/action")
        .expect("the action event");
    // The body is a TYPED value on the wire: the client reads the fields,
    // it does not parse a sentence.
    assert!(action.payload.is_object(), "{:?}", action.payload);
    assert_eq!(action.payload["applied"], serde_json::json!(true));
    assert_eq!(
        action.payload["action"]["ticket"],
        serde_json::json!("TASK-9")
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn two_supervisors_that_start_at_the_same_time_cannot_both_win() {
    // The decision is the KERNEL's lock, not a probe: two controllers
    // that ask for the same identity at the same time cannot both take
    // it. One of them is refused.
    let path = temp_socket_path("concurrent");
    let project = "gc-fixture-concurrent";
    let (first, second) = tokio::join!(server::bind(&path, project), server::bind(&path, project));
    let winners = [&first, &second]
        .iter()
        .filter(|result| result.is_ok())
        .count();
    assert_eq!(winners, 1, "exactly one controller may take the identity");
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_client_that_never_speaks_is_dropped() {
    let path = temp_socket_path("idle");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-idle")));
    let bound = server::bind(&path, "gc-fixture-idle").await.expect("bind");
    let limits = server::Limits {
        idle_timeout: Duration::from_millis(100),
        ..server::Limits::default()
    };
    let server_task = tokio::spawn(server::serve(bound, core, None, limits));

    // The client connects and says NOTHING: it does not hold a task of
    // the supervisor forever.
    let mut stream = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    let mut answer = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
        .await
        .expect("the connection was never closed")
        .expect("the read failed");
    assert_eq!(read, 0, "the supervisor closed the silent connection");

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_client_that_does_not_read_is_dropped() {
    // The client asks for a big page and NEVER reads it: the answer does
    // not fit in the socket buffer, the write blocks, and the deadline
    // closes the connection. Without the deadline the client would wait
    // for an answer that never comes.
    let path = temp_socket_path("slow");
    let mut core = engine::default_core("gc-fixture-slow");
    for index in 0..engine::MAX_EVENTS_PER_PAGE {
        engine::emit_event(
            &mut core,
            "test/event",
            serde_json::json!({ "index": index, "blob": "x".repeat(3000) }),
            supervisor::protocol::Provenance::Human,
        );
    }
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(core));
    let bound = server::bind(&path, "gc-fixture-slow").await.expect("bind");
    let limits = server::Limits {
        write_deadline: Duration::from_millis(100),
        ..server::Limits::default()
    };
    let server_task = tokio::spawn(server::serve(bound, core, None, limits));

    let mut stream = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    stream
        .write_all(b"{\"req\":\"events\",\"since\":0}\n")
        .await
        .expect("write the request");
    // The client does not read: the answer fills the buffer and the
    // supervisor gives up on it.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let mut answer = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), stream.read_to_end(&mut answer))
        .await
        .expect("the connection was never closed")
        .expect("the read failed");
    // The connection was CLOSED (the read finished instead of waiting
    // forever) and the answer was CUT: the last event of the page never
    // arrived, because the client never read it.
    assert!(read > 0, "the answer reached the buffer of the client");
    let text = String::from_utf8_lossy(&answer);
    assert!(
        !text.contains("\"index\":255"),
        "the answer was cut before the last event of the page"
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn the_answer_states_the_version_of_the_contract() {
    let path = temp_socket_path("version");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture-version")));
    let bound = server::bind(&path, "gc-fixture-version")
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(bound, core, None, server::Limits::default()));

    // The raw wire: the answer says which version of the contract it is.
    let mut stream = tokio::net::UnixStream::connect(&path)
        .await
        .expect("connect");
    stream
        .write_all(b"{\"req\":\"events\",\"since\":0}\n")
        .await
        .expect("write the request");
    // ONE line is the answer: the connection stays open for the next
    // request, so the client reads the line and not the end of it.
    let mut answer = String::new();
    let mut reader = tokio::io::BufReader::new(stream);
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut answer)
        .await
        .expect("read the answer");
    let response: Response = serde_json::from_str(answer.trim()).expect("parse the answer");
    assert_eq!(
        response.protocol_version(),
        supervisor::protocol::PROTOCOL_VERSION
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

/// A runtime that records what every start asked for, keeps its first
/// attempt pending until the test releases it, and answers with the
/// scripted outcomes in order.
struct ChainingRuntime {
    starts: Mutex<Vec<Option<String>>>,
    outcomes: Mutex<std::collections::VecDeque<ObjectiveOutcome>>,
    release: Arc<tokio::sync::Notify>,
}

impl ChainingRuntime {
    fn new(outcomes: Vec<ObjectiveOutcome>) -> Arc<Self> {
        Arc::new(Self {
            starts: Mutex::new(Vec::new()),
            outcomes: Mutex::new(outcomes.into()),
            release: Arc::new(tokio::sync::Notify::new()),
        })
    }

    fn starts(&self) -> Vec<Option<String>> {
        self.starts
            .lock()
            .map(|starts| starts.clone())
            .unwrap_or_default()
    }

    fn release_first(&self) {
        self.release.notify_one();
    }
}

#[async_trait::async_trait]
impl ObjectiveRuntime for ChainingRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity::live("chaining")
    }

    async fn start(
        &self,
        _run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError> {
        let first = {
            let mut starts = self.starts.lock().expect("the starts");
            starts.push(request.work_ref);
            starts.len() == 1
        };
        let outcome = self
            .outcomes
            .lock()
            .expect("the outcomes")
            .pop_front()
            .unwrap_or(ObjectiveOutcome::Idle {
                reason: "nothing else to do".into(),
            });
        let release = self.release.clone();
        Ok(ObjectiveHandle::new(async move {
            if first {
                release.notified().await;
            }
            outcome
        }))
    }

    fn cancel(&self, _run: RunId) {
        // A stop reaches the attempt it names: the pending completion ends,
        // the way a real attempt winds down when it is stopped.
        self.release.notify_one();
    }
}

/// Wait until an outcome of the given kind is in the record, and return
/// its payload.
///
/// The confirmation is asynchronous; the wait yields instead of sleeping,
/// and a deadline that expires FAILS the test instead of passing quietly.
async fn wait_for_outcome(
    client: &mut supervisor::client::SupervisorClient,
    outcome: &str,
) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let page = client.events(1, None).await.expect("events");
        let recorded = page
            .events
            .iter()
            .find(|event| event.kind == "objective/outcome" && event.payload["outcome"] == outcome)
            .map(|event| event.payload.clone());
        if let Some(payload) = recorded {
            return payload;
        }
        if Instant::now() >= deadline {
            panic!("the {outcome} outcome was never recorded");
        }
        tokio::task::yield_now().await;
    }
}

/// A supervisor serving a runtime that confirms and then selects again.
async fn chaining_supervisor(
    tag: &str,
    outcomes: Vec<ObjectiveOutcome>,
) -> (
    String,
    Arc<Mutex<engine::SupervisorCore>>,
    Arc<ChainingRuntime>,
    tokio::task::JoinHandle<std::io::Result<()>>,
) {
    let path = temp_socket_path(tag);
    let core = Arc::new(Mutex::new(engine::core_with_runtime(
        format!("gc-fixture-{tag}"),
        RuntimeIdentity::live("chaining"),
    )));
    let runtime = ChainingRuntime::new(outcomes);
    let bound = server::bind(&path, &format!("gc-fixture-{tag}"))
        .await
        .expect("bind");
    let server_task = tokio::spawn(server::serve(
        bound,
        core.clone(),
        Some(runtime.clone() as Arc<dyn ObjectiveRuntime>),
        server::Limits::default(),
    ));
    (path, core, runtime, server_task)
}

fn confirmed(work_ref: &str) -> ObjectiveOutcome {
    ObjectiveOutcome::Confirmed {
        work_ref: work_ref.into(),
        detail: "the objective held".into(),
    }
}

#[tokio::test]
async fn a_confirmed_objective_starts_the_next_attempt_of_the_same_run() {
    let (path, core, runtime, server_task) =
        chaining_supervisor("chain-next", vec![confirmed("TASK-9")]).await;
    let mut client = client::connect(&path).await.expect("connect");
    client
        .action(SupervisorAction::Start {
            project: "gc-fixture-chain-next".into(),
            ticket: Some("TASK-9".into()),
            revision: None,
        })
        .await
        .expect("start");
    wait_for_phase(&mut client, run::run::Phase::Running).await;

    // The objective holds: the same run asks for the NEXT selection
    // instead of waiting for a Start the engine would refuse.
    runtime.release_first();
    wait_for_phase(&mut client, run::run::Phase::Idle).await;
    assert_eq!(
        runtime.starts(),
        vec![Some("TASK-9".to_string()), None],
        "the second attempt names no work: the runtime selects it"
    );
    let epochs: Vec<u64> = {
        let guard = core.lock().expect("lock");
        guard.events.iter().map(|event| event.epoch).collect()
    };
    assert!(
        epochs.iter().all(|epoch| *epoch <= 1),
        "the next attempt belongs to the same run: {epochs:?}"
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_paused_run_does_not_start_the_next_attempt() {
    let (path, core, runtime, server_task) =
        chaining_supervisor("chain-paused", vec![confirmed("TASK-9")]).await;
    let mut client = client::connect(&path).await.expect("connect");
    client
        .action(SupervisorAction::Start {
            project: "gc-fixture-chain-paused".into(),
            ticket: Some("TASK-9".into()),
            revision: None,
        })
        .await
        .expect("start");
    wait_for_phase(&mut client, run::run::Phase::Running).await;
    let pause = client.action(SupervisorAction::Pause).await.expect("pause");
    assert!(pause.applied, "{}", pause.detail);

    runtime.release_first();
    wait_for_outcome(&mut client, "confirmed").await;
    assert_eq!(
        runtime.starts(),
        vec![Some("TASK-9".to_string())],
        "a paused run selects nothing"
    );
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.phase, run::run::Phase::Running);
    assert!(snapshot.paused);

    drop(core);
    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_stopped_run_does_not_start_the_next_attempt() {
    let (path, _core, runtime, server_task) =
        chaining_supervisor("chain-stopped", vec![confirmed("TASK-9")]).await;
    let mut client = client::connect(&path).await.expect("connect");
    client
        .action(SupervisorAction::Start {
            project: "gc-fixture-chain-stopped".into(),
            ticket: Some("TASK-9".into()),
            revision: None,
        })
        .await
        .expect("start");
    wait_for_phase(&mut client, run::run::Phase::Running).await;

    // The stop reaches the pending attempt; its confirmed verdict is
    // recorded beside the stop, and no next attempt may follow it.
    let stop = client.action(SupervisorAction::Stop).await.expect("stop");
    assert!(stop.applied, "{}", stop.detail);
    assert_eq!(
        runtime.starts(),
        vec![Some("TASK-9".to_string())],
        "a stopped run selects nothing"
    );
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.phase, run::run::Phase::Stopped);

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn the_chain_ends_the_run_with_a_visible_verdict_at_the_attempt_bound() {
    // Objectives that keep holding forever must not turn the run into an
    // unbounded chain nobody observes: at the bound the supervisor stops
    // selecting, and the run ends with THAT verdict visible.
    let outcomes: Vec<ObjectiveOutcome> = (0..server::MAX_CHAINED_ATTEMPTS)
        .map(|attempt| confirmed(&format!("TASK-{attempt}")))
        .collect();
    let (path, _core, runtime, server_task) = chaining_supervisor("chain-bound", outcomes).await;
    let mut client = client::connect(&path).await.expect("connect");
    client
        .action(SupervisorAction::Start {
            project: "gc-fixture-chain-bound".into(),
            ticket: Some("TASK-0".into()),
            revision: None,
        })
        .await
        .expect("start");
    wait_for_phase(&mut client, run::run::Phase::Running).await;
    runtime.release_first();

    let payload = wait_for_outcome(&mut client, "failed").await;
    let reason = payload["reason"].as_str().unwrap_or_default().to_string();
    assert!(
        reason.contains(&server::MAX_CHAINED_ATTEMPTS.to_string()),
        "the verdict names the bound that was hit: {reason}"
    );
    assert_eq!(
        runtime.starts().len(),
        server::MAX_CHAINED_ATTEMPTS,
        "the chain stops selecting at the bound"
    );
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.phase, run::run::Phase::Stopped);

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}
