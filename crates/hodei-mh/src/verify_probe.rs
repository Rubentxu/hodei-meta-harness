//! `hodei-mh verify-probe`: TASK-9 vertical slice — real PASS/FAIL/NOT_RUN
//! evidence and candidate-change invalidation, demonstrated on an example
//! repository whose dev environment hides a failure the clean verifier
//! exposes.
//!
//! The verification is bound to the candidate that actually ran: the
//! transport, the commit and the content are checked before any check
//! runs, and the promotion demands the evidence of that candidate.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use hodei_mh_workspace::git as workspace_git;
use opensandbox::cleanup::CleanupRegistry;
use promote::git as promote_git;
use verify::evidence::{CheckBinding, EvidenceSet};
use verify::git::{GitRevision, GitTree, Progress, tree_progress};
use verify::policy::{explicit, infer_from_instructions};
use verify::runner::{Candidate, Check, CleanRunner};
use verify::transport::TransportDigest;

struct ProbeCheck {
    name: &'static str,
    passed: bool,
    detail: String,
}

struct ExampleRepo {
    root: PathBuf,
    /// The bundle of the candidate under verification.
    bundle: PathBuf,
    /// A valid bundle of another commit: the candidate the evidence must
    /// refuse to promote.
    base_bundle: PathBuf,
    /// The commit the candidate is promoted over.
    base_revision: GitRevision,
    /// The commit of the candidate.
    revision: GitRevision,
    /// The content of the candidate, as git names it.
    tree: GitTree,
}

fn run_git(repo: &PathBuf, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The output of a git command, as git wrote it.
fn git_output(repo: &PathBuf, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn write_file(path: &PathBuf, content: &str) -> bool {
    std::fs::write(path, content).is_ok()
}

fn read_file(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
}

fn bundle_file(repo: &PathBuf, bundle: &PathBuf) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["bundle", "create"])
        .arg(bundle)
        .arg("--all")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false)
}

