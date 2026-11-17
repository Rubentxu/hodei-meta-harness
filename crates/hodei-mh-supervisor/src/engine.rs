//! Pure supervisor engine: the ONE state machine both clients drive through
//! the socket. Applies actions to the core, ingests structured events with
//! provenance, and renders snapshots. No I/O — the server is a thin adapter
//! over this.

use run::grant::Grant;
use run::run::{Phase, RunState};

use crate::protocol::{
    ActionOutcome, ResourceObservation, Snapshot, StructuredEvent, SupervisorAction,
};
use crate::provenance::{Provenance, redact};

/// The supervisor's whole observable world. `run` reuses the run crate's
/// state machine — the supervisor adds pause/wait semantics around it, it
/// does NOT reimplement session control.
#[derive(Debug, Clone, PartialEq)]
pub struct SupervisorCore {
    pub project: String,
    pub run: Option<RunState>,
    /// Prompt loop gated by Pause/Resume without touching the session.
    pub paused: bool,
    pub ticket: Option<String>,
    pub revision: Option<String>,
    pub resources: ResourceObservation,
    pub context_available: bool,
    pub ttl_remaining_seconds: Option<u64>,
    /// Attached terminal viewers (AC-3): observed, never load-bearing —
    /// their disconnect must not alter the objective.
    pub terminal_viewers: u32,
    pub events: Vec<StructuredEvent>,
    pub last_action: Option<ActionOutcome>,
}

pub fn default_core(project: impl Into<String>) -> SupervisorCore {
    SupervisorCore {
        project: project.into(),
        run: None,
        paused: false,
        ticket: None,
        revision: None,
        resources: ResourceObservation::Unknown {
            reason: "no telemetry yet".into(),
        },
        context_available: false,
        ttl_remaining_seconds: None,
        terminal_viewers: 0,
        events: Vec::new(),
        last_action: None,
    }
}

/// Record a structured event with provenance; credential-bearing text is
/// redacted BEFORE it enters the record (AC-4).
pub fn emit_event(
    core: &mut SupervisorCore,
    kind: impl Into<String>,
    payload: serde_json::Value,
    provenance: Provenance,
) -> StructuredEvent {
    let seq = core.events.len() as u64 + 1;
    let event = StructuredEvent {
        seq,
        kind: kind.into(),
        payload: serde_json::Value::String(redact(
            payload.as_str().unwrap_or(&payload.to_string()),
        )),
        provenance,
    };
    core.events.push(event.clone());
    event
}

/// Ingest observed telemetry. `unknown: true` is recorded AS unknown: the
/// snapshot never upgrades missing telemetry into a measurement or a
/// success.
pub fn ingest_telemetry(
    core: &mut SupervisorCore,
    observation: ResourceObservation,
) -> StructuredEvent {
    core.resources = observation.clone();
    let (unknown, reason) = match &observation {
        ResourceObservation::Measured { cpus, memory_gib } => {
            (false, format!("cpus={cpus} memory_gib={memory_gib}"))
        }
        ResourceObservation::Unknown { reason } => (true, reason.clone()),
    };
    emit_event(
        core,
        "telemetry/resources",
        serde_json::Value::String(reason),
        Provenance::Telemetry { unknown },
    )
}

/// Terminal viewers join and leave freely (AC-3): counted for the record,
/// but NOTHING about the run depends on them.
pub fn viewer_connected(core: &mut SupervisorCore) -> StructuredEvent {
    core.terminal_viewers += 1;
    emit_event(
        core,
        "viewer/connected",
        serde_json::Value::String(format!("viewers={}", core.terminal_viewers)),
        Provenance::Human,
    )
}

pub fn viewer_disconnected(core: &mut SupervisorCore) -> StructuredEvent {
    core.terminal_viewers = core.terminal_viewers.saturating_sub(1);
    emit_event(
        core,
        "viewer/disconnected",
        serde_json::Value::String(format!("viewers={}", core.terminal_viewers)),
        Provenance::Human,
    )
}

