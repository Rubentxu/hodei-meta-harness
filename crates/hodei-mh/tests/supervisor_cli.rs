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
///
/// The needle is a piece of the READABLE tail, and a captured stdout is
/// a pipe, so this client names the channel it reads instead of taking
/// the machine-readable default.
async fn wait_for_event(socket: &str, needle: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let (code, stdout, stderr) = capture(&["sup-events", "--socket", socket, "--pretty"]).await;
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
        // The project is the IDENTITY of the controller: one per project.
        // The tests run concurrently, so each one owns its own.
        let project = format!("cli-{tag}");
        command.args(["supervisor", "--socket", &socket, "--project", &project]);
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
    // The confirmation event IS that transition, recorded durably: a
    // chained rehearsal settles as soon as its scenario runs out of new
    // work, so the test reads the log and the stable fields of the
    // snapshot — never a transient phase it could race.
    let confirmed = wait_for_event(&socket, "objective/confirmed").await;
    assert!(
        confirmed.contains("runtime=rehearsal"),
        "only the runtime the snapshot names can confirm the objective: {confirmed}"
    );
    let status = wait_for(&socket, "ticket: TASK-9").await;
    assert!(
        status.contains("runtime: rehearsal"),
        "a rehearsal never passes for a live run: {status}"
    );

    // And the rehearsal closed nothing: the objective says so. The
    // verdict is recorded AFTER the confirmation, so the test waits for
    // the event instead of racing it.
    let events = wait_for_event(&socket, "objective/outcome").await;
    assert!(
        events.contains("nothing was closed"),
        "the rehearsal publishes nothing: {events}"
    );

    // The chain settles BY ITSELF when nothing new is eligible: the
    // named work ran first, and no Stop was needed to end the session.
    let settled = wait_for_event(&socket, "outcome=idle").await;
    assert!(
        settled.contains("work_ref=TASK-9"),
        "the named work is the one that ran: {settled}"
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
    assert!(stdout.contains("project: cli-surface"), "{stdout}");

    let (code, stdout, stderr) = capture(&["sup-action", "start", "--socket", &socket]).await;
    assert_eq!(code, 0, "sup-action start answers: {stdout}{stderr}");

    // The rehearsal chains until its scenario runs out of NEW work, and
    // the chain then settles the session BY ITSELF: the log carries the
    // verdict, and no Stop was needed to free the session.
    let events = wait_for_event(&socket, "outcome=idle").await;
    assert!(
        events.contains("objective/outcome"),
        "the chained objective is in the log: {events}"
    );

    // PAUSE and RESUME still answer: the chain settled the run, so they
    // are refused with the reason, never silently dropped.
    let (code, stdout, stderr) = capture(&["sup-action", "pause", "--socket", &socket]).await;
    assert_eq!(code, 1, "sup-action pause answers: {stdout}{stderr}");
    assert!(stdout.contains("refused"), "{stdout}");

    let (code, stdout, stderr) = capture(&["sup-action", "resume", "--socket", &socket]).await;
    assert_eq!(code, 1, "sup-action resume answers: {stdout}{stderr}");
    assert!(stdout.contains("refused"), "{stdout}");

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
    assert_eq!(code, 1, "sup-action stop answers: {stdout}{stderr}");
    assert!(stdout.contains("refused"), "{stdout}");

    // A settled run starts again: the chain ending idle freed the
    // session, so the next objective is applied without a Stop.
    let (code, stdout, stderr) = capture(&["sup-action", "start", "--socket", &socket]).await;
    assert_eq!(code, 0, "the next start is applied: {stdout}{stderr}");
    assert!(stdout.contains("applied"), "{stdout}");
}

/// A temporary directory of this test's own.
fn temp_dir(tag: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("hodei-live-{tag}-{}-{nanos}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the temporary directory");
    dir
}

/// A git repository with one commit on `main`.
fn git_fixture(tag: &str) -> std::path::PathBuf {
    let dir = temp_dir(tag);
    hodei_mh_workspace::git::init_fixture_repo(&dir).expect("a fixture repository");
    for (key, value) in [
        ("user.name", "fixture"),
        ("user.email", "fixture@example.invalid"),
    ] {
        let configured = std::process::Command::new("git")
            .args(["-C", dir.to_string_lossy().as_ref(), "config", key, value])
            .output()
            .expect("git config");
        assert!(
            configured.status.success(),
            "git config {key}: {configured:?}"
        );
    }
    std::fs::write(dir.join("README.md"), "fixture\n").expect("the fixture file");
    let added = std::process::Command::new("git")
        .args(["-C", dir.to_string_lossy().as_ref(), "add", "README.md"])
        .output()
        .expect("git add");
    assert!(added.status.success(), "git add: {added:?}");
    hodei_mh_workspace::git::commit_index(&dir, "fixture base").expect("the fixture commit");
    dir
}

/// Run one command of the product with an explicit agent token.
async fn capture_with_token(args: &[&str], token: &str) -> (i32, String, String) {
    let output = Command::new(BIN)
        .args(args)
        .env("ANTHROPIC_AUTH_TOKEN", token)
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

/// The live flags of a boot, with the given directories.
fn live_flags<'a>(
    socket: &'a str,
    project_dir: &'a str,
    tracker_dir: &'a str,
    managed_branch: &'a str,
) -> Vec<&'a str> {
    vec![
        "supervisor",
        "--socket",
        socket,
        "--project",
        "cli-live",
        "--live",
        "--server",
        "http://127.0.0.1:8081",
        "--image",
        "example/image:1",
        "--project-dir",
        project_dir,
        "--tracker-dir",
        tracker_dir,
        "--managed-branch",
        managed_branch,
    ]
}

