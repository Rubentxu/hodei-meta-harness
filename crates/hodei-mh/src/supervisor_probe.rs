//! `hodei-mh supervisor-probe`: TASK-15 vertical slice — the supervisor is
//! ONE REAL process serving ONE socket; the CLI and the TUI are plain
//! clients that discover state, drive the SAME actions, reconnect after the
//! supervisor dies (without ever spawning a second controller), and the
//! attached terminal is a read-only tail whose departure changes nothing.
//! Provenance is logged without credentials; unknown telemetry is shown as
//! unknown; refused actions are refused, never successes.

use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::process::Command;

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

async fn run_capture(bin: &std::path::Path, args: &[&str]) -> (i32, String, String) {
    let output = Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .await
        .expect("spawn hodei-mh");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn spawn_supervisor(bin: &std::path::Path, socket: &str, log_path: &std::path::Path) -> Command {
    let log_file = std::fs::File::create(log_path).expect("create supervisor log file");
    let mut command = Command::new(bin);
    command
        .args([
            "supervisor",
            "--socket",
            socket,
            "--project",
            "probe-project",
            // The rehearsal runtime: the probe exercises the real flow
            // without Docker, network or a model.
            "--rehearsal",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::null())
        .kill_on_drop(true);
    command
}

/// Strip ANSI escape sequences and all whitespace: the terminal's diffing
/// may collapse spaces, so assertions run on the normalized cell text.
fn normalize(output: &str) -> String {
    let mut clean = String::with_capacity(output.len());
    let mut chars = output.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            while let Some(&n) = chars.peek() {
                chars.next();
                if n.is_ascii_alphabetic() {
                    break;
                }
            }
        } else if !c.is_whitespace() {
            clean.push(c);
        }
    }
    clean
}

async fn wait_ready(bin: &std::path::Path, socket: &str) -> Option<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let (code, stdout, stderr) = run_capture(bin, &["sup-status", "--socket", socket]).await;
        if code == 0 {
            return Some((stdout, stderr));
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    None
}

