//! The runtime that executes an objective, and the outcome it reports.
//!
//! A run is `Starting` until a runtime CONFIRMS it: that confirmation is
//! the only thing that moves the run to `Running`. A rehearsal runtime and
//! a real one answer the SAME contract and both name themselves, so a
//! client can always tell which one produced a state — a rehearsal is
//! never presented as a live execution.

use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};

/// Which kind of runtime executes the objective.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    /// A rehearsal: deterministic, without external effects.
    Rehearsal,
    /// A real runtime: sandboxes, ACP and the real tracker.
    Live,
}

/// The identity of the runtime that executes (or confirmed) an objective.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeIdentity {
    pub kind: RuntimeKind,
    pub id: String,
}

impl RuntimeIdentity {
    pub fn rehearsal(id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeKind::Rehearsal,
            id: id.into(),
        }
    }

    pub fn live(id: impl Into<String>) -> Self {
        Self {
            kind: RuntimeKind::Live,
            id: id.into(),
        }
    }

    /// A label that never hides the kind: a rehearsal says so.
    pub fn label(&self) -> String {
        match self.kind {
            RuntimeKind::Rehearsal => format!("rehearsal ({})", self.id),
            RuntimeKind::Live => format!("live ({})", self.id),
        }
    }
}

/// The identity of one run: a number the controller assigns when the run
/// starts and carries to the runtime.
///
/// The controller numbers its runs monotonically, and a stop NAMES the run
/// it was raised for. A stop that belongs to an older run therefore can
/// never reach the attempt of a newer one, and a stop that arrives before
/// the runtime installed the attempt is still delivered to that attempt.
/// The engine tags the events of the run with the same number (its epoch).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RunId(pub u64);

impl RunId {
    /// The number the run is known by.
    pub fn number(self) -> u64 {
        self.0
    }
}

/// What the controller asks the runtime to do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectiveRequest {
    /// The project the objective belongs to.
    pub project: String,
    /// The work item to execute, when the caller named one.
    pub work_ref: Option<String>,
    /// A custom objective, when the caller asked for work that owns no
    /// ticket. It never selects, claims or closes tracker work.
    #[serde(default)]
    pub objective: Option<String>,
}

/// What the objective ended up being.
///
/// ONE vocabulary for the result of an objective: the engine maps every
/// case to a state, so no outcome is left without a transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum ObjectiveOutcome {
    /// The objective completed and its effects were published.
    Confirmed { work_ref: String, detail: String },
    /// The objective ran but did not hold: nothing was published.
    Rejected { work_ref: String, reason: String },
    /// The execution failed.
    Failed { reason: String },
    /// There was no eligible work: nothing was consumed.
    Idle { reason: String },
}

impl ObjectiveOutcome {
    /// How the outcome reads in a record and in a snapshot. One place:
    /// the event log and the clients say the same thing.
    pub fn describe(&self) -> String {
        match self {
            ObjectiveOutcome::Confirmed { work_ref, detail } => {
                format!("confirmed {work_ref}: {detail}")
            }
            ObjectiveOutcome::Rejected { work_ref, reason } => {
                format!("rejected {work_ref}: {reason}")
            }
            ObjectiveOutcome::Failed { reason } => format!("failed: {reason}"),
            ObjectiveOutcome::Idle { reason } => format!("idle: {reason}"),
        }
    }
}

/// What the runtime answered when asked to extend a managed ttl.
///
/// Only a runtime can confirm a renewal: the engine records what it
/// answered and never invents the new value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "renewal", rename_all = "snake_case")]
pub enum RenewOutcome {
    /// The runtime extended the environment and states the ttl it now has.
    Confirmed { ttl_seconds: u64 },
    /// The runtime has no renewable ttl, with the reason it does not.
    Unsupported { reason: String },
}

/// A runtime that could not take the objective.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the runtime refused the objective: {0}")]
pub struct RuntimeError(pub String);

/// What the core records about the objective it last ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ObjectiveReport {
    pub work_ref: Option<String>,
    pub outcome: ObjectiveOutcome,
}

/// The handle of an objective the runtime already accepted.
pub struct ObjectiveHandle {
    completion: Pin<Box<dyn Future<Output = ObjectiveOutcome> + Send + 'static>>,
}

/// The handle prints its own name and nothing else: the future it holds
/// has no observable state worth printing, and pretending otherwise
/// would leak the internals of an execution into a log line.
impl std::fmt::Debug for ObjectiveHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ObjectiveHandle")
    }
}

impl ObjectiveHandle {
    /// A handle whose objective ends when `completion` resolves.
    pub fn new<F>(completion: F) -> Self
    where
        F: Future<Output = ObjectiveOutcome> + Send + 'static,
    {
        Self {
            completion: Box::pin(completion),
        }
    }

    /// A handle that is already finished.
    pub fn finished(outcome: ObjectiveOutcome) -> Self {
        Self::new(async move { outcome })
    }

