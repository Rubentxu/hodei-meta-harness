//! The promotion of one candidate and the closure of its ticket: ONE
//! attempt that survives a crash.
//!
//! The attempt writes its intent BEFORE any effect, asks each authority
//! what it says, and does only what the observation allows. Every step is
//! idempotent: a crash at any point is resumed by observing again, so the
//! candidate is promoted once, the ticket is closed once and the evidence
//! of the operation is appended once.

use std::path::Path;

use verify::evidence::EvidenceSet;

use crate::git::{self, PromoteError};
use crate::intent::{
    ClosureDecision, Conflict, PromotionDecision, PromotionIntent, PromotionOutcome,
    attempt_conflict, decide_closure, decide_promotion,
};
use crate::journal::{IntentJournal, JournalError};
use crate::writer::{WorkWriter, WriterError};

/// What the attempt needs to do its work.
pub struct PromotionRequest<'a> {
    /// The facts of the attempt.
    pub intent: &'a PromotionIntent,
    /// The evidence that speaks for the candidate: without it the
    /// candidate does not move.
    pub evidence: &'a EvidenceSet,
    /// The repository of the project.
    pub repo: &'a Path,
    /// The bundle that carries the candidate.
    pub bundle: &'a Path,
    /// The branch of the bundle that carries the candidate.
    pub candidate_branch: &'a str,
}

/// What the attempt did.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionReport {
    /// The identity of the attempt.
    pub operation_id: String,
    /// The commit the managed branch is at, when the promotion happened.
    pub commit: Option<String>,
    /// Whether the CAS ran in this call.
    pub promoted: bool,
    /// Whether the closure was applied in this call.
    pub closed: bool,
    /// Whether only the evidence was appended: the closure was somebody
    /// else's and it was left untouched.
    pub evidence_appended: bool,
    /// The conflict, when the attempt was refused.
    pub conflict: Option<Conflict>,
}

impl PromotionReport {
    /// Whether the attempt did what it was for.
    pub fn is_complete(&self) -> bool {
        self.conflict.is_none() && self.commit.is_some()
    }
}

