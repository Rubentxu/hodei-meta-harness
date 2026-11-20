//! Pure supervisor engine: the ONE state machine both clients drive through
//! the socket. Applies actions to the core, ingests structured events with
//! provenance, and renders snapshots. No I/O — the server is a thin adapter
//! over this.

use run::grant::Grant;
use run::run::{Phase, RunState};
use run::runtime::{ObjectiveOutcome, ObjectiveReport, RenewOutcome, RuntimeIdentity};

use crate::protocol::{
    ActionOutcome, EventPage, PROTOCOL_VERSION, ResourceObservation, Snapshot, StructuredEvent,
    SupervisorAction,
};
use crate::provenance::{Provenance, redact_value};

/// Upper bound on the retained event history. The log is a RING: the
/// oldest events are dropped, the sequence numbers keep growing. A
/// supervisor that ran for days must not grow without bound.
pub const MAX_RETAINED_EVENTS: usize = 1000;

/// How many events one page of the history carries at most. The client
/// asks for the next page with the cursor the page returns.
pub const MAX_EVENTS_PER_PAGE: usize = 256;

/// How many bytes the body of one event may weigh. A body that does not
/// fit is REPLACED by a marker that says so: it is never quietly cut.
pub const MAX_EVENT_PAYLOAD_BYTES: usize = 4 * 1024;

