//! The durable intent of ONE promotion attempt, and the decisions that the
//! observation of the two authorities produces.
//!
//! The intent states the facts of the attempt BEFORE any effect runs: the
//! candidate that must be promoted, the base it applies to, the ticket, the
//! epoch of the attempt and the project it belongs to. It is the record
//! that survives a crash, so the attempt can be resumed exactly.
//!
//! The phase of the attempt is NOT stored: it is derived from what the two
//! authorities say RIGHT NOW (the ref of the branch and the document of the
//! ticket). A phase stored beside them would be a third truth that could
//! disagree with both after a crash. Each authority decides about its own
//! effects: git about the promotion, the tracker about the closure.

use serde::{Deserialize, Serialize};

use crate::ticket::TicketUpdate;
use crate::writer::TicketSnapshot;

/// The facts of an attempt, as the caller knows them before any effect.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionFacts {
    /// The project the attempt belongs to.
    pub project: String,
    /// The ticket that is closed by the promotion.
    pub work_ref: String,
    /// The epoch of the attempt that owns the promotion.
    pub epoch: u64,
    /// The managed LOCAL branch of the project (never origin).
    pub managed_branch: String,
    /// The commit the promotion applies to.
    pub expected_base: String,
    /// The commit the promotion must produce.
    pub expected_candidate: String,
    /// The status the ticket ends in.
    pub target_status: String,
    /// The evidence of the promotion.
    pub note: String,
    /// The acceptance criteria the closure checks.
    pub check_ac: Vec<usize>,
    /// The closure summary.
    pub final_summary: String,
}

/// The intent of ONE promotion attempt: the facts that must not change
/// while the attempt runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PromotionIntent {
    /// The identity of the attempt, derived from its facts. Two attempts of
    /// the same candidate under the same epoch are the SAME operation.
    pub operation_id: String,
    pub project: String,
    pub work_ref: String,
    pub epoch: u64,
    pub managed_branch: String,
    pub expected_base: String,
    pub expected_candidate: String,
    pub target_status: String,
    pub update: TicketUpdate,
}

/// The intent could not be built: its facts are not usable.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum IntentError {
    #[error("the expected base is not a commit: {0}")]
    Base(String),
    #[error("the expected candidate is not a commit: {0}")]
    Candidate(String),
    #[error("the work item is empty: an intent without a ticket closes nothing")]
    NoWorkRef,
    #[error("the note is empty: it carries no evidence")]
    NoNote,
}

/// The marker that identifies the note of an operation.
///
/// The note is APPENDED by the tracker, so the marker is how a second
/// attempt knows that the evidence of this operation is already there.
pub const OPERATION_MARKER_PREFIX: &str = "[hodei-mh:op:";

impl PromotionIntent {
    /// Build the intent of an attempt.
    ///
    /// The identity of the operation and the marker of its note are derived
    /// from the facts: they are never chosen by the caller, so two callers
    /// that describe the same attempt produce the same operation.
    pub fn new(facts: PromotionFacts) -> Result<Self, IntentError> {
        verify::git::GitRevision::parse(&facts.expected_base)
            .map_err(|error| IntentError::Base(error.to_string()))?;
        verify::git::GitRevision::parse(&facts.expected_candidate)
            .map_err(|error| IntentError::Candidate(error.to_string()))?;
        if facts.work_ref.trim().is_empty() {
            return Err(IntentError::NoWorkRef);
        }
        if facts.note.trim().is_empty() {
            return Err(IntentError::NoNote);
        }
        let operation_id = operation_id_of(&facts.work_ref, facts.epoch, &facts.expected_candidate);
        let update = TicketUpdate {
            note: format!("{} {}", facts.note.trim(), marker_of(&operation_id)),
            check_ac: facts.check_ac,
            final_summary: facts.final_summary,
            target_status: facts.target_status.clone(),
        };
        Ok(Self {
            operation_id,
            project: facts.project,
            work_ref: facts.work_ref,
            epoch: facts.epoch,
            managed_branch: facts.managed_branch,
            expected_base: facts.expected_base,
            expected_candidate: facts.expected_candidate,
            target_status: facts.target_status,
            update,
        })
    }

    /// The marker that identifies the note of this operation.
    pub fn marker(&self) -> String {
        marker_of(&self.operation_id)
    }

    /// Whether a note carries the marker of this operation.
    pub fn marked_by(&self, notes: &str) -> bool {
        notes.contains(&self.marker())
    }

