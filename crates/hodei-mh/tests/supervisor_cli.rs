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
    std::env::temp_dir()
        .join(format!(
            "hodei-cli-{tag}-{}-{}.sock",
            std::process::id(),
            Instant::now().elapsed().as_nanos()
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

/// The controller under test. It is killed when the test ends, however
/// it ends: a controller left running would own its socket forever.
struct Controller {
    child: Child,
    socket: String,
}

impl Controller {
    async fn start(tag: &str) -> Self {
        let socket = socket_path(tag);
        let child = Command::new(BIN)
            .args([
                "supervisor",
                "--socket",
                &socket,
                "--project",
                "cli-project",
                "--rehearsal",
            ])
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
    let controller = Controller::start("flow").await;
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

    // And the rehearsal closed nothing: the objective says so.
    let (_, objective_out, _) = capture(&["sup-events", "--socket", &socket]).await;
    assert!(
        objective_out.contains("nothing was closed"),
        "the rehearsal publishes nothing: {objective_out}"
    );
}

#[tokio::test]
async fn a_controller_without_a_runtime_refuses_to_start_instead_of_pretending() {
    // Without `--rehearsal` there is NO runtime: the real ACP runtime
    // arrives with the ACP slice. The controller must refuse the Start
    // rather than pretend a run began.
    let socket = socket_path("noruntime");
    let child = Command::new(BIN)
        .args([
            "supervisor",
            "--socket",
            &socket,
            "--project",
            "cli-project",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn the supervisor");
    let controller = Controller {
        child,
        socket: socket.clone(),
    };

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
    drop(controller);
}
