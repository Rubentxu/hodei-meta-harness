//! The product test of the rehearsal: a REAL Unix socket, the REAL
//! server, the REAL client and the rehearsal runtime.
//!
//! One test drives the whole sequence — Start, Pause, Resume, Stop and
//! an error — with a fake agent and a fake tracker, and without Docker,
//! network or a model. The gates of the rehearsal make the states
//! OBSERVABLE: the test opens them, so nothing here is a race.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use run::run::Phase;
use run::runtime::{ObjectiveOutcome, ObjectiveRuntime, RuntimeKind};
use runner::rehearsal::TrialRuntime;
use supervisor::client::{self, SupervisorClient};
use supervisor::engine;
use supervisor::protocol::SupervisorAction;
use supervisor::server;

/// Wait for a phase. The transition arrives asynchronously; a deadline
/// that expires FAILS the test instead of passing quietly.
async fn wait_for_phase(client: &mut SupervisorClient, expected: Phase) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = client.snapshot().await.expect("snapshot");
        if snapshot.phase == expected {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the run never reached {expected:?}; it is {:?}",
            snapshot.phase
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Wait for the objective's verdict to be recorded.
async fn wait_for_objective(client: &mut SupervisorClient) -> ObjectiveOutcome {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = client.snapshot().await.expect("snapshot");
        if let Some(report) = snapshot.objective {
            return report.outcome;
        }
        assert!(
            Instant::now() < deadline,
            "the objective never reported a verdict"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

fn temp_socket_path(tag: &str) -> String {
    std::env::temp_dir()
        .join(format!(
            "hodei-runner-product-{tag}-{}-{}.sock",
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
async fn the_rehearsal_drives_the_whole_sequence_through_the_real_socket() {
    let path = temp_socket_path("flow");
    let listener = server::bind(&path, "trial-project").await.expect("bind");
    let (runtime, gates) = TrialRuntime::gated();
    let recorder = runtime.recorder();
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new(engine::core_with_runtime(
        "trial-project",
        runtime.identity(),
    )));
    let server_task = tokio::spawn(server::serve(
        listener,
        core.clone(),
        Some(Arc::new(runtime)),
        server::Limits::default(),
    ));

    let mut client = client::connect(&path).await.expect("connect");

    // The controller is idle, and it says WHICH runtime it has before
    // anything runs: a rehearsal is never presented as a live run.
    let snapshot = client.snapshot().await.expect("snapshot");
    assert_eq!(snapshot.phase, Phase::Idle);
    let wired = snapshot.runtime.expect("the wired runtime is visible");
    assert_eq!(wired.kind, RuntimeKind::Rehearsal);
    assert!(wired.label().starts_with("rehearsal"));

    // START: the action is applied and the run starts — but nothing has
    // confirmed it yet.
    let start = client
        .action(SupervisorAction::Start {
            project: "trial-project".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("start");
    assert!(start.applied, "{}", start.detail);
    wait_for_phase(&mut client, Phase::Starting).await;
    let starting = client.snapshot().await.expect("snapshot");
    assert_eq!(starting.phase, Phase::Starting);
    assert!(starting.objective.is_none(), "no objective has ended yet");

    // The runtime confirms: NOW the run is running.
    gates.accept();
    wait_for_phase(&mut client, Phase::Running).await;

    // THE ERRORS: while the run is in flight, a second controller is
    // refused, and a Start for another project is refused too.
    let second = client
        .action(SupervisorAction::Start {
            project: "trial-project".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("second start");
    assert!(!second.applied, "{}", second.detail);
    assert!(
        second.detail.contains("second controller"),
        "{}",
        second.detail
    );

    let wrong = client
        .action(SupervisorAction::Start {
            project: "another-project".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("start for another project");
    assert!(!wrong.applied, "{}", wrong.detail);
    assert!(
        wrong.detail.contains("project mismatch"),
        "{}",
        wrong.detail
    );

    // PAUSE: the prompt loop is gated, the session stays.
    let pause = client.action(SupervisorAction::Pause).await.expect("pause");
    assert!(pause.applied, "{}", pause.detail);
    assert!(client.snapshot().await.expect("snapshot").paused);

    // RESUME: the prompt loop runs again.
    let resume = client
        .action(SupervisorAction::Resume)
        .await
        .expect("resume");
    assert!(resume.applied, "{}", resume.detail);
    assert!(!client.snapshot().await.expect("snapshot").paused);

    // The objective's work happens now, and its verdict is recorded.
    gates.finish();
    let outcome = wait_for_objective(&mut client).await;
    assert!(
        matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
        "{outcome:?}"
    );

    // STOP: the session ends.
    let stop = client.action(SupervisorAction::Stop).await.expect("stop");
    assert!(stop.applied, "{}", stop.detail);
    assert_eq!(
        client.snapshot().await.expect("snapshot").phase,
        Phase::Stopped
    );

    // The rehearsal published nothing: it closed no ticket and moved no
    // ref. The recording says so.
    let recorded = recorder.publications();
    assert_eq!(recorded.len(), 1, "the rehearsal recorded one publication");
    assert_eq!(
        recorded[0].closure,
        runner::ports::Closed::Unconfirmed,
        "the rehearsal closed nothing"
    );
    assert!(
        recorded[0].detail.contains("nothing was closed"),
        "{}",
        recorded[0].detail
    );

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

/// The ERROR case of the objective itself, over the same real socket:
/// a verification that does not hold rejects the objective, nothing is
/// published, and the session ends with the reason visible.
#[tokio::test]
async fn an_objective_whose_verification_does_not_hold_is_rejected_and_ends_the_session() {
    let path = temp_socket_path("rejected");
    let listener = server::bind(&path, "trial-project-rejected")
        .await
        .expect("bind");
    let runtime = TrialRuntime::rejecting();
    let recorder = runtime.recorder();
    let core: Arc<Mutex<engine::SupervisorCore>> =
        Arc::new(Mutex::new(engine::default_core("trial-project-rejected")));
    let server_task = tokio::spawn(server::serve(
        listener,
        core.clone(),
        Some(Arc::new(runtime)),
        server::Limits::default(),
    ));

    let mut client = client::connect(&path).await.expect("connect");
    let start = client
        .action(SupervisorAction::Start {
            project: "trial-project-rejected".into(),
            ticket: None,
            revision: None,
        })
        .await
        .expect("start");
    assert!(start.applied, "{}", start.detail);

    // The verdict arrives: the objective did NOT hold.
    let outcome = wait_for_objective(&mut client).await;
    assert!(
        matches!(outcome, ObjectiveOutcome::Rejected { .. }),
        "{outcome:?}"
    );
    // A rejected objective ends the session, and the reason is visible.
    wait_for_phase(&mut client, Phase::Stopped).await;
    let snapshot = client.snapshot().await.expect("snapshot");
    assert!(
        snapshot
            .objective
            .as_ref()
            .is_some_and(|report| matches!(report.outcome, ObjectiveOutcome::Rejected { .. })),
        "the verdict is recorded: {:?}",
        snapshot.objective
    );
    // Nothing was published: the gate held.
    assert!(recorder.publications().is_empty());

    server_task.abort();
    let _ = std::fs::remove_file(&path);
}

/// The rehearsal has no path to promote or to close a ticket.
///
/// This crate declares no dependency on any crate that writes outside
/// this process — the promoter, the releaser or the workspace exporter —
/// and the rehearsal's publisher holds no writer at all: it can only
/// record. The route to an effect does not exist because the ports that
/// could carry one are declared HERE, and the tracker port is read-only.
#[test]
fn the_rehearsal_cannot_reach_the_promoter() {
    let manifest = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"))
        .expect("the crate manifest");
    for writer in ["hodei-mh-promote", "hodei-mh-release", "hodei-mh-workspace"] {
        assert!(
            !manifest.contains(writer),
            "the rehearsal must not depend on '{writer}', a crate that writes outside the process: {manifest}"
        );
    }
}
