//! Supervisor surface wiring (TASK-15): the socket server, the plain CLI
//! clients (status/actions/events), the attached terminal viewer (AC-3:
//! read-only, its disconnect does not alter the objective) and the Ratatui
//! TUI. ALL of them are clients of the SAME socket contract.

use std::io::IsTerminal;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hodei_mh_run::runtime::ObjectiveRuntime;
use supervisor::client;
use supervisor::engine;
use supervisor::paths;
use supervisor::protocol::{ResourceObservation, Snapshot, StructuredEvent, SupervisorAction};
use supervisor::render::{render_event, render_snapshot};
use supervisor::server;

/// The control socket a client uses when the caller names none.
///
/// It lives in the PRIVATE runtime directory of the user, not in the
/// shared temporary directory: the path of the old default could be
/// taken over by any local user.
pub fn default_socket() -> String {
    paths::default_socket().to_string_lossy().into_owned()
}

fn flag(args: &[String], name: &str, default: &str) -> String {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
        .unwrap_or_else(|| default.to_string())
}

fn opt_flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Write one line of client output.
///
/// A reader that closed the pipe (`sup-events | head`) ends the command
/// the way a shell pipeline ends, not with a panic; any other write
/// failure is reported and fails the command.
fn say(line: &str) -> Result<(), i32> {
    let mut stdout = std::io::stdout().lock();
    write_line(&mut stdout, line)
}

fn write_line(out: &mut impl std::io::Write, line: &str) -> Result<(), i32> {
    match writeln!(out, "{line}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Err(0),
        Err(error) => {
            eprintln!("cannot write the supervisor output: {error}");
            Err(1)
        }
    }
}

/// How one batch of log lines ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrintOutcome {
    /// Every line reached the output.
    Printed,
    /// The reader closed the pipe: there is nobody left to log to.
    Closed,
    /// The output failed for another reason, and the failure was reported.
    Failed,
}

/// Print the pending events in order, advancing the cursor past exactly
/// the lines that reached the output.
///
/// A line that could not be written is NOT counted as printed: a cursor
/// that moved past it would drop it from the log without saying so. The
/// failure itself is returned, never swallowed.
fn print_events(
    out: &mut impl std::io::Write,
    pending: &[(u64, String)],
    printed_seq: &mut u64,
) -> PrintOutcome {
    for (seq, line) in pending {
        match write_line(out, line) {
            Ok(()) => *printed_seq = *seq,
            Err(0) => return PrintOutcome::Closed,
            Err(_) => return PrintOutcome::Failed,
        }
    }
    match out.flush() {
        Ok(()) => PrintOutcome::Printed,
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => PrintOutcome::Closed,
        Err(error) => {
            eprintln!("cannot flush the supervisor output: {error}");
            PrintOutcome::Failed
        }
    }
}