#[tokio::test]
async fn live_and_rehearsal_together_are_refused() {
    let socket = socket_path("live-exclusive");
    let project = temp_dir("exclusive-project");
    let project = project.to_string_lossy().to_string();
    let mut flags = live_flags(&socket, &project, &project, "main");
    flags.push("--rehearsal");
    let (code, _stdout, stderr) = capture_with_token(&flags, "test-token").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("--live and --rehearsal are exclusive"),
        "the refusal names the conflict: {stderr}"
    );
}

#[tokio::test]
async fn a_live_boot_without_a_required_flag_names_it() {
    let socket = socket_path("live-missing");
    let flags = [
        "supervisor",
        "--socket",
        &socket,
        "--project",
        "cli-live",
        "--live",
        "--server",
        "http://127.0.0.1:8081",
        "--image",
        "example/image:1",
    ];
    let (code, _stdout, stderr) = capture_with_token(&flags, "test-token").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("--project-dir is required with --live"),
        "the refusal names the missing flag: {stderr}"
    );
}

#[tokio::test]
async fn a_live_boot_without_credentials_is_refused() {
    let socket = socket_path("live-token");
    let project = temp_dir("token-project");
    let project = project.to_string_lossy().to_string();
    let flags = live_flags(&socket, &project, &project, "main");
    let (code, _stdout, stderr) = capture_with_token(&flags, "").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("ANTHROPIC_AUTH_TOKEN is empty"),
        "the refusal names the credential: {stderr}"
    );
}

#[tokio::test]
async fn a_live_boot_in_a_directory_that_is_not_a_repository_is_refused() {
    let socket = socket_path("live-repo");
    let project = temp_dir("repo-project");
    let project = project.to_string_lossy().to_string();
    let flags = live_flags(&socket, &project, &project, "main");
    let (code, _stdout, stderr) = capture_with_token(&flags, "test-token").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("is not a git repository"),
        "the refusal names the repository: {stderr}"
    );
}

#[tokio::test]
async fn a_live_boot_without_the_managed_branch_is_refused() {
    let socket = socket_path("live-branch");
    let project = git_fixture("branch-project");
    let project = project.to_string_lossy().to_string();
    let flags = live_flags(&socket, &project, &project, "hodei-mh/main");
    let (code, _stdout, stderr) = capture_with_token(&flags, "test-token").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("managed branch 'hodei-mh/main' does not exist"),
        "the refusal names the branch: {stderr}"
    );
}

#[tokio::test]
async fn a_live_boot_with_a_tracker_that_does_not_answer_is_refused() {
    let socket = socket_path("live-tracker");
    let project = git_fixture("tracker-project");
    let project = project.to_string_lossy().to_string();
    let tracker = temp_dir("tracker-empty");
    let tracker = tracker.to_string_lossy().to_string();
    let flags = live_flags(&socket, &project, &tracker, "main");
    let (code, _stdout, stderr) = capture_with_token(&flags, "test-token").await;
    assert_eq!(code, 1, "{stderr}");
    assert!(
        stderr.contains("the tracker did not answer"),
        "the refusal names the tracker: {stderr}"
    );
}

