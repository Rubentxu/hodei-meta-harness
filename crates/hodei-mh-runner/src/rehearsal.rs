//! The rehearsal: deterministic adapters with NO external effects.
//!
//! The rehearsal runs the SAME flow as the product — select, work,
//! verify, publish — with adapters that touch nothing outside this
//! process. It names itself a rehearsal, and the publication it records
//! says that nothing was closed: a rehearsal never pretends to be a
//! live execution.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use run::runtime::{
    ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RenewOutcome, RunId,
    RuntimeError, RuntimeIdentity,
};
use tokio::sync::Notify;
use tracker::core::{WorkItem, WorkStatus};
use verify::git::{GitRevision, GitTree};
use verify::runner::{Candidate, Check};
use verify::transport::TransportDigest;

use crate::core::GoalRunner;
use crate::ports::{
    AgentError, AgentPort, Objective, ObjectiveCheck, Publication, PublisherError, PublisherPort,
    RequirementsError, RequirementsPort, Verification, VerifierError, VerifierPort,
};

/// The revision the rehearsal's candidate claims to be.
const REHEARSAL_REVISION: &str = "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";
/// The content the rehearsal's candidate claims to carry.
const REHEARSAL_TREE: &str = "3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b";
/// The bytes the rehearsal's candidate travels in.
const REHEARSAL_BUNDLE: &[u8] = b"the rehearsal bundle";

/// The work the rehearsal knows about: two items, the second one
/// depending on the first, so the selection order is observable.
pub fn scenario() -> Vec<WorkItem> {
    vec![
        WorkItem {
            id: "T-1".into(),
            title: "the first piece of work".into(),
            status: WorkStatus::Open,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        },
        WorkItem {
            id: "T-2".into(),
            title: "the dependent piece of work".into(),
            status: WorkStatus::Open,
            provider_ready: Some(true),
            dependencies: vec!["T-1".into()],
        },
    ]
}

/// The scenario with the work the CALLER NAMED added to it.
///
/// The rehearsal rehearses the work it is given: a name its fixed
/// scenario does not know is not an error, it is work the rehearsal
/// performs with its scripted adapters. The item lives in this process
/// and in no other: nothing is created outside it.
fn scenario_with(work_ref: Option<&str>) -> Vec<WorkItem> {
    let mut items = scenario();
    if let Some(work_ref) = work_ref
        && !items.iter().any(|item| item.id == work_ref)
    {
        items.push(WorkItem {
            id: work_ref.into(),
            title: format!("the named work of the rehearsal ({work_ref})"),
            status: WorkStatus::Open,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        });
    }
    items
}

/// The requirements the rehearsal states for a work item: one check per
/// item, so the flow has something to judge.
struct ScriptedRequirements;

impl RequirementsPort for ScriptedRequirements {
    fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
        Ok(Objective {
            work_ref: item.id.clone(),
            instruction: format!("rehearse the work of {} ({})", item.id, item.title),
            checks: vec![ObjectiveCheck::free(Check {
                id: "AC-1".into(),
                command: format!("rehearse {}", item.id),
            })],
        })
    }
}

/// The agent of the rehearsal: it produces a FIXED candidate. It opens
/// no session, runs no process and calls no model.
struct ScriptedAgent;

#[async_trait::async_trait]
impl AgentPort for ScriptedAgent {
    async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError> {
        let revision = GitRevision::parse(REHEARSAL_REVISION).map_err(|error| AgentError {
            work_ref: objective.work_ref.clone(),
            reason: error.to_string(),
        })?;
        let tree = GitTree::parse(REHEARSAL_TREE).map_err(|error| AgentError {
            work_ref: objective.work_ref.clone(),
            reason: error.to_string(),
        })?;
        Ok(Candidate {
            revision,
            tree,
            transport_digest: TransportDigest::of(REHEARSAL_BUNDLE),
            bundle: REHEARSAL_BUNDLE.to_vec(),
        })
    }
}

