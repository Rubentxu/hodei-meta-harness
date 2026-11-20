//! The stop race, COMPOSED: the real engine and the real live runtime,
//! wired in the order the supervisor server drives them.
//!
//! The tests use no socket and no clock: the gates are explicit, so the
//! stop lands exactly where the test says it lands.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use run::run::Phase;
use run::runtime::{ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RunId};
use runner::live::{ClaimError, ClaimPort, Claimed, LiveRuntime};
use runner::ports::{
    AgentError, AgentPort, Objective, ObjectiveCheck, Publication, PublisherError, PublisherPort,
    RequirementsError, RequirementsPort, Verification, VerifierError, VerifierPort,
};
use supervisor::engine::{self, Transition};
use supervisor::protocol::SupervisorAction;
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
            checks: vec![ObjectiveCheck::free(Check {
                id: "AC-1".into(),
                command: "true".into(),
            })],
        })
    }
}

/// An agent that counts how many sessions were created for it.
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

/// A publisher whose effect COMPLETES: the stop lands while it is running.
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

struct GrantingClaims {
    grant: Claimed,
    releases: Mutex<Vec<(String, u64)>>,
}

impl GrantingClaims {
    fn granting(epoch: u64) -> Self {
        Self {
            grant: Claimed { epoch },
            releases: Mutex::new(Vec::new()),
        }
    }

    fn releases(&self) -> Vec<(String, u64)> {
        self.releases
            .lock()
            .map(|releases| releases.clone())
            .unwrap_or_default()
    }
}

impl ClaimPort for GrantingClaims {
    fn claim(&self, _work_ref: &str) -> Result<Claimed, ClaimError> {
        Ok(self.grant)
    }

    fn release(&self, work_ref: &str, epoch: u64) {
        if let Ok(mut releases) = self.releases.lock() {
            releases.push((work_ref.to_string(), epoch));
        }
    }

    fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
        Ok(BTreeMap::new())
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

fn work() -> WorkItem {
    WorkItem {
        id: "TASK-9".into(),
        title: "work TASK-9".into(),
        status: WorkStatus::Open,
        provider_ready: Some(true),
        dependencies: Vec::new(),
    }
}

fn runtime_with(
    agent: Arc<dyn AgentPort>,
    publisher: Arc<dyn PublisherPort>,
    claims: Arc<GrantingClaims>,
) -> LiveRuntime {
    LiveRuntime::new(
        "project",
        Arc::new(FakeTracker::with_items(vec![work()])),
        Arc::new(FixedRequirements),
        agent,
        Arc::new(HoldingVerifier),
        publisher,
        claims,
    )
}

fn request() -> ObjectiveRequest {
    ObjectiveRequest {
        project: "project".into(),
        work_ref: None,
    }
}

#[tokio::test]
async fn a_stop_that_lands_between_start_and_the_attempt_is_never_lost() {
    // The order the server drives: the engine applies the Start, another
    // connection applies the Stop before the execution was spawned, the
    // server cancels the run with no handle to abort, and only then the
    // delayed Start reaches the runtime.
    let claims = Arc::new(GrantingClaims::granting(7));
    let agent = Arc::new(CountingAgent {
        calls: AtomicUsize::new(0),
    });
    let runtime = runtime_with(
        agent.clone(),
        Arc::new(GatedPublisher {
            entered: Arc::new(tokio::sync::Notify::new()),
            gate: Arc::new(tokio::sync::Notify::new()),
        }),
        claims.clone(),
    );
    let core = Mutex::new(engine::core_with_runtime("project", runtime.identity()));
    let run = {
        let mut guarded = core.lock().expect("lock");
        let start = engine::apply(
            &mut guarded,
            SupervisorAction::Start {
                project: "project".into(),
                ticket: None,
                revision: None,
            },
        );
        assert!(start.applied, "{}", start.detail);
        let run = RunId(guarded.epoch);
        let stop = engine::apply(&mut guarded, SupervisorAction::Stop);
        assert!(stop.applied, "{}", stop.detail);
        run
    };

    runtime.cancel(run);
    let outcome = runtime
        .start(run, request())
        .await
        .expect("the runtime accepts the objective")
        .completion()
        .await;

    assert!(
        matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
        "{outcome:?}"
    );
    assert_eq!(agent.calls(), 0, "a stopped attempt creates no agent");
    assert_eq!(
        claims.releases(),
        vec![("TASK-9".to_string(), 7)],
        "the claim goes back, it is not left held"
    );
    // The engine refuses to record the stopped attempt's verdict and does
    // not resurrect the run.
    let mut guarded = core.lock().expect("lock");
    assert!(matches!(
        engine::record_outcome(&mut guarded, run.number(), outcome),
        Transition::Refused { .. }
    ));
    let snapshot = engine::snapshot(&guarded);
    assert_eq!(snapshot.phase, Phase::Stopped);
    assert!(snapshot.objective.is_none());
}

#[tokio::test]
async fn a_confirmed_effect_that_completes_after_a_stop_is_recorded_as_a_fact() {
    let claims = Arc::new(GrantingClaims::granting(11));
    let entered = Arc::new(tokio::sync::Notify::new());
    let gate = Arc::new(tokio::sync::Notify::new());
    let runtime = runtime_with(
        Arc::new(CountingAgent {
            calls: AtomicUsize::new(0),
        }),
        Arc::new(GatedPublisher {
            entered: entered.clone(),
            gate: gate.clone(),
        }),
        claims.clone(),
    );
    let core = Mutex::new(engine::core_with_runtime("project", runtime.identity()));
    {
        let mut guarded = core.lock().expect("lock");
        let start = engine::apply(
            &mut guarded,
            SupervisorAction::Start {
                project: "project".into(),
                ticket: None,
                revision: None,
            },
        );
        assert!(start.applied, "{}", start.detail);
    }
    let run = RunId(1);

    let handle = runtime
        .start(run, request())
        .await
        .expect("the runtime accepts the objective");
    let completion = tokio::spawn(async move { handle.completion().await });
    // The effect is being moved when the stop lands: it completes, so it
    // is a fact of the past, not an uncertainty.
    entered.notified().await;
    {
        let mut guarded = core.lock().expect("lock");
        assert!(engine::apply(&mut guarded, SupervisorAction::Stop).applied);
    }
    gate.notify_one();
    let outcome = completion.await.expect("the attempt ends");
    assert!(
        matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "TASK-9"),
        "{outcome:?}"
    );

    let mut guarded = core.lock().expect("lock");
    assert_eq!(
        engine::record_outcome(&mut guarded, run.number(), outcome),
        Transition::Applied
    );
    let snapshot = engine::snapshot(&guarded);
    assert_eq!(snapshot.phase, Phase::Stopped, "the run is not resurrected");
    let objective = snapshot.objective.expect("the verdict is recorded");
    assert!(matches!(
        objective.outcome,
        ObjectiveOutcome::Confirmed { .. }
    ));
    let reason = guarded.stop_reason.as_deref().expect("the stop reason");
    assert!(reason.contains("verdict"), "{reason}");
    assert!(!reason.contains("uncertain"), "{reason}");
    assert_eq!(claims.releases(), vec![("TASK-9".to_string(), 11)]);
}
