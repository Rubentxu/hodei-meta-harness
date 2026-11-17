//! Ratatui TUI (TASK-15): a CLIENT of the supervisor socket. It renders the
//! same render contract as the CLI, drives the SAME actions through the
//! SAME socket, reconnects when the supervisor is absent — and never
//! starts a second controller (AC-1/AC-2). `--simulated` exercises the
//! surface without a supervisor (DoD #1) and `--frames N` renders
//! non-interactively for real local sessions under test.

use std::time::Duration;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::{Frame, Terminal, TerminalOptions, Viewport, layout::Rect, text::Line};

use supervisor::client::{self, SupervisorClient};
use supervisor::protocol::{Snapshot, SupervisorAction};
use supervisor::render::{render_event, render_snapshot};

struct TuiState {
    snapshot: Option<Snapshot>,
    events: Vec<String>,
    banner: Option<String>,
    simulated: bool,
    quit: bool,
}

impl TuiState {
    fn new(simulated: bool) -> Self {
        Self {
            snapshot: None,
            events: Vec::new(),
            banner: None,
            simulated,
            quit: false,
        }
    }
}

/// One observation round: fetch snapshot + events from the supervisor, or
/// report the socket as unreachable (and keep retrying — reconnect, never
/// spawn).
async fn observe(client: &mut Option<SupervisorClient>, socket: &str, state: &mut TuiState) {
    if state.simulated {
        state.snapshot = Some(crate::supervisor_cli::simulated_snapshot());
        state.events = crate::supervisor_cli::simulated_events()
            .iter()
            .map(render_event)
            .collect();
        state.banner = Some("SIMULATED state — nothing here is a live supervisor".into());
        return;
    }
    if client.is_none() {
        match client::connect(socket).await {
            Ok(new_client) => *client = Some(new_client),
            Err(e) => {
                state.snapshot = None;
                state.banner = Some(format!("supervisor not reachable — reconnecting ({e})"));
                return;
            }
        }
    }
    let Some(open_client) = client.as_mut() else {
        return;
    };
    match open_client.snapshot().await {
        Ok(snapshot) => state.snapshot = Some(snapshot),
        Err(e) => {
            *client = None;
            state.banner = Some(format!("supervisor connection lost — reconnecting ({e})"));
            return;
        }
    }
    if let Some(open_client) = client.as_mut() {
        match open_client.events(1).await {
            Ok((events, _)) => {
                state.events = events.iter().map(render_event).collect();
            }
            Err(e) => {
                *client = None;
                state.banner = Some(format!("supervisor connection lost — reconnecting ({e})"));
            }
        }
    }
}

async fn drive_action(
    client: &mut Option<SupervisorClient>,
    socket: &str,
    state: &mut TuiState,
    action: SupervisorAction,
) {
    if state.simulated {
        state.banner = Some("SIMULATED mode: actions are NOT sent anywhere".into());
        return;
    }
    if client.is_none() {
        match client::connect(socket).await {
            Ok(new_client) => *client = Some(new_client),
            Err(e) => {
                state.banner = Some(format!("supervisor not reachable — action not sent ({e})"));
                return;
            }
        }
    }
    if let Some(open_client) = client.as_mut() {
        match open_client.action(action).await {
            Ok(outcome) => {
                state.banner = Some(format!(
                    "{}: {}",
                    if outcome.applied {
                        "applied"
                    } else {
                        "refused"
                    },
                    outcome.detail
                ));
            }
            Err(e) => {
                *client = None;
                state.banner = Some(format!("action failed ({e}) — reconnecting"));
            }
        }
    }
}

