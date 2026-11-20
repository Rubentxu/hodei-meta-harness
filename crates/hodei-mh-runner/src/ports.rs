//! The ports the runner talks to.
//!
//! Every port is an EFFECT that lives outside the runner: the runner
//! decides, the port does. The types the ports exchange are the ones the
//! product already owns — the tracker's work items, the verifier's
//! checks, candidate and evidence — so no contract is written twice.

use tracker::core::WorkItem;
use verify::evidence::EvidenceSet;
use verify::runner::{Candidate, Check};

/// A check the objective declares, with the acceptance criterion that
/// declares it.
///
/// The index is the one the tracker states for the criterion, never a
/// position in a list: a check declared in the description, or under a
/// criterion the tracker does not index, carries no index instead of a
/// guessed one.
#[derive(Debug, Clone, PartialEq)]
pub struct ObjectiveCheck {
    /// The executable check.
    pub check: Check,
    /// The acceptance criterion that declares it, when one does.
    pub ac: Option<usize>,
}

impl ObjectiveCheck {
    /// A check declared outside any acceptance criterion.
    pub fn free(check: Check) -> Self {
        Self { check, ac: None }
    }

    /// A check declared under an acceptance criterion.
    pub fn in_ac(check: Check, ac: usize) -> Self {
        Self {
            check,
            ac: Some(ac),
        }
    }
}

/// What the objective's publication closes.
///
/// A custom objective owns no ticket: it is promoted like any candidate,
/// but there is nothing to close, and the tracker is never written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closes {
    Ticket,
    Nothing,
}

/// The work to execute and the requirements that decide whether it held.
#[derive(Debug, Clone, PartialEq)]
pub struct Objective {
    /// The work item this objective belongs to.
    pub work_ref: String,
    /// What the agent is asked to do.
    pub instruction: String,
    /// The checks that decide whether the objective held, each with the
    /// criterion that declares it.
    pub checks: Vec<ObjectiveCheck>,
    /// The controller's run that carries this objective, when one named
    /// it. It is the identity the durable checkpoint records.
    pub run: Option<u64>,
    /// What the objective closes when it is published.
    pub closes: Closes,
}

impl Objective {
    /// The ids of the checks that must hold for the objective to count.
    pub fn required(&self) -> Vec<String> {
        self.checks
            .iter()
            .map(|declared| declared.check.id.clone())
            .collect()
    }

    /// The acceptance criteria the checks are declared under, in the
    /// order they are declared and without repeats.
    ///
    /// These are the criteria a closure checks: a check the description
    /// declares maps to no criterion and is not guessed into one.
    pub fn check_ac(&self) -> Vec<usize> {
        let mut indices: Vec<usize> = Vec::new();
        for index in self.checks.iter().filter_map(|declared| declared.ac) {
            if !indices.contains(&index) {
                indices.push(index);
            }
        }
        indices
    }
}

/// What the verification says about the objective.
///
/// The verdict is stated; the evidence travels with it when there is one.
/// A rehearsal has no evidence: it measured nothing, and it says so
/// instead of pretending it did.
#[derive(Debug, Clone)]
pub struct Verification {
    /// The checks that hold, by id.
    pub satisfied: Vec<String>,
    /// The evidence that speaks for the candidate, when the verifier
    /// really observed it.
    pub evidence: Option<EvidenceSet>,
}

impl Verification {
    /// A verification that holds every one of the given checks.
    pub fn satisfying(required: &[String]) -> Self {
        Self {
            satisfied: required.to_vec(),
            evidence: None,
        }
    }

    /// Whether every required check holds.
    pub fn covers(&self, required: &[String]) -> bool {
        required
            .iter()
            .all(|id| self.satisfied.iter().any(|held| held == id))
    }

    /// The required checks that do NOT hold.
    pub fn missing(&self, required: &[String]) -> Vec<String> {
        required
            .iter()
            .filter(|id| !self.satisfied.iter().any(|held| held == *id))
            .cloned()
            .collect()
    }
}

/// The state of the durable attempt record once the effects moved.
///
/// The record is a SEPARATE fact from the closure: an effect that moved
/// but whose record could not be settled is never re-applied to fix the
/// record, and it is never reported as a promotion failure inviting a
/// second promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Checkpointed {
    /// The record says the effect moved: it must not happen again.
    Settled,
    /// The effect moved and the record could not say so: the effect is
    /// durable, only the record is uncertain.
    Uncertain,
}

/// How the closure of a publication ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Closed {
    /// A ticket was closed and the re-read confirmed it.
    Confirmed,
    /// The objective owns no ticket: nothing had to be closed.
    NotRequired,
    /// Effects moved but the closure was not confirmed.
    Unconfirmed,
}

/// What the publisher did with the candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct Publication {
    /// The revision that was published, as git names it.
    pub revision: String,
    /// How the closure ended.
    pub closure: Closed,
    /// Whether the durable record settled with the effect.
    pub checkpoint: Checkpointed,
    /// What happened, in words.
    pub detail: String,
}

/// The requirements of a work item: what the agent is told to do, and
/// the checks that decide whether the objective held.
pub trait RequirementsPort: Send + Sync {
    fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError>;
}

/// The requirements could not be read.
#[derive(Debug, thiserror::Error)]
#[error("the requirements of {work_ref} could not be read: {reason}")]
pub struct RequirementsError {
    pub work_ref: String,
    pub reason: String,
}

/// The agent that executes the objective in a fresh session.
#[async_trait::async_trait]
pub trait AgentPort: Send + Sync {
    async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError>;
}

/// The agent could not run the objective.
#[derive(Debug, thiserror::Error)]
#[error("the agent could not run {work_ref}: {reason}")]
pub struct AgentError {
    pub work_ref: String,
    pub reason: String,
}

/// The verifier that judges the candidate against the objective.
#[async_trait::async_trait]
pub trait VerifierPort: Send + Sync {
    async fn verify(
        &self,
        objective: &Objective,
        candidate: &Candidate,
    ) -> Result<Verification, VerifierError>;
}

/// The verification could not be made.
#[derive(Debug, thiserror::Error)]
#[error("the candidate of {work_ref} could not be verified: {reason}")]
pub struct VerifierError {
    pub work_ref: String,
    pub reason: String,
}

/// The publisher that promotes the candidate and closes the work item.
#[async_trait::async_trait]
pub trait PublisherPort: Send + Sync {
    async fn publish(
        &self,
        objective: &Objective,
        candidate: &Candidate,
        verification: &Verification,
    ) -> Result<Publication, PublisherError>;
}

/// The publication could not be made.
#[derive(Debug, thiserror::Error)]
#[error("the candidate of {work_ref} could not be published: {reason}")]
pub struct PublisherError {
    pub work_ref: String,
    pub reason: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn required() -> Vec<String> {
        vec!["AC-1".to_string(), "AC-2".to_string()]
    }

    #[test]
    fn a_verification_covers_only_the_checks_that_hold() {
        let verification = Verification {
            satisfied: vec!["AC-1".into()],
            evidence: None,
        };
        assert!(!verification.covers(&required()));
        assert_eq!(verification.missing(&required()), vec!["AC-2".to_string()]);
    }

    #[test]
    fn a_verification_without_evidence_does_not_claim_to_have_measured() {
        let verification = Verification::satisfying(&required());
        assert!(verification.covers(&required()));
        assert!(verification.evidence.is_none());
    }
}
