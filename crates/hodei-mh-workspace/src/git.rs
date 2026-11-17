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
    let mut command = Command::new("git");
    command.args(args);
    if let Some(dir) = cwd {
        command.current_dir(dir);
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

    fn commit_file(dir: &Path, name: &str, content: &str, message: &str) {
        std::fs::write(dir.join(name), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", message]);
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
