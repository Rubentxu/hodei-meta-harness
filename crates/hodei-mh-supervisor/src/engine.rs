//! Pure supervisor engine: the ONE state machine both clients drive through
//! the socket. Applies actions to the core, ingests structured events with
//! provenance, and renders snapshots. No I/O — the server is a thin adapter
//! over this.

use run::grant::Grant;
use run::run::{Phase, RunState};
use run::runtime::{ObjectiveOutcome, ObjectiveReport, RuntimeIdentity};

use crate::protocol::{
    ActionOutcome, ResourceObservation, Snapshot, StructuredEvent, SupervisorAction,
};
use crate::provenance::{Provenance, redact};

/// Upper bound on the retained event history. The log is a RING: the
/// oldest events are dropped, the sequence numbers keep growing. A
/// supervisor that ran for days must not grow without bound.
pub const MAX_RETAINED_EVENTS: usize = 1000;

/// Whether a transition the engine applies took effect. A refusal is a
/// real answer with its reason, never a silent no-op.
#[derive(Debug, Clone, PartialEq)]
pub enum Transition {
    Applied,
    Refused { reason: String },
}

impl Transition {
    fn refused(reason: impl Into<String>) -> Self {
        Transition::Refused {
            reason: reason.into(),
        }
    }
}

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
    /// The runtime that executes objectives. Set when the controller is
    /// wired: it is what makes a rehearsal visible as a rehearsal, and
    /// its absence is why a Start is refused instead of pretended.
    pub runtime: Option<RuntimeIdentity>,
    /// What the last objective did. The verdict of an objective is a
    /// recorded fact, not a phase: the session outlives it.
    pub objective: Option<ObjectiveReport>,
    pub resources: ResourceObservation,
    pub context_available: bool,
    pub ttl_remaining_seconds: Option<u64>,
    /// Attached terminal viewers (AC-3): observed, never load-bearing —
    /// their disconnect must not alter the objective.
    pub terminal_viewers: u32,
    pub events: Vec<StructuredEvent>,
    /// Monotonic sequence counter. Independent of `events.len()` so the
    /// ring buffer never renumbers history.
    pub next_seq: u64,
    /// Monotonic epoch of the CURRENT run. Bumped on every Start; it is
    /// NOT the event count (that changes on every event).
    pub epoch: u64,
    pub last_action: Option<ActionOutcome>,
    /// Set when the core had to recover from a poisoned lock: the state
    /// is usable but the incident is visible, never silently swallowed.
    pub degraded: Option<String>,
}

pub fn default_core(project: impl Into<String>) -> SupervisorCore {
    SupervisorCore {
        project: project.into(),
        run: None,
        paused: false,
        ticket: None,
        revision: None,
        runtime: None,
        objective: None,
        resources: ResourceObservation::Unknown {
            reason: "no telemetry yet".into(),
        },
        context_available: false,
        ttl_remaining_seconds: None,
        terminal_viewers: 0,
        events: Vec::new(),
        next_seq: 1,
        epoch: 0,
        last_action: None,
        degraded: None,
    }
}

