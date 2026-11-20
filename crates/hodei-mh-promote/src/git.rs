//! Git promotion adapter (GC-10): integrates a validated candidate bundle
//! into the project's managed LOCAL branch with a native compare-and-swap on
//! the base ref (`git update-ref <ref> <new> <old>` is atomic). Everything
//! is plumbing — rev-parse, fetch into our own ref namespace, update-ref —
//! so the user's checkout and worktree are never touched. No push, no merge
//! into other branches: promotion is local by design.

use std::path::Path;
use std::process::Command;

use thiserror::Error;

/// Ref namespace reserved for hodei-mh scratch refs (never the user's).
pub const SCRATCH_REFS_PREFIX: &str = "refs/hodei-mh/";

const ZERO_REF: &str = "0000000000000000000000000000000000000000";

#[derive(Debug, Error, PartialEq)]
pub enum PromoteError {
    /// The managed branch moved since the candidate was verified: the flow
    /// must revalidate against the new state before promoting.
    #[error("managed branch moved since verification (expected {expected}, current {current})")]
    BaseMoved { expected: String, current: String },
    /// The host refused the update of the managed branch: the ref did not
    /// move, so this is a policy refusal, not a concurrent move.
    #[error(
        "the host refused the branch update (git: {detail}): the candidate or its message violates the repository policy; nothing was moved"
    )]
    RefRejected { detail: String },
    /// The candidate adds nothing on top of the base.
    #[error("candidate is already integrated at {0}")]
    AlreadyIntegrated(String),
    /// The candidate is not a descendant of the verified base.
    #[error("candidate is not a fast-forward over the verified base")]
    NotFastForward,
    #[error("git {context} failed: {detail}")]
    Git { context: String, detail: String },
    /// The candidate has no evidence that speaks for it.
    #[error("the candidate was not verified: {detail}")]
    Unverified { detail: String },
}

fn run_git(repo: &Path, args: &[&str]) -> Result<(bool, String, String), PromoteError> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .map_err(|e| PromoteError::Git {
            context: args.first().copied().unwrap_or("?").to_string(),
            detail: e.to_string(),
        })?;
    Ok((
        output.status.success(),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    ))
}

fn must(repo: &Path, context: &str, args: &[&str]) -> Result<String, PromoteError> {
    let (ok, out, err) = run_git(repo, args)?;
    if !ok {
        return Err(PromoteError::Git {
            context: context.to_string(),
            detail: err.trim().to_string(),
        });
    }
    Ok(out)
}

/// Current commit of a local branch; `None` when the branch does not exist.
pub fn current_ref(repo: &Path, branch: &str) -> Result<Option<String>, PromoteError> {
    let reference = format!("refs/heads/{branch}");
    let (ok, out, err) = run_git(repo, &["rev-parse", "--verify", "--quiet", &reference])?;
    if ok {
        return Ok(Some(out.trim().to_string()));
    }
    // `--quiet` reports an unknown revision as a failure with no message.
    // A failure WITH a message is git being unable to answer at all — not a
    // repository, an unreadable directory — and it is not a missing branch.
    let detail = err.trim();
    if detail.is_empty() {
        return Ok(None);
    }
    Err(PromoteError::Git {
        context: format!("rev-parse {reference}"),
        detail: detail.to_string(),
    })
}

/// Candidate head read straight from the bundle (no fetch needed).
pub fn candidate_head(bundle: &Path, candidate_branch: &str) -> Result<String, PromoteError> {
    let (ok, out, err) = run_git(
        Path::new("."),
        &["bundle", "list-heads", &bundle.display().to_string()],
    )?;
    if !ok {
        return Err(PromoteError::Git {
            context: "bundle list-heads".into(),
            detail: err.trim().to_string(),
        });
    }
    let wanted = format!("refs/heads/{candidate_branch}");
    for line in out.lines() {
        let mut parts = line.split_whitespace();
        let Some(sha) = parts.next() else {
            continue;
        };
        if parts.next() == Some(wanted.as_str()) {
            return Ok(sha.to_string());
        }
    }
    Err(PromoteError::Git {
        context: "bundle list-heads".into(),
        detail: format!("branch {candidate_branch} not present in bundle"),
    })
}