    /// Wait for the objective to end.
    pub async fn completion(self) -> ObjectiveOutcome {
        self.completion.await
    }
}

/// The runtime that executes objectives.
///
/// `start` returns when the runtime has CONFIRMED the objective: that
/// confirmation is what moves the run from `Starting` to `Running`. The
/// objective continues in the background and its outcome is reported by
/// the returned handle.
#[async_trait::async_trait]
pub trait ObjectiveRuntime: Send + Sync {
    /// Which runtime this is. The identity is never hidden.
    fn identity(&self) -> RuntimeIdentity;

    /// Take the objective for `run` and confirm it.
    ///
    /// `run` is the identity the controller assigned: every stop names it,
    /// so the runtime can tell which attempt the stop belongs to.
    async fn start(
        &self,
        run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError>;

    /// Ask the attempt of `run` to stop.
    ///
    /// This is a SIGNAL, not a drop: the attempt is expected to wind down
    /// on its own and return its outcome, so it can release what it owns
    /// (a sandbox, a session, a claim). The controller keeps a bounded
    /// grace and aborts the attempt only when the signal is ignored.
    ///
    /// A stop for a run that is no longer the current one is not delivered
    /// to the attempt in flight: one run's stop never cancels another.
    ///
    /// The default does nothing: a runtime whose attempts cannot be
    /// interrupted cooperatively has no signal to offer, and the
    /// controller's grace is what ends them.
    fn cancel(&self, _run: RunId) {}

    /// Extend the ttl of the managed environment by `added_seconds`.
    ///
    /// The default is honest about what most runtimes can do: without a
    /// renewable environment the answer is `Unsupported` with its reason,
    /// never a simulated confirmation.
    async fn renew(&self, _added_seconds: u64) -> RenewOutcome {
        RenewOutcome::Unsupported {
            reason: format!("{} does not support a ttl renewal", self.identity().label()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_identity_orders_and_survives_the_wire() {
        // Run identities are ordered: a stop can be compared with the run
        // in flight to know whether it belongs to it.
        assert!(RunId(1) < RunId(2));
        assert_eq!(RunId(7).number(), 7);
        let encoded = serde_json::to_string(&RunId(7)).expect("encode");
        assert_eq!(encoded, "7");
        let back: RunId = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(back, RunId(7));
    }

    #[test]
    fn a_rehearsal_never_looks_like_a_live_runtime() {
        let rehearsal = RuntimeIdentity::rehearsal("trial-default");
        let live = RuntimeIdentity::live("execd");
        assert!(rehearsal.label().starts_with("rehearsal"));
        assert!(live.label().starts_with("live"));
        assert_ne!(rehearsal.kind, live.kind);
        assert!(rehearsal.label().contains("trial-default"));
    }

    #[tokio::test]
    async fn a_handle_reports_the_outcome_of_its_objective() {
        let handle = ObjectiveHandle::finished(ObjectiveOutcome::Confirmed {
            work_ref: "TASK-1".into(),
            detail: "published".into(),
        });
        let outcome = handle.completion().await;
        assert!(matches!(
            outcome,
            ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "TASK-1"
        ));
    }

    #[test]
    fn the_outcome_survives_the_wire_with_its_kind() {
        let outcome = ObjectiveOutcome::Rejected {
            work_ref: "TASK-2".into(),
            reason: "the evidence did not hold".into(),
        };
        let encoded = serde_json::to_string(&outcome).expect("encode");
        assert!(encoded.contains("\"outcome\":\"rejected\""), "{encoded}");
        let back: ObjectiveOutcome = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(back, outcome);
    }

    struct NonRenewableRuntime;

    #[async_trait::async_trait]
    impl ObjectiveRuntime for NonRenewableRuntime {
        fn identity(&self) -> RuntimeIdentity {
            RuntimeIdentity::live("non-renewable")
        }

        async fn start(
            &self,
            _run: RunId,
            _request: ObjectiveRequest,
        ) -> Result<ObjectiveHandle, RuntimeError> {
            Ok(ObjectiveHandle::finished(ObjectiveOutcome::Idle {
                reason: "nothing to do".into(),
            }))
        }
    }

    #[tokio::test]
    async fn a_runtime_without_a_renewable_ttl_says_so() {
        let renewal = NonRenewableRuntime.renew(300).await;
        assert!(
            matches!(renewal, RenewOutcome::Unsupported { ref reason } if reason.contains("ttl renewal")),
            "{renewal:?}"
        );
    }

    #[test]
    fn a_renewal_survives_the_wire_with_its_kind() {
        let confirmed = RenewOutcome::Confirmed { ttl_seconds: 420 };
        let encoded = serde_json::to_string(&confirmed).expect("encode");
        assert!(encoded.contains("\"renewal\":\"confirmed\""), "{encoded}");
        let back: RenewOutcome = serde_json::from_str(&encoded).expect("decode");
        assert_eq!(back, confirmed);
    }
}
