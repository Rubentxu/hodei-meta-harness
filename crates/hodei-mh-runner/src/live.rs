//! The live runtime: select, claim, work, verify, publish — for real.
//!
//! The runtime owns the ORDER of an attempt. Selection and the claim
//! happen before any effect, so idle, ineligible and already-claimed work
//! never reaches the agent. The attempt ends in ONE terminal value, and
//! the claim is released exactly once whichever way it ends.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use run::runtime::{
    ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RuntimeError,
    RuntimeIdentity,
};
use tracker::core::TrackerRead;

use crate::core::{GoalRunner, RunnerError};
use crate::ports::{AgentPort, Objective, PublisherPort, RequirementsPort, VerifierPort};

/// The claim this runtime holds on the work it selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claimed {
    /// The epoch of the claim. It is the value the release must state, so
    /// a stale attempt cannot release a claim it no longer owns.
    pub epoch: u64,
}

/// Why the claim was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    #[error("the work is already claimed by {holder}")]
    Denied { holder: String },
    #[error("the claim store could not be reached: {reason}")]
    Unavailable { reason: String },
}

/// The claim store the live runtime talks to.
pub trait ClaimPort: Send + Sync {
    /// Take the work for this runtime. One holder per work: a second
    /// claim is denied with the holder's name.
    fn claim(&self, work_ref: &str) -> Result<Claimed, ClaimError>;

    /// Give the work back. Called once per successful claim, whatever the
    /// attempt ended with.
    fn release(&self, work_ref: &str, epoch: u64);
}

/// How a live attempt ended, before it is read as an objective outcome.
///
/// The vocabulary is TOTAL: every way an attempt can stop has a value,
/// so no path ends without a recorded verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptEnd {
    /// The candidate was published; `closed` says whether the work was
    /// closed with it.
    Published {
        work_ref: String,
        closed: bool,
        detail: String,
    },
    /// The verification did not cover every required check.
    ChecksNotHeld {
        work_ref: String,
        missing: Vec<String>,
    },
    /// The agent produced no new revision to verify.
    NoNewCommit { work_ref: String },
    /// The named work was not eligible in the observed snapshot.
    NamedNotEligible { work_ref: String, reason: String },
    /// The candidate could not be verified.
    Unverifiable { work_ref: String, reason: String },
    /// Somebody else holds the work.
    ClaimDenied { work_ref: String, holder: String },
    /// The attempt was cancelled after the agent returned.
    Cancelled { work_ref: String },
    /// The agent could not run the objective.
    AgentFailed { work_ref: String, reason: String },
    /// The infrastructure failed around the attempt.
    InfraFailed { reason: String },
    /// The candidate could not be promoted.
    PromotionFailed { work_ref: String, reason: String },
    /// There was no eligible work at all.
    NoEligibleWork { reason: String },
}

/// The outcome the attempt ended with.
///
/// Effects that moved without a confirmed closure are a FAILURE, never a
/// confirmation: the record must not claim the work was closed.
pub fn outcome_of(end: AttemptEnd) -> ObjectiveOutcome {
    match end {
        AttemptEnd::Published {
            work_ref,
            closed: true,
            detail,
        } => ObjectiveOutcome::Confirmed { work_ref, detail },
        AttemptEnd::Published {
            work_ref,
            closed: false,
            detail,
        } => ObjectiveOutcome::Failed {
            reason: format!(
                "the effects of {work_ref} were moved but the closure was not confirmed: {detail}"
            ),
        },
        AttemptEnd::ChecksNotHeld { work_ref, missing } => ObjectiveOutcome::Rejected {
            work_ref,
            reason: format!("the evidence does not satisfy {}", missing.join(", ")),
        },
        AttemptEnd::NoNewCommit { work_ref } => ObjectiveOutcome::Rejected {
            work_ref,
            reason: "the agent produced no new commit".into(),
        },
        AttemptEnd::NamedNotEligible { work_ref, reason } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} is not eligible: {reason}"),
            work_ref,
        },
        AttemptEnd::Unverifiable { work_ref, reason } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} could not be verified: {reason}"),
            work_ref,
        },
        AttemptEnd::ClaimDenied { work_ref, holder } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} is claimed by {holder}"),
            work_ref,
        },
        AttemptEnd::Cancelled { work_ref } => ObjectiveOutcome::Failed {
            reason: format!("{work_ref} was cancelled before it was published"),
        },
        AttemptEnd::AgentFailed { work_ref, reason } => ObjectiveOutcome::Failed {
            reason: format!("the agent could not run {work_ref}: {reason}"),
        },
        AttemptEnd::InfraFailed { reason } => ObjectiveOutcome::Failed { reason },
        AttemptEnd::PromotionFailed { work_ref, reason } => ObjectiveOutcome::Failed {
            reason: format!("the candidate of {work_ref} could not be promoted: {reason}"),
        },
        AttemptEnd::NoEligibleWork { reason } => ObjectiveOutcome::Idle { reason },
    }
}

