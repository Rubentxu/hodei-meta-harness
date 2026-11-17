//! Ticket updates (AC-1/AC-2): closure data is APPEND-ONLY by construction —
//! notes accumulate, prior ACs and notes are never overwritten, and the
//! terminal status comes from tracker configuration, not from this crate.

use serde::{Deserialize, Serialize};

/// What the tracker will be told, once, after the candidate is promoted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TicketUpdate {
    /// Evidence appended to existing notes; nothing is replaced.
    pub note: String,
    /// Acceptance criteria to mark checked, by 1-based index.
    pub check_ac: Vec<usize>,
    /// The closure summary.
    pub final_summary: String,
    /// Must equal the plan's configured status; validated before applying.
    pub target_status: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("ticket update rejected: {0}")]
pub struct UpdateRejected(pub String);

impl TicketUpdate {
    /// Guard: the update only exists in the context of a promoted candidate.
    pub fn validate(&self, target_status_from_config: &str) -> Result<(), UpdateRejected> {
        if self.note.trim().is_empty() {
            return Err(UpdateRejected("empty note carries no evidence".into()));
        }
        if self.final_summary.trim().is_empty() {
            return Err(UpdateRejected(
                "empty final summary is not a closure".into(),
            ));
        }
        if self.target_status != target_status_from_config {
            return Err(UpdateRejected(format!(
                "status '{}' is not the configured terminal status '{}'",
                self.target_status, target_status_from_config
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn update() -> TicketUpdate {
        TicketUpdate {
            note: "evidence: promoted abc".into(),
            check_ac: vec![1],
            final_summary: "candidate integrated".into(),
            target_status: "Done".into(),
        }
    }

    #[test]
    fn valid_update_passes_against_configured_status() {
        assert_eq!(update().validate("Done"), Ok(()));
    }

    #[test]
    fn status_must_come_from_configuration_not_from_the_caller() {
        let err = update().validate("Completed").unwrap_err();
        assert!(err.to_string().contains("configured terminal status"));
    }

    #[test]
    fn empty_evidence_or_summary_is_rejected() {
        let mut bad = update();
        bad.note = "   ".into();
        assert!(bad.validate("Done").is_err());
        bad.note = "x".into();
        bad.final_summary = String::new();
        assert!(bad.validate("Done").is_err());
    }
}
