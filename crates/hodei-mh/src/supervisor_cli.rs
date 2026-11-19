//! Supervisor surface wiring (TASK-15): the socket server, the plain CLI
//! clients (status/actions/events), the attached terminal viewer (AC-3:
//! read-only, its disconnect does not alter the objective) and the Ratatui
//! TUI. ALL of them are clients of the SAME socket contract.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use hodei_mh_run::runtime::ObjectiveRuntime;
use supervisor::client;
use supervisor::engine;
use supervisor::protocol::{ResourceObservation, Snapshot, StructuredEvent, SupervisorAction};
use supervisor::render::{render_event, render_snapshot};
use supervisor::server;

pub const DEFAULT_SOCKET: &str = "/tmp/hodei-mh-supervisor.sock";

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

/// `hodei-mh supervisor`: THE controller. One per machine/project; serves
/// the socket until interrupted. Every event is logged to stdout WITH its
/// provenance and AFTER credential redaction (AC-4).
///
/// `--rehearsal` wires the rehearsal runtime: a deterministic run with no
/// external effects, which says so in every snapshot. Without it the
/// controller has NO runtime and refuses to start, because a run nobody
/// can execute is not a run.
pub async fn run_supervisor(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
    let project = flag(args, "--project", "hodei-mh");
    let rehearsal = args.iter().any(|a| a == "--rehearsal");
    let listener = match server::bind(&socket).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("cannot bind supervisor socket: {e}");
            return 1;
        }
    };
    // Ownership of the socket file: only OUR file is removed on exit, so a
    // successor supervisor that already replaced it keeps its socket.
    let owned_socket = server::identity_of(std::path::Path::new(&socket));
    let runtime: Option<Arc<dyn ObjectiveRuntime>> = if rehearsal {
        Some(Arc::new(runner::rehearsal::TrialRuntime::new()))
    } else {
        None
    };
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new({
        let mut core = match &runtime {
            Some(runtime) => engine::core_with_runtime(project.clone(), runtime.identity()),
            None => engine::default_core(project.clone()),
        };
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
    println!(
        "supervisor listening on {socket}; logs identify provenance, credentials are redacted; runtime: {}",
        runtime
            .as_ref()
            .map(|runtime| runtime.identity().label())
            .unwrap_or_else(|| "(none wired — a Start is refused)".to_string())
    );
    let log_core = core.clone();
    // The log watcher prints every event as it is recorded — provenance
    // included, credentials already redacted by the engine. Flush after
    // every line: a service whose log is stuck in a buffer is no log.
    tokio::spawn(async move {
        // The log follows the SEQUENCE, not an index: the engine's history
        // is a ring buffer, so an index would reprint or skip events.
        let mut printed_seq: u64 = 0;
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let pending: Vec<String> = {
                // A poisoned lock must not silence the log: recover the core
                // and keep printing (the supervisor marks itself degraded).
                let guard = match log_core.lock() {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
                let lines: Vec<String> = guard
                    .events
                    .iter()
                    .filter(|e| e.seq > printed_seq)
                    .map(|e| {
                        format!(
                            "[{}] {}\n",
                            provenance_label(&e.provenance),
                            render_event(e)
                        )
                    })
                    .collect();
                if let Some(last) = guard.events.last() {
                    printed_seq = last.seq;
                }
                lines
            };
            if pending.is_empty() {
                continue;
            }
            use std::io::Write as _;
            let mut out = std::io::stdout().lock();
            for line in &pending {
                let _ = out.write_all(line.as_bytes());
            }
            let _ = out.flush();
        }
    });
    // The runtime goes WITH the controller: the server hands the objective
    // to it, and its confirmation is what moves the run to `Running`.
    // Serving without it would accept the Start and leave the run in
    // `Starting` forever.
    let served = server::serve(listener, core, runtime).await;
    if let Some(identity) = owned_socket {
        server::unlink_if_owned(std::path::Path::new(&socket), identity);
    }
    if served.is_err() {
        return 1;
    }
    0
}

fn provenance_label(provenance: &supervisor::protocol::Provenance) -> &'static str {
    match provenance {
        supervisor::protocol::Provenance::Human => "human",
        supervisor::protocol::Provenance::Automatic { .. } => "auto",
        supervisor::protocol::Provenance::Telemetry { unknown: false } => "telemetry",
        supervisor::protocol::Provenance::Telemetry { unknown: true } => "telemetry-UNKNOWN",
    }
}