/// Promote the candidate into the managed branch as a fast-forward, guarded
/// by a native CAS on the verified base. Returns the candidate head commit.
///
/// UNCHECKED: this function knows nothing about evidence. It moves a
/// candidate that was never verified, and it is named after that fact so
/// the call site cannot hide it. The product path is `promote_verified`,
/// which demands the evidence of the candidate before calling this.
pub fn promote_ff_only_unchecked(
    repo: &Path,
    managed_branch: &str,
    bundle: &Path,
    candidate_branch: &str,
    expected_base: &str,
) -> Result<String, PromoteError> {
    let head = candidate_head(bundle, candidate_branch)?;

    // Early, friendly check (the atomic guard is update-ref's old-value).
    let current = current_ref(repo, managed_branch)?;
    if let Some(current) = &current {
        if current != expected_base {
            return Err(PromoteError::BaseMoved {
                expected: expected_base.to_string(),
                current: current.clone(),
            });
        }
        if current == &head {
            return Err(PromoteError::AlreadyIntegrated(head));
        }
    }

    // Fetch the candidate into OUR namespace (forced so repeat promotions
    // overwrite the scratch ref); the user's refs are untouched.
    let scratch = format!("{SCRATCH_REFS_PREFIX}candidate/{candidate_branch}");
    let refspec = format!("+refs/heads/{candidate_branch}:{scratch}");
    let _ = run_git(
        repo,
        &["fetch", "--quiet", &bundle.display().to_string(), &refspec],
    )?;
    let fetched = must(
        repo,
        "resolve candidate",
        &["rev-parse", "--verify", &scratch],
    )?
    .trim()
    .to_string();
    if fetched != head {
        return Err(PromoteError::Git {
            context: "fetch candidate".into(),
            detail: format!("bundle head {head} but fetched {fetched}"),
        });
    }

    // Fast-forward requirement: the candidate descends from the base (or
    // from zero when creating the managed branch fresh).
    let ancestor_base = current.as_deref().unwrap_or(expected_base);
    let (is_ancestor, _, _) =
        run_git(repo, &["merge-base", "--is-ancestor", ancestor_base, &head])?;
    if !is_ancestor {
        return Err(PromoteError::NotFastForward);
    }

    // Native atomic CAS: fails when the branch is no longer at expected_base.
    let reference = format!("refs/heads/{managed_branch}");
    let old = current.clone().unwrap_or_else(|| ZERO_REF.to_string());
    let (ok, _, err) = run_git(repo, &["update-ref", &reference, &head, &old])?;
    if !ok {
        // A failed update is not proof of a concurrent move: a host hook
        // may have refused exactly this update. The re-read separates the
        // two facts instead of guessing one.
        let now = current_ref(repo, managed_branch)?;
        if now == current {
            let trimmed = err.trim();
            let detail = if trimmed.is_empty() {
                "the refusal carried no message".to_string()
            } else {
                trimmed.to_string()
            };
            return Err(PromoteError::RefRejected { detail });
        }
        return Err(PromoteError::BaseMoved {
            expected: expected_base.to_string(),
            current: now.unwrap_or_else(|| "?".into()),
        });
    }
    Ok(head)
}

/// Promote the candidate into the managed branch, but only with the
/// evidence that speaks for it: the same work, the same commit and the very
/// bundle that was verified. The facts are read from the bundle itself, at
/// the boundary where the candidate is promoted. The CAS is unchanged.
pub fn promote_verified(
    repo: &Path,
    managed_branch: &str,
    bundle: &Path,
    candidate_branch: &str,
    expected_base: &str,
    evidence: &verify::evidence::EvidenceSet,
    work_ref: &str,
) -> Result<String, PromoteError> {
    let candidate = bundle_candidate(bundle, candidate_branch, work_ref)?;
    crate::evidence::require_evidence(evidence, &candidate).map_err(|error| {
        PromoteError::Unverified {
            detail: error.to_string(),
        }
    })?;
    promote_ff_only_unchecked(
        repo,
        managed_branch,
        bundle,
        candidate_branch,
        expected_base,
    )
}

/// The facts of the candidate, read from the bundle at the promotion
/// boundary: the head it names and the digest of its bytes.
fn bundle_candidate(
    bundle: &Path,
    candidate_branch: &str,
    work_ref: &str,
) -> Result<crate::evidence::PromotionCandidate, PromoteError> {
    let head = candidate_head(bundle, candidate_branch)?;
    let revision = verify::git::GitRevision::parse(&head).map_err(|error| PromoteError::Git {
        context: "bundle head".into(),
        detail: error.to_string(),
    })?;
    let bytes = std::fs::read(bundle).map_err(|error| PromoteError::Git {
        context: "read bundle".into(),
        detail: error.to_string(),
    })?;
    Ok(crate::evidence::PromotionCandidate {
        work_ref: work_ref.to_string(),
        revision,
        transport_digest: verify::transport::TransportDigest::of(&bytes),
    })
}

