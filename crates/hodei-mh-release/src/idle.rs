//! Idle release (AC-2): zero-token idle releases the managed environment
//! ONLY after the durable state is confirmed (candidate exported or
//! checkpoint confirmed). The tracker keeps being observed without any
//! inference — releasing decides nothing about the work.

use serde::{Deserialize, Serialize};
use verify::digest::CandidateDigest;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum IdleGate {
    /// There is pending work in the conversation: do not release.
    WorkPending,
    /// The candidate was exported to durable storage (digest known).
    CandidateConfirmed { digest: CandidateDigest },
    /// The rollover checkpoint was confirmed (epoch durable).
    CheckpointConfirmed { epoch: u64 },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReleaseDecision {
    /// Safe to stop the agent and free the environment (zero-token idle).
    Release { durable: String },
    /// Durable state is not confirmed: keep the environment.
    Keep { reason: String },
}

pub fn release_on_idle(gate: &IdleGate) -> ReleaseDecision {
    match gate {
        IdleGate::WorkPending => ReleaseDecision::Keep {
            reason: "pending work would be lost".into(),
        },
        IdleGate::CandidateConfirmed { digest } => ReleaseDecision::Release {
            durable: format!("candidate {}", digest.0),
        },
        IdleGate::CheckpointConfirmed { epoch } => ReleaseDecision::Release {
            durable: format!("checkpoint epoch {epoch}"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_work_blocks_release() {
        let decision = release_on_idle(&IdleGate::WorkPending);
        assert!(matches!(decision, ReleaseDecision::Keep { .. }));
    }

    #[test]
    fn confirmed_candidate_or_checkpoint_releases() {
        let digest = CandidateDigest::from_bytes(b"exported");
        let decision = release_on_idle(&IdleGate::CandidateConfirmed { digest });
        assert!(matches!(decision, ReleaseDecision::Release { .. }));

        let decision = release_on_idle(&IdleGate::CheckpointConfirmed { epoch: 7 });
        assert!(matches!(decision, ReleaseDecision::Release { .. }));
    }
}