/// The ceiling of the TTL of a managed environment. A renewal that would
/// go beyond it is refused with its reason.
pub const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

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
    /// Why the objective was cancelled. A stop never claims that the
    /// work held: the outcome of an interrupted objective is uncertain
    /// and the snapshot says why it is not known.
    pub stop_reason: Option<String>,
    /// Whether an attempt the controller started is still winding down
    /// after a Stop. A Start is refused while one is, so two attempts are
    /// never in flight together; whoever owns the attempt clears it once
    /// the attempt has ended.
    pub winding_down: bool,
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
        stop_reason: None,
        winding_down: false,
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
///
/// The body keeps its STRUCTURE: it is a typed value, redacted as a
/// tree, and it carries the epoch of the run it belongs to. A body that
/// would weigh more than [`MAX_EVENT_PAYLOAD_BYTES`] is replaced by a
/// marker that states its size: the truncation is announced, never
/// hidden.
pub fn emit_event(
    core: &mut SupervisorCore,
    kind: impl Into<String>,
    payload: serde_json::Value,
    provenance: Provenance,
) -> StructuredEvent {
    let seq = core.next_seq;
    core.next_seq += 1;
    let redacted = redact_value(&payload);
    let payload = match serde_json::to_string(&redacted) {
        Ok(encoded) if encoded.len() <= MAX_EVENT_PAYLOAD_BYTES => redacted,
        Ok(encoded) => oversized_payload(encoded.len()),
        Err(_) => oversized_payload(0),
    };
    let event = StructuredEvent {
        seq,
        epoch: core.epoch,
        kind: kind.into(),
        payload,
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

/// The body that replaces one too large to carry.
fn oversized_payload(bytes: usize) -> serde_json::Value {
    serde_json::json!({
        "truncated": true,
        "original_bytes": bytes,
        "note": "the body exceeded the per-event limit and was not carried",
    })
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
        serde_json::json!({ "reason": reason }),
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
    let (unknown, payload) = match &observation {
        ResourceObservation::Measured { cpus, memory_gib } => (
            false,
            serde_json::json!({ "cpus": cpus, "memory_gib": memory_gib }),
        ),
        ResourceObservation::Unknown { reason } => (
            true,
            serde_json::json!({ "unknown": true, "reason": reason }),
        ),
    };
    emit_event(
        core,
        "telemetry/resources",
        payload,
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
        serde_json::json!({ "viewers": core.terminal_viewers }),
        Provenance::Human,
    )
}

pub fn viewer_disconnected(core: &mut SupervisorCore) -> StructuredEvent {
    core.terminal_viewers = core.terminal_viewers.saturating_sub(1);
    emit_event(
        core,
        "viewer/disconnected",
        serde_json::json!({ "viewers": core.terminal_viewers }),
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

/// Record the answer of an action: it becomes the last answer and one
/// event. One place, so an action answered by `apply` and an action
/// answered by the runtime read the same in the record.
fn record_action(core: &mut SupervisorCore, action_outcome: ActionOutcome) -> ActionOutcome {
    let detail = action_outcome.detail.clone();
    let applied = action_outcome.applied;
    let action = action_outcome.action.clone();
    core.last_action = Some(action_outcome.clone());
    emit_event(
        core,
        "supervisor/action",
        serde_json::json!({
            "action": action,
            "applied": applied,
            "detail": detail,
        }),
        Provenance::Human,
    );
    action_outcome
}

/// Record the renewal the runtime answered.
///
/// A renewal is applied ONLY when the runtime confirmed the new ttl. An
/// unsupported renewal is a real answer with its reason, and the tracked
/// ttl does not move; a confirmed ttl beyond the ceiling is refused the
/// same way.
pub fn apply_renewal(
    core: &mut SupervisorCore,
    added_seconds: u64,
    renewal: RenewOutcome,
) -> ActionOutcome {
    let action = SupervisorAction::Renew { added_seconds };
    let answer = match renewal {
        RenewOutcome::Confirmed { ttl_seconds } if ttl_seconds > MAX_TTL_SECONDS => outcome(
            &action,
            false,
            format!(
                "the runtime confirmed {ttl_seconds}s, beyond the ceiling of {MAX_TTL_SECONDS}s"
            ),
        ),
        RenewOutcome::Confirmed { ttl_seconds } => {
            core.ttl_remaining_seconds = Some(ttl_seconds);
            outcome(
                &action,
                true,
                format!("ttl set to {ttl_seconds}s as the runtime confirmed"),
            )
        }
        RenewOutcome::Unsupported { reason } => outcome(
            &action,
            false,
            format!("the renewal of {added_seconds}s was not applied: {reason}"),
        ),
    };
    record_action(core, answer)
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
                run.is_live()
            }) {
                outcome(
                    &action,
                    false,
                    "a run is already in flight: not a second controller",
                )
            } else if core.winding_down {
                // The stopped attempt may still be releasing its sandbox
                // and its session: a new one would run beside it.
                outcome(
                    &action,
                    false,
                    "the previous attempt is still winding down: one attempt at a time",
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
                // The verdict of the PREVIOUS objective is not this run's
                // verdict: a snapshot in `starting` must not show it.
                core.objective = None;
                core.stop_reason = None;
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
            } else if core
                .run
                .as_ref()
                .is_some_and(|r| r.phase() == &Phase::Running)
            {
                core.paused = true;
                outcome(&action, true, "prompt loop paused")
            } else if core
                .run
                .as_ref()
                .is_some_and(|r| r.phase() == &Phase::Starting)
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
        SupervisorAction::Stop => {
            let stopped = match core.run.as_mut() {
                // A run that is still starting is stopped too: nobody is
                // left waiting for a confirmation that will never arrive.
                Some(run) if run.is_live() => {
                    let was_starting = *run.phase() == Phase::Starting;
                    let cancelled = run.stop();
                    Some((was_starting, cancelled, run.initiated_irreversible.clone()))
                }
                _ => None,
            };
            match stopped {
                Some((was_starting, cancelled, irreversible)) => {
                    core.paused = false;
                    // A Stop ends the session: it does not un-say what
                    // already happened. A recorded verdict — a confirmed
                    // effect above all — stands beside the stop; only a run
                    // that produced NO verdict is reported as uncertain.
                    let uncertain = core.objective.is_none();
                    core.winding_down = true;
                    let reason = match (was_starting, uncertain) {
                        (true, true) => {
                            "stopped before the runtime confirmed: the objective is cancelled and its outcome is uncertain"
                        }
                        (false, true) => {
                            "stopped by the user: the objective is cancelled and its outcome is uncertain"
                        }
                        (true, false) => {
                            "stopped before the runtime confirmed: the session ended and the recorded verdict of the objective stands"
                        }
                        (false, false) => {
                            "stopped by the user: the session ended and the recorded verdict of the objective stands"
                        }
                    };
                    core.stop_reason = Some(reason.to_string());
                    emit_event(
                        core,
                        "objective/cancelled",
                        serde_json::json!({
                            "reason": reason,
                            "uncertain": uncertain,
                        }),
                        Provenance::Human,
                    );
                    outcome(
                        &action,
                        true,
                        format!(
                            "{reason}; cancelled_processes={cancelled:?}; irreversible={irreversible:?}"
                        ),
                    )
                }
                None => outcome(&action, false, "no running run to stop"),
            }
        }
        SupervisorAction::Renew { added_seconds } => {
            // The runtime owns the ttl. An action that arrives without
            // its confirmation is answered as not applied, with the
            // reason, instead of inflating a ttl nobody extended.
            return apply_renewal(
                core,
                *added_seconds,
                RenewOutcome::Unsupported {
                    reason: "no runtime confirmation was received".into(),
                },
            );
        }
    };
    record_action(core, outcome)
}

/// The runtime CONFIRMED the objective: the run is running now.
///
/// The confirmation carries the epoch of the run it belongs to: a late
/// confirmation of a run that already ended can never confirm another
/// one.
pub fn confirm_runtime(
    core: &mut SupervisorCore,
    epoch: u64,
    identity: &RuntimeIdentity,
) -> Transition {
    if core.epoch != epoch {
        return Transition::refused(format!(
            "the confirmation belongs to another run (epoch {epoch}; the current run is {})",
            core.epoch
        ));
    }
    // The confirmation must come from the runtime the snapshot NAMES. A
    // run confirmed by a runtime nobody was told about would let a
    // rehearsal be presented as a live run.
    match &core.runtime {
        None => {
            return Transition::refused("no runtime is wired: nothing can confirm the objective");
        }
        Some(named) if named != identity => {
            return Transition::refused(format!(
                "the confirmation comes from '{}' but this controller serves '{}'",
                identity.label(),
                named.label()
            ));
        }
        Some(_) => {}
    }
    let Some(run) = core.run.as_mut() else {
        return Transition::refused("there is no run to confirm");
    };
    if !run.confirm() {
        return Transition::refused(format!(
            "the run is not starting (phase {:?}): it cannot be confirmed",
            run.phase()
        ));
    }
    emit_event(
        core,
        "objective/confirmed",
        serde_json::json!({
            "epoch": epoch,
            "runtime": identity.label(),
            "detail": "the runtime confirmed the objective",
        }),
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
    // The outcome is the attempt telling us it has ended: whether the
    // verdict is recorded or not, no attempt of this run is winding down
    // any more, and the next Start may proceed.
    core.winding_down = false;
    // The outcome belongs to a LIVE run: a run that was stopped or
    // dropped does not get a verdict afterwards.
    let live = core.run.as_ref().is_some_and(|run| run.is_live());
    if !live {
        return Transition::refused("the run is no longer live: the outcome is not recorded");
    }
    let work_ref = match &outcome {
        ObjectiveOutcome::Confirmed { work_ref, .. }
        | ObjectiveOutcome::Rejected { work_ref, .. } => Some(work_ref.clone()),
        ObjectiveOutcome::Failed { .. } | ObjectiveOutcome::Idle { .. } => None,
    };
    // The body keeps the shape of the outcome: what happened, to what,
    // and why. A client reads the fields; it does not parse a sentence.
    let payload = match &outcome {
        ObjectiveOutcome::Confirmed { work_ref, detail } => serde_json::json!({
            "outcome": "confirmed",
            "work_ref": work_ref,
            "detail": detail,
        }),
        ObjectiveOutcome::Rejected { work_ref, reason } => serde_json::json!({
            "outcome": "rejected",
            "work_ref": work_ref,
            "reason": reason,
        }),
        ObjectiveOutcome::Failed { reason } => serde_json::json!({
            "outcome": "failed",
            "reason": reason,
        }),
        ObjectiveOutcome::Idle { reason } => serde_json::json!({
            "outcome": "idle",
            "reason": reason,
        }),
    };
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
        payload,
        Provenance::Automatic {
            rule: "runtime-outcome".into(),
        },
    );
    Transition::Applied
}

/// The snapshot both clients render — the SAME view for TUI and CLI.
pub fn snapshot(core: &SupervisorCore) -> Snapshot {
    Snapshot {
        protocol_version: PROTOCOL_VERSION,
        project: core.project.clone(),
        phase: core
            .run
            .as_ref()
            .map(|r| r.phase().clone())
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

/// The events with sequence number >= `since`, at most `limit` of them.
///
/// The limit is the client's to lower; the page never exceeds
/// [`MAX_EVENTS_PER_PAGE`] whatever the client asks for.
pub fn events_since(core: &SupervisorCore, since: u64, limit: Option<u32>) -> EventPage {
    let limit = limit
        .map(|asked| asked.clamp(1, MAX_EVENTS_PER_PAGE as u32) as usize)
        .unwrap_or(MAX_EVENTS_PER_PAGE);
    let oldest_seq = core.events.first().map(|event| event.seq).unwrap_or(0);
    let events: Vec<StructuredEvent> = core
        .events
        .iter()
        .filter(|event| event.seq >= since)
        .take(limit)
        .cloned()
        .collect();
    // The cursor never goes backwards: a client that asked for events
    // that do not exist yet keeps its own position.
    let next_seq = match events.last() {
        Some(last) => last.seq + 1,
        None => since.max(core.next_seq),
    };
    // The events the client wanted that the ring already dropped. The
    // first sequence number is 1, so a client that asks from 0 wants
    // everything still retained.
    let history_lost = oldest_seq > since.max(1);
    EventPage {
        events,
        next_seq,
        oldest_seq,
        history_lost,
    }
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
        assert_eq!(
            confirm_runtime(core, epoch, &RuntimeIdentity::rehearsal("trial")),
            Transition::Applied
        );
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
            core.run.as_ref().map(|r| r.phase().clone()),
            Some(Phase::Starting)
        );
        assert_eq!(core.ticket.as_deref(), Some("TASK-9"));

        // The confirmation is what makes it running.
        let epoch = core.epoch;
        assert_eq!(
            confirm_runtime(&mut core, epoch, &RuntimeIdentity::rehearsal("trial")),
            Transition::Applied
        );
        assert_eq!(
            core.run.as_ref().map(|r| r.phase().clone()),
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
            confirm_runtime(&mut core, 99, &RuntimeIdentity::rehearsal("trial")),
            Transition::Refused { .. }
        ));
        assert_eq!(
            core.run.as_ref().map(|r| r.phase().clone()),
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
        confirm_runtime(&mut core, epoch, &RuntimeIdentity::rehearsal("trial"));
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
            core.run.as_ref().map(|r| r.phase().clone()),
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
        // The stopped attempt has ended: its wind-down is over.
        core.winding_down = false;
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
        // A page is bounded, and its CURSOR says where to continue. The
        // page starts at the oldest event that is still retained.
        let page = events_since(&core, 1, None);
        assert_eq!(page.events.len(), MAX_EVENTS_PER_PAGE);
        assert_eq!(page.next_seq, 51 + MAX_EVENTS_PER_PAGE as u64);
        // The client asked for events that the ring already dropped: it
        // is TOLD that its history has a hole.
        assert!(page.history_lost, "the lost history must be announced");
        assert_eq!(page.oldest_seq, 51);
    }

    #[test]
    fn a_page_never_exceeds_what_the_client_asked_for() {
        let mut core = default_core("gc");
        for index in 0..10 {
            emit_event(
                &mut core,
                "test/event",
                serde_json::Value::String(format!("event {index}")),
                Provenance::Human,
            );
        }
        let page = events_since(&core, 1, Some(3));
        assert_eq!(page.events.len(), 3);
        assert_eq!(page.next_seq, 4);
        // A client cannot ask for MORE than the page the answer allows.
        let greedy = events_since(&core, 1, Some(100_000));
        assert_eq!(greedy.events.len(), MAX_EVENTS_PER_PAGE.min(10));
    }

    #[test]
    fn the_cursor_never_goes_backwards() {
        let mut core = default_core("gc");
        emit_event(
            &mut core,
            "test/event",
            serde_json::Value::String("one".into()),
            Provenance::Human,
        );
        // A client that asks for events that do not exist yet keeps its
        // own position: the answer must not send it backwards.
        let page = events_since(&core, 99, None);
        assert!(page.events.is_empty());
        assert_eq!(page.next_seq, 99);
        assert!(!page.history_lost, "nothing the client wanted was lost");
    }

    #[test]
    fn a_whole_history_is_announced_as_whole() {
        let mut core = default_core("gc");
        emit_event(
            &mut core,
            "test/event",
            serde_json::Value::String("one".into()),
            Provenance::Human,
        );
        let page = events_since(&core, 0, None);
        assert_eq!(page.events.len(), 1);
        assert!(
            !page.history_lost,
            "a client that asks from the beginning has the whole history"
        );
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
            core.run.as_ref().map(|r| r.phase().clone()),
            Some(Phase::Stopped)
        );
    }

    #[test]
    fn a_renewal_is_applied_only_when_the_runtime_confirmed_it() {
        let mut core = wired("gc");
        let confirmed = apply_renewal(&mut core, 300, RenewOutcome::Confirmed { ttl_seconds: 420 });
        assert!(confirmed.applied, "{}", confirmed.detail);
        assert_eq!(core.ttl_remaining_seconds, Some(420));
        assert_eq!(
            confirmed.action,
            SupervisorAction::Renew { added_seconds: 300 }
        );

        let unsupported = apply_renewal(
            &mut core,
            300,
            RenewOutcome::Unsupported {
                reason: "the lifecycle backend has no ttl renewal".into(),
            },
        );
        assert!(!unsupported.applied);
        assert!(
            unsupported.detail.contains("no ttl renewal"),
            "{}",
            unsupported.detail
        );
        assert_eq!(
            core.ttl_remaining_seconds,
            Some(420),
            "an unsupported renewal does not move the ttl"
        );
    }

    #[test]
    fn a_renewal_action_without_a_runtime_answer_is_refused() {
        let mut core = default_core("gc");
        core.ttl_remaining_seconds = Some(120);
        let renew = apply(&mut core, SupervisorAction::Renew { added_seconds: 300 });
        assert!(!renew.applied);
        assert!(renew.detail.contains("runtime"), "{}", renew.detail);
        assert_eq!(core.ttl_remaining_seconds, Some(120));
    }

    #[test]
    fn a_confirmed_renewal_beyond_the_ceiling_is_refused() {
        let mut core = default_core("gc");
        let beyond = apply_renewal(
            &mut core,
            300,
            RenewOutcome::Confirmed {
                ttl_seconds: MAX_TTL_SECONDS + 1,
            },
        );
        assert!(!beyond.applied);
        assert!(beyond.detail.contains("ceiling"), "{}", beyond.detail);
        assert_eq!(core.ttl_remaining_seconds, None);
    }

    #[test]
    fn a_stop_without_a_verdict_is_cancelled_and_uncertain() {
        let mut core = wired("gc");
        started(&mut core);

        let stop = apply(&mut core, SupervisorAction::Stop);
        assert!(stop.applied, "{}", stop.detail);
        assert!(
            core.objective.is_none(),
            "an interrupted objective produced no verdict"
        );
        let reason = core.stop_reason.as_deref().expect("a stop reason");
        assert!(reason.contains("uncertain"), "{reason}");
        let cancelled = core
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "objective/cancelled")
            .expect("the cancellation event");
        assert_eq!(cancelled.payload["uncertain"], serde_json::json!(true));
        assert_eq!(snapshot(&core).phase, Phase::Stopped);
    }

    #[test]
    fn a_stop_after_a_confirmed_objective_keeps_the_verdict() {
        let mut core = wired("gc");
        let epoch = started(&mut core);
        record_outcome(
            &mut core,
            epoch,
            ObjectiveOutcome::Confirmed {
                work_ref: "TASK-1".into(),
                detail: "published 1f0c3a5b".into(),
            },
        );

        let stop = apply(&mut core, SupervisorAction::Stop);
        assert!(stop.applied, "{}", stop.detail);
        // A confirmed effect is a fact of the past: the stop ends the
        // session, it does not relabel the verdict as unknown.
        let objective = core
            .objective
            .as_ref()
            .expect("the confirmed verdict stands");
        assert_eq!(objective.work_ref.as_deref(), Some("TASK-1"));
        assert!(matches!(
            objective.outcome,
            ObjectiveOutcome::Confirmed { .. }
        ));
        let reason = core.stop_reason.as_deref().expect("a stop reason");
        assert!(!reason.contains("uncertain"), "{reason}");
        assert!(reason.contains("verdict"), "{reason}");
        assert!(!stop.detail.contains("uncertain"), "{}", stop.detail);
        let cancelled = core
            .events
            .iter()
            .rev()
            .find(|event| event.kind == "objective/cancelled")
            .expect("the session stop event");
        assert_eq!(cancelled.payload["uncertain"], serde_json::json!(false));
        assert_eq!(snapshot(&core).phase, Phase::Stopped);
    }

    #[test]
    fn a_start_while_the_stopped_attempt_winds_down_is_refused() {
        let mut core = wired("gc");
        started(&mut core);
        apply(&mut core, SupervisorAction::Stop);
        assert!(core.winding_down, "the stop begins the wind-down");

        let project = core.project.clone();
        let concurrent = apply(
            &mut core,
            SupervisorAction::Start {
                project: project.clone(),
                ticket: None,
                revision: None,
            },
        );
        assert!(!concurrent.applied);
        assert!(
            concurrent.detail.contains("winding down"),
            "{}",
            concurrent.detail
        );
        assert_eq!(
            core.run.as_ref().map(|run| run.phase().clone()),
            Some(Phase::Stopped)
        );

        // The attempt has ended: the next attempt may start.
        core.winding_down = false;
        let next = apply(
            &mut core,
            SupervisorAction::Start {
                project,
                ticket: None,
                revision: None,
            },
        );
        assert!(next.applied, "{}", next.detail);
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
            core.run.as_ref().map(|r| r.phase().clone()),
            Some(Phase::Running)
        );

        let page = events_since(&core, 1, None);
        // The start and its confirmation are part of the history.
        assert_eq!(page.events.len(), 4);
        assert_eq!(page.next_seq, 5);
        let tail = events_since(&core, 3, None);
        assert_eq!(tail.events.len(), 2);
        assert_eq!(tail.events[0].seq, 3);
    }

    #[test]
    fn an_event_keeps_its_structure_and_the_epoch_of_its_run() {
        let mut core = wired("gc");
        let epoch = started(&mut core);
        let event = core.events.last().expect("the confirmation event");
        // The body is a TYPED value: the reader does not have to parse a
        // sentence to know what the event said.
        assert!(event.payload.is_object(), "{:?}", event.payload);
        assert_eq!(event.payload["epoch"], serde_json::json!(epoch));
        assert_eq!(event.epoch, epoch, "the event says which run it is from");
    }

    #[test]
    fn a_body_too_large_is_replaced_by_a_marker_that_says_so() {
        let mut core = default_core("gc");
        let huge = "x".repeat(MAX_EVENT_PAYLOAD_BYTES * 2);
        emit_event(
            &mut core,
            "test/event",
            serde_json::json!({ "blob": huge }),
            Provenance::Human,
        );
        let recorded = core.events.last().expect("event");
        assert_eq!(recorded.payload["truncated"], serde_json::json!(true));
        assert!(
            recorded.payload["original_bytes"].as_u64().unwrap_or(0)
                > MAX_EVENT_PAYLOAD_BYTES as u64,
            "the marker states the size it refused: {:?}",
            recorded.payload
        );
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
