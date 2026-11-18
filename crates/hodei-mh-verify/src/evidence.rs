//! Evidence sets (AC-2/AC-3): every result is bound to the exact content,
//! the transport that carried it, the authorized command and the environment
//! that produced it. A candidate modification invalidates the affected
//! evidence, checks are satisfied individually, and neither NOT_RUN nor an
//! empty commit can vouch progress.
//!
//! Evidence is minted from measurements only: the constructor is private to
//! the crate, so the only producer of evidence is the runner that observed
//! the checks. What the evidence is bound to travels with it: the revision,
//! the content and transport digests, the required checks with their
//! authorized commands, and the image the checks actually ran in.

use serde::{Deserialize, Serialize};

use crate::git::{GitRevision, GitTree};
use crate::result::{ExecutionRecord, Verdict, verdict_of};
use crate::transport::TransportDigest;

/// One required check and the command authorized to answer it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckBinding {
    pub id: String,
    pub command: String,
}

/// What a whole evidence set is bound to: the candidate and the observed
/// policy and environment that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceBinding {
    /// The revision the candidate was materialized from.
    pub revision: GitRevision,
    /// The content the candidate carried, as git names it.
    pub tree: GitTree,
    /// The identity of the bytes that carried the candidate.
    pub transport_digest: TransportDigest,
    /// The checks this evidence answers, with their authorized commands.
    pub required_checks: Vec<CheckBinding>,
    /// The image the checks ran in, as the runtime resolved it.
    pub image_id: String,
}

impl EvidenceBinding {
    /// The authorized command of a check, if the check is required.
    pub fn command_of(&self, check_id: &str) -> Option<&str> {
        self.required_checks
            .iter()
            .find(|check| check.id == check_id)
            .map(|check| check.command.as_str())
    }
}

/// One executed verification result, bound to its provenance.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundResult {
    /// Individual check this result answers (e.g. "AC-1", "dod-1").
    pub check_id: String,
    /// Fingerprint of the environment that executed the command (image
    /// digest / sandbox id), so results are not portable across environments.
    pub environment: String,
    /// The image the command ran in, as the runtime resolved it.
    pub image_id: String,
    /// The content the command ran in, as git names it.
    pub tree: GitTree,
    /// The bytes that carried the candidate into the sandbox.
    pub transport_digest: TransportDigest,
    pub record: ExecutionRecord,
}

/// Lifecycle of bound evidence. `Invalidated` keeps the record for
/// transparency but strips it of all authority.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EvidenceStatus {
    Valid,
    Invalidated { cause: String },
}

/// The evidence for one candidate of one work item.
///
/// The binding is the only answer to "what does this evidence speak
/// for?": the revision, the content, the transport, the policy and the
/// environment live there, and nowhere else.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceSet {
    pub work_ref: String,
    /// What this evidence is bound to: the candidate and the observed
    /// policy and environment.
    pub binding: EvidenceBinding,
    pub entries: Vec<EvidenceEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceEntry {
    pub result: BoundResult,
    pub status: EvidenceStatus,
}

impl EvidenceSet {
    /// Bind results to the exact candidate: content, transport, policy and
    /// environment.
    ///
    /// Private to the crate on purpose: evidence is minted from
    /// measurements, never declared.
    pub(crate) fn bind(
        work_ref: impl Into<String>,
        binding: EvidenceBinding,
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
            binding,
            entries,
        }
    }

    /// AC-2: the candidate moved. Every entry bound to a previous content
    /// is invalidated (its execution no longer refers to the current
    /// state). The binding does not move with the candidate: it still
    /// names the content the checks ran against, and that is exactly
    /// what the invalidated entries say.
    /// Returns how many entries were invalidated.
    pub fn candidate_changed(&mut self, new_tree: &GitTree) -> usize {
        let mut invalidated = 0;
        for entry in &mut self.entries {
            if entry.result.tree != *new_tree && entry.status == EvidenceStatus::Valid {
                entry.status = EvidenceStatus::Invalidated {
                    cause: "the candidate content changed; evidence refers to a previous state"
                        .into(),
                };
                invalidated += 1;
            }
        }
        invalidated
    }

    /// The policy or the environment moved: every entry that no longer
    /// answers the same check with the same command in the same image is
    /// invalidated. Changing the required checks, their commands or the
    /// image invalidates the evidence that depended on them.
    /// Returns how many entries were invalidated.
    pub fn binding_changed(&mut self, new_binding: &EvidenceBinding) -> usize {
        let mut invalidated = 0;
        for entry in &mut self.entries {
            if entry.status != EvidenceStatus::Valid {
                continue;
            }
            if let Some(cause) = cause_for(entry, new_binding) {
                entry.status = EvidenceStatus::Invalidated { cause };
                invalidated += 1;
            }
        }
        self.binding = new_binding.clone();
        invalidated
    }
}