/// The verifier of the rehearsal: it STATES the verdict. It measures
/// nothing, so it carries no evidence — a rehearsal that claimed
/// evidence would be claiming a measurement that never happened.
struct ScriptedVerifier {
    holds: bool,
}

#[async_trait::async_trait]
impl VerifierPort for ScriptedVerifier {
    async fn verify(
        &self,
        objective: &Objective,
        _candidate: &Candidate,
    ) -> Result<Verification, VerifierError> {
        let required = objective.required();
        Ok(if self.holds {
            Verification::satisfying(&required)
        } else {
            Verification {
                satisfied: Vec::new(),
                evidence: None,
            }
        })
    }
}

/// The publisher of the rehearsal: it RECORDS what would have been
/// published.
///
/// It holds no writer at all — not even an in-memory one — so the
/// rehearsal structurally cannot close a ticket or move a ref.
pub struct RecordingPublisher {
    publications: Mutex<Vec<Publication>>,
}

impl RecordingPublisher {
    pub fn new() -> Self {
        Self {
            publications: Mutex::new(Vec::new()),
        }
    }

    /// Everything the rehearsal recorded. Nothing else happened.
    pub fn publications(&self) -> Vec<Publication> {
        self.publications
            .lock()
            .map(|recorded| recorded.clone())
            .unwrap_or_default()
    }
}

impl Default for RecordingPublisher {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl PublisherPort for RecordingPublisher {
    async fn publish(
        &self,
        objective: &Objective,
        candidate: &Candidate,
        _verification: &Verification,
    ) -> Result<Publication, PublisherError> {
        let publication = Publication {
            revision: candidate.revision.as_str().to_string(),
            // The rehearsal closes nothing.
            closed: false,
            detail: format!(
                "the rehearsal recorded the publication of {}; nothing was closed and no ref moved",
                objective.work_ref
            ),
        };
        match self.publications.lock() {
            Ok(mut recorded) => recorded.push(publication.clone()),
            Err(poisoned) => poisoned.into_inner().push(publication.clone()),
        }
        Ok(publication)
    }
}

/// The gates of a gated rehearsal.
///
/// They exist so the states are OBSERVABLE instead of raced against: the
/// test opens the confirmation when it wants the run to start, and the
/// work when it wants the objective to end.
pub struct Gates {
    accept: Arc<Notify>,
    finish: Arc<Notify>,
}

impl Gates {
    /// Let the runtime confirm the objective it was given.
    pub fn accept(&self) {
        self.accept.notify_one();
    }

    /// Let the objective's work happen.
    pub fn finish(&self) {
        self.finish.notify_one();
    }
}

/// The holder an already-ran item is claimed by in the rehearsal's own
/// selection: the rehearsal closes nothing, so the tracker would offer
/// the same work again; the runtime remembers what it ran and skips it.
const ALREADY_RAN_HOLDER: &str = "trial-already-ran";

/// The rehearsal runtime: it names itself a rehearsal, it selects work
/// from a fixed scenario and it publishes nothing.
pub struct TrialRuntime {
    identity: RuntimeIdentity,
    accept: Option<Arc<Notify>>,
    finish: Option<Arc<Notify>>,
    publisher: Arc<RecordingPublisher>,
    /// Whether the scripted verifier holds the objective. The rehearsal
    /// that does not hold is the one that publishes nothing.
    holds: bool,
    /// The work this runtime already ran. The rehearsal closes nothing, so
    /// without this memory every selection would pick the same item again;
    /// with it, the run ends idle when no NEW work is left.
    ran: Mutex<Vec<String>>,
}

impl TrialRuntime {
    /// The rehearsal as the product runs it: it confirms at once.
    pub fn new() -> Self {
        Self::with_verifier(true)
    }

    /// The rehearsal whose verification does not hold: the objective is
    /// rejected and nothing is published.
    pub fn rejecting() -> Self {
        Self::with_verifier(false)
    }

