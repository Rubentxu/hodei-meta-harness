//! Sandbox loss recovery (AC-3): losing the environment (OOM, host reboot)
//! is recovered from DURABLE state only — a confirmed candidate digest with
//! a fresh session. Un-exported files are explicitly marked as possible
//! data loss: the absence of evidence is never read as a snapshot.

use serde::{Deserialize, Serialize};
use verify::digest::CandidateDigest;

/// What survived the loss of a sandbox.
#[derive(Debug, Clone, PartialEq)]
pub struct SandboxLoss {
    /// Digest of the candidate exported to durable storage, if any.
    pub last_export: Option<CandidateDigest>,
    /// Files observed in the sandbox that were never exported.
    pub unexported: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum RecoveryPlan {
    /// Resume from the durable candidate in a NEW session (conversation
    /// continuity is rebuilt from the promoted branch + digest).
    ResumeFromCandidate {
        digest: CandidateDigest,
        new_session: bool,
    },
    /// Nothing durable survived: the un-exported files are marked as
    /// possible data loss instead of being silently assumed recoverable.
    PossibleDataLoss { files: Vec<String> },
}

/// Decide the recovery plan after losing a sandbox. An OOM never implies a
/// snapshot exists: only an explicitly confirmed export counts.
pub fn recover_from_loss(loss: &SandboxLoss) -> RecoveryPlan {
    match &loss.last_export {
        Some(digest) => RecoveryPlan::ResumeFromCandidate {
            digest: digest.clone(),
            new_session: true,
        },
        None => RecoveryPlan::PossibleDataLoss {
            files: loss.unexported.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn confirmed_export_resumes_in_a_new_session() {
        let loss = SandboxLoss {
            last_export: Some(CandidateDigest::from_bytes(b"exported tree")),
            unexported: vec!["notes.md".into()],
        };
        let plan = recover_from_loss(&loss);
        assert_eq!(
            plan,
            RecoveryPlan::ResumeFromCandidate {
                digest: CandidateDigest::from_bytes(b"exported tree"),
                new_session: true,
            }
        );
    }

    #[test]
    fn no_export_marks_possible_data_loss_instead_of_assuming_snapshot() {
        let loss = SandboxLoss {
            last_export: None,
            unexported: vec!["half-edited.rs".into(), "scratch.sh".into()],
        };
        let plan = recover_from_loss(&loss);
        assert_eq!(
            plan,
            RecoveryPlan::PossibleDataLoss {
                files: vec!["half-edited.rs".into(), "scratch.sh".into()],
            }
        );
    }
}
