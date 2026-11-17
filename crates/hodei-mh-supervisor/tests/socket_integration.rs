//! Integration: REAL Unix-socket server + client (AC-1/AC-2). Discovery,
//! exclusive controller, action confirmation, event replay after
//! reconnection, and honest not-reachable when the socket is gone.

use std::sync::{Arc, Mutex};

use supervisor::client::{self, ClientError};
use supervisor::engine;
use supervisor::protocol::{Snapshot, SupervisorAction};
use supervisor::server;

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
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("gc-fixture")));
    let server_task = tokio::spawn(server::serve(listener, core.clone()));

    // AC-2: snapshot before anything — telemetry unknown is honest.
    let mut client = client::connect(&path).await.expect("connect");
    let snapshot: Snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.project, "gc-fixture");
    assert_eq!(snapshot.phase, run::run::Phase::Idle);
    assert!(matches!(
        snapshot.resources,
        supervisor::protocol::ResourceObservation::Unknown { .. }
    ));

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
    let after = client.snapshot().await.expect("snapshot after");
    assert_eq!(after.phase, run::run::Phase::Running);
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
async fn binding_over_a_stale_socket_file_succeeds() {
    let path = temp_socket_path("stale");
    // Simulate a crashed supervisor: a socket file exists but nobody
    // listens. The supervisor must bind anyway (garbage removal).
    std::fs::write(&path, b"stale").expect("create stale file");
    let listener = server::bind(&path).await.expect("bind over stale file");
    drop(listener);
    let _ = std::fs::remove_file(&path);
}