    fn with_verifier(holds: bool) -> Self {
        let publisher = Arc::new(RecordingPublisher::new());
        Self {
            identity: RuntimeIdentity::rehearsal("trial-default"),
            accept: None,
            finish: None,
            publisher,
            holds,
            ran: Mutex::new(Vec::new()),
        }
    }

    /// The already-ran work as the selection sees it: held by the trial
    /// itself, so no later selection repeats it.
    fn ran_held(&self) -> BTreeMap<String, String> {
        let ran = self
            .ran
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        ran.iter()
            .map(|work_ref| (work_ref.clone(), ALREADY_RAN_HOLDER.to_string()))
            .collect()
    }

    /// Remember the work an attempt executed, so no later selection repeats it.
    fn record_ran(&self, work_ref: &str) {
        self.ran
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(work_ref.to_string());
    }

    /// The runner of ONE objective: the scenario plus the work the
    /// caller named. The publisher is shared across objectives, so the
    /// rehearsal records them all.
    fn runner_for(&self, work_ref: Option<&str>) -> Arc<GoalRunner> {
        Arc::new(GoalRunner::new(
            Arc::new(tracker::fake::FakeTracker::with_items(scenario_with(
                work_ref,
            ))),
            Arc::new(ScriptedRequirements),
            Arc::new(ScriptedAgent),
            Arc::new(ScriptedVerifier { holds: self.holds }),
            self.publisher.clone(),
        ))
    }

    /// The rehearsal with gates: the runtime waits for the confirmation
    /// before it confirms, and the objective waits for the work to be
    /// released.
    pub fn gated() -> (Self, Gates) {
        let mut runtime = Self::new();
        let accept = Arc::new(Notify::new());
        let finish = Arc::new(Notify::new());
        runtime.accept = Some(accept.clone());
        runtime.finish = Some(finish.clone());
        (runtime, Gates { accept, finish })
    }

    /// What the rehearsal recorded. Nothing else happened.
    pub fn publications(&self) -> Vec<Publication> {
        self.publisher.publications()
    }

    /// The recorder of the rehearsal, for whoever holds the runtime.
    pub fn recorder(&self) -> Arc<RecordingPublisher> {
        self.publisher.clone()
    }
}

impl Default for TrialRuntime {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ObjectiveRuntime for TrialRuntime {
    fn identity(&self) -> RuntimeIdentity {
        self.identity.clone()
    }

    async fn start(
        &self,
        _run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError> {
        // The gate: the runtime does not confirm until it is opened.
        if let Some(accept) = &self.accept {
            accept.notified().await;
        }
        let runner = self.runner_for(request.work_ref.as_deref());
        // The work this rehearsal already ran is held by the trial itself:
        // a chained selection moves to the NEXT work and the run ends
        // idle when nothing new is left.
        let selected = runner
            .select_in(request.work_ref.as_deref(), &self.ran_held())
            .await
            .map_err(|error| RuntimeError(error.to_string()))?;
        let Some(item) = selected else {
            return Ok(ObjectiveHandle::finished(ObjectiveOutcome::Idle {
                reason: "no eligible work".into(),
            }));
        };
        self.record_ran(&item.id);
        let objective = runner
            .objective_of(&item)
            .map_err(|error| RuntimeError(error.to_string()))?;
        let finish = self.finish.clone();
        Ok(ObjectiveHandle::new(async move {
            // The work gate: the objective happens when the test says so.
            if let Some(finish) = finish {
                finish.notified().await;
            }
            runner.execute(&objective).await
        }))
    }

    /// The rehearsal winds down when the stop reaches it: the gates it
    /// waits on are released, so a stopped chain ends by its own verdict
    /// instead of waiting out the cancel grace and being aborted.
    fn cancel(&self, _run: RunId) {
        if let Some(accept) = &self.accept {
            accept.notify_one();
        }
        if let Some(finish) = &self.finish {
            finish.notify_one();
        }
    }

    /// The rehearsal simulates the renewal it was asked for: it has no
    /// environment whose ttl could be extended, but it also has nothing
    /// to refuse — the simulated ttl is the addition itself.
    async fn renew(&self, added_seconds: u64) -> RenewOutcome {
        RenewOutcome::Confirmed {
            ttl_seconds: added_seconds,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn the_rehearsal_selects_the_first_eligible_work_and_publishes_nothing() {
        let runtime = TrialRuntime::new();
        let handle = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: None,
                },
            )
            .await
            .expect("the rehearsal accepts the objective");
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "{outcome:?}"
        );
        let recorded = runtime.publications();
        assert_eq!(recorded.len(), 1);
        // The rehearsal closed nothing and moved no ref.
        assert!(!recorded[0].closed);
        assert!(recorded[0].detail.contains("nothing was closed"));
    }

