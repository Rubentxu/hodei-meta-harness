//! The publisher of the live pipeline: promote the candidate by CAS and
//! close the work item through the official tracker CLI.
//!
//! The promotion runs through the durable flow: the intent of the attempt
//! is journaled BEFORE any effect, and the promotion and the closure are
//! reconciled by observing each authority, so a crash at any point resumes
//! the same operation instead of moving the effects twice. The candidate
//! moves only with the evidence that speaks for it, and the closure is
//! only reported as closed when the re-read says so.

use std::path::PathBuf;

use hodei_mh_coordinator::store::CoordinatorStore;
use promote::flow::{PromotionReport, PromotionRequest};
use promote::intent::{PromotionFacts, PromotionIntent};
use promote::journal::{CoordinatorJournal, IntentJournal};
use promote::writer::{BacklogWorkWriter, TicketSnapshot, WorkWriter};
use verify::runner::Candidate;

use runner::ports::{Objective, Publication, PublisherError, PublisherPort, Verification};

/// The branch the candidate bundle carries: the worktree inside the
/// sandbox is checked out as `main`, so the exported bundle names it.
pub const CANDIDATE_BRANCH: &str = "main";

/// The status a promoted work item ends in.
pub const CLOSED_STATUS: &str = "Done";

/// The publisher over the real managed branch and the real tracker.
pub struct VerifiedPublisher {
    pub project: String,
    pub project_dir: PathBuf,
    pub managed_branch: String,
    pub bundle_path: PathBuf,
    /// The coordinator state: the journal of the promotion intents and the
    /// claims they are fenced to.
    pub state_db: PathBuf,
    pub writer: BacklogWorkWriter,
}

impl VerifiedPublisher {
    fn failure(work_ref: &str, reason: impl Into<String>) -> PublisherError {
        PublisherError {
            work_ref: work_ref.to_string(),
            reason: reason.into(),
        }
    }

    /// Write the candidate bundle the promotion reads.
    fn write_bundle(&self, work_ref: &str, candidate: &Candidate) -> Result<(), PublisherError> {
        if let Some(parent) = self.bundle_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                Self::failure(work_ref, format!("the bundle directory: {error}"))
            })?;
        }
        std::fs::write(&self.bundle_path, &candidate.bundle)
            .map_err(|error| Self::failure(work_ref, format!("the candidate bundle: {error}")))
    }

    /// The facts of the attempt, as the two authorities state them before
    /// any effect runs.
    fn intent_of(
        &self,
        objective: &Objective,
        epoch: u64,
    ) -> Result<PromotionIntent, PublisherError> {
        let work_ref = objective.work_ref.as_str();
        let base = promote::git::current_ref(&self.project_dir, &self.managed_branch)
            .map_err(|error| Self::failure(work_ref, error.to_string()))?
            .ok_or_else(|| {
                Self::failure(
                    work_ref,
                    format!("the managed branch '{}' has no commit", self.managed_branch),
                )
            })?;
        let expected_candidate = promote::git::candidate_head(&self.bundle_path, CANDIDATE_BRANCH)
            .map_err(|error| Self::failure(work_ref, error.to_string()))?;
        PromotionIntent::new(PromotionFacts {
            project: self.project.clone(),
            work_ref: work_ref.to_string(),
            epoch,
            managed_branch: self.managed_branch.clone(),
            expected_base: base,
            expected_candidate,
            target_status: CLOSED_STATUS.to_string(),
            note: "Evidence: the candidate was verified in a clean environment and promoted by compare-and-swap.".to_string(),
            // The criteria the checks are DECLARED under, never the count
            // of checks: marking by count closes the wrong criteria.
            check_ac: objective.check_ac(),
            final_summary: format!("{work_ref} completed by the live supervisor."),
        })
        .map_err(|error| Self::failure(work_ref, error.to_string()))
    }

    /// The closure, read back from the tracker: the re-read is the only
    /// answer, and a tracker that cannot be read is said out loud.
    fn closure_of(&self, work_ref: &str) -> Result<TicketSnapshot, String> {
        self.writer
            .read_ticket(work_ref)
            .map_err(|error| error.to_string())
    }
}

/// How the promotion reads in a publication, with the conflict that
/// refused the closure when there was one.
fn detail_of(
    report: &PromotionReport,
    commit: &str,
    managed_branch: &str,
    closure: &Result<TicketSnapshot, String>,
) -> String {
    let moved = if report.promoted {
        format!("promoted {commit} onto {managed_branch}")
    } else {
        format!("the candidate {commit} was already on {managed_branch}")
    };
    let conflict = report
        .conflict
        .as_ref()
        .map(|conflict| {
            serde_json::to_string(conflict).unwrap_or_else(|_| "the attempt was refused".into())
        })
        .map(|conflict| format!("; the closure was refused: {conflict}"))
        .unwrap_or_default();
    match closure {
        Ok(snapshot) => format!(
            "{moved}{conflict}; the re-read says the work is {}",
            if snapshot.status == CLOSED_STATUS {
                "closed"
            } else {
                "still open"
            }
        ),
        Err(error) => format!("{moved}{conflict}; the closure could not be re-read: {error}"),
    }
}

#[async_trait::async_trait]
impl PublisherPort for VerifiedPublisher {
    async fn publish(
        &self,
        objective: &Objective,
        candidate: &Candidate,
        verification: &Verification,
    ) -> Result<Publication, PublisherError> {
        let work_ref = objective.work_ref.as_str();
        // The promotion demands the evidence of the candidate it promotes:
        // a verdict without evidence would move an unproven commit.
        let evidence = verification
            .evidence
            .as_ref()
            .ok_or_else(|| Self::failure(work_ref, "the verdict carries no evidence"))?;
        self.write_bundle(work_ref, candidate)?;

        // The journal and the claim live in the same durable store: the
        // intent is written there, before any effect.
        let mut store = CoordinatorStore::open(&self.project, &self.state_db)
            .map_err(|error| Self::failure(work_ref, format!("the coordinator state: {error}")))?;
        let mut journal = CoordinatorJournal::new(&mut store);
        let epoch = journal
            .current_epoch(work_ref)
            .map_err(|error| Self::failure(work_ref, error.to_string()))?
            .ok_or_else(|| {
                Self::failure(
                    work_ref,
                    "the work is not claimed: nothing fences the promotion to an attempt",
                )
            })?;
        let intent = self.intent_of(objective, epoch)?;
        let report = promote::flow::execute(
            PromotionRequest {
                intent: &intent,
                evidence,
                repo: &self.project_dir,
                bundle: &self.bundle_path,
                candidate_branch: CANDIDATE_BRANCH,
            },
            &self.writer,
            &mut journal,
        )
        .map_err(|error| Self::failure(work_ref, error.to_string()))?;

        // A refused attempt moved nothing: it is an error with its reason,
        // never a publication of a commit that is not on the branch.
        let Some(commit) = report.commit.clone() else {
            let reason = report
                .conflict
                .as_ref()
                .map(|conflict| {
                    serde_json::to_string(conflict)
                        .unwrap_or_else(|_| "the attempt was refused".into())
                })
                .unwrap_or_else(|| {
                    "the attempt moved nothing and reported no conflict".to_string()
                });
            return Err(Self::failure(
                work_ref,
                format!("the promotion was refused: {reason}"),
            ));
        };

        let closure = self.closure_of(work_ref);
        let closed = closure
            .as_ref()
            .is_ok_and(|snapshot| snapshot.status == CLOSED_STATUS);
        Ok(Publication {
            revision: commit.clone(),
            closed,
            detail: detail_of(&report, &commit, &self.managed_branch, &closure),
        })
    }
}
