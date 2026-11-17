//! Crash-safe rollover ledger (AC-1/3): a rollover is a two-phase transition
//! — request a NEW session (never resume/load), then adopt its id and bump
//! the epoch. The last CONFIRMED checkpoint is never lost; a pending phase
//! is completed idempotently on recovery, never duplicated.

use serde::{Deserialize, Serialize};

/// The durable handoff state: everything the next session needs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Checkpoint {
    /// Stable across rollovers.
    pub goal_id: String,
    pub work_ref: String,
    /// Changes on every rollover.
    pub epoch: u64,
    pub session_id: String,
    pub next_action: String,
    pub decisions: Vec<String>,
    pub references: Vec<String>,
    pub evidences: Vec<String>,
    pub failed_strategies: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingRollover {
    pub new_epoch: u64,
    /// Whether the provider was already asked for a new session; carrying
    /// the id makes recovery adopt the SAME session instead of duplicating
    /// the controller (AC-3).
    pub requested: bool,
    pub new_session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RolloverLedger {
    pub confirmed: Checkpoint,
    pub pending: Option<PendingRollover>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RolloverStep {
    /// Ask the provider for a fresh session (no resume/load, AC-1).
    RequestNewSession { new_epoch: u64 },
    /// Record that the provider answered with a session id.
    RecordSession { new_epoch: u64, session_id: String },
    /// Nothing to do: a pending phase exists — recover it first.
    RecoverPending,
}

#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryAction {
    /// A pending rollover completed while crashed: adopt without re-request.
    AdoptPending { new_epoch: u64, session_id: String },
    /// A pending rollover never reached the provider: re-request is safe.
    ReRequest { new_epoch: u64 },
}

impl RolloverLedger {
    pub fn initial(confirmed: Checkpoint) -> Self {
        Self {
            confirmed,
            pending: None,
        }
    }

    /// Next step to advance a rollover (AC-1: always a NEW session).
    pub fn next_step(&self) -> RolloverStep {
        if let Some(pending) = &self.pending {
            if pending.requested {
                return RolloverStep::RecoverPending;
            }
            return RolloverStep::RequestNewSession {
                new_epoch: pending.new_epoch,
            };
        }
        RolloverStep::RequestNewSession {
            new_epoch: self.confirmed.epoch + 1,
        }
    }

    /// Phase 1 begins: mark the rollover in flight.
    pub fn begin(&mut self) -> u64 {
        if let Some(pending) = &self.pending {
            return pending.new_epoch;
        }
        let new_epoch = self.confirmed.epoch + 1;
        self.pending = Some(PendingRollover {
            new_epoch,
            requested: true,
            new_session_id: None,
        });
        new_epoch
    }

    /// Phase 2: the provider delivered the fresh session id.
    pub fn record_session(&mut self, session_id: &str) -> u64 {
        match &mut self.pending {
            Some(pending) => {
                pending.new_session_id = Some(session_id.to_string());
                pending.new_epoch
            }
            None => self.confirmed.epoch + 1,
        }
    }

    /// Confirm: the new session actually runs. GoalId and WorkRef survive;
    /// SessionId and epoch change (AC-1).
    pub fn confirm(&mut self) -> Checkpoint {
        let (new_epoch, new_session) = match self.pending.take() {
            Some(pending) => (
                pending.new_epoch,
                pending
                    .new_session_id
                    .clone()
                    .unwrap_or_else(|| "pending-session".into()),
            ),
            None => (self.confirmed.epoch + 1, "pending-session".into()),
        };
        let mut confirmed = self.confirmed.clone();
        confirmed.epoch = new_epoch;
        confirmed.session_id = new_session;
        self.confirmed = confirmed.clone();
        confirmed
    }

    /// Recovery classification after a crash (AC-3).
    pub fn recovery_action(&self) -> RecoveryAction {
        match &self.pending {
            Some(PendingRollover {
                new_epoch,
                requested: true,
                new_session_id: Some(session_id),
            }) => RecoveryAction::AdoptPending {
                new_epoch: *new_epoch,
                session_id: session_id.clone(),
            },
            Some(PendingRollover { new_epoch, .. }) => RecoveryAction::ReRequest {
                new_epoch: *new_epoch,
            },
            None => RecoveryAction::ReRequest {
                new_epoch: self.confirmed.epoch + 1,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint(epoch: u64, session: &str) -> Checkpoint {
        Checkpoint {
            goal_id: "goal-1".into(),
            work_ref: "TASK-7".into(),
            epoch,
            session_id: session.into(),
            next_action: "continue TASK-7".into(),
            decisions: vec!["use pipe transport".into()],
            references: vec!["backlog/tasks/task-7".into()],
            evidences: vec!["probe 6/6".into()],
            failed_strategies: vec!["resume via session/load".into()],
        }
    }

    #[test]
    fn five_consecutive_rollovers_change_session_and_epoch_only() {
        let mut ledger = RolloverLedger::initial(checkpoint(1, "s1"));
        for epoch in 2..=6u64 {
            let step = ledger.next_step();
            assert!(matches!(step, RolloverStep::RequestNewSession { .. }));
            ledger.begin();
            ledger.record_session(&format!("s{epoch}"));
            let confirmed = ledger.confirm();
            assert_eq!(confirmed.epoch, epoch);
            assert_eq!(confirmed.session_id, format!("s{epoch}"));
            // GoalId + WorkRef survive every rollover.
            assert_eq!(confirmed.goal_id, "goal-1");
            assert_eq!(confirmed.work_ref, "TASK-7");
            assert!(ledger.pending.is_none());
        }
        assert_eq!(ledger.confirmed.session_id, "s6");
    }

    #[test]
    fn crash_after_request_adopts_pending_without_duplicating() {
        let mut ledger = RolloverLedger::initial(checkpoint(1, "s1"));
        ledger.begin();
        ledger.record_session("s2-crash");
        // Crash here. Recovery: adopt the SAME pending session; the
        // controller is not duplicated (no second new-session request).
        let action = ledger.recovery_action();
        assert_eq!(
            action,
            RecoveryAction::AdoptPending {
                new_epoch: 2,
                session_id: "s2-crash".into()
            }
        );
        let confirmed = ledger.confirm();
        assert_eq!(confirmed.session_id, "s2-crash");
        assert_eq!(confirmed.epoch, 2);
    }

    #[test]
    fn crash_before_request_rerequests_safely() {
        let mut ledger = RolloverLedger::initial(checkpoint(1, "s1"));
        ledger.begin();
        // Crash before the provider answered: nothing adopted, re-request.
        let action = ledger.recovery_action();
        assert_eq!(action, RecoveryAction::ReRequest { new_epoch: 2 });
        // Confirmed checkpoint intact.
        assert_eq!(ledger.confirmed.session_id, "s1");
        assert_eq!(ledger.confirmed.epoch, 1);
    }

    #[test]
    fn no_resume_or_load_is_used_on_renewal() {
        // next_step always demands a NEW session; resume/load never appears.
        let ledger = RolloverLedger::initial(checkpoint(4, "s4"));
        match ledger.next_step() {
            RolloverStep::RequestNewSession { new_epoch } => assert_eq!(new_epoch, 5),
            other => panic!("expected fresh-session request, got {other:?}"),
        }
    }
}
