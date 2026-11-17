//! Write-ahead promotion ledger (AC-3): promotion (git) and closure
//! (tracker) are SEPARATE steps. The ledger persists between them, so a
//! crash after promoting recovers THE SAME commit — no second commit is
//! created and Done is never declared without evidence.

use serde::{Deserialize, Serialize};

use crate::ticket::TicketUpdate;

/// What to promote, where, and what the closure must look like.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionPlan {
    pub work_ref: String,
    /// Local managed branch of the project (never origin).
    pub managed_branch: String,
    /// The ref that was VERIFIED: promotion is a compare-and-swap against it.
    pub base_ref: String,
    /// The configured terminal status (e.g. "Done"), read from tracker
    /// configuration — not hardcoded.
    pub target_status: String,
    pub update: TicketUpdate,
}

/// Outbox phases, in order. Each transition persists BEFORE the next effect
/// runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase")]
pub enum LedgerPhase {
    /// Intent written; git promotion not attempted yet.
    Planning,
    /// Git promotion done; tracker update pending.
    Promoted { commit: String },
    /// Tracker update applied; closure not yet verified by re-reading.
    TrackerUpdated { commit: String },
    /// Re-read ticket and branch confirmed the outcome.
    Confirmed { commit: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionLedger {
    pub plan: PromotionPlan,
    pub phase: LedgerPhase,
}

/// What to do after a crash or a restart, classified from the persisted
/// phase alone.
#[derive(Debug, Clone, PartialEq)]
pub enum RecoveryAction {
    /// Nothing promoted yet: run the CAS promotion from the verified base.
    PromoteFromBase,
    /// Promotion already produced this commit; update the tracker with THE
    /// SAME commit. Never promote again.
    ReconcileTracker { commit: String },
    /// Tracker updated; verify by re-reading ticket and branch, then close.
    VerifyClose { commit: String },
    /// Fully reconciled: repeating is a no-op (idempotent).
    AlreadyDone { commit: String },
}

impl PromotionLedger {
    pub fn plan(plan: PromotionPlan) -> Self {
        Self {
            plan,
            phase: LedgerPhase::Planning,
        }
    }

    pub fn record_promoted(&mut self, commit: &str) {
        self.phase = LedgerPhase::Promoted {
            commit: commit.to_string(),
        };
    }

    pub fn record_tracker_updated(&mut self) {
        if let LedgerPhase::Promoted { commit } = &self.phase {
            let commit = commit.clone();
            self.phase = LedgerPhase::TrackerUpdated { commit };
        }
    }

    pub fn confirm(&mut self) {
        if let LedgerPhase::TrackerUpdated { commit } = &self.phase {
            let commit = commit.clone();
            self.phase = LedgerPhase::Confirmed { commit };
        }
    }

    pub fn recovery(&self) -> RecoveryAction {
        match &self.phase {
            LedgerPhase::Planning => RecoveryAction::PromoteFromBase,
            LedgerPhase::Promoted { commit } => RecoveryAction::ReconcileTracker {
                commit: commit.clone(),
            },
            LedgerPhase::TrackerUpdated { commit } => RecoveryAction::VerifyClose {
                commit: commit.clone(),
            },
            LedgerPhase::Confirmed { commit } => RecoveryAction::AlreadyDone {
                commit: commit.clone(),
            },
        }
    }

    /// Outbox persistence: serialize the full ledger so the next process can
    /// rebuild it (AC-3 crash boundary).
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }

    pub fn from_json(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan() -> PromotionPlan {
        PromotionPlan {
            work_ref: "TASK-10".into(),
            managed_branch: "hodei-mh/main".into(),
            base_ref: "1111111111111111111111111111111111111111".into(),
            target_status: "Done".into(),
            update: TicketUpdate {
                note: "promoted".into(),
                check_ac: vec![1, 2],
                final_summary: "done".into(),
                target_status: "Done".into(),
            },
        }
    }

    #[test]
    fn crash_after_promotion_recovers_the_same_commit() {
        let mut ledger = PromotionLedger::plan(plan());
        assert_eq!(ledger.recovery(), RecoveryAction::PromoteFromBase);

        ledger.record_promoted("abc123");
        // Crash here, reload from disk: the SAME commit is recovered.
        let reloaded = PromotionLedger::from_json(&ledger.to_json().unwrap()).unwrap();
        assert_eq!(
            reloaded.recovery(),
            RecoveryAction::ReconcileTracker {
                commit: "abc123".into()
            }
        );
    }

    #[test]
    fn phases_advance_without_skipping() {
        let mut ledger = PromotionLedger::plan(plan());
        // record_tracker_updated before any promotion is a no-op.
        ledger.record_tracker_updated();
        assert_eq!(ledger.phase, LedgerPhase::Planning);

        ledger.record_promoted("def456");
        ledger.record_tracker_updated();
        assert_eq!(
            ledger.phase,
            LedgerPhase::TrackerUpdated {
                commit: "def456".into()
            }
        );
        ledger.confirm();
        assert_eq!(
            ledger.phase,
            LedgerPhase::Confirmed {
                commit: "def456".into()
            }
        );
        // confirm twice is harmless.
        ledger.confirm();
        assert_eq!(
            ledger.recovery(),
            RecoveryAction::AlreadyDone {
                commit: "def456".into()
            }
        );
    }

    #[test]
    fn ledger_survives_serialization_roundtrip_in_every_phase() {
        let mut ledger = PromotionLedger::plan(plan());
        let as_json = ledger.to_json().unwrap();
        assert_eq!(
            PromotionLedger::from_json(&as_json).unwrap(),
            PromotionLedger {
                phase: LedgerPhase::Planning,
                ..ledger.clone()
            }
        );

        ledger.record_promoted("cafe");
        let as_json = ledger.to_json().unwrap();
        assert_eq!(PromotionLedger::from_json(&as_json).unwrap(), ledger);

        ledger.record_tracker_updated();
        let as_json = ledger.to_json().unwrap();
        assert_eq!(PromotionLedger::from_json(&as_json).unwrap(), ledger);

        ledger.confirm();
        let as_json = ledger.to_json().unwrap();
        assert_eq!(PromotionLedger::from_json(&as_json).unwrap(), ledger);
    }
}
