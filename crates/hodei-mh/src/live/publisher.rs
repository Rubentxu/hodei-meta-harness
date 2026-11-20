//! The publisher of the live pipeline: promote the candidate by CAS and
//! close the work item through the official tracker CLI.
//!
//! The candidate moves only with the evidence that speaks for it, and the
//! closure is only CONFIRMED when the re-read shows the work closed. An
//! effect that moved without a confirmed closure is reported as such, not
//! upgraded to success.

use std::path::PathBuf;

use promote::intent::{PromotionFacts, PromotionIntent};
use promote::writer::{BacklogWorkWriter, WorkWriter};
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
    pub writer: BacklogWorkWriter,
}

impl VerifiedPublisher {
    fn failure(work_ref: &str, reason: impl Into<String>) -> PublisherError {
        PublisherError {
            work_ref: work_ref.to_string(),
            reason: reason.into(),
        }
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
        if let Some(parent) = self.bundle_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                Self::failure(work_ref, format!("the bundle directory: {error}"))
            })?;
        }
        std::fs::write(&self.bundle_path, &candidate.bundle)
            .map_err(|error| Self::failure(work_ref, format!("the candidate bundle: {error}")))?;
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
        let intent = PromotionIntent::new(PromotionFacts {
            project: self.project.clone(),
            work_ref: work_ref.to_string(),
            epoch: 1,
            managed_branch: self.managed_branch.clone(),
            expected_base: base.clone(),
            expected_candidate,
            target_status: CLOSED_STATUS.to_string(),
            note: "Evidence: the candidate was verified in a clean environment and promoted by compare-and-swap.".to_string(),
            check_ac: (1..=objective.checks.len()).collect(),
            final_summary: format!("{work_ref} completed by the live supervisor."),
        })
        .map_err(|error| Self::failure(work_ref, error.to_string()))?;
        let commit = promote::git::promote_verified(
            &self.project_dir,
            &self.managed_branch,
            &self.bundle_path,
            CANDIDATE_BRANCH,
            &base,
            evidence,
            work_ref,
        )
        .map_err(|error| Self::failure(work_ref, error.to_string()))?;
        // The candidate moved. The closure is a separate effect, and its
        // answer is the re-read: a failure here leaves the effects moved
        // without a confirmed closure, and that is what is reported.
        let closure = self
            .writer
            .apply(work_ref, &intent.update)
            .and_then(|()| self.writer.read_ticket(work_ref));
        let (closed, detail) = match closure {
            Ok(snapshot) => {
                let closed = snapshot.status == CLOSED_STATUS;
                (
                    closed,
                    format!(
                        "promoted {commit} onto {}; the re-read says the work is {}",
                        self.managed_branch,
                        if closed { "closed" } else { "still open" }
                    ),
                )
            }
            Err(error) => (
                false,
                format!(
                    "promoted {commit} onto {}; the closure failed: {error}",
                    self.managed_branch
                ),
            ),
        };
        Ok(Publication {
            revision: commit,
            closed,
            detail,
        })
    }
}
