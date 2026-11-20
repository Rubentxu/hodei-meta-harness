//! The verifier of the live pipeline: the candidate in a clean
//! environment.
//!
//! The verdict is read from the EVIDENCE the clean runner minted: a check
//! counts only when its bound, passing evidence exists for the candidate
//! that was really observed. A verdict that is not backed by evidence is
//! never produced here.

use verify::runner::{Candidate, Check, CleanRunner};

use runner::ports::{Objective, Verification, VerifierError, VerifierPort};

/// The verifier that runs the objective's checks on the candidate in a
/// fresh sandbox.
pub struct CleanVerifier {
    pub runner: CleanRunner,
}

#[async_trait::async_trait]
impl VerifierPort for CleanVerifier {
    async fn verify(
        &self,
        objective: &Objective,
        candidate: &Candidate,
    ) -> Result<Verification, VerifierError> {
        // The verifier judges the executable checks; the criterion that
        // declares each one is the closure's business.
        let checks: Vec<Check> = objective
            .checks
            .iter()
            .map(|declared| declared.check.clone())
            .collect();
        let evidence = self
            .runner
            .verify(&objective.work_ref, candidate, &checks)
            .await
            .map_err(|error| VerifierError {
                work_ref: objective.work_ref.clone(),
                reason: error.to_string(),
            })?;
        let satisfied = satisfied_of(&objective.required(), |check_id| {
            evidence.check_satisfied(check_id)
        });
        Ok(Verification {
            satisfied,
            // The evidence travels with the verdict: the promotion demands
            // it, and a verdict without it could not be promoted.
            evidence: Some(evidence),
        })
    }
}

/// The required checks whose evidence holds.
pub fn satisfied_of(required: &[String], holds: impl Fn(&str) -> bool) -> Vec<String> {
    required
        .iter()
        .filter(|check_id| holds(check_id))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_checks_with_holding_evidence_are_satisfied() {
        let required = vec!["check-1".to_string(), "check-2".to_string()];
        let satisfied = satisfied_of(&required, |check_id| check_id == "check-2");
        assert_eq!(satisfied, vec!["check-2".to_string()]);
    }

    #[test]
    fn no_evidence_satisfies_nothing() {
        let required = vec!["check-1".to_string()];
        assert!(satisfied_of(&required, |_| false).is_empty());
    }
}
