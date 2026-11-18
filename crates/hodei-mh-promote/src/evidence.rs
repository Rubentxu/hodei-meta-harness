//! The evidence a promotion requires.
//!
//! A candidate moves into the managed branch only with the evidence that
//! speaks for it: the same work, the same commit and the very bundle that
//! was verified. Evidence that is absent, stale or foreign stops the
//! promotion — a candidate that was never verified never moves. Evidence
//! that demands nothing is refused too: an empty policy is a hole, not a
//! proof.

use verify::evidence::{EvidenceSet, EvidenceStatus};
use verify::git::GitRevision;
use verify::transport::TransportDigest;

/// What the promotion knows about the candidate it is about to promote,
/// read from the bundle itself: the boundary where the candidate is
/// verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromotionCandidate {
    /// The work item the promotion belongs to.
    pub work_ref: String,
    /// The head the bundle names.
    pub revision: GitRevision,
    /// The digest of the bundle bytes.
    pub transport_digest: TransportDigest,
}

#[derive(Debug, thiserror::Error)]
pub enum EvidenceError {
    #[error("there is no evidence for this candidate")]
    Missing,
    #[error("the evidence belongs to another work item: {found}")]
    OtherWork { found: String },
    #[error(
        "the evidence belongs to another commit: the candidate is {declared}, the evidence speaks for {found}"
    )]
    AnotherCommit { declared: String, found: String },
    #[error("the bundle is not the one that was verified")]
    UnverifiedBundle,
    #[error("the evidence is stale: {cause}")]
    Stale { cause: String },
    #[error("the evidence does not satisfy the check {check_id}")]
    Unsatisfied { check_id: String },
    #[error("the evidence requires no check at all: it demands nothing")]
    EmptyPolicy,
    #[error("the evidence is not coherent with its own binding: {detail}")]
    Inconsistent { detail: String },
}