fn outcome(action: &SupervisorAction, applied: bool, detail: impl Into<String>) -> ActionOutcome {
    ActionOutcome {
        action: action.clone(),
        applied,
        detail: detail.into(),
    }
}

/// Apply a client action. Every refusal is a REAL answer with its reason —
/// never a silent no-op dressed as success.
pub fn apply(core: &mut SupervisorCore, action: SupervisorAction) -> ActionOutcome {
    let outcome = match &action {
        SupervisorAction::Start {
            project,
            ticket,
            revision,
        } => {
            if core
                .run
                .as_ref()
                .is_some_and(|run| run.phase == Phase::Running)
            {
                // One exclusive controller: a second Start is refused, not
                // tolerated silently.
                outcome(&action, false, "already running: not a second controller")
            } else {
                let grant = Grant::new(project, "supervisor-run", core.events.len() as u64, vec![]);
                core.run = Some(RunState::start(grant, "supervisor-session"));
                core.ticket = ticket.clone();
                core.revision = revision.clone();
                core.paused = false;
                outcome(&action, true, format!("run started for {project}"))
            }
        }
        SupervisorAction::Pause => {
            if core.paused {
                outcome(&action, false, "already paused")
            } else if core.run.as_ref().is_some_and(|r| r.phase == Phase::Running) {
                core.paused = true;
                outcome(&action, true, "prompt loop paused")
            } else {
                outcome(&action, false, "no running run to pause")
            }
        }
        SupervisorAction::Resume => {
            if core.paused {
                core.paused = false;
                outcome(&action, true, "prompt loop resumed")
            } else {
                outcome(&action, false, "not paused")
            }
        }
        SupervisorAction::Stop => match core.run.as_mut() {
            Some(run) if run.phase == Phase::Running => {
                let cancelled = run.stop();
                core.paused = false;
                let detail = format!(
                    "stopped; cancelled_processes={:?}; irreversible={:?}",
                    cancelled, run.initiated_irreversible
                );
                outcome(&action, true, detail)
            }
            _ => outcome(&action, false, "no running run to stop"),
        },
        SupervisorAction::Renew { added_seconds } => match core.ttl_remaining_seconds {
            Some(remaining) => {
                core.ttl_remaining_seconds = Some(remaining + added_seconds);
                outcome(&action, true, format!("ttl extended by {added_seconds}s"))
            }
            None => outcome(&action, false, "no managed environment ttl to renew"),
        },
    };
    let event_detail = outcome.detail.clone();
    let applied = outcome.applied;
    core.last_action = Some(outcome.clone());
    emit_event(
        core,
        "supervisor/action",
        serde_json::Value::String(format!(
            "{action:?} applied={applied} detail={event_detail}"
        )),
        Provenance::Human,
    );
    outcome
}

/// The snapshot both clients render — the SAME view for TUI and CLI.
pub fn snapshot(core: &SupervisorCore) -> Snapshot {
    Snapshot {
        project: core.project.clone(),
        phase: core
            .run
            .as_ref()
            .map(|r| r.phase.clone())
            .unwrap_or(Phase::Idle),
        paused: core.paused,
        ticket: core.ticket.clone(),
        revision: core.revision.clone(),
        resources: core.resources.clone(),
        context_available: core.context_available,
        wait_reason: core.paused.then(|| "paused by user".to_string()),
        last_action: core.last_action.clone(),
        ttl_remaining_seconds: core.ttl_remaining_seconds,
    }
}

