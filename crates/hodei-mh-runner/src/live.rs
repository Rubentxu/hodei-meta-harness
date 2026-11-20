//! The live runtime: select, claim, work, verify, publish — for real.
//!
//! The runtime owns the ORDER of an attempt. Selection and the claim
//! happen before any effect, so idle, ineligible and already-claimed work
//! never reaches the agent. The attempt ends in ONE terminal value, and
//! the claim is released exactly once whichever way it ends.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use run::runtime::{
    ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RunId, RuntimeError,
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

    /// The claims held right now, as work ref → holder.
    ///
    /// Selection reads it so work somebody else holds is skipped instead
    /// of selected and denied afterwards. The default is empty: a store
    /// that models no other holder has nothing to report.
    fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
        Ok(BTreeMap::new())
    }
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
    /// The attempt was stopped before its effects could be published.
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

/// A claim that is released exactly once, whichever way the attempt ends.
///
/// The guard is owned by the attempt itself, so an attempt whose task is
/// ABORTED (a Stop) still releases its claim when the future is dropped:
/// the release does not depend on the attempt reaching its end.
struct ClaimGuard {
    claims: Arc<dyn ClaimPort>,
    work_ref: String,
    epoch: u64,
    released: bool,
}

impl ClaimGuard {
    fn new(claims: Arc<dyn ClaimPort>, work_ref: &str, epoch: u64) -> Self {
        Self {
            claims,
            work_ref: work_ref.to_string(),
            epoch,
            released: false,
        }
    }

    /// Give the work back now. Calling it twice releases once.
    fn release_now(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.claims.release(&self.work_ref, self.epoch);
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.release_now();
    }
}

/// The cancellation state of the runtime: the attempt in flight and the
/// stops that arrived before their attempts installed their flags.
///
/// The state is shared through an `Arc` so the attempt and whoever raises
/// the stop read the SAME answer: a stop is never addressed to an attempt
/// that has not started, and never to the one that came after it.
///
/// Every pending stop is remembered by the run it names, so stops for
/// different runs never overwrite each other. A pending stop lives until
/// the run it names installs its flag or a newer run supersedes it.
#[derive(Debug, Default)]
struct Cancellations {
    state: Mutex<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    /// The stops of the runs whose attempts have not installed their flag
    /// yet. They are held here so those attempts begin already cancelled
    /// instead of running work nobody wants anymore.
    early: BTreeSet<RunId>,
    /// The run whose attempt is in flight, with the flag it reads.
    current: Option<(RunId, Arc<AtomicBool>)>,
}