#[tokio::test]
async fn an_objective_that_also_names_a_ticket_never_reaches_the_supervisor() {
    // The real binary, an unreachable socket: the refusal must happen
    // while parsing, before any connection — a contradictory request is
    // never sent, and its two halves are never split into two actions.
    let (code, _stdout, stderr) = capture(&[
        "sup-action",
        "start",
        "--socket",
        "/unused-by-this-test.sock",
        "--objective",
        "summarise the docs",
        "--accept",
        "test -f SUMMARY.md",
        "--ticket",
        "TASK-9",
    ])
    .await;
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("not both"),
        "the refusal names the conflict: {stderr}"
    );
    assert!(
        !stderr.contains("Connection refused"),
        "the malformed request is refused before the socket is touched: {stderr}"
    );
}

#[tokio::test]
async fn an_objective_without_a_criterion_names_the_flag_it_needs() {
    let (code, _stdout, stderr) = capture(&[
        "sup-action",
        "start",
        "--socket",
        "/unused-by-this-test.sock",
        "--objective",
        "summarise the docs",
    ])
    .await;
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("--accept"),
        "the refusal names the missing flag: {stderr}"
    );
    assert!(
        !stderr.contains("Connection refused"),
        "the incomplete request is refused before the socket is touched: {stderr}"
    );
}

/// The event log, read by a script: every line one JSON object.
///
/// A captured stdout IS a pipe, so this is the channel the probes and the
/// UAT oracles get without naming a flag: what they parse is the event as
/// the socket carries it, never a rendering of it.
#[tokio::test]
async fn events_reach_a_pipe_as_one_json_object_per_line() {
    let controller = Controller::start("events-jsonl", true).await;
    let socket = controller.socket.clone();
    wait_for(&socket, "phase:").await;

    let (code, stdout, stderr) =
        capture(&["sup-events", "--socket", &socket, "--since", "0"]).await;
    assert_eq!(code, 0, "the machine channel answers: {stdout}{stderr}");
    let lines: Vec<&str> = stdout.lines().collect();
    assert!(!lines.is_empty(), "the boot event is in the log");
    for line in &lines {
        let read: serde_json::Value =
            serde_json::from_str(line).unwrap_or_else(|error| panic!("not JSON: {line} ({error})"));
        assert!(read["kind"].is_string(), "the kind is a field: {line}");
        assert!(
            read.get("payload").is_some() && read.get("provenance").is_some(),
            "the line carries the whole event the socket carries: {line}"
        );
    }
    let boot = lines
        .iter()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find(|event| event["kind"] == "supervisor/boot")
        .expect("the boot event reaches the machine channel");
    assert_eq!(boot["seq"], 1, "the sequence travels as a field");
    assert!(
        boot["provenance"]["prov"] == "automatic",
        "the provenance travels as a field: {boot}"
    );
}

/// The readable tail is still the tail a person reads, and it is still
/// what `--pretty` forces when stdout is not a terminal.
#[tokio::test]
async fn the_pretty_flag_answers_the_readable_tail_on_a_pipe() {
    let controller = Controller::start("events-pretty", true).await;
    let socket = controller.socket.clone();
    wait_for(&socket, "phase:").await;

    let (code, stdout, stderr) = capture(&["sup-events", "--socket", &socket, "--pretty"]).await;
    assert_eq!(code, 0, "the readable channel answers: {stdout}{stderr}");
    let first = stdout
        .lines()
        .next()
        .expect("the boot event is printed")
        .to_string();
    assert!(
        first.starts_with("[auto] "),
        "the tag prefixes the line: {first}"
    );
    assert!(
        first.contains("supervisor/boot"),
        "the kind is named: {first}"
    );
    assert!(
        serde_json::from_str::<serde_json::Value>(&first).is_err(),
        "the readable tail is not the machine channel: {first}"
    );
}

/// The two channels are exclusive, and the clash is refused before the
/// socket is touched: a contradictory request is never sent.
#[tokio::test]
async fn the_two_event_channels_asked_for_together_are_refused() {
    let (code, _stdout, stderr) = capture(&[
        "sup-events",
        "--socket",
        "/unused-by-this-test.sock",
        "--jsonl",
        "--pretty",
    ])
    .await;
    assert_eq!(code, 2, "{stderr}");
    assert!(
        stderr.contains("--jsonl and --pretty are exclusive"),
        "the refusal names both flags: {stderr}"
    );
    assert!(
        !stderr.contains("Connection refused"),
        "the contradictory request is refused before the socket is touched: {stderr}"
    );
}