/// `hodei-mh supervisor`: THE controller. One per machine/project; serves
/// the socket until interrupted. Every event is logged to stdout WITH its
/// provenance and AFTER credential redaction (AC-4).
///
/// `--rehearsal` wires the rehearsal runtime: a deterministic run with no
/// external effects, which says so in every snapshot. `--live` wires the
/// live runtime: the real tracker, the real sandboxes and the real managed
/// branch. The two are exclusive, and a live boot that cannot execute
/// refuses with its reason instead of starting a controller that would
/// accept a run it can never carry out. Without either, the controller has
/// NO runtime and refuses to start, because a run nobody can execute is
/// not a run.
pub async fn run_supervisor(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", &default_socket());
    let project = flag(args, "--project", "hodei-mh");
    let rehearsal = args.iter().any(|a| a == "--rehearsal");
    let live = args.iter().any(|a| a == "--live");
    if live && rehearsal {
        eprintln!(
            "refusing to start: --live and --rehearsal are exclusive (a rehearsal measures nothing; a live run executes)"
        );
        return 1;
    }
    // The runtime is wired BEFORE the socket is bound: a controller whose
    // configuration cannot execute must not own the identity of a project.
    let runtime: Option<Arc<dyn ObjectiveRuntime>> = if live {
        let config = match crate::live::config_from_args(args, &project) {
            Ok(config) => config,
            Err(error) => {
                eprintln!("refusing the live supervisor: {error}");
                return 1;
            }
        };
        match crate::live::real_ports(&config).await {
            Ok(runtime) => Some(Arc::new(runtime)),
            Err(error) => {
                eprintln!("refusing the live supervisor: {error}");
                return 1;
            }
        }
    } else if rehearsal {
        Some(Arc::new(runner::rehearsal::TrialRuntime::new()))
    } else {
        None
    };
    // The identity of this controller is the project it serves, and the
    // path it serves: both are taken as EXCLUSIVE locks, so a second
    // controller can never assume either of them.
    let bound = match server::bind(&socket, &project).await {
        Ok(bound) => bound,
        Err(e) => {
            eprintln!("cannot bind supervisor socket: {e}");
            return 1;
        }
    };
    // The socket file belongs to the CONTROLLER: the bound socket removes
    // it when the controller stops, and only if it is still the same file.
    // The CLI does not unlink it: that would be the same fact in two
    // places, and the two could disagree.
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new({
        // The runtime is NOT declared here: the server that serves it
        // writes it into the core, so the runtime the snapshot names and
        // the runtime that executes are the same fact, not two.
        let mut core = engine::default_core(project.clone());
        // The boot decision is the supervisor's own: logged with automatic
        // provenance (AC-4: automatic decisions state their rule).
        engine::emit_event(
            &mut core,
            "supervisor/boot",
            serde_json::Value::String(format!(
                "listening on {socket}; credentials will be redacted; runtime: {}",
                runtime
                    .as_ref()
                    .map(|runtime| runtime.identity().label())
                    .unwrap_or_else(|| "(none wired)".to_string())
            )),
            supervisor::protocol::Provenance::Automatic {
                rule: "boot".into(),
            },
        );
        core
    }));
    let _ = say(&format!(
        "supervisor listening on {socket}; logs identify provenance, credentials are redacted; runtime: {}",
        runtime
            .as_ref()
            .map(|runtime| runtime.identity().label())
            .unwrap_or_else(|| "(none wired — a Start is refused)".to_string())
    ));
    let log_core = core.clone();
    // A log that cannot be written is a failure of the command, not a
    // detail to swallow: the watcher says so and the controller ends with
    // an explicit error instead of serving blind.
    let (output_failure, mut output_failed) = tokio::sync::oneshot::channel::<()>();
    // The log watcher prints every event as it is recorded — provenance
    // included, credentials already redacted by the engine. Flush after
    // every batch: a service whose log is stuck in a buffer is no log.
    tokio::spawn(async move {
        // The log follows the SEQUENCE, not an index: the engine's history
        // is a ring buffer, so an index would reprint or skip events.
        let mut printed_seq: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let pending: Vec<(u64, String)> = {
                // A poisoned lock must not silence the log: recover the core
                // and keep printing (the supervisor marks itself degraded).
                let guard = match log_core.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                guard
                    .events
                    .iter()
                    .filter(|event| event.seq > printed_seq)
                    .map(|event| (event.seq, render_event_line(event, EmitFormat::Human)))
                    .collect()
            };
            if pending.is_empty() {
                continue;
            }
            let outcome = {
                let mut out = std::io::stdout().lock();
                print_events(&mut out, &pending, &mut printed_seq)
            };
            match outcome {
                PrintOutcome::Printed => {}
                // The reader closed the pipe (`sup-events | head`): nobody
                // is listening, and the controller keeps serving. Waiting
                // forever keeps the failure channel open.
                PrintOutcome::Closed => std::future::pending::<()>().await,
                PrintOutcome::Failed => {
                    let _ = output_failure.send(());
                    return;
                }
            }
        }
    });
    // The runtime goes WITH the controller: the server hands the objective
    // to it, and its confirmation is what moves the run to `Running`.
    // Serving without it would accept the Start and leave the run in
    // `Starting` forever.
    let mut served = tokio::spawn(server::serve(
        bound,
        core,
        runtime,
        server::Limits::default(),
    ));
    tokio::select! {
        result = &mut served => match result {
            Ok(Ok(())) => 0,
            Ok(Err(error)) => {
                eprintln!("the control surface stopped: {error}");
                1
            }
            Err(error) => {
                eprintln!("the control surface task failed: {error}");
                1
            }
        },
        _ = &mut output_failed => {
            served.abort();
            1
        }
    }
}

