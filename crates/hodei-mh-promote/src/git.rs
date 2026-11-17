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
    /// The candidate adds nothing on top of the base.
    #[error("candidate is already integrated at {0}")]
    AlreadyIntegrated(String),
    /// The candidate is not a descendant of the verified base.
    #[error("candidate is not a fast-forward over the verified base")]
    NotFastForward,
    #[error("git {context} failed: {detail}")]
    Git { context: String, detail: String },
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
    let (ok, out, _err) = run_git(repo, &["rev-parse", "--verify", "--quiet", &reference])?;
    if !ok {
        return Ok(None);
    }
    Ok(Some(out.trim().to_string()))
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
pub fn promote_ff_only(
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
    let old = current.unwrap_or_else(|| ZERO_REF.to_string());
    let (ok, _, _err) = run_git(repo, &["update-ref", &reference, &head, &old])?;
    if !ok {
        let now = current_ref(repo, managed_branch)?.unwrap_or_else(|| "?".into());
        return Err(PromoteError::BaseMoved {
            expected: expected_base.to_string(),
            current: now,
        });
    }
    Ok(head)
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

    fn commit_file(repo: &Path, name: &str, message: &str) -> String {
        std::fs::write(repo.join(name), format!("content of {name}\n")).unwrap();
        must(repo, "add", &["add", name]).unwrap();
        must(repo, "commit", &["commit", "-q", "-m", message]).unwrap();
        must(repo, "rev-parse", &["rev-parse", "HEAD"])
            .unwrap()
            .trim()
            .to_string()
    }

    fn init_repo(repo: &Path) {
        must(repo, "init", &["init", "-q", "-b", "main"]).unwrap();
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
            promote_ff_only(&repo, "hodei-mh/main", &bundle, "candidate", &base).unwrap();
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

        let err = promote_ff_only(&repo, "hodei-mh/main", &bundle, "candidate", &base).unwrap_err();
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

        let err = promote_ff_only(&repo, "hodei-mh/main", &bundle, "candidate", &base).unwrap_err();
        assert_eq!(err, PromoteError::NotFastForward);
        let _ = std::fs::remove_dir_all(&repo);
    }
}