    /// Whether two intents describe the same attempt.
    ///
    /// The evidence (the note) is not part of it: the same promotion with
    /// other words is still the same promotion, and the attempt that was
    /// recorded is the one that runs.
    pub fn same_attempt(&self, other: &PromotionIntent) -> bool {
        self.operation_id == other.operation_id
            && self.project == other.project
            && self.work_ref == other.work_ref
            && self.epoch == other.epoch
            && self.managed_branch == other.managed_branch
            && self.expected_base == other.expected_base
            && self.expected_candidate == other.expected_candidate
            && self.target_status == other.target_status
    }

    /// The description of the attempt, for the record that holds it.
    pub fn description(&self) -> String {
        format!(
            "promote {} onto {} at {}",
            short(&self.expected_candidate),
            self.managed_branch,
            short(&self.expected_base)
        )
    }
}

/// The identity of an attempt: the ticket, the epoch and the candidate.
pub fn operation_id_of(work_ref: &str, epoch: u64, expected_candidate: &str) -> String {
    format!("promote:{work_ref}:{epoch}:{expected_candidate}")
}

/// The marker that identifies the note of an operation.
pub fn marker_of(operation_id: &str) -> String {
    format!("{OPERATION_MARKER_PREFIX}{operation_id}]")
}

/// The first eight characters of a commit, for a human answer.
fn short(revision: &str) -> &str {
    let end = revision.len().min(8);
    &revision[..end]
}

/// What the branch must do, given what it says.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionDecision {
    /// Promote the candidate onto the managed branch.
    Promote,
    /// The candidate is already there: the promotion happened.
    AlreadyPromoted,
    /// The observation contradicts the intent: nothing is touched.
    Conflict(Conflict),
}

/// What the ticket must do, given what it says.
#[derive(Debug, Clone, PartialEq)]
pub enum ClosureDecision {
    /// Close the ticket.
    Close,
    /// The ticket is closed by this operation: there is nothing to do.
    AlreadyClosed,
    /// The ticket is closed by somebody else: the evidence is appended and
    /// the closure of that person is left untouched.
    ClosedByOther,
    /// The observation contradicts the intent: nothing is touched.
    Conflict(Conflict),
}

/// Why the observation contradicts the intent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Conflict {
    /// The managed branch is at neither the expected base nor the expected
    /// candidate: somebody else moved it.
    RefMoved {
        expected_base: String,
        expected_candidate: String,
        observed: String,
    },
    /// The intent belongs to another attempt of the ticket.
    EpochStale {
        intent_epoch: u64,
        current_epoch: u64,
    },
    /// The ticket is not claimed: there is no attempt to fence the intent to.
    NotClaimed { work_ref: String },
    /// The intent belongs to another project.
    ProjectMismatch {
        intent_project: String,
        observed_project: String,
    },
    /// The ticket observed is not the ticket of the intent.
    TicketMismatch {
        intent_work_ref: String,
        observed_id: String,
    },
    /// The ticket was closed by this operation and it is not closed anymore:
    /// somebody changed it.
    TicketChanged { observed_status: String },
    /// The bundle carries another commit than the one the intent names.
    ShaMismatch { expected: String, observed: String },
    /// The intent could not be recorded: another attempt owns this identity.
    IntentRefused { detail: String },
}

/// The identity of the attempt, checked against what is observed.
///
/// The rule lives here because it is the same for both authorities: an
/// intent of another project, or of another attempt, never acts.
pub fn attempt_conflict(
    intent: &PromotionIntent,
    current_epoch: Option<u64>,
    observed_project: &str,
) -> Option<Conflict> {
    if observed_project != intent.project {
        return Some(Conflict::ProjectMismatch {
            intent_project: intent.project.clone(),
            observed_project: observed_project.to_string(),
        });
    }
    match current_epoch {
        Some(current) if current != intent.epoch => Some(Conflict::EpochStale {
            intent_epoch: intent.epoch,
            current_epoch: current,
        }),
        Some(_) => None,
        None => Some(Conflict::NotClaimed {
            work_ref: intent.work_ref.clone(),
        }),
    }
}

/// Decide what the promotion must do, from the branch and the attempt.
pub fn decide_promotion(
    intent: &PromotionIntent,
    ref_head: Option<&str>,
    current_epoch: Option<u64>,
    observed_project: &str,
) -> PromotionDecision {
    if let Some(conflict) = attempt_conflict(intent, current_epoch, observed_project) {
        return PromotionDecision::Conflict(conflict);
    }
    match ref_head {
        Some(head) if head == intent.expected_candidate => PromotionDecision::AlreadyPromoted,
        Some(head) if head == intent.expected_base => PromotionDecision::Promote,
        Some(head) => PromotionDecision::Conflict(Conflict::RefMoved {
            expected_base: intent.expected_base.clone(),
            expected_candidate: intent.expected_candidate.clone(),
            observed: head.to_string(),
        }),
        // The branch does not exist: the CAS creates it, and it can never
        // overwrite a branch that appears in the meantime.
        None => PromotionDecision::Promote,
    }
}