impl Cancellations {
    /// Install the flag of `run`'s attempt and return it.
    ///
    /// The flag REPLACES the previous one, so a stop raised for an earlier
    /// attempt cannot be read by this one. It starts true when its own
    /// stop arrived before the attempt did: a stop raised while the run
    /// was starting is never lost. An attempt that installs late never
    /// steals the slot from a newer one.
    fn install(&self, run: RunId) -> Arc<AtomicBool> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // One attempt per run installs its flag once; a second install
        // keeps the flag a stop may already have reached.
        if let Some((current, flag)) = &state.current
            && *current == run
        {
            return flag.clone();
        }
        // The pending stops are addressed by run, so this install consumes
        // only its own. The stops of older runs are history: this install
        // supersedes them. The stops of newer runs stay pending.
        let stopped_early = state.early.remove(&run);
        state.early.retain(|pending| *pending > run);
        let cancelled = Arc::new(AtomicBool::new(stopped_early));
        if state
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current < run)
        {
            state.current = Some((run, cancelled.clone()));
        }
        cancelled
    }

    /// Deliver a stop to the attempt of `run`, or keep it for the attempt
    /// that has not started yet.
    ///
    /// A stop that belongs to an older run than the attempt in flight is
    /// history: it is dropped instead of reaching an attempt it was never
    /// raised for.
    fn cancel(&self, run: RunId) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &state.current {
            Some((current, flag)) if *current == run => {
                flag.store(true, Ordering::SeqCst);
            }
            Some((current, _)) if *current > run => {}
            _ => {
                state.early.insert(run);
            }
        }
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
    cancellations: Arc<Cancellations>,
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
            cancellations: Arc::new(Cancellations::default()),
        }
    }

    /// Ask the attempt of `run` to stop after the agent returns. The
    /// attempt then releases its claim without verifying or publishing.
    ///
    /// A stop for a run whose attempt has not started yet is REMEMBERED by
    /// that run: the attempt begins already cancelled, so a stop raised
    /// while the run was starting is never lost. A stop for an older run
    /// than the attempt in flight never reaches it.
    pub fn cancel(&self, run: RunId) {
        self.cancellations.cancel(run);
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

    async fn start(
        &self,
        run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError> {
        if request.project != self.project {
            return Err(RuntimeError(format!(
                "this runtime serves '{}', not '{}'",
                self.project, request.project
            )));
        }
        // A cancellation belongs to the run it was raised for: the attempt
        // gets a FRESH flag, and a stop that arrived before the attempt
        // did starts it cancelled instead of being lost.
        let cancelled = self.cancellations.install(run);
        // Work somebody else holds is not selected at all: it is not
        // eligible, and the chain skips it.
        let held = match self.claims.claims() {
            Ok(held) => held,
            Err(error) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::InfraFailed {
                        reason: format!("the held claims could not be read: {error}"),
                    },
                )));
            }
        };
        let selected = self
            .runner()
            .select_in(request.work_ref.as_deref(), &held)
            .await;
        let item = match selected {
            Ok(Some(item)) => item,
            Ok(None) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::NoEligibleWork {
                        reason: no_eligible_work(&held),
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
        // The guard is created with the claim and travels with the attempt:
        // a dropped attempt (aborted) still gives the work back.
        let guard = ClaimGuard::new(self.claims.clone(), &item.id, epoch);
        let objective = match self.requirements.requirements(&item) {
            Ok(objective) => objective,
            Err(error) => {
                drop(guard);
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::Unverifiable {
                        work_ref: item.id,
                        reason: error.to_string(),
                    },
                )));
            }
        };
        let agent = self.agent.clone();
        let verifier = self.verifier.clone();
        let publisher = self.publisher.clone();
        Ok(ObjectiveHandle::new(async move {
            // A stop that arrived while the attempt was being set up ends
            // it before any effect: no sandbox is created for work nobody
            // wants anymore, and the claim goes back with the attempt.
            let end = if cancelled.load(Ordering::SeqCst) {
                AttemptEnd::Cancelled {
                    work_ref: objective.work_ref.clone(),
                }
            } else {
                attempt(&objective, agent, verifier, publisher, cancelled).await
            };
            drop(guard);
            outcome_of(end)
        }))
    }

    /// Signal the attempt of `run`: it ends after the agent returns,
    /// without verifying or publishing, and releases everything it owns.
    fn cancel(&self, run: RunId) {
        LiveRuntime::cancel(self, run)
    }
}