/// The live runtime: the product's ports, wired in the product's order.
pub struct LiveRuntime {
    identity: RuntimeIdentity,
    project: String,
    tracker: Arc<dyn TrackerRead>,
    requirements: Arc<dyn RequirementsPort>,
    agent: Arc<dyn AgentPort>,
    verifier: Arc<dyn VerifierPort>,
    publisher: Arc<dyn PublisherPort>,
    claims: Arc<dyn ClaimPort>,
    cancelled: Arc<AtomicBool>,
}

impl LiveRuntime {
    /// A runtime that serves `project` through the given ports.
    pub fn new(
        project: impl Into<String>,
        tracker: Arc<dyn TrackerRead>,
        requirements: Arc<dyn RequirementsPort>,
        agent: Arc<dyn AgentPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<dyn PublisherPort>,
        claims: Arc<dyn ClaimPort>,
    ) -> Self {
        let project = project.into();
        Self {
            identity: RuntimeIdentity::live(project.clone()),
            project,
            tracker,
            requirements,
            agent,
            verifier,
            publisher,
            claims,
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Ask the current attempt to stop after the agent returns. The
    /// attempt then releases its claim without verifying or publishing.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    fn runner(&self) -> GoalRunner {
        GoalRunner::new(
            self.tracker.clone(),
            self.requirements.clone(),
            self.agent.clone(),
            self.verifier.clone(),
            self.publisher.clone(),
        )
    }
}

#[async_trait::async_trait]
impl ObjectiveRuntime for LiveRuntime {
    fn identity(&self) -> RuntimeIdentity {
        self.identity.clone()
    }

    async fn start(&self, request: ObjectiveRequest) -> Result<ObjectiveHandle, RuntimeError> {
        if request.project != self.project {
            return Err(RuntimeError(format!(
                "this runtime serves '{}', not '{}'",
                self.project, request.project
            )));
        }
        // A cancellation belongs to the attempt it was raised for, never
        // to the next one.
        self.cancelled.store(false, Ordering::SeqCst);
        let selected = self
            .runner()
            .select_in(request.work_ref.as_deref(), &BTreeMap::new())
            .await;
        let item = match selected {
            Ok(Some(item)) => item,
            Ok(None) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::NoEligibleWork {
                        reason: "no eligible work".into(),
                    },
                )));
            }
            Err(error) => {
                return Ok(ObjectiveHandle::finished(outcome_of(early_selection_end(
                    request.work_ref.as_deref(),
                    error,
                ))));
            }
        };
        let epoch = match self.claims.claim(&item.id) {
            Ok(claimed) => claimed.epoch,
            Err(ClaimError::Denied { holder }) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::ClaimDenied {
                        work_ref: item.id,
                        holder,
                    },
                )));
            }
            Err(ClaimError::Unavailable { reason }) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::InfraFailed { reason },
                )));
            }
        };
        let objective = match self.requirements.requirements(&item) {
            Ok(objective) => objective,
            Err(error) => {
                self.claims.release(&item.id, epoch);
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::Unverifiable {
                        work_ref: item.id,
                        reason: error.to_string(),
                    },
                )));
            }
        };
        let work_ref = item.id;
        let agent = self.agent.clone();
        let verifier = self.verifier.clone();
        let publisher = self.publisher.clone();
        let claims = self.claims.clone();
        let cancelled = self.cancelled.clone();
        Ok(ObjectiveHandle::new(async move {
            let end = attempt(&objective, agent, verifier, publisher, cancelled).await;
            claims.release(&work_ref, epoch);
            outcome_of(end)
        }))
    }
}

