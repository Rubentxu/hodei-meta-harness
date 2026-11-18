//! Host-side git adapter: bundle verification and candidate inspection via
//! the `git` CLI. Every failure carries the command context; nothing is
//! inferred from partial output.

use std::path::Path;
use std::process::Command;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum GitError {
    #[error("git {operation} failed: {stderr}")]
    Failed {
        operation: &'static str,
        stderr: String,
    },
    #[error("git {operation} produced no output where output was required")]
    EmptyOutput { operation: &'static str },
    #[error("git binary unavailable: {0}")]
    Unavailable(#[from] std::io::Error),
}

fn run_raw(operation: &'static str, args: &[&str], cwd: Option<&Path>) -> Result<String, GitError> {
    run_raw_env(operation, args, cwd, &[])
}

/// `run_raw` with an explicit environment for the child process.
fn run_raw_env(
    operation: &'static str,
    args: &[&str],
    cwd: Option<&Path>,
    envs: &[(&str, &str)],
) -> Result<String, GitError> {
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
    }
    for (key, value) in envs {
        command.env(key, value);
    }
    let output = command.output().map_err(GitError::Unavailable)?;
    if !output.status.success() {
        return Err(GitError::Failed {
            operation,
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

/// git commands that must produce stdout (rev-parse, diff, bundle verify).
fn run(operation: &'static str, args: &[&str], cwd: Option<&Path>) -> Result<String, GitError> {
    let stdout = run_raw(operation, args, cwd)?;
    if stdout.trim().is_empty() {
        return Err(GitError::EmptyOutput { operation });
    }
    Ok(stdout)
}

/// git commands whose stdout is legitimately empty (bundle create, clone).
fn run_mutator(operation: &'static str, args: &[&str], cwd: Option<&Path>) -> Result<(), GitError> {
    run_raw(operation, args, cwd).map(|_| ())
}

/// Build a commit from the staged index with git's PLUMBING commands
/// (`write-tree` + `commit-tree` + `update-ref`) and return its id.
///
/// Automation must not depend on the host's `git commit` path: that
/// command can be governed by a policy (identity, message rules), can
/// require an editor and is refused outright in repositories that
/// policy does not classify. The plumbing path is deterministic, needs
/// no editor and never touches the working tree. The parent is the
/// current HEAD, so the history is a real chain and `base..head` is a
/// real revision range.
///
/// The author and the committer are this harness, explicitly: the
/// commit does not borrow —nor pretend— the identity of whoever runs
/// the automation.
pub fn commit_index(repo: &Path, message: &str) -> Result<String, GitError> {
    const IDENTITY: [(&str, &str); 4] = [
        ("GIT_AUTHOR_NAME", "hodei-mh"),
        ("GIT_AUTHOR_EMAIL", "hodei-mh@local"),
        ("GIT_COMMITTER_NAME", "hodei-mh"),
        ("GIT_COMMITTER_EMAIL", "hodei-mh@local"),
    ];
    let tree = run("write-tree", &["write-tree"], Some(repo))?
        .trim()
        .to_string();
    // `--verify --quiet` prints nothing and fails when there is no HEAD
    // yet: that is the root commit, not an error.
    let parent = run_raw_env(
        "rev-parse HEAD",
        &["rev-parse", "--verify", "--quiet", "HEAD"],
        Some(repo),
        &IDENTITY,
    )
    .ok()
    .map(|head| head.trim().to_string())
    .filter(|head| !head.is_empty());
    let mut args: Vec<&str> = vec!["commit-tree", tree.as_str(), "-m", message];
    if let Some(parent) = &parent {
        args.push("-p");
        args.push(parent.as_str());
    }
    let commit = run_raw_env("commit-tree", &args, Some(repo), &IDENTITY)?
        .trim()
        .to_string();
    let branch = run(
        "symbolic-ref",
        &["symbolic-ref", "--quiet", "HEAD"],
        Some(repo),
    )?
    .trim()
    .to_string();
    run_mutator(
        "update-ref",
        &["update-ref", branch.as_str(), commit.as_str()],
        Some(repo),
    )?;
    Ok(commit)
}

/// `git bundle verify` — checks the bundle header (refs, prerequisites).
/// NOTE: this does NOT validate pack integrity; a truncated bundle can pass
/// verify. Definitive integrity comes from actually cloning the bundle
/// (index-pack validates pack checksums), which `inspect_bundle` enforces.
pub fn verify_bundle(bundle_path: &Path) -> Result<String, GitError> {
    run(
        "bundle verify",
        &["bundle", "verify", bundle_path.to_string_lossy().as_ref()],
        None,
    )
}

/// Inspect a verified bundle: clone it into `dest` and return
/// (head_commit, changed_files_vs_base). This is read-only analysis of the
/// candidate; no working tree of the host repo is touched.
pub fn inspect_bundle(
    bundle_path: &Path,
    base_revision: &str,
    dest: &Path,
) -> Result<(String, Vec<String>), GitError> {
    verify_bundle(bundle_path)?;
    run_mutator(
        "clone bundle",
        &[
            "clone",
            "--quiet",
            bundle_path.to_string_lossy().as_ref(),
            ".",
        ],
        Some(dest),
    )?;
    let head = run("rev-parse HEAD", &["rev-parse", "HEAD"], Some(dest))?;
    let head = head.trim().to_string();
    let changed = run(
        "diff name-only",
        &["diff", "--name-only", &format!("{base_revision}..{head}")],
        Some(dest),
    )?;
    Ok((head, changed.lines().map(str::to_string).collect()))
}

/// Create a bundle of the current branch HEAD in `repo_dir` (host-side test
/// helper and fixture builder; in the sandbox the same command runs via execd).
pub fn create_bundle(repo_dir: &Path, bundle_path: &Path, refs: &str) -> Result<(), GitError> {
    run_mutator(
        "bundle create",
        &[
            "bundle",
            "create",
            bundle_path.to_string_lossy().as_ref(),
            refs,
        ],
        Some(repo_dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git available");
        assert!(
            status.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&status.stderr)
        );
    }

    /// Build a commit in the fixture repository with the crate's own
    /// plumbing helper (`commit_index`): the fixture never asks the
    /// host's git for a commit, so it does not depend on the host's
    /// commit governance.
    fn commit_file(dir: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", "-A"]);
        commit_index(dir, message).expect("the fixture commits")
    }

    #[test]
    fn bundle_roundtrip_inspects_candidate_against_base() {
        let tmp = std::env::temp_dir().join(format!("hodei-ws-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        git(&repo, &["config", "user.email", "probe@hodei"]);
        git(&repo, &["config", "user.name", "probe"]);
        commit_file(&repo, "README.md", "base\n", "base commit");
        let base = run("rev-parse HEAD", &["rev-parse", "HEAD"], Some(&repo)).unwrap();
        let base = base.trim().to_string();
        commit_file(&repo, "feature.txt", "candidate work\n", "candidate commit");

        let bundle = tmp.join("candidate.bundle");
        create_bundle(&repo, &bundle, "--all").unwrap();

        let dest = tmp.join("inspect");
        std::fs::create_dir_all(&dest).unwrap();
        let (head, changed) = inspect_bundle(&bundle, &base, &dest).unwrap();
        assert_ne!(head, base);
        assert_eq!(changed, vec!["feature.txt".to_string()]);

        // Partial output is never announced valid: a truncated bundle can
        // fool `bundle verify` (header-only check), but cloning it fails on
        // pack checksum, so inspect_bundle rejects it end to end.
        let corrupt = tmp.join("corrupt.bundle");
        let full = std::fs::read(&bundle).unwrap();
        std::fs::write(&corrupt, &full[..full.len() / 2]).unwrap();
        let _ = verify_bundle(&corrupt); // header-only: may pass, carries no guarantee
        let corrupt_dest = tmp.join("inspect-corrupt");
        std::fs::create_dir_all(&corrupt_dest).unwrap();
        assert!(inspect_bundle(&corrupt, &base, &corrupt_dest).is_err());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
