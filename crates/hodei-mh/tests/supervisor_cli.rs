//! The CLI wiring, exercised against the REAL binary.
//!
//! The controller the binary serves must hand the objective to its
//! runtime: a Start that is accepted but never executed leaves the run
//! in `Starting` forever, and the CLI would show a run that never runs.
//! This test drives the product as a user does — the real binary, the
//! real socket, the real CLI commands — with the rehearsal runtime, so
//! no Docker, no network and no model are involved.

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::{Child, Command};

/// The binary under test: cargo builds it before the test runs.
const BIN: &str = env!("CARGO_BIN_EXE_hodei-mh");

/// A socket path of this test's own: two runs never share one.
fn socket_path(tag: &str) -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir()
        .join(format!(
            "hodei-cli-{tag}-{}-{nanos}.sock",
            std::process::id()
        ))
        .to_string_lossy()
        .to_string()
}

/// Run one command of the product and capture what it answered.
async fn capture(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(BIN)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("run hodei-mh");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// Wait until the CLI's own answer contains `needle`.
///
/// A deadline that expires FAILS the test: a state that was never
/// reached is not a state that was observed.
async fn wait_for(socket: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (code, stdout, stderr) = capture(&["sup-status", "--socket", socket]).await;
        if code == 0 && stdout.contains(needle) {
            return stdout;
        }
        assert!(
            Instant::now() < deadline,
            "the supervisor never reported '{needle}'; last answer was: {stdout}{stderr}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// Wait until the supervisor's event log contains `needle`.
///
/// The verdict of an objective is recorded after its confirmation, so a
/// test that reads the log the moment the run is `running` would be
/// racing. This waits for the event itself.
async fn wait_for_event(socket: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (code, stdout, stderr) = capture(&["sup-events", "--socket", socket]).await;
        if code == 0 && stdout.contains(needle) {
            return stdout;
        }
        assert!(
            Instant::now() < deadline,
            "the supervisor never recorded '{needle}'; last answer was: {stdout}{stderr}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The controller under test. It is killed when the test ends, however
/// it ends: a controller left running would own its socket forever.
struct Controller {
    child: Child,
    socket: String,
}

impl Controller {
    async fn start(tag: &str, rehearsal: bool) -> Self {
        let socket = socket_path(tag);
        let mut command = Command::new(BIN);
        command.args([
            "supervisor",
            "--socket",
            &socket,
            "--project",
            "cli-project",
        ]);
        if rehearsal {
            command.arg("--rehearsal");
        }
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the supervisor");
        Self { child, socket }
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        let _ = std::fs::remove_file(&self.socket);
    }
}

#[tokio::test]
async fn the_cli_reaches_running_only_through_the_runtime_confirmation() {
    let controller = Controller::start("flow", true).await;
    let socket = controller.socket.clone();

    // The controller says WHICH runtime it has before anything runs: a
    // rehearsal is never presented as a live execution.
    let idle = wait_for(&socket, "phase: idle").await;
    assert!(
        idle.contains("runtime: rehearsal"),
        "the runtime kind is never hidden: {idle}"
    );

    // The Start is applied — and the run is NOT running yet: nothing has
    // confirmed that the work began.
    let (start_code, start_out, start_err) = capture(&[
        "sup-action",
        "start",
        "--socket",
        &socket,
        "--ticket",
        "TASK-9",
    ])
    .await;
    assert_eq!(
        start_code, 0,
        "the start was not applied: {start_out}{start_err}"
    );
    assert!(start_out.contains("applied"), "{start_out}");

    // The run reaches `running` ONLY because the runtime confirmed it.
    let running = wait_for(&socket, "phase: running").await;
    assert!(
        running.contains("ticket: TASK-9"),
        "the named work is the one that ran: {running}"
    );
    assert!(
        running.contains("runtime: rehearsal"),
        "a rehearsal never passes for a live run: {running}"
    );

    // And the rehearsal closed nothing: the objective says so. The
    // verdict is recorded AFTER the confirmation, so the test waits for
    // the event instead of racing it.
    let events = wait_for_event(&socket, "objective/outcome").await;
    assert!(
        events.contains("nothing was closed"),
        "the rehearsal publishes nothing: {events}"
    );
}

#[tokio::test]
async fn a_controller_without_a_runtime_refuses_to_start_instead_of_pretending() {
    // Without `--rehearsal` there is NO runtime: the real ACP runtime
    // arrives with the ACP slice. The controller must refuse the Start
    // rather than pretend a run began.
    let controller = Controller::start("noruntime", false).await;
    let socket = controller.socket.clone();

    let idle = wait_for(&socket, "phase: idle").await;
    assert!(
        idle.contains("runtime: (none"),
        "a controller with no runtime says so: {idle}"
    );

    let (code, stdout, stderr) = capture(&["sup-action", "start", "--socket", &socket]).await;
    assert_eq!(code, 1, "the start was NOT executed: {stdout}{stderr}");
    assert!(
        stdout.contains("refused") && stdout.contains("no runtime is wired"),
        "the refusal names its reason: {stdout}{stderr}"
    );
}

/// Every public command of the supervisor still answers (AC-4: the
/// public surface is preserved, or the migration is documented).
#[tokio::test]
async fn every_public_supervisor_command_still_answers() {
    let controller = Controller::start("surface", true).await;
    let socket = controller.socket.clone();
    wait_for(&socket, "phase: idle").await;

    let (code, stdout, stderr) = capture(&["sup-status", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-status answers: {stdout}{stderr}");
    assert!(stdout.contains("project: cli-project"), "{stdout}");

    let (code, stdout, stderr) = capture(&["sup-action", "start", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-action start answers: {stdout}{stderr}");
    wait_for(&socket, "phase: running").await;

    let (code, stdout, stderr) = capture(&["sup-action", "pause", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-action pause answers: {stdout}{stderr}");
    assert!(stdout.contains("applied"), "{stdout}");

    let (code, stdout, stderr) = capture(&["sup-action", "resume", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-action resume answers: {stdout}{stderr}");
    assert!(stdout.contains("applied"), "{stdout}");

    let (code, stdout, stderr) = capture(&["sup-events", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-events answers: {stdout}{stderr}");
    assert!(stdout.contains("supervisor/action"), "{stdout}");

    let (code, stdout, stderr) = capture(&[
        "attach-terminal",
        "--socket",
        &socket,
        "--exit-after-events",
        "1",
        "--deadline-seconds",
        "30",
    ])
    .await;
    assert_eq!(code, 0, "attach-terminal answers: {stdout}{stderr}");
    assert!(stdout.contains("[viewer]"), "{stdout}");

    let (code, stdout, stderr) = capture(&["tui", "--socket", &socket, "--frames", "2"]).await;
    assert_eq!(code, 0, "tui answers: {stdout}{stderr}");
    assert!(
        stdout.contains("phase:"),
        "the TUI renders the state: {stdout}"
    );

    let (code, stdout, stderr) = capture(&["sup-action", "stop", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-action stop answers: {stdout}{stderr}");
    assert!(stdout.contains("applied"), "{stdout}");
}