/// A core wired to a runtime: the runtime's identity is known from the
/// boot, so a client sees the kind of runtime before any objective runs.
pub fn core_with_runtime(project: impl Into<String>, runtime: RuntimeIdentity) -> SupervisorCore {
    SupervisorCore {
        runtime: Some(runtime),
        ..default_core(project)
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
    let seq = core.next_seq;
    core.next_seq += 1;
    let event = StructuredEvent {
        seq,
        kind: kind.into(),
        payload: serde_json::Value::String(redact(
            payload.as_str().unwrap_or(&payload.to_string()),
        )),
        provenance,
    };
    core.events.push(event.clone());
    // Ring buffer: the oldest events fall off, the sequence keeps going.
    if core.events.len() > MAX_RETAINED_EVENTS {
        let excess = core.events.len() - MAX_RETAINED_EVENTS;
        core.events.drain(0..excess);
    }
    event
}

/// Mark the core as degraded after recovering from a poisoned lock. The
/// incident is recorded as an event with automatic provenance.
pub fn mark_degraded(core: &mut SupervisorCore, reason: &str) {
    if core.degraded.is_some() {
        return;
    }
    core.degraded = Some(reason.to_string());
    emit_event(
        core,
        "supervisor/degraded",
        serde_json::Value::String(reason.to_string()),
        Provenance::Automatic {
            rule: "poison-recovery".into(),
        },
    );
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
            if project != &core.project {
                // One controller PER PROJECT: a Start naming another
                // project is refused, not applied under a different name.
                outcome(
                    &action,
                    false,
                    format!(
                        "project mismatch: this supervisor serves '{}', not '{project}'",
                        core.project
                    ),
                )
            } else if core.runtime.is_none() {
                // No runtime, no objective: the controller refuses instead
                // of pretending that a run started.
                outcome(
                    &action,
                    false,
                    "no runtime is wired: this supervisor cannot execute an objective yet",
                )
            } else if core.run.as_ref().is_some_and(|run| {
                // One exclusive controller: a run that is STARTING counts as
                // in flight, or two controllers would share the state.
                matches!(run.phase, Phase::Starting | Phase::Running)
            }) {
                outcome(
                    &action,
                    false,
                    "a run is already in flight: not a second controller",
                )
            } else {
                core.epoch += 1;
                let grant = Grant::new(project, "supervisor-run", core.epoch, vec![]);
                // The run is STARTING, not running: nothing has confirmed
                // that the work began.
                core.run = Some(RunState::pending(grant, "supervisor-session"));
                core.ticket = ticket.clone();
                core.revision = revision.clone();
                core.paused = false;
                outcome(
                    &action,
                    true,
                    format!(
                        "starting for {project} (epoch {}); awaiting the runtime's confirmation",
                        core.epoch
                    ),
                )
            }
        }
        SupervisorAction::Pause => {
            if core.paused {
                outcome(&action, false, "already paused")
            } else if core.run.as_ref().is_some_and(|r| r.phase == Phase::Running) {
                core.paused = true;
                outcome(&action, true, "prompt loop paused")
            } else if core
                .run
                .as_ref()
                .is_some_and(|r| r.phase == Phase::Starting)
            {
                outcome(
                    &action,
                    false,
                    "the run has not started yet: there is no prompt loop to pause",
                )
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
            // A run that is still starting is stopped too: nobody is left
            // waiting for a confirmation that will never arrive.
            Some(run) if matches!(run.phase, Phase::Running | Phase::Starting) => {
                let was_starting = run.phase == Phase::Starting;
                let cancelled = run.stop();
                core.paused = false;
                let detail = format!(
                    "stopped{}; cancelled_processes={:?}; irreversible={:?}",
                    if was_starting {
                        " before the runtime confirmed"
                    } else {
                        ""
                    },
                    cancelled,
                    run.initiated_irreversible
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

/// The runtime CONFIRMED the objective: the run is running now.
///
/// The confirmation carries the epoch of the run it belongs to: a late
/// confirmation of a run that already ended can never confirm another
/// one.
pub fn confirm_runtime(core: &mut SupervisorCore, epoch: u64) -> Transition {
    if core.epoch != epoch {
        return Transition::refused(format!(
            "the confirmation belongs to another run (epoch {epoch}; the current run is {})",
            core.epoch
        ));
    }
    let Some(run) = core.run.as_mut() else {
        return Transition::refused("there is no run to confirm");
    };
    if !run.confirm() {
        return Transition::refused(format!(
            "the run is not starting (phase {:?}): it cannot be confirmed",
            run.phase
        ));
    }
    emit_event(
        core,
        "objective/confirmed",
        serde_json::Value::String(format!(
            "the runtime confirmed the objective (epoch {epoch})"
        )),
        // The confirmation comes from the runtime, not from a person.
        Provenance::Automatic {
            rule: "runtime-confirmation".into(),
        },
    );
    Transition::Applied
}

/// The runtime reported how the objective ended.
///
/// The verdict is RECORDED, and every outcome has a transition: a
/// confirmed objective leaves the session alive, a rejected or failed
/// one ends it, and an idle objective returns the supervisor to waiting.
pub fn record_outcome(
    core: &mut SupervisorCore,
    epoch: u64,
    outcome: ObjectiveOutcome,
) -> Transition {
    if core.epoch != epoch {
        return Transition::refused(format!(
            "the outcome belongs to another run (epoch {epoch}; the current run is {})",
            core.epoch
        ));
    }
    // The outcome belongs to a LIVE run: a run that was stopped or
    // dropped does not get a verdict afterwards.
    let live = core
        .run
        .as_ref()
        .is_some_and(|run| matches!(run.phase, Phase::Starting | Phase::Running));
    if !live {
        return Transition::refused("the run is no longer live: the outcome is not recorded");
    }
    let work_ref = match &outcome {
        ObjectiveOutcome::Confirmed { work_ref, .. }
        | ObjectiveOutcome::Rejected { work_ref, .. } => Some(work_ref.clone()),
        ObjectiveOutcome::Failed { .. } | ObjectiveOutcome::Idle { .. } => None,
    };
    let detail = outcome.describe();
    match outcome {
        ObjectiveOutcome::Confirmed { .. } => {
            // The objective held: the session continues.
        }
        ObjectiveOutcome::Rejected { .. } | ObjectiveOutcome::Failed { .. } => {
            // The objective did not hold, or the execution failed: the
            // session ends with the reason visible.
            if let Some(run) = core.run.as_mut() {
                run.stop();
            }
        }
        ObjectiveOutcome::Idle { .. } => {
            // Nothing to do: the supervisor waits, and a new objective can
            // be started later.
            core.run = None;
        }
    }
    core.objective = Some(ObjectiveReport { work_ref, outcome });
    emit_event(
        core,
        "objective/outcome",
        serde_json::Value::String(detail),
        Provenance::Automatic {
            rule: "runtime-outcome".into(),
        },
    );
    Transition::Applied
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
        runtime: core.runtime.clone(),
        objective: core.objective.clone(),
        resources: core.resources.clone(),
        context_available: core.context_available,
        wait_reason: core.paused.then(|| "paused by user".to_string()),
        last_action: core.last_action.clone(),
        ttl_remaining_seconds: core.ttl_remaining_seconds,
        epoch: core.epoch,
        // The grant's effect scope, stated explicitly: an empty scope
        // means the supervisor authorizes NO effect by itself.
        grant_effects: core
            .run
            .as_ref()
            .map(|run| {
                run.grant
                    .effects
                    .iter()
                    .map(|effect| format!("{effect:?}"))
                    .collect()
            })
            .unwrap_or_default(),
        degraded: core.degraded.clone(),
    }
}

pub fn events_since(core: &SupervisorCore, since: u64) -> (Vec<StructuredEvent>, u64) {
    let next_seq = core.next_seq;
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

    /// A core wired to a runtime: a controller that can execute.
    fn wired(project: &str) -> SupervisorCore {
        core_with_runtime(project, RuntimeIdentity::rehearsal("trial"))
    }

    /// Start the run and let the runtime confirm it. Returns the epoch.
    fn started(core: &mut SupervisorCore) -> u64 {
        let project = core.project.clone();
        let start = apply(
            core,
            SupervisorAction::Start {
                project,
                ticket: None,
                revision: None,
            },
        );
        assert!(start.applied, "{}", start.detail);
        let epoch = core.epoch;
        assert_eq!(confirm_runtime(core, epoch), Transition::Applied);
        epoch
    }

    #[test]
    fn a_start_without_a_runtime_is_refused_instead_of_pretended() {
        let mut core = default_core("gc");
        let start = apply(
            &mut core,
            SupervisorAction::Start {
                project: "gc".into(),
                ticket: None,
                revision: None,
            },
        );
        assert!(!start.applied);
        assert!(
            start.detail.contains("no runtime is wired"),
            "{}",
            start.detail
        );
        assert!(core.run.is_none());
    }

    #[test]
    fn a_run_is_running_only_after_the_runtime_confirms_it() {
        let mut core = wired("gc");
        let project = core.project.clone();
        let start = apply(
            &mut core,
            SupervisorAction::Start {
                project,
                ticket: Some("TASK-9".into()),
                revision: Some("abc123".into()),
            },
        );
        assert!(start.applied);
        // STARTING: nothing has confirmed that the work began.
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Starting)
        );
        assert_eq!(core.ticket.as_deref(), Some("TASK-9"));

        // The confirmation is what makes it running.
        let epoch = core.epoch;
        assert_eq!(confirm_runtime(&mut core, epoch), Transition::Applied);
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Running)
        );
    }

    #[test]
    fn a_confirmation_of_another_run_is_refused() {
        let mut core = wired("gc");
        started(&mut core);
        // A late confirmation of a run that already ended must never
        // confirm the next one.
        assert!(matches!(
            confirm_runtime(&mut core, 99),
            Transition::Refused { .. }
        ));
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Running)
        );
    }

    #[test]
    fn every_outcome_has_a_transition() {
        // A confirmed objective leaves the session alive.
        let mut core = wired("gc");
        let epoch = started(&mut core);
        assert_eq!(
            record_outcome(
                &mut core,
                epoch,
                ObjectiveOutcome::Confirmed {
                    work_ref: "TASK-1".into(),
                    detail: "published".into(),
                },
            ),
            Transition::Applied
        );
        assert_eq!(snapshot(&core).phase, Phase::Running);
        assert!(snapshot(&core).objective.is_some());

        // A rejected one ends it.
        let mut core = wired("gc");
        let epoch = started(&mut core);
        record_outcome(
            &mut core,
            epoch,
            ObjectiveOutcome::Rejected {
                work_ref: "TASK-2".into(),
                reason: "the evidence did not hold".into(),
            },
        );
        assert_eq!(snapshot(&core).phase, Phase::Stopped);

        // A failed one ends it.
        let mut core = wired("gc");
        let epoch = started(&mut core);
        record_outcome(
            &mut core,
            epoch,
            ObjectiveOutcome::Failed {
                reason: "the sandbox could not be created".into(),
            },
        );
        assert_eq!(snapshot(&core).phase, Phase::Stopped);

        // An idle one leaves the supervisor waiting, with no run.
        let mut core = wired("gc");
        let epoch = started(&mut core);
        record_outcome(
            &mut core,
            epoch,
            ObjectiveOutcome::Idle {
                reason: "no eligible work".into(),
            },
        );
        assert_eq!(snapshot(&core).phase, Phase::Idle);
        assert!(core.run.is_none());
    }

    #[test]
    fn an_outcome_of_a_stopped_run_is_not_recorded() {
        let mut core = wired("gc");
        let epoch = started(&mut core);
        apply(&mut core, SupervisorAction::Stop);
        assert!(matches!(
            record_outcome(
                &mut core,
                epoch,
                ObjectiveOutcome::Confirmed {
                    work_ref: "TASK-1".into(),
                    detail: "published".into(),
                },
            ),
            Transition::Refused { .. }
        ));
    }

    #[test]
    fn the_snapshot_states_the_runtime_kind() {
        let core = wired("gc");
        let snap = snapshot(&core);
        let runtime = snap.runtime.expect("the wired runtime is visible");
        assert_eq!(runtime.kind, run::runtime::RuntimeKind::Rehearsal);
        // A rehearsal is never presented as a live execution.
        assert!(runtime.label().starts_with("rehearsal"));
    }

    #[test]
    fn start_is_exclusive_and_pauses_and_resumes() {
        let mut core = wired("gc");
        let project = core.project.clone();
        let start = apply(
            &mut core,
            SupervisorAction::Start {
                project,
                ticket: Some("TASK-9".into()),
                revision: Some("abc123".into()),
            },
        );
        assert!(start.applied);
        assert_eq!(core.ticket.as_deref(), Some("TASK-9"));

        // A second controller is refused with a real reason — also while
        // the first run is still starting.
        let project = core.project.clone();
        let second = apply(
            &mut core,
            SupervisorAction::Start {
                project,
                ticket: None,
                revision: None,
            },
        );
        assert!(!second.applied);
        assert!(
            second.detail.contains("second controller"),
            "{}",
            second.detail
        );

        // A run that has not started has no prompt loop to pause.
        let pause = apply(&mut core, SupervisorAction::Pause);
        assert!(!pause.applied);

        let epoch = core.epoch;
        confirm_runtime(&mut core, epoch);
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
    fn a_run_that_is_starting_can_be_stopped() {
        let mut core = wired("gc");
        let project = core.project.clone();
        apply(
            &mut core,
            SupervisorAction::Start {
                project,
                ticket: None,
                revision: None,
            },
        );
        // Nobody is left waiting for a confirmation that will not arrive.
        let stop = apply(&mut core, SupervisorAction::Stop);
        assert!(stop.applied);
        assert!(
            stop.detail.contains("before the runtime confirmed"),
            "{}",
            stop.detail
        );
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Stopped)
        );
    }

    #[test]
    fn a_start_for_another_project_is_refused_not_applied() {
        let mut core = default_core("alpha");
        let start = apply(
            &mut core,
            SupervisorAction::Start {
                project: "beta".into(),
                ticket: None,
                revision: None,
            },
        );
        assert!(!start.applied);
        assert!(
            start.detail.contains("project mismatch"),
            "{}",
            start.detail
        );
        // The core keeps serving ITS project, with no run started.
        assert!(core.run.is_none());
        assert_eq!(snapshot(&core).project, "alpha");
        assert_eq!(snapshot(&core).phase, Phase::Idle);
    }

    #[test]
    fn the_epoch_advances_only_on_start_not_on_every_event() {
        let mut core = wired("gc");
        started(&mut core);
        let epoch_after_start = core.epoch;
        assert_eq!(epoch_after_start, 1);
        // Events (and other actions) do not move the epoch.
        ingest_telemetry(
            &mut core,
            ResourceObservation::Unknown {
                reason: "cgroup unavailable".into(),
            },
        );
        viewer_connected(&mut core);
        apply(&mut core, SupervisorAction::Pause);
        assert_eq!(core.epoch, epoch_after_start);
        // A new run after a stop advances it again.
        apply(&mut core, SupervisorAction::Stop);
        let second = started(&mut core);
        assert_eq!(core.epoch, 2);
        assert_eq!(second, 2);
    }

    #[test]
    fn the_snapshot_states_the_grant_scope_instead_of_implying_one() {
        let mut core = wired("gc");
        started(&mut core);
        let snap = snapshot(&core);
        assert_eq!(snap.phase, Phase::Running);
        // The supervisor run authorizes no effect on its own — and says so.
        assert!(
            snap.grant_effects.is_empty(),
            "the declared scope must match the grant: {:?}",
            snap.grant_effects
        );
    }

    #[test]
    fn the_event_history_is_bounded_and_the_sequence_keeps_growing() {
        let mut core = default_core("gc");
        for index in 0..(MAX_RETAINED_EVENTS + 50) {
            emit_event(
                &mut core,
                "test/event",
                serde_json::Value::String(format!("event {index}")),
                Provenance::Human,
            );
        }
        assert_eq!(core.events.len(), MAX_RETAINED_EVENTS);
        // The oldest events fell off, but the sequence never restarted.
        assert_eq!(core.events.first().map(|e| e.seq), Some(51));
        assert_eq!(core.next_seq, MAX_RETAINED_EVENTS as u64 + 51);
        let (events, next_seq) = events_since(&core, 1);
        assert_eq!(events.len(), MAX_RETAINED_EVENTS);
        assert_eq!(next_seq, MAX_RETAINED_EVENTS as u64 + 51);
    }

    #[test]
    fn stop_reports_irreversibles_and_refusals_are_real_answers() {
        let mut core = wired("gc");
        let stop_without_run = apply(&mut core, SupervisorAction::Stop);
        assert!(!stop_without_run.applied);
        assert!(stop_without_run.detail.contains("no running run"));

        started(&mut core);
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
        let mut core = wired("gc");
        started(&mut core);
        let _ = viewer_connected(&mut core);
        viewer_disconnected(&mut core);
        // The objective is untouched by the viewer leaving.
        assert_eq!(core.terminal_viewers, 0);
        assert_eq!(
            core.run.as_ref().map(|r| r.phase.clone()),
            Some(Phase::Running)
        );

        let (all, next) = events_since(&core, 1);
        // The start and its confirmation are part of the history.
        assert_eq!(all.len(), 4);
        assert_eq!(next, 5);
        let (tail, _) = events_since(&core, 3);
        assert_eq!(tail.len(), 2);
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