/// The example candidate: `test.sh` passes ONLY when an untracked,
/// gitignored `fixture.local` is present — exactly the residue a dev
/// environment accumulates and a clean checkout never has.
fn make_example_repo() -> Option<ExampleRepo> {
    let root = std::env::temp_dir().join(format!("hodei-verify-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).ok()?;
    let bundle =
        std::env::temp_dir().join(format!("hodei-verify-bundle-{}.bundle", std::process::id()));
    let base_bundle = std::env::temp_dir().join(format!(
        "hodei-verify-base-bundle-{}.bundle",
        std::process::id()
    ));

    if !write_file(
        &root.join("test.sh"),
        "#!/bin/sh\nexec grep -q dev-override fixture.local\n",
    ) {
        return None;
    }
    let _ = Command::new("chmod")
        .arg("+x")
        .arg(root.join("test.sh"))
        .output();
    if !write_file(&root.join(".gitignore"), "fixture.local\n") {
        return None;
    }
    if workspace_git::init_fixture_repo(&root).is_err() {
        return None;
    }
    // The fixture commits with git's PLUMBING: automation must not
    // depend on the host's `git commit` path, which can be governed by
    // a policy that refuses repositories it does not classify.
    if !run_git(&root, &["add", "-A"]) {
        return None;
    }
    let base_commit = workspace_git::commit_index(&root, "base").ok()?;
    let base_revision = GitRevision::parse(&base_commit).ok()?;
    // A valid bundle of ANOTHER commit: the promotion must refuse it.
    if !bundle_file(&root, &base_bundle) {
        return None;
    }

    // The candidate commit: the same check, marked as the candidate.
    if !write_file(
        &root.join("test.sh"),
        "#!/bin/sh\n# candidate\nexec grep -q dev-override fixture.local\n",
    ) {
        return None;
    }
    if !run_git(&root, &["add", "-A"]) {
        return None;
    }
    let candidate_commit = workspace_git::commit_index(&root, "candidate").ok()?;
    let revision = GitRevision::parse(&candidate_commit).ok()?;
    let tree = GitTree::parse(&git_output(&root, &["rev-parse", "HEAD^{tree}"])?).ok()?;
    if !bundle_file(&root, &bundle) {
        return None;
    }
    Some(ExampleRepo {
        root,
        bundle,
        base_bundle,
        base_revision,
        revision,
        tree,
    })
}

/// The dev environment: same repo, but with the untracked residue present.
fn dev_environment_passes(repo: &PathBuf) -> (bool, String) {
    let _ = write_file(&repo.join("fixture.local"), "dev-override\n");
    let output = Command::new("sh")
        .arg("test.sh")
        .current_dir(repo)
        .output()
        .expect("run dev test.sh");
    let _ = std::fs::remove_file(repo.join("fixture.local"));
    (
        output.status.success(),
        format!("dev test.sh exit={}", output.status.code().unwrap_or(-1)),
    )
}

pub async fn run(server: &str, image: &str) -> i32 {
    let started = Instant::now();
    let mut checks: Vec<ProbeCheck> = Vec::new();
    let finish = |checks: &[ProbeCheck], elapsed: Duration| -> i32 {
        let summary = serde_json::json!({
            "task": "TASK-9",
            "checks": checks.iter().map(|c| serde_json::json!({
                "name": c.name, "passed": c.passed, "detail": c.detail,
            })).collect::<Vec<_>>(),
            "all_passed": checks.iter().all(|c| c.passed),
            "elapsed_seconds": elapsed.as_secs_f64(),
        });
        println!(
            "{}",
            serde_json::to_string_pretty(&summary).unwrap_or_default()
        );
        if checks.iter().all(|c| c.passed) {
            0
        } else {
            1
        }
    };

    // Example repository with a contaminated-dev trap.
    let Some(repo) = make_example_repo() else {
        checks.push(ProbeCheck {
            name: "example-repo-setup",
            passed: false,
            detail: "could not create example repo".into(),
        });
        return finish(&checks, started.elapsed());
    };
    checks.push(ProbeCheck {
        name: "example-repo-setup",
        passed: true,
        detail: format!(
            "repo {:?} revision {}.. tree {}..",
            repo.root.display(),
            &repo.revision.as_str()[..12],
            &repo.tree.as_str()[..12]
        ),
    });

    // AC-4, half one: the DEV environment passes (contaminated).
    let (dev_pass, dev_detail) = dev_environment_passes(&repo.root);
    checks.push(ProbeCheck {
        name: "dev-environment-passes-contaminated",
        passed: dev_pass,
        detail: dev_detail,
    });

    let runner = CleanRunner {
        lifecycle_url: server.to_string(),
        image: image.to_string(),
        journal: CleanupRegistry::default_journal_path(),
        timeout: Duration::from_secs(3),
    };
    let bundle_bytes = read_file(&repo.bundle);
    let candidate = Candidate {
        revision: repo.revision.clone(),
        tree: repo.tree.clone(),
        transport_digest: TransportDigest::of(&bundle_bytes),
        bundle: bundle_bytes,
    };
    let checks_config = vec![
        Check {
            id: "clean-check".into(),
            command: "./test.sh".into(),
        },
        Check {
            id: "timeout-check".into(),
            command: "sleep 10".into(),
        },
        Check {
            id: "pass-check".into(),
            command: "echo hodei-verify-ok".into(),
        },
    ];

    // The candidate that claims another commit is rejected BEFORE the
    // checks: the check that would have run takes a minute, and the
    // rejection does not wait for it.
    let wrong = Candidate {
        revision: GitRevision::parse("2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5")
            .expect("a revision"),
        tree: repo.tree.clone(),
        transport_digest: candidate.transport_digest.clone(),
        bundle: candidate.bundle.clone(),
    };
    let wrong_started = Instant::now();
    let wrong_result = runner
        .verify(
            "TASK-9",
            &wrong,
            &[Check {
                id: "slow-check".into(),
                command: "sleep 60".into(),
            }],
        )
        .await;
    let wrong_elapsed = wrong_started.elapsed();
    checks.push(ProbeCheck {
        name: "a-candidate-of-another-commit-is-rejected-before-the-checks",
        passed: wrong_result.is_err() && wrong_elapsed < Duration::from_secs(30),
        detail: format!(
            "rejected={} in {:.1}s without running the check",
            wrong_result.is_err(),
            wrong_elapsed.as_secs_f64()
        ),
    });

    // Clean verification: fresh sandbox, bundle clone, per-check runs.
    let mut set: EvidenceSet = match runner.verify("TASK-9", &candidate, &checks_config).await {
        Ok(set) => set,
        Err(err) => {
            checks.push(ProbeCheck {
                name: "clean-verifier-runs",
                passed: false,
                detail: err.to_string(),
            });
            let _ = std::fs::remove_dir_all(&repo.root);
            let _ = std::fs::remove_file(&repo.bundle);
            let _ = std::fs::remove_file(&repo.base_bundle);
            return finish(&checks, started.elapsed());
        }
    };
    checks.push(ProbeCheck {
        name: "clean-verifier-runs",
        passed: true,
        detail: format!(
            "verified revision {}.. tree {}..",
            &repo.revision.as_str()[..12],
            &repo.tree.as_str()[..12]
        ),
    });

    // The clean verifier really RAN the command and saw it FAIL: the
    // dev environment passed the same command only because of the
    // residue it accumulated.
    let clean_verdict = set.effective_verdict("clean-check");
    checks.push(ProbeCheck {
        name: "clean-verifier-fails-uncontaminated",
        passed: clean_verdict == verify::result::Verdict::Fail,
        detail: format!("clean-check verdict={clean_verdict:?} (dev passed the same command)"),
    });

    // AC-1: a timeout produced an ABSENT record — never a PASS.
    let timeout_entry = set
        .entries
        .iter()
        .find(|e| e.result.check_id == "timeout-check");
    let absent_ok = timeout_entry
        .map(|e| {
            e.result.record.exit_code.is_none()
                && set.effective_verdict("timeout-check") == verify::result::Verdict::NotRun
        })
        .unwrap_or(false);
    checks.push(ProbeCheck {
        name: "absent-result-never-pass",
        passed: absent_ok,
        detail: format!(
            "timeout-check exit={:?}",
            timeout_entry.and_then(|e| e.result.record.exit_code)
        ),
    });

    // The evidence speaks for the candidate that actually ran.
    let bound_ok = set
        .entries
        .iter()
        .all(|e| e.result.tree == repo.tree && !e.result.environment.is_empty());
    checks.push(ProbeCheck {
        name: "real-pass-bound-to-the-content-and-the-environment",
        passed: set.effective_verdict("pass-check") == verify::result::Verdict::Pass && bound_ok,
        detail: format!(
            "pass-check verdict={:?}; every entry carries the content and the environment",
            set.effective_verdict("pass-check")
        ),
    });

    // AC-3: checks are individual — one pass does not satisfy the others.
    let individual_ok = !set.all_checks_satisfied(&["clean-check", "timeout-check", "pass-check"])
        && !set.check_satisfied("clean-check")
        && !set.check_satisfied("timeout-check")
        && set.check_satisfied("pass-check");
    checks.push(ProbeCheck {
        name: "checks-verified-individually",
        passed: individual_ok,
        detail: "pass-check satisfied alone; FAIL and NOT_RUN satisfied nothing".into(),
    });

    // The promotion is CLOSED while the policy is not satisfied: the
    // evidence of a candidate whose check failed moves nothing.
    let unsatisfied = promote_git::promote_verified(
        &repo.root,
        "hodei-mh/main",
        &repo.bundle,
        "main",
        repo.base_revision.as_str(),
        &set,
        "TASK-9",
    );
    checks.push(ProbeCheck {
        name: "a-candidate-with-a-failed-check-is-not-promoted",
        passed: unsatisfied.is_err(),
        detail: match &unsatisfied {
            Ok(commit) => format!("promoted {commit}.. with an unsatisfied check"),
            Err(err) => err.to_string(),
        },
    });

    // A valid bundle of ANOTHER commit is not promoted with this evidence.
    let other_bundle = promote_git::promote_verified(
        &repo.root,
        "hodei-mh/main",
        &repo.base_bundle,
        "main",
        repo.base_revision.as_str(),
        &set,
        "TASK-9",
    );
    checks.push(ProbeCheck {
        name: "a-bundle-of-another-commit-is-not-promoted",
        passed: other_bundle.is_err(),
        detail: match &other_bundle {
            Ok(commit) => format!("promoted {commit}.. with the wrong evidence"),
            Err(err) => err.to_string(),
        },
    });

    // The evidence of a policy that PASSES unlocks the promotion. This is
    // the real path, without an agent in the loop: the same runner, the
    // same sandbox lifecycle, the same promotion.
    let promotable = match runner
        .verify(
            "TASK-9",
            &candidate,
            &[Check {
                id: "pass-check".into(),
                command: "echo hodei-verify-ok".into(),
            }],
        )
        .await
    {
        Ok(set) => set,
        Err(err) => {
            checks.push(ProbeCheck {
                name: "the-evidence-of-a-satisfied-policy-is-produced",
                passed: false,
                detail: err.to_string(),
            });
            let _ = std::fs::remove_dir_all(&repo.root);
            let _ = std::fs::remove_file(&repo.bundle);
            let _ = std::fs::remove_file(&repo.base_bundle);
            return finish(&checks, started.elapsed());
        }
    };
    checks.push(ProbeCheck {
        name: "the-evidence-of-a-satisfied-policy-is-produced",
        passed: promotable.check_satisfied("pass-check"),
        detail: format!(
            "pass-check satisfied={}",
            promotable.check_satisfied("pass-check")
        ),
    });

    // The promotion demands the evidence of the candidate it promotes.
    let promoted = promote_git::promote_verified(
        &repo.root,
        "hodei-mh/main",
        &repo.bundle,
        "main",
        repo.base_revision.as_str(),
        &promotable,
        "TASK-9",
    );
    checks.push(ProbeCheck {
        name: "the-promotion-demands-the-evidence-of-the-candidate",
        passed: promoted.is_ok(),
        detail: match &promoted {
            Ok(commit) => format!("promoted {commit}.."),
            Err(err) => err.to_string(),
        },
    });

    // The evidence of another policy does not promote either.
    let mut other_policy = promotable.binding.clone();
    other_policy.required_checks = vec![CheckBinding {
        id: "clean-check".into(),
        command: "make check".into(),
    }];
    let mut stale = promotable.clone();
    let invalidated = stale.binding_changed(&other_policy);
    let stale_promotion = promote_git::promote_verified(
        &repo.root,
        "hodei-mh/main",
        &repo.bundle,
        "main",
        repo.base_revision.as_str(),
        &stale,
        "TASK-9",
    );
    checks.push(ProbeCheck {
        name: "evidence-of-another-policy-is-not-promoted",
        passed: invalidated > 0 && stale_promotion.is_err(),
        detail: format!(
            "invalidated={invalidated}; promotion refused={}; reason={}",
            stale_promotion.is_err(),
            stale_promotion
                .err()
                .map(|error| error.to_string())
                .unwrap_or_default()
        ),
    });

    // AC-2: a real content change invalidates the bound evidence.
    let _ = write_file(
        &repo.root.join("test.sh"),
        "#!/bin/sh\n# changed\nexec grep -q dev-override fixture.local\n",
    );
    let changed = run_git(&repo.root, &["add", "-A"])
        && workspace_git::commit_index(&repo.root, "change candidate").is_ok();
    let new_tree = git_output(&repo.root, &["rev-parse", "HEAD^{tree}"])
        .and_then(|tree| GitTree::parse(&tree).ok());
    let invalidated = changed
        && new_tree
            .as_ref()
            .map(|tree| set.candidate_changed(tree) == 3)
            .unwrap_or(false);
    checks.push(ProbeCheck {
        name: "candidate-change-invalidates-evidence",
        passed: invalidated
            && new_tree
                .as_ref()
                .map(|tree| set.binding.tree != *tree)
                .unwrap_or(false),
        detail: format!(
            "commit ok={changed}; invalidated=3; the binding still names the content that ran; now satisfied={}",
            set.all_checks_satisfied(&["pass-check"])
        ),
    });

    // AC-3: an empty commit (same tree) does not credit progress.
    let empty = workspace_git::commit_index(&repo.root, "empty").is_ok();
    let after_empty = git_output(&repo.root, &["rev-parse", "HEAD^{tree}"])
        .and_then(|tree| GitTree::parse(&tree).ok());
    checks.push(ProbeCheck {
        name: "empty-commit-not-progress",
        passed: empty
            && new_tree
                .as_ref()
                .zip(after_empty.as_ref())
                .map(|(before, after)| tree_progress(before, after) == Progress::NoChange)
                .unwrap_or(false),
        detail: format!(
            "empty commit left the content unchanged={}",
            new_tree == after_empty
        ),
    });

    // Policy inference: proposal documented, never passed off as configured.
    let proposal = infer_from_instructions("Run cargo test before merge; keep clippy clean.");
    let configured = explicit(vec!["cargo test --workspace".into()]);
    let policy_ok = !proposal.policy.commands.is_empty()
        && proposal.policy.inferred
        && !configured.inferred
        && infer_from_instructions("nothing to see here")
            .policy
            .commands
            .is_empty();
    checks.push(ProbeCheck {
        name: "policy-inference-documented",
        passed: policy_ok,
        detail: format!(
            "inferred commands={:?}; rationale={:?}",
            proposal.policy.commands,
            infer_from_instructions("no commands").rationale
        ),
    });

    // Cleanup: every artifact is removed, even if one of them resists.
    let mut cleaned = true;
    for removed in [
        std::fs::remove_dir_all(&repo.root).is_ok(),
        std::fs::remove_file(&repo.bundle).is_ok(),
        std::fs::remove_file(&repo.base_bundle).is_ok(),
    ] {
        cleaned &= removed;
    }
    checks.push(ProbeCheck {
        name: "cleanup",
        passed: cleaned,
        detail: "example repo and bundles removed".into(),
    });

    finish(&checks, started.elapsed())
}
