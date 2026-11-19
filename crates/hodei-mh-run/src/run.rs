//! Run state machine (AC-2/3/4): one exclusive ACP session with
//! run/session/epoch-tagged events; stop semantics that invalidate pending
//! work, cancel controlled processes and honestly report irreversible ones;
//! goal-less agents work via explicit `session/prompt` turns.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::grant::Grant;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Phase {
    Idle,
    /// The objective was handed to the runtime, but no runtime has
    /// CONFIRMED it yet. A run in this phase is not running: nothing has
    /// answered that the work started.
    Starting,
    Running,
    Stopping,
    Stopped,
}

/// Identity envelope every legitimate event must carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventIdentity {
    pub run_id: String,
    pub session_id: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaggedEvent {
    pub identity: EventIdentity,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, PartialEq)]
pub struct ForeignEventDetail {
    pub kind: String,
    pub observed: EventIdentity,
    pub expected: EventIdentity,
}

#[derive(Debug, Error, PartialEq)]
pub enum RunError {
    #[error("event belongs to another run/session/epoch")]
    ForeignEvent(Box<ForeignEventDetail>),
    #[error("run is not running (phase {0:?})")]
    NotRunning(Phase),
}

#[derive(Debug, Clone, PartialEq)]
pub struct PendingResponse {
    pub event_kind: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RunState {
    pub grant: Grant,
    pub session_id: String,
    pub phase: Phase,
    /// Responses and effects awaiting delivery — invalidated by stop.
    pub pending: Vec<PendingResponse>,
    /// Effects already sent to the outside world; stop cannot undo these,
    /// it can only report them.
    pub initiated_irreversible: Vec<String>,
    /// Controlled processes (session pty ids) cancelled on stop.
    pub cancelled_processes: Vec<String>,
}

impl RunState {
    /// The run is REQUESTED: the session is bound to the grant's identity,
    /// but no runtime has confirmed it yet. `Phase::Running` is reachable
    /// ONLY through `confirm`.
    pub fn pending(grant: Grant, session_id: impl Into<String>) -> Self {
        Self {
            grant,
            session_id: session_id.into(),
            phase: Phase::Starting,
            pending: Vec::new(),
            initiated_irreversible: Vec::new(),
            cancelled_processes: Vec::new(),
        }
    }

    /// The runtime CONFIRMED the run: it is running now. A run that is
    /// not `Starting` cannot be confirmed — the confirmation is a
    /// transition, not a state anybody may declare.
    pub fn confirm(&mut self) -> bool {
        if self.phase != Phase::Starting {
            return false;
        }
        self.phase = Phase::Running;
        true
    }

    fn identity(&self) -> EventIdentity {
        EventIdentity {
            run_id: self.grant.run_id.clone(),
            session_id: self.session_id.clone(),
            epoch: self.grant.epoch,
        }
    }

    /// Receive an identity-tagged event. Foreign events (other run, session
    /// or epoch) are rejected — hodei-mh controls one session exclusively.
    pub fn receive(&mut self, event: TaggedEvent) -> Result<(), RunError> {
        let expected = self.identity();
        if event.identity != expected {
            return Err(RunError::ForeignEvent(Box::new(ForeignEventDetail {
                kind: event.kind,
                observed: event.identity,
                expected,
            })));
        }
        if self.phase != Phase::Running && self.phase != Phase::Starting {
            // Late arrivals after stop started are invalidated, not applied.
            return Err(RunError::NotRunning(self.phase.clone()));
        }
        self.pending.push(PendingResponse {
            event_kind: event.kind,
        });
        Ok(())
    }

    /// An effect physically sent to the world: stop cannot un-send it.
    pub fn mark_initiated(&mut self, description: impl Into<String>) {
        self.initiated_irreversible.push(description.into());
    }

    pub fn track_process(&mut self, pty_session_id: impl Into<String>) {
        self.cancelled_processes.push(pty_session_id.into());
    }

    /// STOP: invalidate everything pending, cancel controlled processes,
    /// report the irreversible actions already in flight. A run that has
    /// not been confirmed yet is stopped too: nobody is left waiting for
    /// a confirmation that will never arrive.
    pub fn stop(&mut self) -> Vec<String> {
        self.pending.clear();
        if self.phase == Phase::Running || self.phase == Phase::Starting {
            self.phase = Phase::Stopping;
        }
        let cancelled = self.cancelled_processes.clone();
        self.cancelled_processes.clear();
        self.phase = Phase::Stopped;
        cancelled
    }
}

/// AC-4: a goal-less agent performs the same finite work as explicit
/// `session/prompt` turns. No goal capability is simulated anywhere.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RunPlan {
    /// The agent advertises native goal support: handed the objective.
    NativeGoal { objective: String },
    /// Plain agent: the objective decomposes into explicit prompts.
    FinitePrompts { prompts: Vec<String> },
}

