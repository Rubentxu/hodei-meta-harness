//! The GoalRunner: select the work, then execute it through the ports.
//!
//! The runner is the application-level control. It owns no phase of its
//! own — the state machine lives in the supervisor's engine, and the
//! runner only reports what it did. Every decision it makes is a pure
//! one: the work is selected from an observed snapshot, and an objective
//! is published only when the verification covers every required check.

use std::collections::BTreeMap;
use std::sync::Arc;

use goal::selection::{SelectionReason, SupervisorDecision, explain_selection, next_decision};
use run::runtime::ObjectiveOutcome;
use tracker::core::{TrackerError, TrackerRead, WorkItem};

use crate::ports::{AgentPort, Objective, PublisherPort, RequirementsPort, VerifierPort};

/// Why the runner could not do its work.
#[derive(Debug, thiserror::Error)]
pub enum RunnerError {
    #[error("the tracker could not be read: {0}")]
    Tracker(#[from] TrackerError),
    #[error("the work item could not be prepared: {0}")]
    Requirements(String),
}

/// The application-level control.
pub struct GoalRunner {
    tracker: Arc<dyn TrackerRead>,
    requirements: Arc<dyn RequirementsPort>,
    agent: Arc<dyn AgentPort>,
    verifier: Arc<dyn VerifierPort>,
    publisher: Arc<dyn PublisherPort>,
}

impl GoalRunner {
    pub fn new(
        tracker: Arc<dyn TrackerRead>,
        requirements: Arc<dyn RequirementsPort>,
        agent: Arc<dyn AgentPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<dyn PublisherPort>,
    ) -> Self {
        Self {
            tracker,
            requirements,
            agent,
            verifier,
            publisher,
        }
    }

    /// The work to do: the one that was named, or the next eligible in
    /// the reproducible chain order. `None` means there is nothing to do.
    ///
    /// The selection is a pure decision over an observed snapshot: the
    /// same snapshot always yields the same choice.
    pub async fn select(&self, work_ref: Option<&str>) -> Result<Option<WorkItem>, RunnerError> {
        self.select_in(work_ref, &BTreeMap::new()).await
    }

    /// The work to do, given the claims other agents already hold.
    ///
    /// A named work that is claimed is not eligible, and the chain skips
    /// the claimed work: the caller does not receive work somebody else
    /// owns.
    pub async fn select_in(
        &self,
        work_ref: Option<&str>,
        claims: &BTreeMap<String, String>,
    ) -> Result<Option<WorkItem>, RunnerError> {
        let items = self.tracker.snapshot().await?;
        let explained = explain_selection(&items, claims);
        if let Some(work_ref) = work_ref {
            let Some(entry) = explained.iter().find(|entry| entry.item.id == work_ref) else {
                return Err(RunnerError::Requirements(format!(
                    "{work_ref} is not in the tracker snapshot"
                )));
            };
            if entry.reason != SelectionReason::Eligible {
                return Err(RunnerError::Requirements(format!(
                    "{work_ref} is not eligible: {:?}",
                    entry.reason
                )));
            }
            return Ok(Some(entry.item.clone()));
        }
        match next_decision(&explained, claims) {
            SupervisorDecision::Work { ticket_id } => Ok(explained
                .iter()
                .find(|entry| entry.item.id == ticket_id)
                .map(|entry| entry.item.clone())),
            SupervisorDecision::Idle => Ok(None),
        }
    }

    /// The objective of a work item: what the agent is told to do and
    /// what decides whether it held.
    pub fn objective_of(&self, item: &WorkItem) -> Result<Objective, RunnerError> {
        self.requirements
            .requirements(item)
            .map_err(|error| RunnerError::Requirements(error.to_string()))
    }

    /// Execute one objective: the agent works, the verifier judges, and
    /// only a verified candidate is published.
    ///
    /// The gate is here, not in the ports: an objective whose verification
    /// does not cover EVERY required check is NOT published, whatever the
    /// publisher would have done with it.
    ///
    /// The gate is about the CHECKS, not about the evidence set: the
    /// rehearsal's verifier states a verdict without measuring anything —
    /// it has no evidence to show, and it says so — but the verdict is
    /// still a verdict. The evidence-bound gate, where a check counts
    /// only when valid and passing evidence exists for it, belongs to the
    /// live verifier of MH-A06, which really observes the work.
    pub async fn execute(&self, objective: &Objective) -> ObjectiveOutcome {
        let candidate = match self.agent.execute(objective).await {
            Ok(candidate) => candidate,
            Err(error) => {
                return ObjectiveOutcome::Failed {
                    reason: error.to_string(),
                };
            }
        };
        let verification = match self.verifier.verify(objective, &candidate).await {
            Ok(verification) => verification,
            Err(error) => {
                return ObjectiveOutcome::Failed {
                    reason: error.to_string(),
                };
            }
        };
        let required = objective.required();
        if !verification.covers(&required) {
            // A check that did not hold: the objective is rejected and
            // the publisher is never called.
            return ObjectiveOutcome::Rejected {
                work_ref: objective.work_ref.clone(),
                reason: format!(
                    "the evidence does not satisfy {}",
                    verification.missing(&required).join(", ")
                ),
            };
        }
        match self
            .publisher
            .publish(objective, &candidate, &verification)
            .await
        {
            Ok(publication) => ObjectiveOutcome::Confirmed {
                work_ref: objective.work_ref.clone(),
                detail: publication.detail,
            },
            Err(error) => ObjectiveOutcome::Failed {
                reason: error.to_string(),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{AgentError, PublisherError, Verification, VerifierError};
    use std::sync::Mutex;
    use tracker::core::WorkStatus;
    use verify::git::{GitRevision, GitTree};
    use verify::runner::Candidate;
    use verify::transport::TransportDigest;

    fn candidate() -> Candidate {
        Candidate {
            revision: GitRevision::parse("1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5")
                .expect("a revision"),
            tree: GitTree::parse("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b").expect("a tree"),
            transport_digest: TransportDigest::of(b"the bundle"),
            bundle: b"the bundle".to_vec(),
        }
    }

    fn item(id: &str) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("work {id}"),
            status: WorkStatus::Open,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        }
    }

    struct FixedRequirements;

    impl RequirementsPort for FixedRequirements {
        fn requirements(
            &self,
            item: &WorkItem,
        ) -> Result<Objective, crate::ports::RequirementsError> {
            Ok(Objective {
                work_ref: item.id.clone(),
                instruction: format!("do {}", item.id),
                checks: vec![verify::runner::Check {
                    id: "AC-1".into(),
                    command: "true".into(),
                }],
            })
        }
    }

    struct FixedAgent;

    #[async_trait::async_trait]
    impl AgentPort for FixedAgent {
        async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
            Ok(candidate())
        }
    }