/// The attempt could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum PromotionError {
    #[error("the journal failed: {0}")]
    Journal(#[from] JournalError),
    #[error("the tracker failed: {0}")]
    Tracker(#[from] WriterError),
    #[error("the promotion failed: {0}")]
    Git(#[from] PromoteError),
    #[error("the closure did not hold: {0}")]
    ClosureNotVerified(String),
}

/// Promote the candidate and close the ticket, exactly once.
pub fn execute(
    request: PromotionRequest<'_>,
    tracker: &dyn WorkWriter,
    journal: &mut dyn IntentJournal,
) -> Result<PromotionReport, PromotionError> {
    let requested = request.intent;
    let mut report = PromotionReport {
        operation_id: requested.operation_id.clone(),
        commit: None,
        promoted: false,
        closed: false,
        evidence_appended: false,
        conflict: None,
    };

    // The attempt must own the ticket: an intent of another attempt, or of
    // another project, writes NOTHING at all.
    let epoch = journal.current_epoch(&requested.work_ref)?;
    let project = journal.project().to_string();
    if let Some(conflict) = attempt_conflict(requested, epoch, &project) {
        report.conflict = Some(conflict);
        return Ok(report);
    }

    // The record of the attempt: the intent is written BEFORE any effect,
    // and the record that is already there is the one that runs.
    let intent = match journal.load(&requested.operation_id)? {
        Some(record) => {
            if !record.intent.same_attempt(requested) {
                // The record belongs to another attempt: it is not written.
                report.conflict = Some(Conflict::IntentRefused {
                    detail: format!(
                        "the operation {} is recorded with another attempt",
                        requested.operation_id
                    ),
                });
                return Ok(report);
            }
            record.intent
        }
        None => {
            if let Err(error) = journal.record(requested) {
                // The store refused the intent: another attempt owns it.
                report.conflict = Some(Conflict::IntentRefused {
                    detail: error.to_string(),
                });
                return Ok(report);
            }
            requested.clone()
        }
    };

    // The promotion: what the branch says decides it.
    let ref_head = git::current_ref(request.repo, &intent.managed_branch)?;
    let commit = match decide_promotion(&intent, ref_head.as_deref(), epoch, &project) {
        PromotionDecision::Promote => {
            // The bundle must carry the candidate the intent names.
            let head = git::candidate_head(request.bundle, request.candidate_branch)?;
            if head != intent.expected_candidate {
                let conflict = Conflict::ShaMismatch {
                    expected: intent.expected_candidate.clone(),
                    observed: head,
                };
                report.conflict = Some(refuse(&intent, journal, conflict)?);
                return Ok(report);
            }
            let commit = git::promote_verified(
                request.repo,
                &intent.managed_branch,
                request.bundle,
                request.candidate_branch,
                &intent.expected_base,
                request.evidence,
                &intent.work_ref,
            )?;
            if commit != intent.expected_candidate {
                let conflict = Conflict::ShaMismatch {
                    expected: intent.expected_candidate.clone(),
                    observed: commit,
                };
                report.conflict = Some(refuse(&intent, journal, conflict)?);
                return Ok(report);
            }
            report.promoted = true;
            commit
        }
        PromotionDecision::AlreadyPromoted => intent.expected_candidate.clone(),
        PromotionDecision::Conflict(conflict) => {
            report.conflict = Some(refuse(&intent, journal, conflict)?);
            return Ok(report);
        }
    };
    report.commit = Some(commit.clone());

    // The closure: what the ticket says decides it.
    let ticket = match tracker.read_ticket(&intent.work_ref) {
        Ok(ticket) => ticket,
        Err(error) => {
            // The tracker did not answer, so the closure did not happen:
            // the promotion is a fact and the record says so.
            settle_once(
                journal,
                &intent,
                PromotionOutcome::Promoted {
                    commit: commit.clone(),
                    conflict: None,
                },
            )?;
            return Err(PromotionError::Tracker(error));
        }
    };
    match decide_closure(&intent, Some(&ticket), epoch, &project) {
        ClosureDecision::Close => {
            if let Err(error) = tracker.apply(&intent.work_ref, &intent.update) {
                // The promotion is a fact and the closure did not happen:
                // the record says so, so the next attempt knows what is
                // left to do.
                settle_once(
                    journal,
                    &intent,
                    PromotionOutcome::Promoted {
                        commit: commit.clone(),
                        conflict: None,
                    },
                )?;
                return Err(PromotionError::Tracker(error));
            }
            report.closed = true;
            if let Err(error) = verify_closure(&intent, tracker) {
                // The promotion is a fact; the closure did not hold, and the
                // attempt is left open for the next one.
                settle_once(
                    journal,
                    &intent,
                    PromotionOutcome::Promoted {
                        commit: commit.clone(),
                        conflict: None,
                    },
                )?;
                return Err(error);
            }
        }
        ClosureDecision::AlreadyClosed => {}
        ClosureDecision::ClosedByOther => {
            // The closure of that person is not ours to write: only the
            // evidence of this operation is appended.
            tracker.append_note(&intent.work_ref, &intent.update.note)?;
            report.evidence_appended = true;
            verify_evidence(&intent, tracker)?;
        }
        ClosureDecision::Conflict(conflict) => {
            // The candidate is already promoted: the promotion is not
            // un-applied by a refusal of the closure.
            settle_once(
                journal,
                &intent,
                PromotionOutcome::Promoted {
                    commit: commit.clone(),
                    conflict: Some(conflict.clone()),
                },
            )?;
            report.conflict = Some(conflict);
            return Ok(report);
        }
    }

    settle_once(
        journal,
        &intent,
        PromotionOutcome::Closed {
            commit: commit.clone(),
        },
    )?;
    Ok(report)
}

/// Refuse the attempt and record the refusal.
fn refuse(
    intent: &PromotionIntent,
    journal: &mut dyn IntentJournal,
    conflict: Conflict,
) -> Result<Conflict, PromotionError> {
    settle_once(
        journal,
        intent,
        PromotionOutcome::Conflicted {
            conflict: conflict.clone(),
        },
    )?;
    Ok(conflict)
}

/// Settle the attempt, unless the record already says that the operation
/// finished.
///
/// A record that says `Closed` is the story of an operation that completed:
/// a later observation of the same operation does not rewrite it, and the
/// caller still sees the refusal in the report.
fn settle_once(
    journal: &mut dyn IntentJournal,
    intent: &PromotionIntent,
    outcome: PromotionOutcome,
) -> Result<(), PromotionError> {
    let finished = journal
        .load(&intent.operation_id)?
        .is_some_and(|record| matches!(record.outcome, Some(PromotionOutcome::Closed { .. })));
    if finished {
        return Ok(());
    }
    journal.settle(&intent.operation_id, &outcome)?;
    Ok(())
}

/// The closure is verified by re-reading the ticket: the tracker says what
/// it did, and the claim of the flow is not taken for an answer.
fn verify_closure(
    intent: &PromotionIntent,
    tracker: &dyn WorkWriter,
) -> Result<(), PromotionError> {
    let ticket = tracker.read_ticket(&intent.work_ref)?;
    if ticket.status != intent.target_status {
        return Err(PromotionError::ClosureNotVerified(format!(
            "the ticket is at '{}' and not at '{}'",
            ticket.status, intent.target_status
        )));
    }
    if !intent.marked_by(&ticket.notes) {
        return Err(PromotionError::ClosureNotVerified(
            "the note of the operation is not in the ticket".into(),
        ));
    }
    let unchecked: Vec<usize> = intent
        .update
        .check_ac
        .iter()
        .copied()
        .filter(|index| !ticket.checked_ac.contains(index))
        .collect();
    if !unchecked.is_empty() {
        return Err(PromotionError::ClosureNotVerified(format!(
            "the criteria {unchecked:?} are not checked"
        )));
    }
    Ok(())
}

/// The evidence of the operation is verified by re-reading the ticket.
fn verify_evidence(
    intent: &PromotionIntent,
    tracker: &dyn WorkWriter,
) -> Result<(), PromotionError> {
    let ticket = tracker.read_ticket(&intent.work_ref)?;
    if !intent.marked_by(&ticket.notes) {
        return Err(PromotionError::ClosureNotVerified(
            "the note of the operation is not in the ticket".into(),
        ));
    }
    if ticket.status != intent.target_status {
        return Err(PromotionError::ClosureNotVerified(format!(
            "the ticket is at '{}' and not at '{}'",
            ticket.status, intent.target_status
        )));
    }
    Ok(())
}