/// Decide what the closure must do, from the ticket and the attempt.
pub fn decide_closure(
    intent: &PromotionIntent,
    ticket: Option<&TicketSnapshot>,
    current_epoch: Option<u64>,
    observed_project: &str,
) -> ClosureDecision {
    if let Some(conflict) = attempt_conflict(intent, current_epoch, observed_project) {
        return ClosureDecision::Conflict(conflict);
    }
    let Some(ticket) = ticket else {
        return ClosureDecision::Conflict(Conflict::TicketMismatch {
            intent_work_ref: intent.work_ref.clone(),
            observed_id: String::new(),
        });
    };
    if ticket.id != intent.work_ref {
        return ClosureDecision::Conflict(Conflict::TicketMismatch {
            intent_work_ref: intent.work_ref.clone(),
            observed_id: ticket.id.clone(),
        });
    }
    if ticket.status != intent.target_status {
        if intent.marked_by(&ticket.notes) {
            // Our operation is in the ticket and the ticket is not closed:
            // somebody changed it, and that change is not ours to undo.
            return ClosureDecision::Conflict(Conflict::TicketChanged {
                observed_status: ticket.status.clone(),
            });
        }
        return ClosureDecision::Close;
    }
    if intent.marked_by(&ticket.notes) {
        ClosureDecision::AlreadyClosed
    } else {
        ClosureDecision::ClosedByOther
    }
}

/// What an attempt did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "outcome")]
pub enum PromotionOutcome {
    /// The candidate was promoted and the ticket is closed.
    Closed { commit: String },
    /// The candidate was promoted; the ticket is not closed. The promotion
    /// is a fact even when the closure was refused: the conflict says why
    /// the ticket was left as it is.
    Promoted {
        commit: String,
        conflict: Option<Conflict>,
    },
    /// The attempt was refused before the promotion: the observation
    /// contradicted the intent and nothing was touched.
    Conflicted { conflict: Conflict },
}

impl PromotionOutcome {
    /// Whether the PROMOTION happened.
    ///
    /// The promotion is the effect: the closure of the ticket is the
    /// tracker's business and is observed there, so a promoted candidate
    /// whose ticket stayed open is an applied effect with work left to do,
    /// never an effect that may run again.
    pub fn is_applied(&self) -> bool {
        match self {
            PromotionOutcome::Closed { .. } | PromotionOutcome::Promoted { .. } => true,
            PromotionOutcome::Conflicted { .. } => false,
        }
    }
}