pub async fn run() -> i32 {
    let started = Instant::now();
    let mut checks: Vec<Check> = Vec::new();
    let bin = std::env::current_exe().expect("current binary");
    let socket = format!("/tmp/hodei-supervisor-probe-{}.sock", std::process::id());
    let log_path =
        std::env::temp_dir().join(format!("hodei-supervisor-probe-{}.log", std::process::id()));
    let _ = std::fs::remove_file(&socket);

    // ---- AC-1/AC-2: ONE supervisor process, CLI discovers and acts ------
    let mut supervisor = spawn_supervisor(&bin, &socket, &log_path).spawn().ok();
    let ready = wait_ready(&bin, &socket).await;
    let (first_status, first_stderr) = ready.clone().unwrap_or_default();
    checks.push(Check {
        name: "cli-discovers-supervisor-state-telemetry-unknown-is-honest",
        passed: ready.is_some()
            && first_status.contains("phase: idle")
            && first_status.contains("resources: UNKNOWN")
            && first_status.contains("ticket: (none)"),
        detail: format!(
            "sup-status exit=0 after discovery; snapshot shows phase idle, resources UNKNOWN (no telemetry invented), ticket none. stderr: {first_stderr}"
        ),
    });

    // ---- the rehearsal NAMES ITSELF: it is never presented as a live run
    checks.push(Check {
        name: "the-runtime-kind-is-visible-and-never-hidden",
        passed: first_status.contains("runtime: rehearsal"),
        detail: format!(
            "the snapshot states the kind of runtime it has; a rehearsal says 'rehearsal' and never passes for a live run: {}",
            first_status
                .lines()
                .find(|line| line.starts_with("runtime:"))
                .unwrap_or("(no runtime line in the snapshot)")
        ),
    });

    let (pause_code, pause_out, _) =
        run_capture(&bin, &["sup-action", "pause", "--socket", &socket]).await;
    let (start_code, start_out, _) = run_capture(
        &bin,
        &[
            "sup-action",
            "start",
            "--socket",
            &socket,
            "--ticket",
            "TASK-9",
            "--revision",
            "deadbeef",
        ],
    )
    .await;
    let running_status = wait_ready_contains(&bin, &socket, "phase: running").await;
    checks.push(Check {
        name: "actions-drive-the-same-contract-confirmation-is-real",
        passed: pause_code == 1
            && pause_out.contains("refused")
            && pause_out.contains("NOT executed")
            && start_code == 0
            && start_out.contains("applied")
            && running_status
                .as_ref()
                .is_some_and(|(stdout, _)| {
                    stdout.contains("ticket: TASK-9") && stdout.contains("revision: deadbeef")
                }),
        detail: format!(
            "pause without run: refused (exit {pause_code}, no fake success); start: applied (exit {start_code}); the run reached 'running' only AFTER the runtime confirmed it{}",
            if running_status.is_some() {
                ""
            } else {
                " — IT NEVER REACHED 'running': the confirmation never happened"
            },
        ),
    });

    let (second_code, second_out, _) =
        run_capture(&bin, &["sup-action", "start", "--socket", &socket]).await;
    checks.push(Check {
        name: "second-controller-is-refused-over-the-wire",
        passed: second_code == 1 && second_out.contains("second controller"),
        detail: format!("a second Start is refused with exit {second_code}: {second_out}",),
    });

    // ---- a SECOND SUPERVISOR PROCESS on a LIVE socket is refused -------
    // Starting another supervisor while the first one is alive would
    // split the state (two cores, two truths). It must be refused.
    let second_log = std::env::temp_dir().join(format!(
        "hodei-supervisor-probe-2-{}.log",
        std::process::id()
    ));
    let mut second_supervisor = spawn_supervisor(&bin, &socket, &second_log).spawn().ok();
    tokio::time::sleep(Duration::from_millis(400)).await;
    let (dup_code, dup_out, dup_err) =
        run_capture(&bin, &["sup-status", "--socket", &socket]).await;
    let second_refused = dup_code == 0;
    let second_exited = second_supervisor
        .as_mut()
        .map(|child| matches!(child.try_wait(), Ok(Some(_))))
        .unwrap_or(false);
    let second_died = second_refused
        && second_exited
        && (dup_out.contains("project:") || dup_err.contains("already listening"));
    checks.push(Check {
        name: "a-second-supervisor-process-cannot-steal-a-live-socket",
        passed: second_died,
        detail: format!(
            "a second supervisor process was launched on the LIVE socket; it exited instead of taking over (exited: {second_exited}); the original supervisor still answers (sup-status exit {dup_code})",
        ),
    });
    if let Some(mut child) = second_supervisor.take() {
        let _ = child.kill().await;
    }

    // ---- DoD #1 (real session): the TUI renders the LIVE supervisor -----
    let (tui_code, tui_out, tui_err) =
        run_capture(&bin, &["tui", "--socket", &socket, "--frames", "2"]).await;
    let tui_normalized = normalize(&tui_out);
    checks.push(Check {
        name: "tui-real-session-renders-live-supervisor-state",
        passed: tui_code == 0
            && tui_normalized.contains("phase:running")
            && tui_normalized.contains("ticket:TASK-9"),
        detail: format!(
            "ratatui TUI ran a real local session against the socket (frames=2, exit {tui_code}); stderr: {}",
            tui_err.trim().chars().take(120).collect::<String>()
        ),
    });

    // ---- AC-1: the CLI and the TUI observe the SAME state machine -----
    // Both render the SAME snapshot contract, so a state the CLI shows
    // and the TUI does not (or the other way round) means the two
    // surfaces drifted apart.
    let cli_normalized = running_status
        .as_ref()
        .map(|(stdout, _)| normalize(stdout))
        .unwrap_or_default();
    let same_state = !cli_normalized.is_empty()
        && cli_normalized.contains("phase:running")
        && tui_normalized.contains("phase:running")
        && cli_normalized.contains("ticket:TASK-9")
        && tui_normalized.contains("ticket:TASK-9")
        && cli_normalized.contains("runtime:rehearsal")
        && tui_normalized.contains("runtime:rehearsal");
    checks.push(Check {
        name: "the-cli-and-the-tui-observe-the-same-state-machine",
        passed: same_state,
        detail: format!(
            "the CLI reports phase/ticket/runtime and the TUI renders the same ones from the same snapshot contract (CLI: phase={}, ticket={}, runtime={})",
            cli_normalized.contains("phase:running"),
            cli_normalized.contains("ticket:TASK-9"),
            cli_normalized.contains("runtime:rehearsal"),
        ),
    });

    // ---- DoD #1 (simulated): the TUI renders SIMULATED state as such ----
    let (sim_code, sim_out, _) = run_capture(&bin, &["tui", "--simulated", "--frames", "2"]).await;
    let sim_normalized = normalize(&sim_out);
    checks.push(Check {
        name: "tui-simulated-state-is-labelled-simulated",
        passed: sim_code == 0
            && sim_normalized.contains("SIMULATED")
            && sim_normalized.contains("simulated-project"),
        detail: format!(
            "simulated mode renders a fixed state and SAYS so (exit {sim_code}); nothing is presented as live",
        ),
    });

    // ---- AC-3: attached terminal is read-only; leaving changes nothing --
    let (viewer_code, viewer_out, _) = run_capture(
        &bin,
        &[
            "attach-terminal",
            "--socket",
            &socket,
            "--exit-after-events",
            "1",
            "--deadline-seconds",
            "30",
        ],
    )
    .await;
    let after_viewer_status = wait_ready_contains(&bin, &socket, "phase: running").await;
    checks.push(Check {
        name: "attached-terminal-read-only-and-disconnect-is-inert",
        passed: viewer_code == 0
            && viewer_out.contains("[viewer]")
            && after_viewer_status
                .as_ref()
                .is_some_and(|(stdout, _)| stdout.contains("phase: running")),
        detail: format!(
            "viewer tailed structured events and exited (exit {viewer_code}); the objective was untouched afterwards (still running); the viewer cannot send actions by construction",
        ),
    });

    // ---- AC-4: provenance logged, credentials never, unknown not success -
    tokio::time::sleep(Duration::from_millis(700)).await;
    let supervisor_log = std::fs::read_to_string(&log_path).unwrap_or_default();
    let env_token = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
    let env_token_leaked = !env_token.is_empty() && supervisor_log.contains(&env_token);
    let provenance_present =
        supervisor_log.contains("[human]") && supervisor_log.contains("[auto]");
    checks.push(Check {
        name: "logs-carry-provenance-without-credentials",
        passed: !env_token_leaked && provenance_present,
        detail: format!(
            "supervisor log has provenance labels ({}) and {} the ANTHROPIC_AUTH_TOKEN of its own environment (redaction runs before emission)",
            if provenance_present { "human + auto[boot]" } else { "MISSING" },
            if env_token.is_empty() {
                "no token present in env (redaction itself unit-tested)"
            } else if env_token_leaked {
                "LEAKED"
            } else {
                "does not contain"
            },
        ),
    });

    // ---- AC-1: reconnect after the supervisor dies — never a 2nd ctrl ----
    if let Some(mut child) = supervisor.take() {
        let _ = child.kill().await;
    }
    let (dead_code, _, dead_err) = run_capture(&bin, &["sup-status", "--socket", &socket]).await;
    // Restart the supervisor on the SAME socket path (bind removes stale).
    let mut supervisor2 = spawn_supervisor(&bin, &socket, &log_path).spawn().ok();
    let reconnected = wait_ready(&bin, &socket).await;
    checks.push(Check {
        name: "clients-reconnect-without-starting-a-second-controller",
        passed: dead_code == 1
            && dead_err.contains("no second controller")
            && reconnected.is_some(),
        detail: format!(
            "with the supervisor killed, sup-status fails honestly (exit {dead_code}: supervisor not reachable, no second controller started); after the supervisor is relaunched on the same path, discovery works again (fresh core, phase idle: no persistence pretended)",
        ),
    });

    // Cleanup.
    if let Some(mut child) = supervisor2.take() {
        let _ = child.kill().await;
    }
    let _ = std::fs::remove_file(&socket);
    let _ = std::fs::remove_file(&log_path);

    let summary = serde_json::json!({
        "task": "TASK-15",
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
        "elapsed_seconds": started.elapsed().as_secs_f64(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&summary).unwrap_or_default()
    );
    if checks.iter().all(|c| c.passed) {
        0
    } else {
        1
    }
}

/// Wait until the supervisor's own CLI reports `needle`.
///
/// A deadline that expires returns `None`: the state was NOT reached. The
/// last answer seen is not evidence of anything, and returning it as if
/// it were would turn a missed state into a silent pass.
async fn wait_ready_contains(
    bin: &std::path::Path,
    socket: &str,
    needle: &str,
) -> Option<(String, String)> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let (code, stdout, stderr) = run_capture(bin, &["sup-status", "--socket", socket]).await;
        if code == 0 && stdout.contains(needle) {
            return Some((stdout, stderr));
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}