/// Why the selection found nothing eligible. Work somebody else holds is
/// NAMED: a bare "no work" would hide that the queue is held, not empty.
fn no_eligible_work(held: &BTreeMap<String, String>) -> String {
    if held.is_empty() {
        return "no eligible work".to_string();
    }
    format!(
        "no eligible work: {} claim(s) held: {}",
        held.len(),
        held.keys().cloned().collect::<Vec<_>>().join(", ")
    )
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
    // A stop observed while verifying ends the attempt BEFORE its verdict
    // is turned into a rejection: the work was not judged, it was stopped.
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
    }
    let required = objective.required();
    if !verification.covers(&required) {
        return AttemptEnd::ChecksNotHeld {
            work_ref,
            missing: verification.missing(&required),
        };
    }
    // The last gate: verifying is harmless, publishing is not.
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
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
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
        seen: Mutex<Vec<String>>,
    }

    impl CountingAgent {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn seen(&self) -> Vec<String> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl AgentPort for CountingAgent {
        async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(objective.work_ref.clone());
            }
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
        held: BTreeMap<String, String>,
    }

    impl FakeClaims {
        fn granting(epoch: u64) -> Self {
            Self {
                grant: Some(Claimed { epoch }),
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: BTreeMap::new(),
            }
        }

        fn denying() -> Self {
            Self {
                grant: None,
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: BTreeMap::new(),
            }
        }

        /// A claim store that already reports `held` and grants `epoch`.
        fn holding(epoch: u64, held: &[(&str, &str)]) -> Self {
            Self {
                grant: Some(Claimed { epoch }),
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: held
                    .iter()
                    .map(|(work_ref, holder)| (work_ref.to_string(), holder.to_string()))
                    .collect(),
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

        fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
            Ok(self.held.clone())
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
        run: RunId,
        work_ref: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ObjectiveHandle, RuntimeError>> {
        runtime.start(
            run,
            ObjectiveRequest {
                project: "project".into(),
                work_ref: work_ref.map(str::to_string),
            },
        )
    }

    async fn outcome_of_start(runtime: &LiveRuntime, work_ref: Option<&str>) -> ObjectiveOutcome {
        start(runtime, RunId(1), work_ref)
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
    async fn held_claims_are_skipped_by_selection_and_not_eligible_by_name() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::holding(7, &[("T-1", "another-agent")]));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open), item("T-2", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        // Unnamed: the claimed work is skipped, not selected and denied
        // afterwards.
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-2"),
            "{outcome:?}"
        );
        assert_eq!(agent.seen(), vec!["T-2".to_string()]);
        assert_eq!(claims.releases(), vec![("T-2".to_string(), 7)]);

        // Named: the claimed work is not eligible, and nothing runs.
        let named = outcome_of_start(&runtime, Some("T-1")).await;
        assert!(
            matches!(named, ObjectiveOutcome::Rejected { ref reason, .. } if reason.contains("ClaimedBy")),
            "{named:?}"
        );
        assert_eq!(agent.calls(), 1, "named claimed work costs no agent call");
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
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        runtime.cancel(RunId(1));
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stop that precedes the attempt costs no effect"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 17)]);
    }

    #[tokio::test]
    async fn a_stop_that_arrives_before_the_attempt_starts_is_not_lost() {
        // The window between the engine applying a Start and the runtime
        // installing its attempt: the stop lands with no attempt to signal.
        // The attempt must then BEGIN cancelled instead of running work
        // nobody wants anymore.
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
        runtime.cancel(RunId(1));
        let outcome = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stopped attempt never reaches the agent"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 17)],
            "the claim goes back instead of staying held"
        );
    }

    #[tokio::test]
    async fn every_pending_stop_is_remembered_by_its_run() {
        // Two stops raised before their attempts start, each naming a
        // different run: the second one must not overwrite the first, so
        // run 1's attempt still begins cancelled when it finally starts.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(19));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        runtime.cancel(RunId(1));
        runtime.cancel(RunId(2));
        let outcome = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "the stop of run 1 survives the stop of run 2: {outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stopped attempt never reaches the agent"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 19)],
            "the claim goes back instead of staying held"
        );
    }

    #[tokio::test]
    async fn a_pending_stop_does_not_cancel_a_different_run() {
        // A pending stop names the run it was raised for: the attempt of
        // another run is not cancelled by it.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(23));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        runtime.cancel(RunId(1));
        let outcome = start(&runtime, RunId(2), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "a stop for run 1 must not cancel run 2: {outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 23)]);
    }

    #[tokio::test]
    async fn a_stop_during_verification_never_reaches_the_publisher() {
        struct GatedVerifier {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
            verified: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl VerifierPort for GatedVerifier {
            async fn verify(
                &self,
                objective: &Objective,
                _candidate: &Candidate,
            ) -> Result<Verification, VerifierError> {
                self.entered.notify_one();
                self.gate.notified().await;
                self.verified.store(true, Ordering::SeqCst);
                Ok(Verification::satisfying(&objective.required()))
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let verified = Arc::new(AtomicBool::new(false));
        let claims = Arc::new(FakeClaims::granting(29));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(CountingAgent::default()),
            Arc::new(GatedVerifier {
                entered: entered.clone(),
                gate: gate.clone(),
                verified: verified.clone(),
            }),
            publisher.clone(),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The attempt is inside the verifier when the stop arrives.
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert!(
            verified.load(Ordering::SeqCst),
            "the verification that was already running completed"
        );
        assert_eq!(
            publisher.calls(),
            0,
            "a stop observed after verification publishes nothing"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 29)]);
    }

    #[tokio::test]
    async fn a_stop_during_publication_leaves_the_completed_effect_confirmed() {
        struct GatedPublisher {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl PublisherPort for GatedPublisher {
            async fn publish(
                &self,
                _objective: &Objective,
                _candidate: &Candidate,
                _verification: &Verification,
            ) -> Result<Publication, PublisherError> {
                self.entered.notify_one();
                self.gate.notified().await;
                Ok(Publication {
                    revision: "1f0c3a5b".into(),
                    closed: true,
                    detail: "the publisher moved the effects".into(),
                })
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let claims = Arc::new(FakeClaims::granting(31));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(CountingAgent::default()),
            Arc::new(HoldingVerifier),
            Arc::new(GatedPublisher {
                entered: entered.clone(),
                gate: gate.clone(),
            }),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The stop lands while the effect is being moved: the effect
        // completes, so it is a FACT and the attempt reports it.
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "{outcome:?}"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 31)]);
    }

    #[tokio::test]
    async fn a_stop_for_one_run_does_not_cancel_the_next() {
        // A stop that belongs to a run that already ended must never reach
        // the attempt of the run that followed it.
        struct GatedAgent {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl AgentPort for GatedAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.entered.notify_one();
                self.gate.notified().await;
                Ok(candidate())
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let claims = Arc::new(FakeClaims::granting(37));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(GatedAgent {
                entered: entered.clone(),
                gate: gate.clone(),
            }),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        // Run 1 is stopped before its attempt begins: it never runs.
        runtime.cancel(RunId(1));
        let first = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(first, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{first:?}"
        );

        // Run 2 is in flight when the stop of run 1 is repeated.
        let second = start(&runtime, RunId(2), None)
            .await
            .expect("the runtime accepts the next objective");
        let completion = tokio::spawn(async move { second.completion().await });
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "a stop for run 1 must not cancel run 2: {outcome:?}"
        );
        assert_eq!(publisher.calls(), 1);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 37), ("T-1".to_string(), 37)],
            "both runs gave their claim back"
        );
    }

    #[tokio::test]
    async fn idle_with_held_claims_names_the_held_work() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::holding(7, &[("T-1", "another-agent")]));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        match outcome {
            ObjectiveOutcome::Idle { reason } => {
                assert!(reason.contains("T-1"), "the held work is named: {reason}");
                assert!(reason.contains("claim"), "the reason says why: {reason}");
            }
            other => panic!("expected idle, got {other:?}"),
        }
        assert_eq!(agent.calls(), 0);
        assert_eq!(claims.claim_calls(), 0);
    }

    #[tokio::test]
    async fn a_cooperative_stop_lets_the_attempt_end_and_releases_its_claim() {
        // An agent that winds down when the stop reaches it — the way the
        // real one closes its session and releases its sandbox — instead
        // of being dropped mid-flight.
        struct WindingDownAgent {
            started: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
            closed: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl AgentPort for WindingDownAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.started.notify_one();
                self.gate.notified().await;
                self.closed.store(true, Ordering::SeqCst);
                Ok(candidate())
            }
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(AtomicBool::new(false));
        let claims = Arc::new(FakeClaims::granting(23));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(WindingDownAgent {
                started: started.clone(),
                gate: gate.clone(),
                closed: closed.clone(),
            }),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The attempt is inside the agent when the stop arrives.
        started.notified().await;
        // The stop is a SIGNAL: the attempt returns and closes what it owns
        // instead of being dropped.
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert!(
            closed.load(Ordering::SeqCst),
            "the attempt reached its own end and closed its session"
        );
        assert_eq!(
            publisher.calls(),
            0,
            "a cancelled attempt publishes nothing"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 23)]);
    }

    #[tokio::test]
    async fn an_aborted_attempt_still_releases_its_claim() {
        // A Stop aborts the execution task: the attempt future is dropped
        // mid-agent. The claim must be given back even then — a cancelled
        // attempt that keeps the work claimed starves the next one.
        struct BlockingAgent {
            started: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl AgentPort for BlockingAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.started.notify_one();
                std::future::pending::<()>().await;
                Ok(candidate())
            }
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let agent = Arc::new(BlockingAgent {
            started: started.clone(),
        });
        let claims = Arc::new(FakeClaims::granting(19));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            agent,
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        started.notified().await;
        completion.abort();
        let _ = completion.await;
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 19)],
            "an aborted attempt releases its claim exactly once"
        );
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
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "another-project".into(),
                    work_ref: None,
                },
            )
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