fn ui(f: &mut Frame, state: &TuiState) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(45),
            Constraint::Percentage(45),
            Constraint::Length(1),
        ])
        .split(f.area());

    let mut state_lines: Vec<Line> = state
        .snapshot
        .as_ref()
        .map(|snapshot| {
            render_snapshot(snapshot)
                .into_iter()
                .map(Line::from)
                .collect()
        })
        .unwrap_or_else(|| vec![Line::from("(no snapshot yet — waiting for the supervisor)")]);
    if let Some(banner) = &state.banner {
        state_lines.push(Line::from(String::new()));
        state_lines.push(Line::from(format!("!! {banner}")));
    }
    let state_panel = Paragraph::new(state_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("supervisor state"),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(state_panel, chunks[0]);

    let mut event_lines: Vec<Line> = state.events.iter().map(|e| Line::from(e.clone())).collect();
    if event_lines.is_empty() {
        event_lines.push(Line::from("(no events yet)"));
    }
    let events_panel = Paragraph::new(event_lines)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("structured events"),
        )
        .wrap(Wrap { trim: false });
    f.render_widget(events_panel, chunks[1]);

    let footer = Paragraph::new(Line::from(
        "keys: s=start p=pause r=resume t=stop n=renew(+300s) q=quit",
    ));
    f.render_widget(footer, chunks[2]);
}

fn draw_once(terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>, state: &TuiState) {
    let _ = terminal.draw(|f| ui(f, state));
}

pub async fn tui_main(socket: &str, simulated: bool, frames: usize) -> i32 {
    let stdout = std::io::stdout();
    let backend = CrosstermBackend::new(stdout);
    // Non-tty stdout (captured/piped) reports a 0x0 size; a FIXED viewport
    // keeps the frames real even when the surface is not a terminal.
    let mut terminal = match Terminal::with_options(
        backend,
        TerminalOptions {
            viewport: Viewport::Fixed(Rect::new(0, 0, 110, 34)),
        },
    ) {
        Ok(terminal) => terminal,
        Err(e) => {
            eprintln!("cannot init TUI terminal: {e}");
            return 1;
        }
    };

    let mut state = TuiState::new(simulated);
    let mut client: Option<SupervisorClient> = None;

    // Non-interactive probe mode: render N frames of a REAL local session
    // (or the simulated state) and leave. No raw mode, no alternate screen.
    if frames > 0 {
        for _ in 0..frames {
            observe(&mut client, socket, &mut state).await;
            draw_once(&mut terminal, &state);
            tokio::time::sleep(Duration::from_millis(150)).await;
        }
        return 0;
    }

    if !simulated {
        if let Err(e) = crossterm::terminal::enable_raw_mode() {
            eprintln!("cannot enable raw mode: {e}");
            return 1;
        }
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::EnterAlternateScreen);
    }

    loop {
        observe(&mut client, socket, &mut state).await;
        let _ = terminal.draw(|f| ui(f, &state));

        let has_event = event::poll(Duration::from_millis(400)).unwrap_or(false);
        if has_event
            && let Ok(Event::Key(key)) = event::read()
            && key.kind == KeyEventKind::Press
        {
            match key.code {
                KeyCode::Char('q') => state.quit = true,
                KeyCode::Char('s') => {
                    let project = state
                        .snapshot
                        .as_ref()
                        .map(|s| s.project.clone())
                        .unwrap_or_else(|| "hodei-mh".into());
                    drive_action(
                        &mut client,
                        socket,
                        &mut state,
                        SupervisorAction::Start {
                            project,
                            ticket: None,
                            revision: None,
                        },
                    )
                    .await;
                }
                KeyCode::Char('p') => {
                    drive_action(&mut client, socket, &mut state, SupervisorAction::Pause).await;
                }
                KeyCode::Char('r') => {
                    drive_action(&mut client, socket, &mut state, SupervisorAction::Resume).await;
                }
                KeyCode::Char('t') => {
                    drive_action(&mut client, socket, &mut state, SupervisorAction::Stop).await;
                }
                KeyCode::Char('n') => {
                    drive_action(
                        &mut client,
                        socket,
                        &mut state,
                        SupervisorAction::Renew { added_seconds: 300 },
                    )
                    .await;
                }
                _ => {}
            }
        }
        if state.quit {
            break;
        }
    }

    if !simulated {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stdout(), crossterm::terminal::LeaveAlternateScreen);
    }
    0
}