/// The evidence a promotion requires before the candidate moves.
pub fn require_evidence(
    evidence: &EvidenceSet,
    candidate: &PromotionCandidate,
) -> Result<(), EvidenceError> {
    if evidence.entries.is_empty() {
        return Err(EvidenceError::Missing);
    }
    if evidence.work_ref != candidate.work_ref {
        return Err(EvidenceError::OtherWork {
            found: evidence.work_ref.clone(),
        });
    }
    if evidence.binding.revision != candidate.revision {
        return Err(EvidenceError::AnotherCommit {
            declared: candidate.revision.as_str().to_string(),
            found: evidence.binding.revision.as_str().to_string(),
        });
    }
    if evidence.binding.transport_digest != candidate.transport_digest {
        return Err(EvidenceError::UnverifiedBundle);
    }
    // Evidence that requires nothing vouches for nothing: a policy with
    // no checks is a hole, not a proof.
    if evidence.binding.required_checks.is_empty() {
        return Err(EvidenceError::EmptyPolicy);
    }
    for entry in &evidence.entries {
        if let EvidenceStatus::Invalidated { cause } = &entry.status {
            return Err(EvidenceError::Stale {
                cause: cause.clone(),
            });
        }
    }
    // The document must speak with one voice: every result answers the
    // content, the transport, the image, the environment and the command
    // its own binding declares.
    for entry in &evidence.entries {
        let check_id = entry.result.check_id.as_str();
        if entry.result.tree != evidence.binding.tree {
            return Err(EvidenceError::Inconsistent {
                detail: format!("the result of {check_id} refers to another content"),
            });
        }
        if entry.result.transport_digest != evidence.binding.transport_digest {
            return Err(EvidenceError::Inconsistent {
                detail: format!("the result of {check_id} was produced by another transport"),
            });
        }
        if entry.result.image_id != evidence.binding.image_id {
            return Err(EvidenceError::Inconsistent {
                detail: format!("the result of {check_id} ran in another image"),
            });
        }
        if entry.result.environment != evidence.binding.environment {
            return Err(EvidenceError::Inconsistent {
                detail: format!("the result of {check_id} ran in another environment"),
            });
        }
        match evidence.binding.command_of(check_id) {
            None => {
                return Err(EvidenceError::Inconsistent {
                    detail: format!(
                        "the result of {check_id} answers a check that is not required"
                    ),
                });
            }
            Some(command) if command != entry.result.command => {
                return Err(EvidenceError::Inconsistent {
                    detail: format!("the result of {check_id} ran another command"),
                });
            }
            Some(_) => {}
        }
    }
    for check in &evidence.binding.required_checks {
        if !evidence.check_satisfied(&check.id) {
            return Err(EvidenceError::Unsatisfied {
                check_id: check.id.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const REVISION: &str = "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5";

    /// The content the evidence speaks for.
    const TREE: &str = "3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b";

    /// Evidence as it arrives from the store: a document, not a value the
    /// promotion builds itself.
    fn evidence_document(
        work_ref: &str,
        revision: &str,
        transport: &str,
        status: serde_json::Value,
    ) -> serde_json::Value {
        json!({
            "work_ref": work_ref,
            "binding": {
                "revision": revision,
                "tree": TREE,
                "transport_digest": TransportDigest::of(transport.as_bytes()).as_str(),
                "required_checks": [{"id": "AC-1", "command": "make check"}],
                "image_id": "sha256:image",
                "environment": "sha256:image::sandbox-1"
            },
            "entries": [{
                "result": {
                    "check_id": "AC-1",
                    "environment": "sha256:image::sandbox-1",
                    "image_id": "sha256:image",
                    "tree": TREE,
                    "transport_digest": TransportDigest::of(transport.as_bytes()).as_str(),
                    "command": "make check",
                    "record": {
                        "command": "make check",
                        "exit_code": 0,
                        "output": "ok",
                        "output_truncated": false,
                        "stream_completed": true
                    }
                },
                "status": status
            }]
        })
    }

    fn evidence(
        work_ref: &str,
        revision: &str,
        transport: &str,
        status: serde_json::Value,
    ) -> EvidenceSet {
        serde_json::from_value(evidence_document(work_ref, revision, transport, status))
            .expect("the evidence document is well formed")
    }

    fn candidate(work_ref: &str, revision: &str, transport: &str) -> PromotionCandidate {
        PromotionCandidate {
            work_ref: work_ref.to_string(),
            revision: GitRevision::parse(revision).expect("a revision"),
            transport_digest: TransportDigest::of(transport.as_bytes()),
        }
    }

    #[test]
    fn the_evidence_of_the_candidate_unlocks_the_promotion() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let evidence = evidence("TASK-9", REVISION, "the bundle", json!("Valid"));
        assert!(require_evidence(&evidence, &candidate).is_ok());
    }

    #[test]
    fn evidence_of_another_work_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let evidence = evidence("TASK-8", REVISION, "the bundle", json!("Valid"));
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::OtherWork { .. })
        ));
    }

    #[test]
    fn evidence_of_another_commit_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let other = "2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5a";
        let evidence = evidence("TASK-9", other, "the bundle", json!("Valid"));
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::AnotherCommit { .. })
        ));
    }

    #[test]
    fn evidence_of_another_bundle_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let evidence = evidence("TASK-9", REVISION, "another bundle", json!("Valid"));
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::UnverifiedBundle)
        ));
    }

    #[test]
    fn stale_evidence_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let evidence = evidence(
            "TASK-9",
            REVISION,
            "the bundle",
            json!({"Invalidated": {"cause": "the candidate content changed"}}),
        );
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Stale { .. })
        ));
    }

    #[test]
    fn absent_evidence_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut evidence = evidence("TASK-9", REVISION, "the bundle", json!("Valid"));
        evidence.entries.clear();
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Missing)
        ));
    }

    #[test]
    fn an_evidence_that_contradicts_its_own_binding_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut document = evidence_document("TASK-9", REVISION, "the bundle", json!("Valid"));
        document["entries"][0]["result"]["transport_digest"] =
            json!(TransportDigest::of(b"another transport").as_str());
        let evidence: EvidenceSet =
            serde_json::from_value(document).expect("the evidence document is well formed");
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Inconsistent { .. })
        ));
    }

    #[test]
    fn evidence_that_requires_nothing_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut document = evidence_document("TASK-9", REVISION, "the bundle", json!("Valid"));
        document["binding"]["required_checks"] = json!([]);
        let evidence: EvidenceSet =
            serde_json::from_value(document).expect("the evidence document is well formed");
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::EmptyPolicy)
        ));
    }

    #[test]
    fn evidence_of_another_environment_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut document = evidence_document("TASK-9", REVISION, "the bundle", json!("Valid"));
        document["entries"][0]["result"]["environment"] = json!("sha256:image::other-sandbox");
        let evidence: EvidenceSet =
            serde_json::from_value(document).expect("the evidence document is well formed");
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Inconsistent { .. })
        ));
    }

    #[test]
    fn evidence_that_ran_another_command_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut document = evidence_document("TASK-9", REVISION, "the bundle", json!("Valid"));
        document["entries"][0]["result"]["command"] = json!("make another-check");
        let evidence: EvidenceSet =
            serde_json::from_value(document).expect("the evidence document is well formed");
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Inconsistent { .. })
        ));
    }

    #[test]
    fn evidence_that_does_not_satisfy_its_checks_is_refused() {
        let candidate = candidate("TASK-9", REVISION, "the bundle");
        let mut document = evidence_document("TASK-9", REVISION, "the bundle", json!("Valid"));
        document["entries"][0]["result"]["record"]["exit_code"] = json!(1);
        let evidence: EvidenceSet =
            serde_json::from_value(document).expect("the evidence document is well formed");
        assert!(matches!(
            require_evidence(&evidence, &candidate),
            Err(EvidenceError::Unsatisfied { .. })
        ));
    }
}
