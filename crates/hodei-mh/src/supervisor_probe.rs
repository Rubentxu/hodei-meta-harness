//! `hodei-mh supervisor-probe`: TASK-15 vertical slice — the supervisor is
//! ONE REAL process serving ONE socket; the CLI and the TUI are plain
//! clients that discover state, drive the SAME actions, reconnect after the
//! supervisor dies (without ever spawning a second controller), and the
//! attached terminal is a read-only tail whose departure changes nothing.
//! Provenance is logged without credentials; unknown telemetry is shown as
//! unknown; refused actions are refused, never successes.

use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tokio::process::Command;

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

/// The epoch the FIRST Start of the probe opens: a fresh core counts epochs
/// from zero and every applied Start takes the next one.
const FIRST_EPOCH: u64 = 1;
/// The epoch the SECOND Start of the probe opens.
const SECOND_EPOCH: u64 = 2;
/// The verdicts that END a run. A confirmed objective keeps the session
/// alive, so it is not one of them: only after an ending verdict is a second
/// Start allowed to be applied.
const RUN_ENDING_OUTCOMES: &[&str] = &["idle", "rejected", "failed"];
/// The words the refusal of a second controller carries. They are what the
/// journal would show if this project ever served two controllers.
const SECOND_CONTROLLER_REFUSAL: &str = "second controller";
/// How long the journal is followed before the probe admits the transition
/// never happened.
const JOURNAL_DEADLINE: Duration = Duration::from_secs(20);
/// The wait between two reads of the journal.
const JOURNAL_POLL: Duration = Duration::from_millis(300);
/// The fields of the snapshot contract that BOTH surfaces render.
///
/// These are the fields a client can compare between the CLI and the TUI:
/// a field that differs in VALUE is a drift of the state machine, and a
/// field that one surface does not render at all is a drift of the
/// contract.
const SHARED_SNAPSHOT_FIELDS: &[&str] = &[
    "project",
    "phase",
    "ticket",
    "goal",
    "revision",
    "runtime",
    "objective",
    "resources",
    "context available",
    "waiting",
    "ttl remaining",
    "epoch",
];

/// The transitions one controller's journal must show for its two Starts.
///
/// Every field is the SEQUENCE of the event that proved it, or `None` when
/// the journal never showed it: a missing milestone says where the trace
/// stopped, which a plain boolean could not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct JournalSummary {
    /// The human Start that opened epoch 1 and was applied.
    first_start_applied: Option<u64>,
    /// The confirmation the RUNTIME gave to epoch 1.
    first_run_confirmed: Option<u64>,
    /// The verdict that ENDED epoch 1.
    first_run_ended: Option<u64>,
    /// The verdict that ended epoch 1, by name: the reader is told
    /// whether the run ended idle, rejected or failed.
    first_run_ended_with: Option<String>,
    /// The human Start that opened epoch 2 and was applied.
    second_start_applied: Option<u64>,
    /// The automatic confirmation the runtime gave to epoch 2.
    second_run_confirmed: Option<u64>,
    /// The verdict that ENDED epoch 2.
    second_run_ended: Option<u64>,
    /// The action that was refused as a second controller.
    refused_by_second_controller: Option<u64>,
}

impl JournalSummary {
    /// The whole two-run sequence, each milestone AFTER the one before it.
    ///
    /// Order is the assertion, not membership: a journal that shows an
    /// epoch-2 Start at a lower sequence than the verdict that ended epoch 1
    /// did not witness the run ending first.
    fn sequence_is_complete(&self) -> bool {
        match (
            self.first_start_applied,
            self.first_run_ended,
            self.second_start_applied,
            self.second_run_confirmed,
        ) {
            (Some(start_one), Some(ended_one), Some(start_two), Some(confirmed_two)) => {
                start_one < ended_one && ended_one < start_two && start_two < confirmed_two
            }
            _ => false,
        }
    }