/// Why this entry no longer speaks for the candidate, if it does not.
fn cause_for(entry: &EvidenceEntry, binding: &EvidenceBinding) -> Option<String> {
    if binding.tree != entry.result.tree {
        return Some("the candidate content changed".to_string());
    }
    if binding.transport_digest != entry.result.transport_digest {
        return Some("the transport that carried the candidate changed".to_string());
    }
    match binding.command_of(&entry.result.check_id) {
        None => Some(format!(
            "the check {} is no longer required",
            entry.result.check_id
        )),
        Some(command) if command != entry.result.record.command => Some(format!(
            "the authorized command of {} changed",
            entry.result.check_id
        )),
        Some(_) if binding.image_id != entry.result.image_id => {
            Some("the image the checks ran in changed".to_string())
        }
        Some(_) => None,
    }
}

impl EvidenceSet {
    /// Whether this evidence still speaks for the given binding.
    pub fn is_bound_to(&self, binding: &EvidenceBinding) -> bool {
        self.binding == *binding
            && self
                .entries
                .iter()
                .all(|entry| matches!(entry.status, EvidenceStatus::Valid))
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

    fn tree(seed: &str) -> GitTree {
        GitTree::parse(seed).expect("a tree")
    }

    fn revision(seed: &str) -> GitRevision {
        GitRevision::parse(seed).expect("a revision")
    }

    fn binding(image_id: &str, command: &str) -> EvidenceBinding {
        EvidenceBinding {
            revision: revision("1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5"),
            tree: tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b"),
            transport_digest: TransportDigest::of(b"bundle"),
            required_checks: vec![CheckBinding {
                id: "AC-1".into(),
                command: command.into(),
            }],
            image_id: image_id.into(),
        }
    }

    fn passing(check_id: &str, content: &GitTree, command: &str, image: &str) -> BoundResult {
        BoundResult {
            check_id: check_id.into(),
            environment: format!("{image}::sandbox-1"),
            image_id: image.into(),
            tree: content.clone(),
            transport_digest: TransportDigest::of(b"bundle"),
            record: ExecutionRecord::terminal(command, 0, "ok"),
        }
    }

    fn failing(check_id: &str, content: &GitTree) -> BoundResult {
        BoundResult {
            check_id: check_id.into(),
            environment: "img:sandbox-1".into(),
            image_id: "img".into(),
            tree: content.clone(),
            transport_digest: TransportDigest::of(b"bundle"),
            record: ExecutionRecord::terminal("make check", 1, "assertion failed"),
        }
    }

    #[test]
    fn only_valid_terminal_pass_satisfies_a_check() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        assert!(set.check_satisfied("AC-1"));
        assert_eq!(set.effective_verdict("AC-1"), Verdict::Pass);
        assert!(set.is_bound_to(&bound));
    }

    #[test]
    fn not_run_and_fail_never_satisfy() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let absent = BoundResult {
            check_id: "AC-1".into(),
            environment: "env".into(),
            image_id: "img".into(),
            tree: content.clone(),
            transport_digest: TransportDigest::of(b"bundle"),
            record: ExecutionRecord::absent("make check", "timeout"),
        };
        let set = EvidenceSet::bind("TASK-9", bound.clone(), vec![absent]);
        assert!(!set.check_satisfied("AC-1"));