pub fn events_since(core: &SupervisorCore, since: u64) -> (Vec<StructuredEvent>, u64) {
    let next_seq = core.events.len() as u64 + 1;
    let events = core
        .events
        .iter()
        .filter(|e| e.seq >= since)
        .cloned()
        .collect();
    (events, next_seq)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_is_exclusive_and_pauses_and_resumes() {
        let mut core = default_core("gc");
        let start = apply(
            &mut core,
            SupervisorAction::Start {
                project: "gc".into(),
                ticket: Some("TASK-9".into()),
                revision: Some("abc123".into()),
            },
        );
        assert!(start.applied);
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Running)
        );
        assert_eq!(core.ticket.as_deref(), Some("TASK-9"));

        // A second controller is refused with a real reason.
        let second = apply(
            &mut core,
            SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        );
        assert!(!second.applied);
        assert!(second.detail.contains("second controller"));

        let pause = apply(&mut core, SupervisorAction::Pause);
        assert!(pause.applied);
        assert_eq!(
            snapshot(&core).wait_reason.as_deref(),
            Some("paused by user")
        );
        let pause_again = apply(&mut core, SupervisorAction::Pause);
        assert!(!pause_again.applied);
        let resume = apply(&mut core, SupervisorAction::Resume);
        assert!(resume.applied);
        assert!(!core.paused);
    }

    #[test]
    fn stop_reports_irreversibles_and_refusals_are_real_answers() {
        let mut core = default_core("gc");
        let stop_without_run = apply(&mut core, SupervisorAction::Stop);
        assert!(!stop_without_run.applied);
        assert!(stop_without_run.detail.contains("no running run"));

        let _ = apply(
            &mut core,
            SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        );
        if let Some(run) = core.run.as_mut() {
            run.mark_initiated("git bundle written to staging");
        }
        let stop = apply(&mut core, SupervisorAction::Stop);
        assert!(stop.applied);
        assert!(stop.detail.contains("git bundle written to staging"));
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Stopped)
        );
    }

    #[test]
    fn renew_works_only_with_a_tracked_ttl() {
        let mut core = default_core("gc");
        let renew = apply(&mut core, SupervisorAction::Renew { added_seconds: 300 });
        assert!(!renew.applied);
        core.ttl_remaining_seconds = Some(120);
        let renew = apply(&mut core, SupervisorAction::Renew { added_seconds: 300 });
        assert!(renew.applied);
        assert_eq!(core.ttl_remaining_seconds, Some(420));
    }

    #[test]
    fn telemetry_is_recorded_as_unknown_when_it_is_unknown() {
        let mut core = default_core("gc");
        ingest_telemetry(
            &mut core,
            ResourceObservation::Unknown {
                reason: "cgroup unavailable".into(),
            },
        );
        let snap = snapshot(&core);
        assert_eq!(
            snap.resources,
            ResourceObservation::Unknown {
                reason: "cgroup unavailable".into()
            }
        );
        let last = core.events.last().expect("telemetry event");
        assert_eq!(last.provenance, Provenance::Telemetry { unknown: true });
    }

    #[test]
    fn viewer_disconnect_does_not_touch_the_run_and_events_replay_by_seq() {
        let mut core = default_core("gc");
        let _ = apply(
            &mut core,
            SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        );
        let _ = viewer_connected(&mut core);
        viewer_disconnected(&mut core);
        // The objective is untouched by the viewer leaving.
        assert_eq!(core.terminal_viewers, 0);
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Running)
        );

        let (all, next) = events_since(&core, 1);
        assert_eq!(all.len(), 3);
        assert_eq!(next, 4);
        let (tail, _) = events_since(&core, 3);
        assert_eq!(tail.len(), 1);
        assert_eq!(tail[0].seq, 3);
    }

    #[test]
    fn secrets_are_redacted_before_entering_the_record() {
        let mut core = default_core("gc");
        emit_event(
            &mut core,
            "agent/env",
            serde_json::Value::String("launch with ANTHROPIC_AUTH_TOKEN=sk-secret-1".into()),
            Provenance::Automatic {
                rule: "spawn-agent".into(),
            },
        );
        let recorded = core.events.last().expect("event");
        let text = serde_json::to_string(&recorded.payload).expect("encode");
        assert!(!text.contains("sk-secret-1"));
        assert!(text.contains("[redacted]"));
    }
}