impl RunPlan {
    pub fn for_agent(has_native_goal: bool, objective: &str, decomposition: Vec<String>) -> Self {
        if has_native_goal {
            Self::NativeGoal {
                objective: objective.to_string(),
            }
        } else {
            Self::FinitePrompts {
                prompts: decomposition,
            }
        }
    }

    /// The prompt turns this plan will actually send.
    pub fn prompt_turns(&self) -> Vec<String> {
        match self {
            Self::NativeGoal { objective } => vec![objective.clone()],
            Self::FinitePrompts { prompts } => prompts.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn running() -> RunState {
        let grant = Grant::new("proj", "run-7", 3, vec![]);
        let mut run = RunState::pending(grant, "sess-9");
        assert!(run.confirm(), "a runtime confirms the pending run");
        run
    }

    fn event(kind: &str, identity: EventIdentity) -> TaggedEvent {
        TaggedEvent {
            identity,
            kind: kind.into(),
            payload: json!({}),
        }
    }

    #[test]
    fn a_run_is_running_only_after_a_runtime_confirms_it() {
        let grant = Grant::new("proj", "run-7", 3, vec![]);
        let mut run = RunState::pending(grant, "sess-9");
        // Nothing answered that the work started: the run is not running.
        assert_eq!(run.phase, Phase::Starting);
        assert!(run.confirm(), "the confirmation is what starts the run");
        assert_eq!(run.phase, Phase::Running);
        // A second confirmation is not a transition: it changes nothing.
        assert!(!run.confirm());
        assert_eq!(run.phase, Phase::Running);
    }

    #[test]
    fn events_are_accepted_while_the_run_starts() {
        let grant = Grant::new("proj", "run-7", 3, vec![]);
        let mut run = RunState::pending(grant, "sess-9");
        let identity = EventIdentity {
            run_id: "run-7".into(),
            session_id: "sess-9".into(),
            epoch: 3,
        };
        // The runtime may answer while the run is still starting: those
        // events are part of the run, not late arrivals.
        assert!(run.receive(event("session/update", identity)).is_ok());
        assert_eq!(run.pending.len(), 1);
    }

    #[test]
    fn a_run_that_is_not_confirmed_can_be_stopped() {
        let grant = Grant::new("proj", "run-7", 3, vec![]);
        let mut run = RunState::pending(grant, "sess-9");
        run.track_process("pty-1");
        // Nobody is left waiting for a confirmation that will not arrive.
        let cancelled = run.stop();
        assert_eq!(cancelled, vec!["pty-1".to_string()]);
        assert_eq!(run.phase, Phase::Stopped);
        assert!(!run.confirm(), "a stopped run cannot be confirmed");
    }

    #[test]
    fn events_carry_and_verify_run_session_epoch_identity() {
        let mut run = running();
        let good = EventIdentity {
            run_id: "run-7".into(),
            session_id: "sess-9".into(),
            epoch: 3,
        };
        assert!(run.receive(event("session/update", good.clone())).is_ok());
        assert_eq!(run.pending.len(), 1);

        for (rid, sid, epoch) in [
            ("other", "sess-9", 3u64),
            ("run-7", "other", 3),
            ("run-7", "sess-9", 4),
        ] {
            let foreign = EventIdentity {
                run_id: rid.into(),
                session_id: sid.into(),
                epoch,
            };
            let err = run.receive(event("session/update", foreign)).unwrap_err();
            assert!(matches!(err, RunError::ForeignEvent(_)));
        }
        assert_eq!(run.pending.len(), 1); // foreign events never queued
    }

    #[test]
    fn stop_invalidates_pending_cancels_and_reports_irreversible() {
        let mut run = running();
        let identity = EventIdentity {
            run_id: "run-7".into(),
            session_id: "sess-9".into(),
            epoch: 3,
        };
        let _ = run.receive(event("update-1", identity.clone()));
        run.mark_initiated("git bundle written to staging");
        run.track_process("pty-1");

        let cancelled = run.stop();
        assert_eq!(cancelled, vec!["pty-1".to_string()]);
        assert!(run.pending.is_empty());
        assert_eq!(run.phase, Phase::Stopped);
        assert_eq!(
            run.initiated_irreversible,
            vec!["git bundle written to staging"]
        );

        // Late events after stop are refused, not silently applied.
        let late = run.receive(event("late", identity));
        assert!(matches!(late, Err(RunError::NotRunning(Phase::Stopped))));
    }

    #[test]
    fn goalless_agents_run_finite_prompts_without_goal_simulation() {
        let native = RunPlan::for_agent(true, "ship the feature", vec![]);
        assert_eq!(native.prompt_turns(), vec!["ship the feature"]);
        let goalless = RunPlan::for_agent(
            false,
            "ship the feature",
            vec!["implement".to_string(), "test".to_string()],
        );
        assert!(matches!(goalless, RunPlan::FinitePrompts { .. }));
        assert_eq!(goalless.prompt_turns(), vec!["implement", "test"]);
    }
}