fn parse_action(args: &[String], name: &str) -> Option<SupervisorAction> {
    match name {
        "start" => Some(SupervisorAction::Start {
            project: flag(args, "--project", ""),
            ticket: opt_flag(args, "--ticket"),
            revision: opt_flag(args, "--revision"),
        }),
        "pause" => Some(SupervisorAction::Pause),
        "resume" => Some(SupervisorAction::Resume),
        "stop" => Some(SupervisorAction::Stop),
        "renew" => Some(SupervisorAction::Renew {
            added_seconds: flag(args, "--added-seconds", "300").parse().ok()?,
        }),
        _ => None,
    }
}

/// `hodei-mh sup-status`: the CLI view of the SAME render contract the TUI
/// uses.
pub async fn run_status(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
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
                println!("{line}");
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
    let Some(action) = parse_action(&args[1..], name) else {
        eprintln!("unknown action {name}; expected start|pause|resume|stop|renew");
        return 2;
    };
    let socket = flag(&args[1..], "--socket", DEFAULT_SOCKET);
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
        other => other,
    };
    match client.action(action).await {
        Ok(outcome) => {
            println!(
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
            );
            if outcome.applied { 0 } else { 1 }
        }
        Err(e) => {
            eprintln!("supervisor rejected the request: {e}");
            1
        }
    }
}

/// `hodei-mh sup-events`: replay structured events from a sequence.
pub async fn run_events(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
    let since: u64 = flag(args, "--since", "0").parse().unwrap_or(0);
    let mut client = match client::connect(&socket).await {
        Ok(client) => client,
        Err(e) => {
            eprintln!("{e}");
            return 1;
        }
    };
    match client.events(since).await {
        Ok((events, _next)) => {
            for event in &events {
                println!(
                    "[{}] {}",
                    provenance_label(&event.provenance),
                    render_event(event)
                );
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
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
    let max_events: u64 = flag(args, "--exit-after-events", "0").parse().unwrap_or(0);
    // A viewer whose supervisor never answers must NOT hang forever: the
    // deadline turns a dead supervisor into an explicit failure.
    let deadline_secs: u64 = flag(args, "--deadline-seconds", "60").parse().unwrap_or(60);
    let started = std::time::Instant::now();
    println!(
        "attached terminal viewer: read-only event tail; leaving does not affect the objective"
    );
    let mut seen: u64 = 1;
    let mut delivered: u64 = 0;
    loop {
        match client::connect(&socket).await {
            Ok(mut client) => {
                let result = client.events(seen).await;
                drop(client);
                match result {
                    Ok((events, _next)) => {
                        for event in events {
                            println!("[viewer] {}", render_event(&event));
                            seen = event.seq + 1;
                            delivered += 1;
                        }
                    }
                    Err(e) => println!("[viewer] supervisor rejected the poll: {e}"),
                }
            }
            Err(e) => {
                println!(
                    "[viewer] supervisor not reachable ({e}); still attached, still read-only"
                );
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
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
    let simulated = args.iter().any(|a| a == "--simulated");
    let frames: usize = flag(args, "--frames", "0").parse().unwrap_or(0);
    crate::tui::tui_main(&socket, simulated, frames).await
}

/// Simulated state for DoD #1 (TUI exercised without a supervisor).
pub fn simulated_snapshot() -> Snapshot {
    Snapshot {
        project: "simulated-project".into(),
        phase: hodei_mh_run::run::Phase::Running,
        paused: false,
        ticket: Some("TASK-17".into()),
        revision: Some("0f3e91c".into()),
        resources: ResourceObservation::Measured {
            cpus: 1.5,
            memory_gib: 2.0,
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
            kind: "supervisor/action".into(),
            payload: serde_json::Value::String("Start applied=true".into()),
            provenance: supervisor::protocol::Provenance::Human,
        },
        StructuredEvent {
            seq: 2,
            kind: "telemetry/resources".into(),
            payload: serde_json::Value::String("cpus=1.5 memory_gib=2".into()),
            provenance: supervisor::protocol::Provenance::Telemetry { unknown: false },
        },
        StructuredEvent {
            seq: 3,
            kind: "telemetry/resources".into(),
            payload: serde_json::Value::String("cgroup unavailable".into()),
            provenance: supervisor::protocol::Provenance::Telemetry { unknown: true },
        },
    ]
}
