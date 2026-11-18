//! Clean-environment runner (AC-4, effects adapter): materializes the
//! candidate from a git bundle inside a throwaway sandbox — never the
//! developer's working tree — executes the policy commands there, and binds
//! every result to the candidate digest and environment fingerprint.
//! A timeout or lost session yields an ABSENT record: never a PASS.
//!
//! Nothing runs until the candidate is the one it claims to be: the
//! transport is checked against its declared digest, the head of the
//! checkout is compared with the declared revision, and the content of the
//! checkout is compared with the declared content, as git names it. A
//! candidate that fails any of those is rejected BEFORE the checks, so a
//! wrong candidate never produces evidence.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use opensandbox::execd::api;

use crate::evidence::{BoundResult, CheckBinding, EvidenceBinding, EvidenceSet};
use crate::git::{GitRevision, GitTree, RevisionMatch, compare};
use crate::result::ExecutionRecord;
use crate::transport::TransportDigest;

/// Bounded wait for the sandbox's execd to accept sessions. A fresh
/// sandbox publishes its port before the server inside can answer.
const EXECD_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounded wait for the observations of the checkout (head and files).
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the candidate is materialized inside the sandbox.
const CHECKOUT: &str = "/workspace/checkout";

/// One named verification command (e.g. check "AC-1" runs `cargo test`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub command: String,
}

/// The candidate under verification: its identity and the transport that
/// carries it.
#[derive(Debug, Clone)]
pub struct Candidate {
    /// The revision the candidate claims to be.
    pub revision: GitRevision,
    /// The content the candidate claims to carry, as git names it.
    pub tree: GitTree,
    /// The identity of the bytes that carry the candidate.
    pub transport_digest: TransportDigest,
    /// The transport itself: the git bundle.
    pub bundle: Vec<u8>,
}

#[derive(Debug, Error)]
#[error("clean runner failed: {0}")]
pub struct RunnerError(pub String);

pub struct CleanRunner {
    /// Lifecycle server base URL (e.g. http://127.0.0.1:8081).
    pub lifecycle_url: String,
    /// Image for the clean environment.
    pub image: String,
    /// Per-check timeout: expiry produces an ABSENT record, not a failure
    /// invented out of thin air.
    pub timeout: Duration,
}