/// The attempt that ended before the agent ran: the selection itself
/// refused, either because the named work is not eligible or because the
/// tracker could not be read.
fn early_selection_end(work_ref: Option<&str>, error: RunnerError) -> AttemptEnd {
    match error {
        RunnerError::Requirements(reason) => AttemptEnd::NamedNotEligible {
            work_ref: work_ref.unwrap_or_default().to_string(),
            reason,
        },
        RunnerError::Tracker(error) => AttemptEnd::InfraFailed {
            reason: error.to_string(),
        },
    }
}

/// One live attempt from the agent to the publisher, ending in one
/// terminal value. Every effect it performs is a port call.
async fn attempt(
    objective: &Objective,
    agent: Arc<dyn AgentPort>,
    verifier: Arc<dyn VerifierPort>,
    publisher: Arc<dyn PublisherPort>,
    cancelled: Arc<AtomicBool>,
) -> AttemptEnd {
    let work_ref = objective.work_ref.clone();
    let candidate = match agent.execute(objective).await {
        Ok(candidate) => candidate,
        Err(error) => {
            return AttemptEnd::AgentFailed {
                work_ref,
                reason: error.to_string(),
            };
        }
    };
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
    }
    let verification = match verifier.verify(objective, &candidate).await {
        Ok(verification) => verification,
        Err(error) => {
            return AttemptEnd::Unverifiable {
                work_ref,
                reason: error.to_string(),
            };
        }
    };
    let required = objective.required();
    if !verification.covers(&required) {
        return AttemptEnd::ChecksNotHeld {
            work_ref,
            missing: verification.missing(&required),
        };
    }
    match publisher
        .publish(objective, &candidate, &verification)
        .await
    {
        Ok(publication) => AttemptEnd::Published {
            work_ref,
            closed: publication.closed,
            detail: publication.detail,
        },
        Err(error) => AttemptEnd::PromotionFailed {
            work_ref,
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::ports::{
        AgentError, Publication, PublisherError, RequirementsError, Verification, VerifierError,
    };
    use tracker::core::{WorkItem, WorkStatus};
    use tracker::fake::FakeTracker;
    use verify::git::{GitRevision, GitTree};
    use verify::runner::{Candidate, Check};
    use verify::transport::TransportDigest;

    struct FixedRequirements;

    impl RequirementsPort for FixedRequirements {
        fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
            Ok(Objective {
                work_ref: item.id.clone(),
                instruction: format!("do {}", item.id),
                checks: vec![Check {
                    id: "AC-1".into(),
                    command: "true".into(),
                }],
            })
        }
    }

    struct FailingRequirements;

    impl RequirementsPort for FailingRequirements {
        fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
            Err(RequirementsError {
                work_ref: item.id.clone(),
                reason: "the contract could not be read".into(),
            })
        }
    }

    #[derive(Default)]
    struct CountingAgent {
        calls: AtomicUsize,
    }

    impl CountingAgent {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl AgentPort for CountingAgent {
        async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(candidate())
        }
    }

    struct HoldingVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for HoldingVerifier {
        async fn verify(
            &self,
            objective: &Objective,
            _candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            Ok(Verification::satisfying(&objective.required()))
        }
    }

    struct EmptyVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for EmptyVerifier {
        async fn verify(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            Ok(Verification {
                satisfied: Vec::new(),
                evidence: None,
            })
        }
    }

    struct CountingPublisher {
        calls: AtomicUsize,
        closed: bool,
    }

    impl CountingPublisher {
        fn confirm_closing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                closed: true,
            }
        }

        fn confirm_without_closure() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                closed: false,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PublisherPort for CountingPublisher {
        async fn publish(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
            _verification: &Verification,
        ) -> Result<Publication, PublisherError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Publication {
                revision: "1f0c3a5b".into(),
                closed: self.closed,
                detail: "the publisher moved the effects".into(),
            })
        }
    }

    struct FakeClaims {
        grant: Option<Claimed>,
        claim_calls: AtomicUsize,
        releases: Mutex<Vec<(String, u64)>>,
    }

    impl FakeClaims {
        fn granting(epoch: u64) -> Self {
            Self {
                grant: Some(Claimed { epoch }),
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
            }
        }

        fn denying() -> Self {
            Self {
                grant: None,
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
            }
        }

        fn claim_calls(&self) -> usize {
            self.claim_calls.load(Ordering::SeqCst)
        }

        fn releases(&self) -> Vec<(String, u64)> {
            self.releases
                .lock()
                .map(|releases| releases.clone())
                .unwrap_or_default()
        }
    }

    impl ClaimPort for FakeClaims {
        fn claim(&self, _work_ref: &str) -> Result<Claimed, ClaimError> {
            self.claim_calls.fetch_add(1, Ordering::SeqCst);
            match self.grant {
                Some(claimed) => Ok(claimed),
                None => Err(ClaimError::Denied {
                    holder: "another-agent".into(),
                }),
            }
        }

        fn release(&self, work_ref: &str, epoch: u64) {
            if let Ok(mut releases) = self.releases.lock() {
                releases.push((work_ref.to_string(), epoch));
            }
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            revision: GitRevision::parse("1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5")
                .expect("a revision"),
            tree: GitTree::parse("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b").expect("a tree"),
            transport_digest: TransportDigest::of(b"the bundle"),
            bundle: b"the bundle".to_vec(),
        }
    }

    fn item(id: &str, status: WorkStatus) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("work {id}"),
            status,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        }
    }

    fn runtime_with(
        items: Vec<WorkItem>,
        requirements: Arc<dyn RequirementsPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<CountingPublisher>,
        claims: Arc<FakeClaims>,
        agent: Arc<CountingAgent>,
    ) -> LiveRuntime {
        LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(items)),
            requirements,
            agent,
            verifier,
            publisher,
            claims,
        )
    }

    fn start(
        runtime: &LiveRuntime,
        work_ref: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ObjectiveHandle, RuntimeError>> {
        runtime.start(ObjectiveRequest {
            project: "project".into(),
            work_ref: work_ref.map(str::to_string),
        })
    }

    async fn outcome_of_start(runtime: &LiveRuntime, work_ref: Option<&str>) -> ObjectiveOutcome {
        start(runtime, work_ref)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await
    }

    #[test]
    fn every_attempt_end_maps_to_its_outcome() {
        assert_eq!(
            outcome_of(AttemptEnd::Published {
                work_ref: "T-1".into(),
                closed: true,
                detail: "published".into(),
            }),
            ObjectiveOutcome::Confirmed {
                work_ref: "T-1".into(),
                detail: "published".into(),
            }
        );
        let unconfirmed = outcome_of(AttemptEnd::Published {
            work_ref: "T-1".into(),
            closed: false,
            detail: "moved".into(),
        });
        assert!(
            matches!(unconfirmed, ObjectiveOutcome::Failed { ref reason } if reason.contains("closure")),
            "{unconfirmed:?}"
        );
        for end in [
            AttemptEnd::ChecksNotHeld {
                work_ref: "T-1".into(),
                missing: vec!["AC-1".into()],
            },
            AttemptEnd::NoNewCommit {
                work_ref: "T-1".into(),
            },
            AttemptEnd::NamedNotEligible {
                work_ref: "T-1".into(),
                reason: "done".into(),
            },
            AttemptEnd::Unverifiable {
                work_ref: "T-1".into(),
                reason: "no evidence".into(),
            },
            AttemptEnd::ClaimDenied {
                work_ref: "T-1".into(),
                holder: "another-agent".into(),
            },
        ] {
            assert!(
                matches!(outcome_of(end.clone()), ObjectiveOutcome::Rejected { .. }),
                "{end:?} must be rejected"
            );
        }
        for end in [
            AttemptEnd::AgentFailed {
                work_ref: "T-1".into(),
                reason: "the session ended".into(),
            },
            AttemptEnd::InfraFailed {
                reason: "the sandbox could not be created".into(),
            },
            AttemptEnd::PromotionFailed {
                work_ref: "T-1".into(),
                reason: "the ref moved concurrently".into(),
            },
        ] {
            assert!(
                matches!(outcome_of(end.clone()), ObjectiveOutcome::Failed { .. }),
                "{end:?} must fail"
            );
        }
        assert_eq!(
            outcome_of(AttemptEnd::NoEligibleWork {
                reason: "none".into(),
            }),
            ObjectiveOutcome::Idle {
                reason: "none".into(),
            }
        );
    }

    #[tokio::test]
    async fn idle_work_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(7));
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Idle { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0, "idle work costs no agent call");
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.claim_calls(), 0, "idle work takes no claim");
        assert!(claims.releases().is_empty());
    }

    #[tokio::test]
    async fn work_that_is_not_eligible_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(7));
        let runtime = runtime_with(
            vec![item("T-2", WorkStatus::Done)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, Some("T-2")).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.claim_calls(), 0);
    }

    #[tokio::test]
    async fn a_denied_claim_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::denying());
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { ref reason, .. } if reason.contains("another-agent")),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert!(
            claims.releases().is_empty(),
            "a claim that was never taken is not released"
        );
    }

    #[tokio::test]
    async fn an_unverifiable_objective_never_reaches_the_agent_and_releases_the_claim() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(3));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FailingRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 3)]);
    }

    #[tokio::test]
    async fn checks_that_do_not_hold_are_rejected_and_never_published() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(5));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(EmptyVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(
            publisher.calls(),
            0,
            "a rejected objective is never published"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 5)]);
    }

    #[tokio::test]
    async fn a_published_objective_is_confirmed_and_releases_the_claim_once() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(11));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 11)]);
    }

    #[tokio::test]
    async fn moved_effects_without_a_confirmed_closure_are_a_failure() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_without_closure());
        let claims = Arc::new(FakeClaims::granting(13));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("closure")),
            "{outcome:?}"
        );
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 13)]);
    }

    #[tokio::test]
    async fn a_cancelled_attempt_is_not_verified_or_published() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(17));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let handle = start(&runtime, None)
            .await
            .expect("the runtime accepts the objective");
        runtime.cancel();
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 17)]);
    }

    #[tokio::test]
    async fn the_live_runtime_does_not_claim_a_ttl_it_cannot_renew() {
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            Arc::new(FakeClaims::granting(1)),
            Arc::new(CountingAgent::default()),
        );
        assert!(
            matches!(
                runtime.renew(300).await,
                run::runtime::RenewOutcome::Unsupported { .. }
            ),
            "the lifecycle backend has no ttl renewal"
        );
    }

    #[tokio::test]
    async fn a_project_mismatch_is_refused_instead_of_served() {
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            Arc::new(FakeClaims::granting(1)),
            Arc::new(CountingAgent::default()),
        );
        let refused = runtime
            .start(ObjectiveRequest {
                project: "another-project".into(),
                work_ref: None,
            })
            .await;
        assert!(refused.is_err(), "the runtime serves its own project only");
    }

    #[tokio::test]
    async fn a_tracker_that_cannot_be_read_is_an_infrastructure_failure() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(1));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::failing("simulated outage")),
            Arc::new(FixedRequirements),
            agent.clone(),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert!(claims.releases().is_empty());
    }
}