/// The record of an attempt: its intent and what it did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JournalRecord {
    pub intent: PromotionIntent,
    pub outcome: Option<PromotionOutcome>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";
    const CANDIDATE: &str = "2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";
    const FOREIGN: &str = "3f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";

    fn facts() -> PromotionFacts {
        PromotionFacts {
            project: "alpha".into(),
            work_ref: "TASK-1".into(),
            epoch: 3,
            managed_branch: "hodei-mh/main".into(),
            expected_base: BASE.into(),
            expected_candidate: CANDIDATE.into(),
            target_status: "Done".into(),
            note: "evidence of the promotion".into(),
            check_ac: vec![1],
            final_summary: "promoted and closed".into(),
        }
    }

    fn intent() -> PromotionIntent {
        PromotionIntent::new(facts()).expect("a valid intent")
    }

    fn ticket(status: &str, notes: &str) -> TicketSnapshot {
        TicketSnapshot {
            id: "TASK-1".into(),
            status: status.into(),
            checked_ac: vec![1],
            notes: notes.into(),
            final_summary: "closed".into(),
            raw: String::new(),
        }
    }

    #[test]
    fn the_identity_of_the_operation_is_derived_from_its_facts() {
        let intent = intent();
        assert_eq!(intent.operation_id, format!("promote:TASK-1:3:{CANDIDATE}"));
        assert!(intent.update.note.contains(&intent.marker()));
        assert!(
            intent.marked_by(&format!("evidence {}", intent.marker())),
            "the marker of the operation travels in the note"
        );
    }

    #[test]
    fn an_intent_without_usable_facts_is_refused() {
        let mut broken = facts();
        broken.expected_base = "not a commit".into();
        assert!(matches!(
            PromotionIntent::new(broken),
            Err(IntentError::Base(_))
        ));
        let mut broken = facts();
        broken.expected_candidate = "not a commit".into();
        assert!(matches!(
            PromotionIntent::new(broken),
            Err(IntentError::Candidate(_))
        ));
        let mut broken = facts();
        broken.note = "  ".into();
        assert!(matches!(
            PromotionIntent::new(broken),
            Err(IntentError::NoNote)
        ));
    }

    #[test]
    fn a_branch_at_the_base_is_promoted_and_one_at_the_candidate_is_not() {
        let intent = intent();
        assert_eq!(
            decide_promotion(&intent, Some(BASE), Some(3), "alpha"),
            PromotionDecision::Promote
        );
        assert_eq!(
            decide_promotion(&intent, Some(CANDIDATE), Some(3), "alpha"),
            PromotionDecision::AlreadyPromoted
        );
        assert_eq!(
            decide_promotion(&intent, None, Some(3), "alpha"),
            PromotionDecision::Promote,
            "a branch that does not exist is created by the CAS"
        );
    }

    #[test]
    fn a_branch_that_moved_is_a_conflict_and_nothing_is_adopted() {
        assert_eq!(
            decide_promotion(&intent(), Some(FOREIGN), Some(3), "alpha"),
            PromotionDecision::Conflict(Conflict::RefMoved {
                expected_base: BASE.into(),
                expected_candidate: CANDIDATE.into(),
                observed: FOREIGN.into(),
            })
        );
    }

    #[test]
    fn an_intent_of_another_attempt_or_project_is_a_conflict() {
        let intent = intent();
        assert_eq!(
            decide_promotion(&intent, Some(BASE), Some(4), "alpha"),
            PromotionDecision::Conflict(Conflict::EpochStale {
                intent_epoch: 3,
                current_epoch: 4,
            })
        );
        assert_eq!(
            decide_promotion(&intent, Some(BASE), Some(3), "beta"),
            PromotionDecision::Conflict(Conflict::ProjectMismatch {
                intent_project: "alpha".into(),
                observed_project: "beta".into(),
            })
        );
        assert_eq!(
            decide_promotion(&intent, Some(BASE), None, "alpha"),
            PromotionDecision::Conflict(Conflict::NotClaimed {
                work_ref: "TASK-1".into(),
            })
        );
    }

    #[test]
    fn a_ticket_closed_by_this_operation_is_not_closed_again() {
        let intent = intent();
        let notes = format!("evidence {}", intent.marker());
        assert_eq!(
            decide_closure(&intent, Some(&ticket("Done", &notes)), Some(3), "alpha"),
            ClosureDecision::AlreadyClosed
        );
    }

    #[test]
    fn a_ticket_closed_by_somebody_else_keeps_its_closure() {
        assert_eq!(
            decide_closure(
                &intent(),
                Some(&ticket("Done", "human note")),
                Some(3),
                "alpha"
            ),
            ClosureDecision::ClosedByOther
        );
    }

    #[test]
    fn an_open_ticket_is_closed() {
        assert_eq!(
            decide_closure(&intent(), Some(&ticket("To Do", "")), Some(3), "alpha"),
            ClosureDecision::Close
        );
    }

    #[test]
    fn a_ticket_that_changed_after_our_closure_is_a_conflict() {
        let intent = intent();
        let notes = format!("evidence {}", intent.marker());
        assert_eq!(
            decide_closure(
                &intent,
                Some(&ticket("In Progress", &notes)),
                Some(3),
                "alpha"
            ),
            ClosureDecision::Conflict(Conflict::TicketChanged {
                observed_status: "In Progress".into(),
            })
        );
    }

    #[test]
    fn the_ticket_observed_must_be_the_ticket_of_the_intent() {
        let mut foreign = ticket("To Do", "");
        foreign.id = "TASK-9".into();
        assert_eq!(
            decide_closure(&intent(), Some(&foreign), Some(3), "alpha"),
            ClosureDecision::Conflict(Conflict::TicketMismatch {
                intent_work_ref: "TASK-1".into(),
                observed_id: "TASK-9".into(),
            })
        );
    }

    #[test]
    fn the_same_attempt_with_other_words_is_the_same_attempt() {
        let intent = intent();
        let mut other = intent.clone();
        other.update.note = "other words".into();
        assert!(intent.same_attempt(&other));
        let mut other = intent.clone();
        other.expected_base = FOREIGN.into();
        assert!(!intent.same_attempt(&other));
    }
}