impl CleanRunner {
    /// Verify the candidate in a fresh sandbox: upload bundle → real clone
    /// (integrity gate) → observe what was actually checked out → run each
    /// check → bind results. The sandbox is deleted on every path,
    /// including errors.
    pub async fn verify(
        &self,
        work_ref: &str,
        candidate: &Candidate,
        checks: &[Check],
    ) -> Result<EvidenceSet, RunnerError> {
        // The transport is checked before anything is executed: a bundle
        // that is not the declared one never reaches the sandbox.
        let observed_transport = TransportDigest::of(&candidate.bundle);
        if observed_transport != candidate.transport_digest {
            return Err(RunnerError(format!(
                "the transport does not match its declared digest: declared {}, observed {}",
                candidate.transport_digest.as_str(),
                observed_transport.as_str()
            )));
        }

        let client = reqwest::Client::new();
        let (sandbox_id, created) = self.create_sandbox(&client).await?;
        // Everything that can fail AFTER the sandbox exists runs inside
        // this block, so the sandbox is deleted on every path: a response
        // the runner cannot read must not leave the host with residue.
        let outcome = async {
            let image_id = observed_image_id(&created)?;
            let (bound, observed) = self
                .run_checks_on(
                    &client,
                    &sandbox_id,
                    &image_id,
                    &observed_transport,
                    candidate,
                    checks,
                )
                .await?;
            Ok::<_, RunnerError>((bound, observed, image_id))
        }
        .await;
        let deleted = client
            .delete(format!("{}/v1/sandboxes/{sandbox_id}", self.lifecycle_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        let (bound, observed, image_id) = outcome?;
        if !deleted {
            tracing::warn!(%sandbox_id, "clean sandbox deletion failed");
        }
        // The evidence records what was OBSERVED, not what was declared:
        // the head and the content git reported in the checkout that ran,
        // and the bytes that really travelled. The declarations were
        // compared with them; a difference never reached this point.
        let binding = EvidenceBinding {
            revision: observed.revision,
            tree: observed.tree,
            transport_digest: observed_transport,
            required_checks: checks
                .iter()
                .map(|check| CheckBinding {
                    id: check.id.clone(),
                    command: check.command.clone(),
                })
                .collect(),
            image_id,
        };
        Ok(EvidenceSet::bind(work_ref, binding, bound))
    }

    /// Create the clean sandbox: its id and the response that describes
    /// it. This fails only when NO sandbox was created.
    async fn create_sandbox(
        &self,
        client: &reqwest::Client,
    ) -> Result<(String, serde_json::Value), RunnerError> {
        let response = client
            .post(format!("{}/v1/sandboxes", self.lifecycle_url))
            .json(&serde_json::json!({
                "image": {"uri": self.image},
                "entrypoint": ["sleep", "infinity"],
                "timeout": 1800,
                "metadata": {"purpose": "hodei-mh-clean-verify"},
            }))
            .send()
            .await
            .map_err(|e| RunnerError(format!("sandbox create: {e}")))?;
        if !response.status().is_success() {
            return Err(RunnerError(format!(
                "sandbox create status {}",
                response.status()
            )));
        }
        let value: serde_json::Value = response
            .json()
            .await
            .map_err(|e| RunnerError(e.to_string()))?;
        let sandbox_id = value["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| RunnerError("sandbox create returned no id".into()))?;
        Ok((sandbox_id, value))
    }

    async fn run_checks_on(
        &self,
        client: &reqwest::Client,
        sandbox_id: &str,
        image_id: &str,
        transport: &TransportDigest,
        candidate: &Candidate,
        checks: &[Check],
    ) -> Result<(Vec<BoundResult>, ObservedCandidate), RunnerError> {
        let endpoint = client
            .get(format!(
                "{}/v1/sandboxes/{sandbox_id}/endpoints/44772",
                self.lifecycle_url
            ))
            .send()
            .await
            .map_err(|e| RunnerError(format!("endpoint resolve: {e}")))?;
        let value: serde_json::Value = endpoint
            .json()
            .await
            .map_err(|e| RunnerError(e.to_string()))?;
        let host = value["endpoint"]
            .as_str()
            .ok_or_else(|| RunnerError("no execd endpoint".into()))?;
        let execd_base = format!("http://{host}");
        let environment = format!("{image_id}::{sandbox_id}");

        // The sandbox publishes its execd port BEFORE the server inside
        // can answer: wait (bounded) for it, so the upload does not fail
        // for a reason that has nothing to do with the candidate.
        if !opensandbox::execd::oneshot::wait_until_ready(&execd_base, EXECD_READY_TIMEOUT).await {
            return Err(RunnerError(
                "the sandbox execd never became usable".to_string(),
            ));
        }

        // The sandbox port mapping needs a moment to accept connections
        // right after creation; a short retry covers that window.
        let mut uploaded = Err(RunnerError("bundle upload not attempted".into()));
        for attempt in 0..5 {
            uploaded = api::upload_file(
                &execd_base,
                None,
                candidate.bundle.clone(),
                "/tmp/candidate.bundle",
            )
            .await
            .map_err(|e| RunnerError(format!("bundle upload: {e}")));
            if uploaded.is_ok() {
                break;
            }
            if attempt < 4 {
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
        }
        uploaded?;
        // A real clone is the integrity gate: a truncated or tampered bundle
        // fails here instead of producing bogus verdicts. The product flow
        // normalizes candidate repos to branch `main`.
        let prepared = run_with_timeout(
            &execd_base,
            "git init -q -b main /workspace/checkout && git -C /workspace/checkout fetch -q /tmp/candidate.bundle 'refs/heads/*:refs/remotes/bundle/*' && git -C /workspace/checkout checkout -q -f -B main refs/remotes/bundle/main",
            Duration::from_secs(60),
        )
        .await;
        if prepared.exit_code != Some(0) {
            return Err(RunnerError(format!(
                "candidate clone failed integrity gate: {}",
                prepared.output
            )));
        }

        // What was actually checked out must be what the candidate claims:
        // the head and the content it carries. Both are observed in the
        // sandbox, and both are compared BEFORE any check runs.
        let observed_head = observe(&execd_base, "git rev-parse HEAD").await?;
        let observed_revision = GitRevision::parse(&observed_head)
            .map_err(|e| RunnerError(format!("the checkout did not report a usable head: {e}")))?;
        if let RevisionMatch::Differs { declared, observed } =
            compare(&candidate.revision, &observed_revision)
        {
            return Err(RunnerError(format!(
                "the checkout ran another commit: declared {}, observed {}",
                declared.as_str(),
                observed.as_str()
            )));
        }
        let observed_tree = observe(&execd_base, "git rev-parse HEAD^{tree}").await?;
        let observed_tree = GitTree::parse(&observed_tree)
            .map_err(|e| RunnerError(format!("the checkout did not report a usable tree: {e}")))?;
        if observed_tree != candidate.tree {
            return Err(RunnerError(format!(
                "the checkout holds another content: declared {}, observed {}",
                candidate.tree.as_str(),
                observed_tree.as_str()
            )));
        }

        let mut bound = Vec::new();
        for check in checks {
            let command = format!("cd {CHECKOUT} && {}", check.command);
            let record = run_with_timeout(&execd_base, &command, self.timeout).await;
            bound.push(BoundResult {
                check_id: check.id.clone(),
                environment: environment.clone(),
                image_id: image_id.to_string(),
                tree: observed_tree.clone(),
                transport_digest: transport.clone(),
                record,
            });
        }
        Ok((
            bound,
            ObservedCandidate {
                revision: observed_revision,
                tree: observed_tree,
            },
        ))
    }
}

/// What the checkout really was: the identity git reported inside the
/// sandbox that ran the checks.
#[derive(Debug, Clone)]
struct ObservedCandidate {
    revision: GitRevision,
    tree: GitTree,
}

/// The id of the image the runtime resolved, read from the create
/// response: the environment the checks ran in is identified by the id,
/// not by the tag. The control plane names every field of its contract in
/// camelCase; the snake_case spelling is accepted as an alias, and an
/// absent id is a failure: the binding needs the image, not the tag.
fn observed_image_id(created: &serde_json::Value) -> Result<String, RunnerError> {
    created["imageId"]
        .as_str()
        .or_else(|| created["image_id"].as_str())
        .map(str::to_string)
        .filter(|id| !id.is_empty())
        .ok_or_else(|| RunnerError("the runtime did not report the image it resolved".into()))
}

/// Run one command and read its output: a command that does not succeed is
/// an observation failure, never an empty observation.
async fn observe(execd_base: &str, command: &str) -> Result<String, RunnerError> {
    let outcome = opensandbox::execd::oneshot::run_when_ready(
        execd_base,
        &format!("cd {CHECKOUT} && {command}"),
        OBSERVE_TIMEOUT,
        EXECD_READY_TIMEOUT,
    )
    .await;
    if outcome.exit_code != Some(0) {
        return Err(RunnerError(format!(
            "the observation {command:?} did not succeed: {}",
            outcome.output
        )));
    }
    Ok(outcome.output.trim().to_string())
}

/// Run one command to its terminal state or to the timeout (shared,
/// attach-first oneshot), mapped into an ExecutionRecord: timeout and
/// signal deaths produce absence — never a PASS (AC-1).
///
/// The sandbox may still be booting when the verification starts: the
/// execd refuses the first attempts. The readiness wait runs a harmless
/// probe, so the check command itself runs EXACTLY ONCE.
async fn run_with_timeout(execd_base: &str, command: &str, timeout: Duration) -> ExecutionRecord {
    let outcome = opensandbox::execd::oneshot::run_when_ready(
        execd_base,
        command,
        timeout,
        EXECD_READY_TIMEOUT,
    )
    .await;
    match outcome.exit_code {
        Some(code) => ExecutionRecord::terminal(command, code, outcome.output),
        None => ExecutionRecord::absent(
            command,
            if outcome.output.is_empty() {
                "no terminal result observed"
            } else {
                "process died by signal; no terminal exit status was produced"
            },
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_records_never_carry_an_exit_code() {
        let timeout = ExecutionRecord::absent("cmd", "timed out");
        assert_eq!(timeout.exit_code, None);
        assert!(!timeout.stream_completed);
    }

    /// The transport is checked BEFORE the sandbox exists: the control
    /// plane here answers nothing, so a failure that mentions the
    /// transport proves the order.
    #[tokio::test]
    async fn a_transport_that_does_not_match_its_digest_is_refused_before_the_sandbox() {
        let runner = CleanRunner {
            lifecycle_url: "http://127.0.0.1:1".to_string(),
            image: "no-such-image".to_string(),
            timeout: Duration::from_secs(1),
        };
        let candidate = Candidate {
            revision: GitRevision::parse("1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5")
                .expect("a revision"),
            tree: GitTree::parse("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b").expect("a tree"),
            transport_digest: TransportDigest::of(b"the declared bytes"),
            bundle: b"other bytes".to_vec(),
        };
        let error = runner
            .verify(
                "TASK-9",
                &candidate,
                &[Check {
                    id: "check".into(),
                    command: "true".into(),
                }],
            )
            .await
            .expect_err("the transport does not match its declared digest");
        assert!(
            error.to_string().contains("transport"),
            "the transport is refused before the sandbox: {error}"
        );
    }
}