        let set = EvidenceSet::bind("TASK-9", bound.clone(), vec![failing("AC-1", &content)]);
        assert!(!set.check_satisfied("AC-1"));
        assert_eq!(set.effective_verdict("AC-1"), Verdict::Fail);
    }

    #[test]
    fn candidate_change_invalidates_bound_evidence() {
        let old = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let new = tree("4d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![
                passing("AC-1", &old, "make check", "img"),
                failing("AC-2", &old),
            ],
        );
        assert!(set.check_satisfied("AC-1"));

        let invalidated = set.candidate_changed(&new);
        assert_eq!(invalidated, 2);
        // The binding still names the content the checks ran against.
        assert_eq!(set.binding.tree, old);
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
        let old = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &old, "make check", "img")],
        );
        assert_eq!(set.candidate_changed(&old), 0);
        assert!(set.check_satisfied("AC-1"));
    }

    #[test]
    fn checks_are_verified_individually() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = EvidenceBinding {
            required_checks: vec![
                CheckBinding {
                    id: "AC-1".into(),
                    command: "make check".into(),
                },
                CheckBinding {
                    id: "AC-2".into(),
                    command: "make check".into(),
                },
            ],
            ..binding("img", "make check")
        };
        let set = EvidenceSet::bind(
            "TASK-9",
            bound,
            vec![
                passing("AC-1", &content, "make check", "img"),
                passing("AC-2", &content, "make check", "img"),
            ],
        );
        // AC-3 done, AC-4 missing: the whole requirement is NOT satisfied.
        assert!(!set.all_checks_satisfied(&["AC-1", "AC-2", "AC-3", "AC-4"]));
        assert!(set.all_checks_satisfied(&["AC-1", "AC-2"]));
        assert_eq!(set.recorded_checks(), vec!["AC-1", "AC-2"]);
    }

    #[test]
    fn invalidated_pass_cannot_mask_a_missing_check() {
        let old = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let new = tree("4d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound,
            vec![passing("AC-1", &old, "make check", "img")],
        );
        let _ = set.candidate_changed(&new);
        assert!(!set.all_checks_satisfied(&["AC-1"]));
    }

    #[test]
    fn a_changed_command_invalidates_the_evidence_of_that_check() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        assert!(set.check_satisfied("AC-1"));

        let mut other = bound.clone();
        other.required_checks[0].command = "make other-check".into();
        assert_eq!(set.binding_changed(&other), 1);
        assert!(!set.check_satisfied("AC-1"));
        assert_eq!(set.effective_verdict("AC-1"), Verdict::NotRun);
    }

    #[test]
    fn a_changed_image_invalidates_the_evidence() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        let mut other = bound;
        other.image_id = "other-img".into();
        assert_eq!(set.binding_changed(&other), 1);
        assert!(!set.check_satisfied("AC-1"));
    }

    #[test]
    fn a_check_that_is_no_longer_required_invalidates_its_evidence() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        let mut other = bound;
        other.required_checks.clear();
        assert_eq!(set.binding_changed(&other), 1);
        assert!(!set.check_satisfied("AC-1"));
        assert!(!set.is_bound_to(&other));
    }

    #[test]
    fn a_changed_transport_invalidates_the_evidence() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        assert!(set.check_satisfied("AC-1"));

        let mut other = bound;
        other.transport_digest = TransportDigest::of(b"another bundle");
        assert_eq!(set.binding_changed(&other), 1);
        assert!(!set.check_satisfied("AC-1"));
    }

    #[test]
    fn an_unchanged_binding_invalidates_nothing() {
        let content = tree("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b");
        let bound = binding("img", "make check");
        let mut set = EvidenceSet::bind(
            "TASK-9",
            bound.clone(),
            vec![passing("AC-1", &content, "make check", "img")],
        );
        assert_eq!(set.binding_changed(&bound), 0);
        assert!(set.check_satisfied("AC-1"));
        assert!(set.is_bound_to(&bound));
    }
}