    #[tokio::test]
    async fn the_rehearsal_rejects_an_objective_whose_evidence_does_not_hold() {
        let runtime = TrialRuntime::rejecting();
        let handle = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: None,
                },
            )
            .await
            .expect("the rehearsal accepts the objective");
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        // Nothing was published: the gate holds.
        assert!(runtime.publications().is_empty());
    }

    #[tokio::test]
    async fn the_rehearsal_rehearses_the_work_the_client_names_and_never_hangs() {
        let runtime = TrialRuntime::new();
        // The runtime must ANSWER: a runtime that hangs leaves the run
        // in `Starting` forever, and a silent hang is the worst kind of
        // failure. The work the client names is rehearsed even when the
        // fixed scenario does not know it: that is the work it was asked
        // for, and the rehearsal is where it is rehearsed.
        let answered = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            runtime.start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: Some("TASK-9".into()),
                },
            ),
        )
        .await;
        let handle = answered
            .expect("the runtime answers instead of hanging")
            .expect("the named work is rehearsed, not refused");
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "TASK-9"),
            "{outcome:?}"
        );
        // The rehearsal still closed nothing: the recording says so.
        let recorded = runtime.publications();
        assert_eq!(recorded.len(), 1);
        assert!(!recorded[0].closed);
    }

    #[tokio::test]
    async fn the_rehearsal_chain_selects_new_work_and_never_repeats_what_it_ran() {
        // The chained selection of a live run: after the objective the
        // client named, every unnamed selection asks for the NEXT work
        // of the same run. The rehearsal closes nothing, so the only
        // thing that moves the chain forward is what this runtime
        // already ran; with no NEW work left the run ends idle.
        let runtime = TrialRuntime::new();
        let named = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: Some("TASK-9".into()),
                },
            )
            .await
            .expect("the named work is rehearsed")
            .completion()
            .await;
        assert!(
            matches!(&named, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "TASK-9"),
            "{named:?}"
        );

        // The chained attempt names nothing: the runtime selects, and it
        // must not repeat the work it already ran.
        let next = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: None,
                },
            )
            .await
            .expect("the chain selects again")
            .completion()
            .await;
        assert!(
            matches!(&next, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "T-1"),
            "the chain moves to the next new work: {next:?}"
        );

        // T-1 is already ran and T-2 stays blocked by a rehearsal that
        // closes nothing: there is no new work, and the chain ends idle.
        let idle = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "trial".into(),
                    work_ref: None,
                },
            )
            .await
            .expect("the runtime answers")
            .completion()
            .await;
        assert!(
            matches!(idle, ObjectiveOutcome::Idle { .. }),
            "no new work left is idle, never a repeat: {idle:?}"
        );
    }

    #[tokio::test]
    async fn the_rehearsal_names_itself_a_rehearsal() {
        let runtime = TrialRuntime::new();
        assert_eq!(
            runtime.identity().kind,
            run::runtime::RuntimeKind::Rehearsal
        );
        assert!(runtime.identity().label().starts_with("rehearsal"));
    }

    #[tokio::test]
    async fn the_rehearsal_confirms_its_simulated_ttl() {
        let runtime = TrialRuntime::new();
        assert_eq!(
            runtime.renew(300).await,
            RenewOutcome::Confirmed { ttl_seconds: 300 }
        );
    }
}