fn provenance_label(provenance: &supervisor::protocol::Provenance) -> &'static str {
    match provenance {
        supervisor::protocol::Provenance::Human => "human",
        supervisor::protocol::Provenance::Automatic { .. } => "auto",
        supervisor::protocol::Provenance::Telemetry { unknown: false } => "telemetry",
        supervisor::protocol::Provenance::Telemetry { unknown: true } => "telemetry-UNKNOWN",
    }
}

/// Parse a `start` into the action it names.
///
/// A custom objective is judged ONLY by the operator's termination
/// criterion: an `--objective` without `--accept` would be an objective
/// that can never be confirmed, so it is refused here, with its cause,
/// instead of being sent to the runtime as an unconfirmable request.
fn parse_start(args: &[String]) -> Result<SupervisorAction, String> {
    let objective = opt_flag(args, "--objective");
    let ticket = opt_flag(args, "--ticket");
    let accept = opt_flag(args, "--accept");
    let expect_exit = opt_flag(args, "--expect-exit");
    let project = flag(args, "--project", "");
    match objective {
        Some(text) => {
            if ticket.is_some() {
                return Err(
                    "a custom objective owns no ticket: name a ticket or an objective, not both"
                        .to_string(),
                );
            }
            let command = accept.ok_or_else(|| {
                "a custom objective needs an operator termination criterion: pass --accept <command> (and optionally --expect-exit <N>); without one it could never be confirmed".to_string()
            })?;
            let expect_exit = match expect_exit {
                Some(raw) => raw
                    .parse::<i32>()
                    .map_err(|_| format!("--expect-exit is not an integer: {raw}"))?,
                None => 0,
            };
            Ok(SupervisorAction::StartObjective {
                project,
                objective: hodei_mh_run::runtime::CustomObjective {
                    instruction: text,
                    accept: goal::source::TerminationCriterion {
                        command,
                        expect_exit,
                    },
                },
            })
        }
        None => {
            if accept.is_some() || expect_exit.is_some() {
                return Err(
                    "--accept and --expect-exit belong to --objective: ticket work is judged by its own acceptance criteria"
                        .to_string(),
                );
            }
            Ok(SupervisorAction::Start {
                project,
                ticket,
                revision: opt_flag(args, "--revision"),
            })
        }
    }
}

/// How the CLI read the requested action: applied, unknown, or refused
/// with the reason it must NOT be sent to the supervisor.
#[derive(Debug)]
enum ParsedAction {
    Action(SupervisorAction),
    Unknown,
    Refused(String),
}

fn parse_action(args: &[String], name: &str) -> ParsedAction {
    match name {
        "start" => match parse_start(args) {
            Ok(action) => ParsedAction::Action(action),
            Err(reason) => ParsedAction::Refused(reason),
        },
        "pause" => ParsedAction::Action(SupervisorAction::Pause),
        "resume" => ParsedAction::Action(SupervisorAction::Resume),
        "stop" => ParsedAction::Action(SupervisorAction::Stop),
        "renew" => match flag(args, "--added-seconds", "300").parse() {
            Ok(added_seconds) => ParsedAction::Action(SupervisorAction::Renew { added_seconds }),
            Err(_) => {
                ParsedAction::Refused("--added-seconds must be a number of seconds".to_string())
            }
        },
        _ => ParsedAction::Unknown,
    }
}