    struct HoldingVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for HoldingVerifier {
        async fn verify(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            // The evidence does not cover the required check.
            Ok(Verification {
                satisfied: Vec::new(),
                evidence: None,
            })
        }
    }

    struct RecordingPublisher {
        published: Mutex<Vec<String>>,
    }

    #[async_trait::async_trait]
    impl PublisherPort for RecordingPublisher {
        async fn publish(
            &self,
            objective: &Objective,
            _candidate: &Candidate,
            _verification: &Verification,
        ) -> Result<crate::ports::Publication, PublisherError> {
            self.published
                .lock()
                .expect("lock")
                .push(objective.work_ref.clone());
            Ok(crate::ports::Publication {
                revision: "1f0c3a5b".into(),
                closed: true,
                detail: "published".into(),
            })
        }
    }

    fn runner(
        agent: Arc<dyn AgentPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<dyn PublisherPort>,
    ) -> GoalRunner {
        let tracker = tracker::fake::FakeTracker::with_items(vec![item("T-1"), item("T-2")]);
        GoalRunner::new(
            Arc::new(tracker),
            Arc::new(FixedRequirements),
            agent,
            verifier,
            publisher,
        )
    }

    #[tokio::test]
    async fn the_selection_is_reproducible_and_named_work_must_be_eligible() {
        let runner = runner(
            Arc::new(FixedAgent),
            Arc::new(HoldingVerifier),
            Arc::new(RecordingPublisher {
                published: Mutex::new(Vec::new()),
            }),
        );
        let first = runner.select(None).await.expect("select");
        let second = runner.select(None).await.expect("select");
        assert_eq!(
            first.as_ref().map(|item| item.id.clone()),
            Some("T-1".to_string())
        );
        assert_eq!(first.map(|item| item.id), second.map(|item| item.id));
        let named = runner.select(Some("T-2")).await.expect("select named");
        assert_eq!(named.map(|item| item.id), Some("T-2".to_string()));
        assert!(runner.select(Some("T-404")).await.is_err());
    }

    #[tokio::test]
    async fn a_claimed_work_is_neither_selected_nor_accepted_by_name() {
        let runner = runner(
            Arc::new(FixedAgent),
            Arc::new(HoldingVerifier),
            Arc::new(RecordingPublisher {
                published: Mutex::new(Vec::new()),
            }),
        );
        let claims = BTreeMap::from([("T-1".to_string(), "another-agent".to_string())]);
        let next = runner.select_in(None, &claims).await.expect("select");
        assert_eq!(next.map(|item| item.id), Some("T-2".to_string()));
        let named = runner.select_in(Some("T-1"), &claims).await;
        assert!(named.is_err(), "{named:?}");
    }

    #[tokio::test]
    async fn an_objective_without_evidence_is_not_published() {
        let publisher = Arc::new(RecordingPublisher {
            published: Mutex::new(Vec::new()),
        });
        let runner = runner(
            Arc::new(FixedAgent),
            Arc::new(HoldingVerifier),
            publisher.clone(),
        );
        let objective = runner.objective_of(&item("T-1")).expect("objective");
        let outcome = runner.execute(&objective).await;
        // The evidence did not hold: the objective is rejected, and the
        // publisher was NEVER called.
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert!(publisher.published.lock().expect("lock").is_empty());
    }

    #[tokio::test]
    async fn a_verified_objective_is_published() {
        struct PassingVerifier;

        #[async_trait::async_trait]
        impl VerifierPort for PassingVerifier {
            async fn verify(
                &self,
                objective: &Objective,
                _candidate: &Candidate,
            ) -> Result<Verification, VerifierError> {
                Ok(Verification::satisfying(&objective.required()))
            }
        }

        let publisher = Arc::new(RecordingPublisher {
            published: Mutex::new(Vec::new()),
        });
        let runner = runner(
            Arc::new(FixedAgent),
            Arc::new(PassingVerifier),
            publisher.clone(),
        );
        let objective = runner.objective_of(&item("T-1")).expect("objective");
        let outcome = runner.execute(&objective).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { .. }),
            "{outcome:?}"
        );
        assert_eq!(
            publisher.published.lock().expect("lock").as_slice(),
            ["T-1"]
        );
    }
}