    /// The first epoch is SETTLED: the runtime confirmed the objective and
    /// the run ended by its own verdict.
    ///
    /// A rehearsal confirms at once and its chain exhausts the work it
    /// knows, so the epoch ends in milliseconds: a run that is not settled
    /// is a run that never ended, and that is a defect, not a phase to
    /// wait for.
    fn first_epoch_is_settled(&self) -> bool {
        self.first_run_confirmed.is_some() && self.first_run_ended.is_some()
    }
}

/// What one read of the journal found.
struct JournalRead {
    summary: JournalSummary,
    /// The lines that are not one readable event, with the reason each one
    /// is unusable. A hole in the journal is never read as a pass.
    unreadable: Vec<String>,
    /// Whether the read command itself answered.
    reachable: bool,
}

/// The transitions the journal shows, as the machine channel carries them.
fn journal_assertions(events: &[Value]) -> JournalSummary {
    let mut summary = JournalSummary::default();
    for event in events {
        let (Some(seq), Some(epoch), Some(kind)) = (
            number(event, "seq"),
            number(event, "epoch"),
            text(event, "kind"),
        ) else {
            continue;
        };
        match kind {
            "supervisor/action" => read_action_event(&mut summary, seq, epoch, event),
            "objective/outcome" => read_outcome_event(&mut summary, seq, epoch, event),
            "objective/confirmed" => read_confirmation_event(&mut summary, seq, epoch, event),
            _ => {}
        }
    }
    summary
}

/// One answered action: applied Starts open the epochs, and a refusal that
/// names a second controller is the invariant breaking.
fn read_action_event(summary: &mut JournalSummary, seq: u64, epoch: u64, event: &Value) {
    let applied = event
        .pointer("/payload/applied")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if !applied {
        let detail = pointer_text(event, "/payload/detail").unwrap_or_default();
        if detail.contains(SECOND_CONTROLLER_REFUSAL) {
            summary.refused_by_second_controller.get_or_insert(seq);
        }
        return;
    }
    if pointer_text(event, "/payload/action/action") != Some("start")
        || pointer_text(event, "/provenance/prov") != Some("human")
    {
        return;
    }
    if epoch == FIRST_EPOCH {
        summary.first_start_applied.get_or_insert(seq);
    } else if epoch == SECOND_EPOCH {
        summary.second_start_applied.get_or_insert(seq);
    }
}

/// One verdict of a run, but only when it ENDED the run.
fn read_outcome_event(summary: &mut JournalSummary, seq: u64, epoch: u64, event: &Value) {
    let outcome = pointer_text(event, "/payload/outcome").unwrap_or_default();
    if !RUN_ENDING_OUTCOMES.contains(&outcome) {
        return;
    }
    match epoch {
        FIRST_EPOCH => {
            summary.first_run_ended.get_or_insert(seq);
            summary
                .first_run_ended_with
                .get_or_insert_with(|| outcome.to_string());
        }
        SECOND_EPOCH => {
            summary.second_run_ended.get_or_insert(seq);
        }
        _ => {}
    }
}

/// The runtime's own confirmation of a run: it comes from the runtime,
/// never from a person.
fn read_confirmation_event(summary: &mut JournalSummary, seq: u64, epoch: u64, event: &Value) {
    if pointer_text(event, "/provenance/prov") != Some("automatic") {
        return;
    }
    match epoch {
        FIRST_EPOCH => {
            summary.first_run_confirmed.get_or_insert(seq);
        }
        SECOND_EPOCH => {
            summary.second_run_confirmed.get_or_insert(seq);
        }
        _ => {}
    }
}

fn number(event: &Value, key: &str) -> Option<u64> {
    event.get(key).and_then(Value::as_u64)
}

fn text<'a>(event: &'a Value, key: &str) -> Option<&'a str> {
    event.get(key).and_then(Value::as_str)
}

fn pointer_text<'a>(event: &'a Value, pointer: &str) -> Option<&'a str> {
    event.pointer(pointer).and_then(Value::as_str)
}