/// `hodei-mh sup-status`: the CLI view of the SAME render contract the TUI
/// uses.
pub async fn run_status(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", &default_socket());
    let mut client = match client::connect(&socket).await {
        Ok(client) => client,
        // Honest failure: the supervisor is not reachable; a client never
        // starts a second controller from here.
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match client.snapshot().await {
        Ok(snapshot) => {
            for line in render_snapshot(&snapshot) {
                if let Err(code) = say(&line) {
                    return code;
                }
            }
            0
        }
        Err(e) => {
            eprintln!("supervisor rejected the request: {e}");
            1
        }
    }
}

/// `hodei-mh sup-action <name>`: Start/Pause/Resume/Stop/Renew through the
/// same contract the TUI keys drive.
pub async fn run_action(args: &[String]) -> i32 {
    let Some(name) = args.first().map(String::as_str) else {
        eprintln!("usage: hodei-mh sup-action <start|pause|resume|stop|renew> [--socket PATH]");
        return 2;
    };
    let action = match parse_action(&args[1..], name) {
        ParsedAction::Action(action) => action,
        ParsedAction::Refused(reason) => {
            eprintln!("refusing to send the action: {reason}");
            return 2;
        }
        ParsedAction::Unknown => {
            eprintln!("unknown action {name}; expected start|pause|resume|stop|renew");
            return 2;
        }
    };
    let socket = flag(&args[1..], "--socket", &default_socket());
    let mut client = match client::connect(&socket).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    // A Start names a project. When the caller does not name one, the
    // SUPERVISOR's own project is used: the supervisor serves one project
    // and refuses a Start for another one.
    let action = match action {
        SupervisorAction::Start {
            project,
            ticket,
            revision,
        } if project.is_empty() => match client.snapshot().await {
            Ok(snapshot) => SupervisorAction::Start {
                project: snapshot.project,
                ticket,
                revision,
            },
            Err(e) => {
                eprintln!("cannot read the supervisor's project: {e}");
                return 1;
            }
        },
        SupervisorAction::StartObjective { project, objective } if project.is_empty() => {
            match client.snapshot().await {
                Ok(snapshot) => SupervisorAction::StartObjective {
                    project: snapshot.project,
                    objective,
                },
                Err(e) => {
                    eprintln!("cannot read the supervisor's project: {e}");
                    return 1;
                }
            }
        }
        other => other,
    };
    match client.action(action).await {
        Ok(outcome) => {
            if let Err(code) = say(&format!(
                "{}: {} ({})",
                if outcome.applied {
                    "applied"
                } else {
                    "refused"
                },
                outcome.detail,
                if outcome.applied {
                    "the supervisor applied it"
                } else {
                    "this was NOT executed"
                }
            )) {
                return code;
            }
            if outcome.applied { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("supervisor rejected the request: {e}");
            1
        }
    }
}

/// Read the whole history from `since`, following the CURSOR of every
/// page.
///
/// The supervisor bounds every page: a client that reads one page and
/// ignores the cursor would present a TRUNCATED history as if it were
/// the whole one. The events that are gone are announced, never hidden.
async fn read_history(
    client: &mut client::SupervisorClient,
    since: u64,
) -> Result<(Vec<StructuredEvent>, Option<u64>), client::ClientError> {
    let mut events: Vec<StructuredEvent> = Vec::new();
    let mut cursor = since;
    let mut oldest_lost: Option<u64> = None;
    loop {
        let page = client.events(cursor, None).await?;
        if page.history_lost {
            oldest_lost = Some(page.oldest_seq);
        }
        let last_seq = page.events.last().map(|event| event.seq);
        events.extend(page.events);
        match last_seq {
            // The page was the last one: there is nothing more to read.
            Some(last) if page.next_seq <= last => break,
            None => break,
            Some(_) => cursor = page.next_seq,
        }
    }
    Ok((events, oldest_lost))
}

/// The channel `hodei-mh sup-events` writes its answer on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EmitFormat {
    /// The readable tail: a provenance tag and the rendered event.
    Human,
    /// One JSON object per line, exactly as the socket carries it.
    Jsonl,
}

/// The channel the flags and stdout ask for, or the reason they clash.
#[derive(Debug, PartialEq, Eq)]
enum EmitChoice {
    Format(EmitFormat),
    Refused(String),
}

/// Resolve the output channel from the flags and what stdout really is.
///
/// Pure: reading a file descriptor is an effect, so the caller reads it
/// and hands the fact over. The flags win over the terminal because a
/// script that names a channel means it even when a person pipes it into
/// a pager. With no flag the channel follows stdout: a pipe, a file or a
/// subprocess parses the answer, and a parser reads JSON, not a tail.
fn resolve_emit(args: &[String], stdout_is_terminal: bool) -> EmitChoice {
    let jsonl = args.iter().any(|a| a == "--jsonl");
    let pretty = args.iter().any(|a| a == "--pretty");
    match (jsonl, pretty) {
        (true, true) => EmitChoice::Refused(
            "--jsonl and --pretty are exclusive (--jsonl is one JSON object per line for a script, --pretty is the readable tail for a person)".to_string(),
        ),
        (true, false) => EmitChoice::Format(EmitFormat::Jsonl),
        (false, true) => EmitChoice::Format(EmitFormat::Human),
        (false, false) => EmitChoice::Format(if stdout_is_terminal {
            EmitFormat::Human
        } else {
            EmitFormat::Jsonl
        }),
    }
}

/// The line one event becomes in the chosen channel.
///
/// The machine channel is the event AS THE WIRE CARRIES IT — the same
/// fields the socket's answer has — so a `jq` consumer and the CLI can
/// never read two different renderings of one event. `serde_json`
/// escapes the control characters inside the strings, so one event is
/// always exactly one line.
fn render_event_line(event: &StructuredEvent, format: EmitFormat) -> String {
    match format {
        EmitFormat::Human => {
            format!(
                "[{}] {}",
                provenance_label(&event.provenance),
                render_event(event)
            )
        }
        EmitFormat::Jsonl => match serde_json::to_string(event) {
            Ok(line) => line,
            // A body that cannot be encoded must not vanish and must not
            // print a line that pretends to be the event: the failure is
            // carried as a line that says what failed.
            Err(error) => serde_json::json!({
                "seq": event.seq,
                "epoch": event.epoch,
                "render_error": error.to_string(),
            })
            .to_string(),
        },
    }
}

/// `hodei-mh sup-events`: replay structured events from a sequence.
///
/// `--jsonl` writes one JSON object per line and `--pretty` the readable
/// tail; with neither, the answer is JSONL when stdout is a pipe or a
/// file (scripts, probes and UAT oracles parse it) and the readable tail
/// when stdout is a terminal (a person reads it).
pub async fn run_events(args: &[String]) -> i32 {
    let format = match resolve_emit(args, std::io::stdout().is_terminal()) {
        EmitChoice::Format(format) => format,
        EmitChoice::Refused(reason) => {
            eprintln!("refusing to read the events: {reason}");
            return 2;
        }
    };
    let socket = flag(args, "--socket", &default_socket());
    let since: u64 = flag(args, "--since", "0").parse().unwrap_or(0);
    let mut client = match client::connect(&socket).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match read_history(&mut client, since).await {
        Ok((events, oldest_lost)) => {
            if let Some(oldest) = oldest_lost {
                // The history the client asked for had a hole: it is said
                // out loud, not papered over.
                let notice =
                    format!("history lost: the events before #{oldest} are no longer retained");
                match format {
                    // The machine channel is parsed line by line: the
                    // announcement travels on stderr so every stdout line
                    // stays one event.
                    EmitFormat::Jsonl => eprintln!("{notice}"),
                    EmitFormat::Human => {
                        if let Err(code) = say(&notice) {
                            return code;
                        }
                    }
                }
            }
            for event in &events {
                if let Err(code) = say(&render_event_line(event, format)) {
                    return code;
                }
            }
            0
        }
        Err(e) => {
            eprintln!("supervisor rejected the request: {e}");
            1
        }
    }
}

/// `hodei-mh attach-terminal` (AC-3): a SEPARATE read-only terminal. It
/// polls structured events and NEVER touches the ACP conversation;
/// disconnecting it changes nothing for the supervisor.
pub async fn run_attach_terminal(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", &default_socket());
    let max_events: u64 = flag(args, "--exit-after-events", "0").parse().unwrap_or(0);
    // A viewer whose supervisor never answers must NOT hang forever: the
    // deadline turns a dead supervisor into an explicit failure.
    let deadline_secs: u64 = flag(args, "--deadline-seconds", "60").parse().unwrap_or(60);
    let started = std::time::Instant::now();
    if let Err(code) =
        say("attached terminal viewer: read-only event tail; leaving does not affect the objective")
    {
        return code;
    }
    let mut seen: u64 = 1;
    let mut delivered: u64 = 0;
    loop {
        match client::connect(&socket).await {
            Ok(mut client) => {
                let result = client.events(seen, None).await;
                drop(client);
                match result {
                    Ok(page) => {
                        if page.history_lost
                            && let Err(code) = say(&format!(
                                "[viewer] history lost: the events before #{} are no longer retained",
                                page.oldest_seq
                            ))
                        {
                            return code;
                        }
                        for event in page.events {
                            if let Err(code) = say(&format!("[viewer] {}", render_event(&event))) {
                                return code;
                            }
                            seen = event.seq + 1;
                            delivered += 1;
                        }
                    }
                    Err(e) => {
                        if let Err(code) =
                            say(&format!("[viewer] supervisor rejected the poll: {e}"))
                        {
                            return code;
                        }
                    }
                }
            }
            Err(e) => {
                if let Err(code) = say(&format!(
                    "[viewer] supervisor not reachable ({e}); still attached, still read-only"
                )) {
                    return code;
                }
            }
        }
        if max_events > 0 && delivered >= max_events {
            return 0;
        }
        if started.elapsed().as_secs() >= deadline_secs {
            eprintln!(
                "viewer deadline reached after {deadline_secs}s: delivered {delivered} events (wanted {max_events})"
            );
            return 1;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

/// `hodei-mh tui` (AC-1/AC-2): Ratatui client of the supervisor socket.
/// `--simulated` renders a fixed state without any socket (nothing is
/// presented as real); `--frames N` renders N frames non-interactively so
/// the TUI surface can be exercised in a real local session.
pub async fn run_tui(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", &default_socket());
    let simulated = args.iter().any(|a| a == "--simulated");
    let frames: usize = flag(args, "--frames", "0").parse().unwrap_or(0);
    crate::tui::tui_main(&socket, simulated, frames).await
}

/// Simulated state for DoD #1 (TUI exercised without a supervisor).
pub fn simulated_snapshot() -> Snapshot {
    Snapshot {
        protocol_version: supervisor::protocol::PROTOCOL_VERSION,
        project: "simulated-project".into(),
        phase: hodei_mh_run::run::Phase::Running,
        paused: false,
        ticket: Some("TASK-17".into()),
        goal: None,
        revision: Some("0f3e91c".into()),
        // The simulated state measured nothing: it says UNKNOWN, the
        // same way a real supervisor says it when it has no telemetry.
        resources: ResourceObservation::Unknown {
            reason: "the simulated state measured nothing".into(),
        },
        context_available: true,
        wait_reason: None,
        last_action: None,
        ttl_remaining_seconds: Some(1800),
        epoch: 1,
        grant_effects: Vec::new(),
        degraded: None,
        runtime: Some(hodei_mh_run::runtime::RuntimeIdentity::rehearsal(
            "simulated",
        )),
        objective: None,
    }
}

pub fn simulated_events() -> Vec<StructuredEvent> {
    vec![
        StructuredEvent {
            seq: 1,
            epoch: 1,
            kind: "supervisor/action".into(),
            payload: serde_json::json!({
                "action": "start",
                "applied": true,
                "detail": "the simulated state applied a start",
            }),
            provenance: supervisor::protocol::Provenance::Human,
        },
        StructuredEvent {
            seq: 2,
            epoch: 1,
            kind: "telemetry/resources".into(),
            payload: serde_json::json!({
                "unknown": true,
                "reason": "the simulated state measured nothing",
            }),
            provenance: supervisor::protocol::Provenance::Telemetry { unknown: true },
        },
        StructuredEvent {
            seq: 3,
            epoch: 1,
            kind: "telemetry/resources".into(),
            payload: serde_json::json!({
                "unknown": true,
                "reason": "cgroup unavailable",
            }),
            provenance: supervisor::protocol::Provenance::Telemetry { unknown: true },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::{
        EmitChoice, EmitFormat, ParsedAction, PrintOutcome, parse_action, print_events,
        provenance_label, render_event, render_event_line, resolve_emit, write_line,
    };
    use std::io;
    use supervisor::protocol::{Provenance, StructuredEvent, SupervisorAction};

    struct Failing(io::ErrorKind);

    impl io::Write for Failing {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            Err(self.0.into())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    /// A writer that accepts `budget` bytes and then fails.
    struct FailingAfter {
        budget: usize,
        kind: io::ErrorKind,
        written: Vec<u8>,
    }

    impl FailingAfter {
        fn new(budget: usize, kind: io::ErrorKind) -> Self {
            Self {
                budget,
                kind,
                written: Vec::new(),
            }
        }
    }

    impl io::Write for FailingAfter {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            if buffer.len() > self.budget {
                return Err(self.kind.into());
            }
            self.budget -= buffer.len();
            self.written.extend_from_slice(buffer);
            Ok(buffer.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn a_line_is_written_verbatim() {
        let mut out: Vec<u8> = Vec::new();
        assert_eq!(write_line(&mut out, "[auto] started"), Ok(()));
        assert_eq!(String::from_utf8(out).expect("utf-8"), "[auto] started\n");
    }

    #[test]
    fn a_closed_reader_ends_the_output_without_error() {
        // `sup-events | head` closes stdout: the pipeline ending is not a
        // failure of the client.
        assert_eq!(
            write_line(&mut Failing(io::ErrorKind::BrokenPipe), "line"),
            Err(0)
        );
    }

    #[test]
    fn any_other_write_failure_fails_the_command() {
        assert_eq!(
            write_line(&mut Failing(io::ErrorKind::StorageFull), "line"),
            Err(1)
        );
    }

    #[test]
    fn a_real_write_failure_is_reported_and_keeps_the_cursor_honest() {
        // The first line reaches the output, the second does not: the
        // cursor stops at the first one, so no event is counted as
        // printed without having been printed, and no event is printed
        // twice.
        let mut out = FailingAfter::new("second\n".len(), io::ErrorKind::StorageFull);
        let pending = vec![(2, "second".to_string()), (3, "third".to_string())];
        let mut printed = 1;
        assert_eq!(
            print_events(&mut out, &pending, &mut printed),
            PrintOutcome::Failed
        );
        assert_eq!(printed, 2, "only the written event advanced the cursor");
        assert_eq!(String::from_utf8(out.written).expect("utf-8"), "second\n");
    }

    #[test]
    fn a_closed_reader_stops_the_log_without_counting_a_line_as_printed() {
        let mut out = FailingAfter::new(0, io::ErrorKind::BrokenPipe);
        let pending = vec![(2, "second".to_string())];
        let mut printed = 1;
        assert_eq!(
            print_events(&mut out, &pending, &mut printed),
            PrintOutcome::Closed
        );
        assert_eq!(printed, 1, "nothing was written, nothing was counted");
        assert!(out.written.is_empty());
    }

    #[test]
    fn every_written_event_advances_the_cursor() {
        let mut out: Vec<u8> = Vec::new();
        let pending = vec![(2, "second".to_string()), (3, "third".to_string())];
        let mut printed = 1;
        assert_eq!(
            print_events(&mut out, &pending, &mut printed),
            PrintOutcome::Printed
        );
        assert_eq!(printed, 3);
        assert_eq!(
            String::from_utf8(out).expect("utf-8"),
            "second\nthird\n",
            "each event is printed once"
        );
    }

    fn cli(args: &[&str]) -> Vec<String> {
        args.iter().map(std::string::ToString::to_string).collect()
    }

    #[test]
    fn an_objective_carries_its_operator_criterion() {
        let parsed = parse_action(
            &cli(&[
                "--project",
                "gc",
                "--objective",
                "summarise the docs",
                "--accept",
                "test -f SUMMARY.md",
                "--expect-exit",
                "3",
            ]),
            "start",
        );
        match parsed {
            ParsedAction::Action(SupervisorAction::StartObjective { project, objective }) => {
                assert_eq!(project, "gc");
                assert_eq!(objective.instruction, "summarise the docs");
                assert_eq!(objective.accept.command, "test -f SUMMARY.md");
                assert_eq!(objective.accept.expect_exit, 3);
            }
            other => panic!("the criterion travels with the objective: {other:?}"),
        }
    }

    #[test]
    fn an_objective_without_a_criterion_is_refused_with_its_cause() {
        // An unconfirmable objective must never reach the runtime: the
        // refusal names the flag that is missing.
        let parsed = parse_action(
            &cli(&["--project", "gc", "--objective", "summarise the docs"]),
            "start",
        );
        match parsed {
            ParsedAction::Refused(reason) => {
                assert!(reason.contains("--accept"), "{reason}");
            }
            other => panic!("an objective without a criterion is refused: {other:?}"),
        }
    }

    #[test]
    fn an_objective_that_also_names_a_ticket_is_refused_not_dropped() {
        // The CLI used to discard --ticket in silence, skipping the
        // runtime's own "not both" refusal.
        let parsed = parse_action(
            &cli(&[
                "--objective",
                "summarise the docs",
                "--accept",
                "true",
                "--ticket",
                "TASK-9",
            ]),
            "start",
        );
        match parsed {
            ParsedAction::Refused(reason) => {
                assert!(reason.contains("not both"), "{reason}");
            }
            other => panic!("an objective never carries a ticket: {other:?}"),
        }
    }

    #[test]
    fn a_criterion_without_an_objective_is_refused() {
        let parsed = parse_action(&cli(&["--ticket", "TASK-9", "--accept", "true"]), "start");
        assert!(
            matches!(parsed, ParsedAction::Refused(ref reason) if reason.contains("--objective")),
            "{parsed:?}"
        );
    }

    #[test]
    fn a_non_numeric_expect_exit_is_refused_with_the_value() {
        let parsed = parse_action(
            &cli(&[
                "--objective",
                "summarise",
                "--accept",
                "true",
                "--expect-exit",
                "soon",
            ]),
            "start",
        );
        assert!(
            matches!(parsed, ParsedAction::Refused(ref reason) if reason.contains("soon")),
            "{parsed:?}"
        );
    }

    /// The two shapes of event the channels really print: an automatic
    /// decision and a telemetry observation whose body carries a
    /// newline, the case that must never split a machine-readable line.
    fn sample_events() -> Vec<StructuredEvent> {
        vec![
            StructuredEvent {
                seq: 1,
                epoch: 1,
                kind: "supervisor/boot".into(),
                payload: serde_json::json!({
                    "message": "listening on the control socket; credentials are redacted",
                }),
                provenance: Provenance::Automatic {
                    rule: "boot".into(),
                },
            },
            StructuredEvent {
                seq: 2,
                epoch: 4,
                kind: "telemetry/resources".into(),
                payload: serde_json::json!({
                    "unknown": true,
                    "reason": "the cgroup path is\nnot readable",
                }),
                provenance: Provenance::Telemetry { unknown: true },
            },
        ]
    }

    #[test]
    fn the_jsonl_channel_carries_the_whole_event_in_one_line() {
        for event in sample_events() {
            let line = render_event_line(&event, EmitFormat::Jsonl);
            assert!(
                !line.contains('\n'),
                "one event is one line, never two: {line}"
            );
            let read: StructuredEvent =
                serde_json::from_str(&line).expect("every JSONL line is one JSON object");
            assert_eq!(read, event, "the machine channel loses nothing");
        }
    }

    #[test]
    fn the_readable_tail_keeps_the_line_the_cli_printed_before() {
        for event in sample_events() {
            let line = render_event_line(&event, EmitFormat::Human);
            assert_eq!(
                line,
                format!(
                    "[{}] {}",
                    provenance_label(&event.provenance),
                    render_event(&event)
                ),
                "the human line is the provenance tag over the shared rendering"
            );
        }
        let tail = render_event_line(&sample_events()[1], EmitFormat::Human);
        assert!(tail.contains("[telemetry-UNKNOWN]"), "{tail}");
        assert!(tail.contains("#2"), "{tail}");
    }

    fn format_of(args: &[&str], stdout_is_terminal: bool) -> EmitChoice {
        resolve_emit(&cli(args), stdout_is_terminal)
    }

    #[test]
    fn the_two_channels_asked_for_together_are_refused_with_their_cause() {
        let choice = format_of(&["--jsonl", "--pretty"], true);
        match choice {
            EmitChoice::Refused(reason) => {
                assert!(reason.contains("--jsonl"), "{reason}");
                assert!(reason.contains("--pretty"), "{reason}");
                assert!(reason.contains("exclusive"), "{reason}");
            }
            other => panic!("one answer cannot be in two channels: {other:?}"),
        }
    }

    #[test]
    fn the_jsonl_flag_is_the_machine_channel_even_on_a_terminal() {
        assert_eq!(
            format_of(&["--jsonl"], true),
            EmitChoice::Format(EmitFormat::Jsonl)
        );
    }

    #[test]
    fn the_pretty_flag_is_the_readable_tail_even_on_a_pipe() {
        assert_eq!(
            format_of(&["--pretty"], false),
            EmitChoice::Format(EmitFormat::Human)
        );
    }

    #[test]
    fn a_pipe_without_flags_is_answered_in_json_lines() {
        assert_eq!(
            format_of(&["--since", "0"], false),
            EmitChoice::Format(EmitFormat::Jsonl)
        );
    }

    #[test]
    fn a_terminal_without_flags_keeps_the_readable_tail() {
        assert_eq!(format_of(&[], true), EmitChoice::Format(EmitFormat::Human));
    }
}
