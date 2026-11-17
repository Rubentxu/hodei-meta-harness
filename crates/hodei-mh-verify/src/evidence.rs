//! Evidence sets (AC-2/AC-3): every result is bound to the exact candidate
//! digest, the command and the environment that produced it. A candidate
//! modification invalidates the affected evidence, checks are satisfied
//! individually, and neither NOT_RUN nor an empty commit can vouch progress.

use serde::{Deserialize, Serialize};

use crate::digest::CandidateDigest;
use crate::result::{ExecutionRecord, Verdict, verdict_of};

/// One executed verification result, bound to its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundResult {
    /// Individual check this result answers (e.g. "AC-1", "dod-1").
    pub check_id: String,
    /// Fingerprint of the environment that executed the command (image
    /// digest / sandbox id), so results are not portable across environments.
    pub environment: String,
    pub candidate_digest: CandidateDigest,
    pub record: ExecutionRecord,
}

/// Lifecycle of bound evidence. `Invalidated` keeps the record for
/// transparency but strips it of all authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceStatus {
    Valid,
    Invalidated { cause: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEntry {
    pub result: BoundResult,
    pub status: EvidenceStatus,
}

/// The evidence for one candidate of one work item.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSet {
    pub work_ref: String,
    /// Revision (git sha) the candidate was materialized from.
    pub revision: String,
    pub candidate_digest: CandidateDigest,
    pub entries: Vec<EvidenceEntry>,
}

impl EvidenceSet {
    /// Bind results to the exact candidate: digest, revision, environment.
    pub fn bind(
        work_ref: impl Into<String>,
        revision: impl Into<String>,
        candidate_digest: CandidateDigest,
        results: Vec<BoundResult>,
    ) -> Self {
        let entries = results
            .into_iter()
            .map(|result| EvidenceEntry {
                result,
                status: EvidenceStatus::Valid,
            })
            .collect();
        Self {
            work_ref: work_ref.into(),
            revision: revision.into(),
            candidate_digest,
            entries,
        }
    }

    /// AC-2: the candidate moved. Every entry bound to a previous digest is
    /// invalidated (its execution no longer refers to the current state).
    /// Returns how many entries were invalidated.
    pub fn candidate_changed(&mut self, new_digest: &CandidateDigest) -> usize {
        let mut invalidated = 0;
        for entry in &mut self.entries {
            if entry.result.candidate_digest != *new_digest && entry.status == EvidenceStatus::Valid
            {
                entry.status = EvidenceStatus::Invalidated {
                    cause: "candidate digest changed; evidence refers to a previous state".into(),
                };
                invalidated += 1;
            }
        }
        if self.candidate_digest != *new_digest {
            self.candidate_digest = new_digest.clone();
        }
        invalidated
    }

    /// Verdict of a check as of NOW: an invalidated entry behaves as NotRun
    /// even if its stored verdict was Pass.
    pub fn effective_verdict(&self, check_id: &str) -> Verdict {
        self.entries
            .iter()
            .filter(|entry| entry.result.check_id == check_id)
            .map(|entry| match entry.status {
                EvidenceStatus::Valid => verdict_of(&entry.result.record),
                EvidenceStatus::Invalidated { .. } => Verdict::NotRun,
            })
            .find(|verdict| verdict != &Verdict::NotRun)
            .unwrap_or(Verdict::NotRun)
    }

    /// AC-3: a check is satisfied only by valid, terminal, passing evidence.
    /// NOT_RUN and FAIL never satisfy; invalidated never satisfies.
    pub fn check_satisfied(&self, check_id: &str) -> bool {
        self.effective_verdict(check_id) == Verdict::Pass
    }

    /// AC-3: acceptance and DoD items are verified INDIVIDUALLY — every
    /// required check must be satisfied on its own.
    pub fn all_checks_satisfied(&self, required: &[&str]) -> bool {
        required
            .iter()
            .all(|check_id| self.check_satisfied(check_id))
    }

