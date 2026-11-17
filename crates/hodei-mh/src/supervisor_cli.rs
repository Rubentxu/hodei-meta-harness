//! Supervisor surface wiring (TASK-15): the socket server, the plain CLI
//! clients (status/actions/events), the attached terminal viewer (AC-3:
//! read-only, its disconnect does not alter the objective) and the Ratatui
//! TUI. ALL of them are clients of the SAME socket contract.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use supervisor::client::{self, SupervisorClient};
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
pub async fn run_supervisor(args: &[String]) -> i32 {
    let socket = flag(args, "--socket", DEFAULT_SOCKET);
    let project = flag(args, "--project", "hodei-mh");
    let listener = match server::bind(&socket).await {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("cannot bind supervisor socket: {e}");
            return 1;
        }
    };
    let core: Arc<Mutex<engine::SupervisorCore>> = Arc::new(Mutex::new({
        let mut core = engine::default_core(project);
        // The boot decision is the supervisor's own: logged with automatic
        // provenance (AC-4: automatic decisions state their rule).
        engine::emit_event(
            &mut core,
            "supervisor/boot",
            serde_json::Value::String(format!(
                "listening on {socket}; credentials will be redacted"
            )),
            supervisor::protocol::Provenance::Automatic {
                rule: "boot".into(),
            },
        );
        core
    }));
    println!(
        "supervisor listening on {socket}; logs identify provenance, credentials are redacted"
    );
    let log_core = core.clone();
    // The log watcher prints every event as it is recorded — provenance
    // included, credentials already redacted by the engine. Flush after
    // every line: a service whose log is stuck in a buffer is no log.
    tokio::spawn(async move {
        let mut printed = 0usize;
        loop {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let pending: Vec<String> = {
                let Ok(guard) = log_core.lock() else {
                    return;
                };
                let start = printed.min(guard.events.len());
                let lines: Vec<String> = guard.events[start..]
                    .iter()
                    .map(|e| {
                        format!(
                            "[{}] {}\n",
                            provenance_label(&e.provenance),
                            render_event(e)
                        )
                    })
                    .collect();
                printed = guard.events.len();
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
    let served = server::serve(listener, core).await;
    let _ = std::fs::remove_file(&socket);
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
            project: flag(args, "--project", "hodei-mh"),
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
                    "confirmation is real"
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

#[allow(dead_code)]
fn _unused_helper(_client: &SupervisorClient) {}