/// Re-read the managed branch: the crash-recovery verification compares the
/// persisted commit against the live ref.
pub fn ensure_ref(repo: &Path, branch: &str, expected: &str) -> Result<bool, PromoteError> {
    Ok(current_ref(repo, branch)?.as_deref() == Some(expected))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hodei-promote-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Build a commit in the fixture repository with git's PLUMBING
    /// commands (`write-tree` + `commit-tree` + `update-ref`).
    ///
    /// The fixture does not ask the host's git for a commit: it builds a
    /// synthetic history in a scratch directory with no remote that is
    /// never pushed. Plumbing keeps the fixture independent of the
    /// host's commit governance, which governs the user's repositories.
    /// The parent is explicit, so the history is a real chain and
    /// "base..head" is a real revision range.
    fn commit_file(repo: &Path, name: &str, message: &str) -> String {
        std::fs::write(repo.join(name), format!("content of {name}\n")).unwrap();
        must(repo, "add", &["add", name]).unwrap();
        let tree = must(repo, "write-tree", &["write-tree"])
            .unwrap()
            .trim()
            .to_string();
        let (has_head, head, _err) =
            run_git(repo, &["rev-parse", "--verify", "--quiet", "HEAD"]).unwrap();
        let head = if has_head {
            head.trim().to_string()
        } else {
            String::new()
        };
        let mut args: Vec<&str> = vec!["commit-tree", tree.as_str(), "-m", message];
        if !head.is_empty() {
            args.push("-p");
            args.push(&head);
        }
        let commit = must(repo, "commit-tree", &args).unwrap().trim().to_string();
        let branch = must(repo, "symbolic-ref", &["symbolic-ref", "--quiet", "HEAD"])
            .unwrap()
            .trim()
            .to_string();
        must(repo, "update-ref", &["update-ref", &branch, &commit]).unwrap();
        commit
    }

    /// Initialise the fixture repository OUT of the host's git
    /// governance: the host may install hooks globally (through the
    /// configuration itself), and those hooks would judge a scratch
    /// repository that is never pushed. Every inherited hook is
    /// disabled, by name, in the repository's OWN configuration: the
    /// promotion under test is free of them and nothing outside the
    /// fixture changes.
    fn init_repo(repo: &Path) {
        must(repo, "init", &["init", "-q", "-b", "main"]).unwrap();
        let hooks = repo.join(".git/hodei-no-hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        must(
            repo,
            "config hooksPath",
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        )
        .unwrap();
        let inherited = run_git(
            repo,
            &["config", "--global", "--get-regexp", r"^hook\..*\.command$"],
        )
        .map(|(_, out, _)| out)
        .unwrap_or_default();
        for line in inherited.lines() {
            let Some((key, _command)) = line.split_once(' ') else {
                continue;
            };
            must(
                repo,
                "disable inherited hook",
                &["config", "--local", key, "true"],
            )
            .unwrap();
        }
    }

    #[test]
    fn a_git_that_cannot_answer_is_not_a_missing_branch() {
        // An unknown revision is a missing branch...
        let repo = temp_dir("missing-ref");
        init_repo(&repo);
        assert_eq!(current_ref(&repo, "hodei-mh/main").unwrap(), None);
        // ...but a directory git cannot even read is git's own failure,
        // never reported as a branch that is not there.
        let not_a_repository = temp_dir("not-a-repository");
        match current_ref(&not_a_repository, "hodei-mh/main") {
            Err(PromoteError::Git { detail, .. }) => assert!(
                detail.contains("not a git repository") || detail.contains("no es un repositorio"),
                "the underlying failure is surfaced: {detail}"
            ),
            other => panic!("expected the underlying git failure, got {other:?}"),
        }
    }

    #[test]
    fn promotion_moves_managed_branch_and_never_touches_user_branch() {
        let repo = temp_dir("promote");
        init_repo(&repo);
        let base = commit_file(&repo, "a.txt", "base");

        // Candidate branch in a bundle: one commit on top of base.
        must(&repo, "checkout", &["checkout", "-q", "-b", "candidate"]).unwrap();
        let head = commit_file(&repo, "b.txt", "candidate work");
        let bundle = repo.join("candidate.bundle");
        must(
            &repo,
            "bundle",
            &["bundle", "create", &bundle.display().to_string(), "--all"],
        )
        .unwrap();

        // User checkout moves away; managed branch does not exist yet.
        must(&repo, "checkout", &["checkout", "-q", "main"]).unwrap();
        must(&repo, "branch", &["branch", "-q", "hodei-mh/main", &base]).unwrap();

        let promoted =
            promote_ff_only_unchecked(&repo, "hodei-mh/main", &bundle, "candidate", &base).unwrap();
        assert_eq!(promoted, head);
        assert_eq!(
            current_ref(&repo, "hodei-mh/main").unwrap().as_deref(),
            Some(head.as_str())
        );
        // The user's checkout branch is untouched.
        assert_eq!(
            current_ref(&repo, "main").unwrap().as_deref(),
            Some(base.as_str())
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn moved_base_is_rejected_without_touching_anything() {
        let repo = temp_dir("moved");
        init_repo(&repo);
        let base = commit_file(&repo, "a.txt", "base");

        must(&repo, "checkout", &["checkout", "-q", "-b", "candidate"]).unwrap();
        let _ = commit_file(&repo, "b.txt", "candidate work");
        let bundle = repo.join("candidate.bundle");
        must(
            &repo,
            "bundle",
            &["bundle", "create", &bundle.display().to_string(), "--all"],
        )
        .unwrap();

        must(&repo, "checkout", &["checkout", "-q", "main"]).unwrap();
        must(&repo, "branch", &["branch", "-q", "hodei-mh/main", &base]).unwrap();
        // A human commit lands ON THE MANAGED BRANCH after verification.
        must(&repo, "checkout", &["checkout", "-q", "hodei-mh/main"]).unwrap();
        let human = commit_file(&repo, "c.txt", "human change on managed");
        must(&repo, "checkout", &["checkout", "-q", "main"]).unwrap();

        let err = promote_ff_only_unchecked(&repo, "hodei-mh/main", &bundle, "candidate", &base)
            .unwrap_err();
        match err {
            PromoteError::BaseMoved { current, .. } => assert_eq!(current, human),
            other => panic!("expected BaseMoved, got {other:?}"),
        }
        // The human change is intact.
        assert_eq!(
            current_ref(&repo, "hodei-mh/main").unwrap().as_deref(),
            Some(human.as_str())
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// Install a `reference-transaction` hook in the fixture's OWN hooks
    /// path that refuses only the managed-branch update: the scratch refs
    /// the promotion fetches keep passing.
    #[cfg(unix)]
    fn refuse_managed_updates(repo: &Path, message: &str) {
        use std::os::unix::fs::PermissionsExt;

        let hook = repo.join(".git/hodei-no-hooks/reference-transaction");
        let script = format!(
            "#!/bin/sh\n[ \"$1\" = prepared ] || exit 0\nwhile read -r old new ref; do\n  if [ \"$ref\" = refs/heads/hodei-mh/main ]; then\n    echo '{message}' >&2\n    exit 1\n  fi\ndone\nexit 0\n"
        );
        std::fs::write(&hook, script).unwrap();
        let mut permissions = std::fs::metadata(&hook).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&hook, permissions).unwrap();
    }

    #[test]
    #[cfg(unix)]
    fn a_refused_update_is_reported_as_rejected_and_not_as_a_moved_base() {
        let repo = temp_dir("refused");
        init_repo(&repo);
        let base = commit_file(&repo, "a.txt", "base");

        must(&repo, "checkout", &["checkout", "-q", "-b", "candidate"]).unwrap();
        let _ = commit_file(&repo, "b.txt", "candidate work");
        let bundle = repo.join("candidate.bundle");
        must(
            &repo,
            "bundle",
            &["bundle", "create", &bundle.display().to_string(), "--all"],
        )
        .unwrap();

        must(&repo, "checkout", &["checkout", "-q", "main"]).unwrap();
        must(&repo, "branch", &["branch", "-q", "hodei-mh/main", &base]).unwrap();
        let policy = "policy: the managed branch accepts no automated update";
        refuse_managed_updates(&repo, policy);

        let err = promote_ff_only_unchecked(&repo, "hodei-mh/main", &bundle, "candidate", &base)
            .unwrap_err();
        match &err {
            PromoteError::RefRejected { detail } => {
                assert!(
                    detail.contains(policy),
                    "the refusal carries the hook message: {detail}"
                );
                assert!(
                    err.to_string().contains("nothing was moved"),
                    "the refusal is honest about the untouched ref"
                );
            }
            other => panic!("expected RefRejected, got {other:?}"),
        }
        // The ref never moved: nothing was promoted.
        assert_eq!(
            current_ref(&repo, "hodei-mh/main").unwrap().as_deref(),
            Some(base.as_str())
        );
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn non_descendant_candidate_is_not_a_fast_forward() {
        let repo = temp_dir("nff");
        init_repo(&repo);
        let _base = commit_file(&repo, "a.txt", "base");
        // Candidate diverges from an earlier commit: not a descendant.
        must(
            &repo,
            "checkout",
            &["checkout", "-q", "--orphan", "candidate"],
        )
        .unwrap();
        must(&repo, "reset", &["reset", "--hard"]).unwrap();
        let _head = commit_file(&repo, "x.txt", "orphan candidate");
        let bundle = repo.join("candidate.bundle");
        must(
            &repo,
            "bundle",
            &["bundle", "create", &bundle.display().to_string(), "--all"],
        )
        .unwrap();
        must(&repo, "checkout", &["checkout", "-q", "main"]).unwrap();
        let base = must(&repo, "rev-parse", &["rev-parse", "main"])
            .unwrap()
            .trim()
            .to_string();

        let err = promote_ff_only_unchecked(&repo, "hodei-mh/main", &bundle, "candidate", &base)
            .unwrap_err();
        assert_eq!(err, PromoteError::NotFastForward);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