    /// Ids of checks with at least one recorded entry (run or not).
    pub fn recorded_checks(&self) -> Vec<&str> {
        let mut ids: Vec<&str> = self
            .entries
            .iter()
            .map(|entry| entry.result.check_id.as_str())
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(seed: &str) -> CandidateDigest {
        CandidateDigest::from_bytes(seed.as_bytes())
    }

    fn passing(check_id: &str, candidate: &CandidateDigest) -> BoundResult {
        BoundResult {
            check_id: check_id.into(),
            environment: "img:opensandbox-test@sha256:abc".into(),
            candidate_digest: candidate.clone(),
            record: ExecutionRecord::terminal("make check", 0, "ok"),
        }
    }

    fn failing(check_id: &str, candidate: &CandidateDigest) -> BoundResult {
        BoundResult {
            check_id: check_id.into(),
            environment: "img:opensandbox-test@sha256:abc".into(),
            candidate_digest: candidate.clone(),
            record: ExecutionRecord::terminal("make check", 1, "assertion failed"),
        }
    }

    #[test]
    fn only_valid_terminal_pass_satisfies_a_check() {
        let candidate = digest("c1");
        let set = EvidenceSet::bind(
            "TASK-9",
            "rev1",
            candidate.clone(),
            vec![passing("AC-1", &candidate)],
        );
        assert!(set.check_satisfied("AC-1"));
        assert_eq!(set.effective_verdict("AC-1"), Verdict::Pass);
    }

    #[test]
    fn not_run_and_fail_never_satisfy() {
        let candidate = digest("c2");
        let absent = BoundResult {
            check_id: "AC-1".into(),
            environment: "env".into(),
            candidate_digest: candidate.clone(),
            record: ExecutionRecord::absent("make check", "timeout"),
        };
        let set = EvidenceSet::bind("TASK-9", "rev1", candidate.clone(), vec![absent]);
        assert!(!set.check_satisfied("AC-1"));

        let set = EvidenceSet::bind(
            "TASK-9",
            "rev1",
            candidate.clone(),
            vec![failing("AC-1", &candidate)],
        );
        assert!(!set.check_satisfied("AC-1"));
        assert_eq!(set.effective_verdict("AC-1"), Verdict::Fail);
    }

    #[test]
    fn candidate_change_invalidates_bound_evidence() {
        let old = digest("old");
        let new = digest("new");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            "rev1",
            old.clone(),
            vec![passing("AC-1", &old), failing("AC-2", &old)],
        );
        assert!(set.check_satisfied("AC-1"));

        let invalidated = set.candidate_changed(&new);
        assert_eq!(invalidated, 2);
        assert_eq!(set.candidate_digest, new);
        // The stored verdict was Pass, but the evidence no longer refers to
        // the current candidate: it acts as NotRun.
        assert_eq!(set.effective_verdict("AC-1"), Verdict::NotRun);
        assert!(!set.check_satisfied("AC-1"));
        for entry in &set.entries {
            assert!(matches!(entry.status, EvidenceStatus::Invalidated { .. }));
        }
    }

    #[test]
    fn same_candidate_change_invalidates_nothing() {
        let old = digest("same");
        let mut set = EvidenceSet::bind("TASK-9", "rev1", old.clone(), vec![passing("AC-1", &old)]);
        assert_eq!(set.candidate_changed(&old), 0);
        assert!(set.check_satisfied("AC-1"));
    }

    #[test]
    fn checks_are_verified_individually() {
        let candidate = digest("c3");
        let set = EvidenceSet::bind(
            "TASK-9",
            "rev1",
            candidate.clone(),
            vec![passing("AC-1", &candidate), passing("AC-2", &candidate)],
        );
        // AC-3 done, AC-4 missing: the whole requirement is NOT satisfied.
        assert!(!set.all_checks_satisfied(&["AC-1", "AC-2", "AC-3", "AC-4"]));
        assert!(set.all_checks_satisfied(&["AC-1", "AC-2"]));
        assert_eq!(set.recorded_checks(), vec!["AC-1", "AC-2"]);
    }

    #[test]
    fn invalidated_pass_cannot_mask_a_missing_check() {
        let old = digest("o");
        let new = digest("n");
        let mut set = EvidenceSet::bind("TASK-9", "rev1", old.clone(), vec![passing("AC-1", &old)]);
        let _ = set.candidate_changed(&new);
        assert!(!set.all_checks_satisfied(&["AC-1"]));
    }
}