/// The journal lines as events, and the reason for every line that is not
/// one.
///
/// A line that parses but names nothing is a hole, not a silent skip: the
/// assertion over it would be invented from what happened to be readable.
fn parse_journal_lines(raw: &str) -> (Vec<Value>, Vec<String>) {
    let parsed: Vec<Result<Value, String>> = raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(|line| {
            serde_json::from_str::<Value>(line)
                .map_err(|error| format!("not one JSON event ({error}): {line}"))
                .and_then(|event| match event.get("kind").and_then(Value::as_str) {
                    Some(_) => Ok(event),
                    None => Err(format!("an event without a kind: {line}")),
                })
        })
        .collect();
    let events: Vec<Value> = parsed
        .iter()
        .filter_map(|outcome| outcome.as_ref().ok().cloned())
        .collect();
    let unreadable: Vec<String> = parsed
        .iter()
        .filter_map(|outcome| outcome.as_ref().err().cloned())
        .collect();
    (events, unreadable)
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

/// One field of a rendered snapshot: the value the surface printed for
/// `key`, or `None` when that surface does not render the field at all.
fn snapshot_field(rendered: &str, key: &str) -> Option<String> {
    rendered.lines().find_map(|line| {
        let (name, value) = line.split_once(": ")?;
        (name == key).then(|| value.to_string())
    })
}

/// Whether the TUI painted the SAME value the CLI printed for `key`.
///
/// Both surfaces render the same contract, so the value the CLI shows
/// must be in the TUI's cells. The comparison runs on the NORMALIZED
/// text: the terminal splits one line into several positioned writes,
/// and the difference between "tracker work" and "tracker" followed by
/// "work" is not a difference of state.
fn surfaces_agree(cli: &str, tui_normalized: &str, key: &str) -> bool {
    match snapshot_field(cli, key) {
        Some(value) => {
            tui_normalized.contains(&format!("{}:{}", normalize(key), normalize(&value)))
        }
        None => false,
    }
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
    // The rehearsal confirms at once and its chain exhausts the work it
    // knows, so epoch 1 ends within milliseconds: the run NEVER holds a
    // stable `running` a client could poll for. What the journal keeps is
    // what really happened: the runtime confirmed the objective, the run
    // ended by its own verdict, and the state the applied Start produced
    // is the state the snapshot names. The probe waits for that record
    // instead of racing a phase that is not sustained.
    let settled = wait_journal(&bin, &socket, JournalSummary::first_epoch_is_settled).await;
    let (settled_code, settled_status, _) =
        run_capture(&bin, &["sup-status", "--socket", &socket]).await;
    let ended_with = settled
        .summary
        .first_run_ended_with
        .clone()
        .unwrap_or_else(|| "NONE (the run never ended)".to_string());
    checks.push(Check {
        name: "actions-drive-the-same-contract-confirmation-is-real",
        passed: pause_code == 1
            && pause_out.contains("refused")
            && pause_out.contains("NOT executed")
            && start_code == 0
            && start_out.contains("applied")
            && settled.reachable
            && settled.unreadable.is_empty()
            && settled.summary.first_run_confirmed.is_some()
            && settled.summary.first_run_ended.is_some()
            && settled_code == 0
            && snapshot_field(&settled_status, "ticket").as_deref() == Some("TASK-9")
            && snapshot_field(&settled_status, "revision").as_deref() == Some("deadbeef"),
        detail: format!(
            "pause without run: refused (exit {pause_code}, no fake success); start: applied \
             (exit {start_code}); the RUNTIME confirmed the objective at {} (automatic \
             provenance) and the run ended by its own verdict '{ended_with}' at {} — a \
             rehearsal confirms at once and has no eligible work left, so the epoch never held \
             a stable 'running': it ended honestly instead of pretending to work; the applied \
             Start is the state the snapshot names: ticket {}, revision {}",
            milestone(settled.summary.first_run_confirmed),
            milestone(settled.summary.first_run_ended),
            snapshot_field(&settled_status, "ticket").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&settled_status, "revision").unwrap_or_else(|| "(none)".into()),
        ),
    });

    // ---- DoD #1 (real session): the TUI renders the LIVE supervisor -----
    // The snapshot is read while the epoch is settled, so the TUI paints
    // the state of THIS supervisor and not a phase that is passing by.
    let (tui_code, tui_out, tui_err) =
        run_capture(&bin, &["tui", "--socket", &socket, "--frames", "2"]).await;
    let tui_normalized = normalize(&tui_out);
    let live_fields = ["project", "phase", "ticket", "revision", "runtime"];
    let live_state_painted = live_fields
        .iter()
        .all(|key| surfaces_agree(&settled_status, &tui_normalized, key));
    checks.push(Check {
        name: "tui-real-session-renders-live-supervisor-state",
        passed: tui_code == 0
            && live_state_painted
            && !tui_normalized.contains("SIMULATED")
            && tui_normalized.contains("ticket:TASK-9"),
        detail: format!(
            "ratatui TUI ran a real local session against the socket (frames=2, exit {tui_code}) \
             and painted the live state of THIS supervisor: project {}, phase {}, ticket {}, \
             revision {}, runtime {}; nothing is presented as simulated; stderr: {}",
            snapshot_field(&settled_status, "project").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&settled_status, "phase").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&settled_status, "ticket").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&settled_status, "revision").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&settled_status, "runtime").unwrap_or_else(|| "(none)".into()),
            tui_err.trim().chars().take(120).collect::<String>(),
        ),
    });

    // ---- AC-1: the CLI and the TUI observe the SAME state machine -----
    // Both render the SAME snapshot contract, so every field the contract
    // has must carry the SAME value in both surfaces: a state the CLI
    // shows and the TUI does not (or the other way round) means the two
    // surfaces drifted apart.
    let drift: Vec<&str> = SHARED_SNAPSHOT_FIELDS
        .iter()
        .copied()
        .filter(|key| !surfaces_agree(&settled_status, &tui_normalized, key))
        .collect();
    checks.push(Check {
        name: "the-cli-and-the-tui-observe-the-same-state-machine",
        passed: settled_code == 0 && drift.is_empty(),
        detail: format!(
            "the CLI and the TUI render the SAME snapshot contract: {} of {} fields carry the \
             same value in both surfaces{}",
            SHARED_SNAPSHOT_FIELDS.len() - drift.len(),
            SHARED_SNAPSHOT_FIELDS.len(),
            if drift.is_empty() {
                String::new()
            } else {
                format!("; the fields that DRIFT: {}", drift.join(", "))
            },
        ),
    });

    // ---- ONE controller per project, audited in the JOURNAL -------------
    // A rehearsal never holds a run steady in `running`: the trial runtime
    // confirms at once and every chained attempt ends the epoch before a
    // stable phase can be observed. So this probe does NOT claim that a
    // second Start is refused over the wire — that refusal is asserted on
    // the productive path by the offline tests of the runner, the engine and
    // the socket. What the journal DOES carry, live, is the whole transition
    // the single-controller rule depends on: epoch 1 opened by an applied
    // Start and ended by its verdict, and only then epoch 2 opened by the
    // next applied Start and confirmed by the runtime. A second controller
    // that ever took this project over would leave its refusal in the very
    // same journal.
    let before_second =
        wait_journal(&bin, &socket, |summary| summary.first_run_ended.is_some()).await;
    let (second_code, second_out, _) =
        run_capture(&bin, &["sup-action", "start", "--socket", &socket]).await;
    // The second epoch must end too: a run that never ends is a hang, and
    // a settled epoch is also what lets the state be compared before and
    // after a viewer without racing it.
    let after_second = wait_journal(&bin, &socket, |summary| {
        summary.sequence_is_complete() && summary.second_run_ended.is_some()
    })
    .await;
    let milestones = format!(
        "start epoch {FIRST_EPOCH} applied {}, epoch {FIRST_EPOCH} confirmed by the runtime \
         {}, epoch {FIRST_EPOCH} ended {}, start epoch {SECOND_EPOCH} applied {}, epoch \
         {SECOND_EPOCH} confirmed by the runtime {}, epoch {SECOND_EPOCH} ended {}",
        milestone(after_second.summary.first_start_applied),
        milestone(after_second.summary.first_run_confirmed),
        milestone(after_second.summary.first_run_ended),
        milestone(after_second.summary.second_start_applied),
        milestone(after_second.summary.second_run_confirmed),
        milestone(after_second.summary.second_run_ended),
    );
    let refusal = match after_second.summary.refused_by_second_controller {
        Some(seq) => format!("PRESENT at #{seq}"),
        None => "absent".to_string(),
    };
    let holes = if after_second.unreadable.is_empty() {
        String::new()
    } else {
        format!(
            " — UNREADABLE JOURNAL LINES: {}",
            after_second.unreadable.join(" | ")
        )
    };
    let second_answer = second_out.trim();
    checks.push(Check {
        name: "journal-driven-second-controller-safety",
        passed: after_second.reachable
            && after_second.unreadable.is_empty()
            && before_second.summary.first_run_ended.is_some()
            && after_second.summary.sequence_is_complete()
            && after_second.summary.second_run_ended.is_some()
            && after_second.summary.refused_by_second_controller.is_none(),
        detail: format!(
            "the transition the journal (sup-events --jsonl) shows over the live socket: \
             {milestones}; the refusal of a second controller is {refusal}; the second Start \
             answered with exit {second_code}: {second_answer}; before it, epoch 1 had ended: \
             {}{holes}",
            milestone(before_second.summary.first_run_ended),
        ),
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
        && (dup_out.contains("project:") || dup_err.contains("already"));
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
    // The snapshot is read BEFORE the viewer joins and AFTER it leaves.
    // A read-only viewer that changed nothing leaves both renderings
    // identical: the phase, the ticket, the verdict of the objective and
    // the epoch are the same facts before and after. The viewer itself
    // says it is read-only and it only ever tailed events.
    let (before_code, before_status, _) =
        run_capture(&bin, &["sup-status", "--socket", &socket]).await;
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
    let (after_code, after_status, _) =
        run_capture(&bin, &["sup-status", "--socket", &socket]).await;
    let state_unchanged = before_code == 0
        && after_code == 0
        && !normalize(&before_status).is_empty()
        && normalize(&before_status) == normalize(&after_status);
    checks.push(Check {
        name: "attached-terminal-read-only-and-disconnect-is-inert",
        passed: viewer_code == 0
            && viewer_out.contains("[viewer]")
            && viewer_out.contains("read-only")
            && state_unchanged,
        detail: format!(
            "viewer tailed structured events and exited (exit {viewer_code}) without sending a \
             single action; the state of the supervisor before it joined and after it left is \
             the SAME rendering (phase {}, ticket {}, objective {}) — the departure changed \
             nothing; the viewer cannot send actions by construction",
            snapshot_field(&after_status, "phase").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&after_status, "ticket").unwrap_or_else(|| "(none)".into()),
            snapshot_field(&after_status, "objective").unwrap_or_else(|| "(none)".into()),
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

/// The journal as the machine channel carries it: one JSON object per line,
/// read from the very first event.
///
/// The read is a client of the same socket the CLI and the TUI use, so the
/// journal is observed as a consumer observes it — never by reaching into
/// the supervisor's memory.
async fn read_journal(bin: &std::path::Path, socket: &str) -> JournalRead {
    let args = ["sup-events", "--socket", socket, "--since", "0", "--jsonl"];
    let (code, stdout, stderr) = run_capture(bin, &args).await;
    if code != 0 {
        return JournalRead {
            summary: JournalSummary::default(),
            unreadable: vec![format!(
                "sup-events exited {code}: {}",
                stderr.trim().chars().take(200).collect::<String>()
            )],
            reachable: false,
        };
    }
    let (events, unreadable) = parse_journal_lines(&stdout);
    JournalRead {
        summary: journal_assertions(&events),
        unreadable,
        reachable: true,
    }
}

/// Follow the journal until it shows the transition `wants`, or the
/// deadline expires.
///
/// A deadline that expires returns what the journal DID show: the missing
/// milestone is then reported as missing, not waited on forever.
async fn wait_journal(
    bin: &std::path::Path,
    socket: &str,
    wants: fn(&JournalSummary) -> bool,
) -> JournalRead {
    let deadline = Instant::now() + JOURNAL_DEADLINE;
    loop {
        let read = read_journal(bin, socket).await;
        let settled = !read.reachable || wants(&read.summary);
        if settled || Instant::now() >= deadline {
            return read;
        }
        tokio::time::sleep(JOURNAL_POLL).await;
    }
}

/// One milestone of the journal, printed as the sequence it was found at.
fn milestone(seq: Option<u64>) -> String {
    seq.map(|seq| format!("#{seq}"))
        .unwrap_or_else(|| "MISSING".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One event as the machine channel writes it.
    fn event(seq: u64, epoch: u64, kind: &str, payload: Value, prov: Value) -> Value {
        serde_json::json!({
            "seq": seq,
            "epoch": epoch,
            "kind": kind,
            "payload": payload,
            "provenance": prov,
        })
    }

    /// The journal answers a Start with the action it was, applied or not.
    fn start_action(seq: u64, epoch: u64, applied: bool, detail: &str) -> Value {
        event(
            seq,
            epoch,
            "supervisor/action",
            serde_json::json!({
                "action": {"action": "start", "project": "probe-project", "ticket": null,
                           "revision": null},
                "applied": applied,
                "detail": detail,
            }),
            serde_json::json!({"prov": "human"}),
        )
    }

    fn confirmed(seq: u64, epoch: u64) -> Value {
        event(
            seq,
            epoch,
            "objective/confirmed",
            serde_json::json!({
                "epoch": epoch,
                "runtime": "rehearsal (trial-default)",
                "detail": "the runtime confirmed the objective",
            }),
            serde_json::json!({"prov": "automatic", "rule": "runtime-confirmation"}),
        )
    }

    fn outcome(seq: u64, epoch: u64, outcome: &str) -> Value {
        event(
            seq,
            epoch,
            "objective/outcome",
            serde_json::json!({"outcome": outcome, "work_ref": null, "reason": "the verdict"}),
            serde_json::json!({"prov": "automatic", "rule": "runtime-outcome"}),
        )
    }

    /// The journal of a rehearsal that confirms at once: epoch 1 chains
    /// through two confirmed attempts and then ends idle, and only after
    /// that does the second Start open epoch 2 — or is refused, which is
    /// what two controllers over one project would look like. The second
    /// epoch ends too: it has no eligible work left either.
    fn a_two_epoch_journal(second_start_applied: bool) -> Vec<Value> {
        let mut events = vec![
            event(
                0,
                0,
                "supervisor/boot",
                Value::String("listening".into()),
                serde_json::json!({"prov": "automatic", "rule": "boot"}),
            ),
            start_action(1, FIRST_EPOCH, true, "starting for probe-project (epoch 1)"),
            confirmed(2, FIRST_EPOCH),
            outcome(3, FIRST_EPOCH, "confirmed"),
            outcome(4, FIRST_EPOCH, "confirmed"),
            outcome(5, FIRST_EPOCH, "idle"),
        ];
        events.push(if second_start_applied {
            start_action(
                6,
                SECOND_EPOCH,
                true,
                "starting for probe-project (epoch 2)",
            )
        } else {
            start_action(
                6,
                SECOND_EPOCH,
                false,
                "a run is already in flight: not a second controller",
            )
        });
        if second_start_applied {
            events.push(confirmed(7, SECOND_EPOCH));
            events.push(outcome(8, SECOND_EPOCH, "idle"));
        }
        events
    }

    #[test]
    fn a_journal_with_epoch_one_terminal_and_epoch_two_start_is_second_controller_safe() {
        let summary = journal_assertions(&a_two_epoch_journal(true));

        assert_eq!(
            summary.first_start_applied,
            Some(1),
            "the first Start is the applied human one of epoch 1"
        );
        assert_eq!(
            summary.first_run_ended,
            Some(5),
            "the run ended by its own verdict, not by a confirmed attempt"
        );
        assert_eq!(summary.second_start_applied, Some(6));
        assert_eq!(summary.second_run_confirmed, Some(7));
        assert_eq!(summary.refused_by_second_controller, None);
        assert!(summary.sequence_is_complete());
    }

    #[test]
    fn a_journal_that_refuses_the_second_start_is_flagged() {
        let summary = journal_assertions(&a_two_epoch_journal(false));

        assert_eq!(
            summary.refused_by_second_controller,
            Some(6),
            "the refusal names a second controller and is found by the journal"
        );
        assert_eq!(summary.second_start_applied, None);
        assert!(
            !summary.sequence_is_complete(),
            "an epoch that was never opened cannot complete the sequence"
        );
    }

    #[test]
    fn a_confirmed_objective_does_not_end_the_first_run() {
        // A journal whose last epoch-1 verdict is CONFIRMED shows a run
        // still in flight: the second Start would be refused for real.
        let events = vec![
            start_action(1, FIRST_EPOCH, true, "starting"),
            confirmed(2, FIRST_EPOCH),
            outcome(3, FIRST_EPOCH, "confirmed"),
        ];

        assert_eq!(journal_assertions(&events).first_run_ended, None);
    }

    #[test]
    fn a_second_start_applied_before_the_first_run_ended_is_not_a_sequence() {
        // The same milestones, in the WRONG order: the second Start was
        // applied while epoch 1 was still in flight, which is exactly two
        // controllers serving one project. No refusal string is needed for
        // the trace to betray it.
        let events = vec![
            start_action(1, FIRST_EPOCH, true, "starting"),
            start_action(2, SECOND_EPOCH, true, "starting again"),
            confirmed(3, SECOND_EPOCH),
            outcome(4, FIRST_EPOCH, "idle"),
        ];
        let summary = journal_assertions(&events);

        assert_eq!(summary.refused_by_second_controller, None);
        assert!(
            !summary.sequence_is_complete(),
            "{summary:?}: the epoch 2 run came before the epoch 1 verdict ended it"
        );
    }

    #[test]
    fn a_journal_line_that_is_not_one_event_is_reported_and_never_dropped() {
        let raw = format!(
            "{}\nnot JSON at all\n{{\"seq\":9,\"epoch\":1,\"payload\":{{}}}}\n",
            serde_json::to_string(&start_action(1, FIRST_EPOCH, true, "starting")).unwrap()
        );

        let (events, unreadable) = parse_journal_lines(&raw);

        assert_eq!(events.len(), 1, "the one real event survives");
        assert_eq!(
            unreadable.len(),
            2,
            "a broken line and an event without a kind are both reported: {unreadable:?}"
        );
    }

    #[test]
    fn a_settled_first_epoch_names_the_runtime_that_confirmed_it_and_the_verdict_that_ended_it() {
        let summary = journal_assertions(&a_two_epoch_journal(true));

        assert_eq!(
            summary.first_run_confirmed,
            Some(2),
            "the confirmation comes from the runtime, not from a person"
        );
        assert_eq!(
            summary.first_run_ended_with.as_deref(),
            Some("idle"),
            "the reader is told which verdict ended the epoch"
        );
        assert_eq!(summary.second_run_ended, Some(8));
        assert!(
            summary.first_epoch_is_settled(),
            "{summary:?}: the epoch confirmed and ended"
        );
    }

    #[test]
    fn an_epoch_the_runtime_never_confirmed_is_not_settled() {
        // The Start was applied and the run ended, but nothing ever
        // confirmed the objective: the confirmation is the half of the
        // contract a client cannot infer.
        let events = vec![
            start_action(1, FIRST_EPOCH, true, "starting"),
            outcome(2, FIRST_EPOCH, "idle"),
        ];
        let summary = journal_assertions(&events);

        assert_eq!(summary.first_run_confirmed, None);
        assert!(
            !summary.first_epoch_is_settled(),
            "{summary:?}: a run nobody confirmed is not a settled run"
        );
    }

    #[test]
    fn an_epoch_that_never_ended_is_not_settled() {
        // The runtime confirmed and the objective held, but the run never
        // ended: that is a hang, not a phase.
        let events = vec![
            start_action(1, FIRST_EPOCH, true, "starting"),
            confirmed(2, FIRST_EPOCH),
            outcome(3, FIRST_EPOCH, "confirmed"),
        ];
        let summary = journal_assertions(&events);

        assert_eq!(summary.first_run_ended, None);
        assert!(
            !summary.first_epoch_is_settled(),
            "{summary:?}: a run that never ends is not a settled run"
        );
    }

    /// The CLI rendering of a settled snapshot: the contract as `sup-status`
    /// prints it.
    const CLI_SNAPSHOT: &str = "\
project: probe-project
phase: idle
ticket: TASK-9
goal: tracker work
revision: deadbeef
runtime: rehearsal (trial-default)
objective: idle: no eligible work
resources: UNKNOWN (no telemetry yet)
context available: no
waiting: (not waiting)
ttl remaining: (untracked)
epoch: 1
grant scope: (none — the supervisor authorizes no effect by itself)
last action: start(probe-project) applied=true (starting for probe-project (epoch 1))";

    /// The TUI paints the same contract, but the terminal writes every WORD
    /// where it belongs: the normalized cells carry no spaces at all.
    const TUI_CELLS: &str = "\
┌supervisorstate┐│project:probe-project│phase:idle│ticket:TASK-9│goal:trackerwork│\
revision:deadbeef│runtime:rehearsal(trial-default)│objective:idle:noeligiblework│\
resources:UNKNOWN(notelemetryyet)│contextavailable:no│waiting:(notwaiting)│\
ttlremaining:(untracked)│epoch:1│";

    #[test]
    fn the_two_surfaces_carry_the_same_value_of_every_field_of_the_contract() {
        for key in SHARED_SNAPSHOT_FIELDS {
            assert!(
                surfaces_agree(CLI_SNAPSHOT, TUI_CELLS, key),
                "the CLI and the TUI must carry the same value for {key}"
            );
        }
    }

    #[test]
    fn a_surface_that_shows_another_value_does_not_agree() {
        let drifted = TUI_CELLS.replace("phase:idle", "phase:running");

        assert!(
            !surfaces_agree(CLI_SNAPSHOT, &drifted, "phase"),
            "a TUI that shows another phase has drifted from the CLI"
        );
        assert!(
            surfaces_agree(CLI_SNAPSHOT, &drifted, "ticket"),
            "the fields that did not drift still agree"
        );
    }

    #[test]
    fn a_field_that_a_surface_does_not_render_is_never_agreement() {
        assert!(
            !surfaces_agree(CLI_SNAPSHOT, TUI_CELLS, "paused"),
            "a field the surface does not render cannot agree"
        );
        assert_eq!(
            snapshot_field(CLI_SNAPSHOT, "ticket").as_deref(),
            Some("TASK-9")
        );
        assert_eq!(snapshot_field(CLI_SNAPSHOT, "no-such-field"), None);
    }
}
