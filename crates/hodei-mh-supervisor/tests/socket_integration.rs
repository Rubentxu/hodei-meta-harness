//! Integration: REAL Unix-socket server + client (AC-1/AC-2). Discovery,
//! exclusive controller, action confirmation, event replay after
//! reconnection, and honest not-reachable when the socket is gone.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use run::runtime::{
    ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RuntimeError,
    RuntimeIdentity,
};
use supervisor::client::{self, ClientError};
use supervisor::engine;
use supervisor::protocol::{Snapshot, SupervisorAction};
use supervisor::server;

/// A runtime that confirms immediately and reports a confirmed outcome.
/// The supervisor's own test needs no agent: what it proves is the
/// contract between the controller and a runtime.
struct StubRuntime;

#[async_trait::async_trait]
impl ObjectiveRuntime for StubRuntime {
    fn identity(&self) -> RuntimeIdentity {
        RuntimeIdentity::rehearsal("stub")
    }

    async fn start(&self, request: ObjectiveRequest) -> Result<ObjectiveHandle, RuntimeError> {
        Ok(ObjectiveHandle::finished(ObjectiveOutcome::Confirmed {
            work_ref: request.work_ref.unwrap_or_else(|| "TASK-9".into()),
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
    let listener = server::bind(&path).await.expect("bind");
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(engine::core_with_runtime(
        "gc-fixture",
        RuntimeIdentity::rehearsal("stub"),
    )));
    let server_task = tokio::spawn(server::serve_with_runtime(
        listener,
        core.clone(),
        Some(Arc::new(StubRuntime)),
    ));

    // AC-2: snapshot before anything — telemetry unknown is honest.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture");
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
            project: "gc-fixture".into(),
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
    let (events, next) = client2.events(1).await.expect("events");
    assert!(events.len() >= 2, "history replays: {}", events.len());
    assert_eq!(next, events.last().map(|e| e.seq + 1).unwrap_or(1));
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
    let listener = server::bind(&path).await.expect("bind");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture")));
    let server_task = tokio::spawn(server::serve(listener, core.clone()));

    let mut client = client::connect(&path).await.expect("connect");
    let start = client
        .action(SupervisorAction::Start {
            project: "gc-fixture".into(),
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
async fn binding_over_a_stale_socket_file_succeeds() {
    let path = temp_socket_path("stale");
    // Simulate a crashed supervisor: a socket file exists but nobody
    // listens. The supervisor must bind anyway (garbage removal).
    std::fs::write(&path, b"stale").expect("create stale file");
    let listener = server::bind(&path).await.expect("bind over stale file");
    drop(listener);
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_second_supervisor_cannot_steal_a_live_socket() {
    let path = temp_socket_path("live");
    let listener = server::bind(&path).await.expect("bind");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture")));
    let server_task = tokio::spawn(server::serve(listener, core.clone()));

    // A second supervisor on the SAME socket must be refused: two live
    // cores would split the state (the clients would disagree).
    match server::bind(&path).await {
        Err(server::ServerError::AlreadyRunning { path: p }) => assert_eq!(p, path),
        Err(other) => panic!("expected AlreadyRunning, got {other}"),
        Ok(_) => panic!("a second supervisor must not take a live socket"),
    }

    // The original supervisor is still the one serving.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture");

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_supervisor_may_restart_after_the_previous_one_crashed() {
    let path = temp_socket_path("restart");
    // A crashed supervisor leaves a socket file with nobody listening.
    let listener = server::bind(&path).await.expect("bind");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("first")));
    let server_task = tokio::spawn(server::serve(listener, core));
    server_task.abort();
    // The socket file is still there; the listener is gone.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // A new supervisor takes it over (the old one is not alive).
    let listener = server::bind(&path).await.expect("restart binds");
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("second")));
    let server_task = tokio::spawn(server::serve(listener, core));
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "second");
    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_supervisor_only_removes_the_socket_it_owns() {
    let path = temp_socket_path("owned");
    let listener = server::bind(&path).await.expect("bind");
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

    // Our own socket file IS removed.
    drop(listener);
    let _ = std::fs::remove_file(&path);
    std::fs::write(&path, b"stale").expect("recreate");
    let listener = server::bind(&path).await.expect("bind");
    let ours = server::identity_of(std::path::Path::new(&path)).expect("identity");
    drop(listener);
    assert!(server::unlink_if_owned(std::path::Path::new(&path), ours));
    assert!(!std::path::Path::new(&path).exists());
}
