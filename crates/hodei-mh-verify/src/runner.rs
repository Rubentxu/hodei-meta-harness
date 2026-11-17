//! Clean-environment runner (AC-4, effects adapter): materializes the
//! candidate from a git bundle inside a throwaway sandbox — never the
//! developer's working tree — executes the policy commands there, and binds
//! every result to the candidate digest and environment fingerprint.
//! A timeout or lost session yields an ABSENT record: never a PASS.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use opensandbox::execd::api;

use crate::digest::CandidateDigest;
use crate::evidence::{BoundResult, EvidenceSet};
use crate::result::ExecutionRecord;

/// One named verification command (e.g. check "AC-1" runs `cargo test`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub id: String,
    pub command: String,
}

/// The candidate under verification: content identity plus its git bundle.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub revision: String,
    pub digest: CandidateDigest,
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
    /// (integrity gate) → run each check → bind results. The sandbox is
    /// deleted on every path, including errors.
    pub async fn verify(
        &self,
        work_ref: &str,
        candidate: &Candidate,
        checks: &[Check],
    ) -> Result<EvidenceSet, RunnerError> {
        let client = reqwest::Client::new();
        let sandbox_id = self.create_sandbox(&client).await?;
        let outcome = self
            .run_checks_on(&client, &sandbox_id, candidate, checks)
            .await;
        let deleted = client
            .delete(format!("{}/v1/sandboxes/{sandbox_id}", self.lifecycle_url))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false);
        let (bound, _environment) = outcome?;
        if !deleted {
            tracing::warn!(%sandbox_id, "clean sandbox deletion failed");
        }
        Ok(EvidenceSet::bind(
            work_ref,
            candidate.revision.clone(),
            candidate.digest.clone(),
            bound,
        ))
    }

    async fn create_sandbox(&self, client: &reqwest::Client) -> Result<String, RunnerError> {
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
        value["id"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| RunnerError("sandbox create returned no id".into()))
    }

    async fn run_checks_on(
        &self,
        client: &reqwest::Client,
        sandbox_id: &str,
        candidate: &Candidate,
        checks: &[Check],
    ) -> Result<(Vec<BoundResult>, String), RunnerError> {
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
        let environment = format!("{}::{}", self.image, sandbox_id);

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

        let mut bound = Vec::new();
        for check in checks {
            let command = format!("cd /workspace/checkout && {}", check.command);
            let record = run_with_timeout(&execd_base, &command, self.timeout).await;
            bound.push(BoundResult {
                check_id: check.id.clone(),
                environment: environment.clone(),
                candidate_digest: candidate.digest.clone(),
                record,
            });
        }
        Ok((bound, environment))
    }
}

/// Run one command to its terminal state or to the timeout (shared,
/// attach-first oneshot), mapped into an ExecutionRecord: timeout and
/// signal deaths produce absence — never a PASS (AC-1).
async fn run_with_timeout(execd_base: &str, command: &str, timeout: Duration) -> ExecutionRecord {
    let outcome = opensandbox::execd::oneshot::run(execd_base, command, timeout).await;
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
}
