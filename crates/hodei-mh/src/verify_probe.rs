//! `hodei-mh verify-probe`: TASK-9 vertical slice — real PASS/FAIL/NOT_RUN
//! evidence and candidate-change invalidation, demonstrated on an example
//! repository whose dev environment hides a failure the clean verifier
//! exposes.

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use hodei_mh_workspace::git as workspace_git;
use verify::digest::{CandidateDigest, ContentFile, Progress, candidate_progress};
use verify::evidence::EvidenceSet;
use verify::policy::{explicit, infer_from_instructions};
use verify::runner::{Candidate, Check, CleanRunner};

struct ProbeCheck {
    name: &'static str,
    passed: bool,
    detail: String,
}

struct ExampleRepo {
    root: PathBuf,
    bundle: PathBuf,
    digest: CandidateDigest,
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

fn write_file(path: &PathBuf, content: &str) -> bool {
    std::fs::write(path, content).is_ok()
}

fn read_file(path: &PathBuf) -> Vec<u8> {
    std::fs::read(path).unwrap_or_default()
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
    if !run_git(&root, &["add", "-A"]) {
        return None;
    }
    // The fixture commits with git's PLUMBING: automation must not
    // depend on the host's `git commit` path, which can be governed by a
    // policy that refuses repositories it does not classify.
    if workspace_git::commit_index(&root, "candidate").is_err() {
        return None;
    }
    let ok = Command::new("git")
        .arg("-C")
        .arg(&root)
        .args(["bundle", "create"])
        .arg(&bundle)
        .arg("--all")
        .output()
        .map(|out| out.status.success())
        .unwrap_or(false);
    if !ok {
        return None;
    }
    // Content identity: only tracked files enter the digest.
    let digest = CandidateDigest::from_files(&[
        ContentFile {
            path: ".gitignore".into(),
            bytes: read_file(&root.join(".gitignore")),
        },
        ContentFile {
            path: "test.sh".into(),
            bytes: read_file(&root.join("test.sh")),
        },
    ]);
    Some(ExampleRepo {
        root,
        bundle,
        digest,
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
            "repo {:?} digest {}..",
            repo.root.display(),
            &repo.digest.0[..12]
        ),
    });

    // AC-4, half one: the DEV environment passes (contaminated).
    let (dev_pass, dev_detail) = dev_environment_passes(&repo.root);
    checks.push(ProbeCheck {
        name: "dev-environment-passes-contaminated",
        passed: dev_pass,
        detail: dev_detail,
    });

    // Clean verification: fresh sandbox, bundle clone, per-check runs.
    let runner = CleanRunner {
        lifecycle_url: server.to_string(),
        image: image.to_string(),
        timeout: Duration::from_secs(3),
    };
    let candidate = Candidate {
        revision: "example-main".into(),
        digest: repo.digest.clone(),
        bundle: read_file(&repo.bundle),
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
            return finish(&checks, started.elapsed());
        }
    };
    checks.push(ProbeCheck {
        name: "clean-verifier-runs",
        passed: set.entries.len() == 3,
        detail: format!("{} bound results", set.entries.len()),
    });

    // AC-4, half two: the clean verifier FAILS where dev passed.
    let clean_verdict = set.effective_verdict("clean-check");
    checks.push(ProbeCheck {
        name: "clean-verifier-fails-uncontaminated",
        passed: clean_verdict == verify::result::Verdict::Fail,
        detail: format!("clean-check verdict={clean_verdict:?} (dev passed the same command)"),
    });

    // AC-1: timeout produced an ABSENT record — never a PASS.
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

    // Real PASS recorded with full provenance (AC-2).
    let bound_ok = set
        .entries
        .iter()
        .all(|e| e.result.candidate_digest == repo.digest && !e.result.environment.is_empty());
    checks.push(ProbeCheck {
        name: "real-pass-bound-to-digest-and-env",
        passed: set.effective_verdict("pass-check") == verify::result::Verdict::Pass && bound_ok,
        detail: format!(
            "pass-check verdict={:?}; all entries carry digest+environment",
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

    // AC-2: a real content change invalidates the bound evidence.
    let _ = write_file(
        &repo.root.join("test.sh"),
        "#!/bin/sh\n# changed\nexec grep -q dev-override fixture.local\n",
    );
    let changed = run_git(&repo.root, &["add", "-A"])
        && workspace_git::commit_index(&repo.root, "change candidate").is_ok();
    let new_digest = CandidateDigest::from_files(&[
        ContentFile {
            path: ".gitignore".into(),
            bytes: read_file(&repo.root.join(".gitignore")),
        },
        ContentFile {
            path: "test.sh".into(),
            bytes: read_file(&repo.root.join("test.sh")),
        },
    ]);
    let invalidated = changed && set.candidate_changed(&new_digest) == 3;
    checks.push(ProbeCheck {
        name: "candidate-change-invalidates-evidence",
        passed: invalidated && set.candidate_digest == new_digest,
        detail: format!(
            "commit ok={changed}; invalidated=3; now satisfied={}",
            set.all_checks_satisfied(&["pass-check"])
        ),
    });

    // AC-3: an empty commit (same tree) does not credit progress.
    let empty = workspace_git::commit_index(&repo.root, "empty").is_ok();
    let after_empty = CandidateDigest::from_files(&[
        ContentFile {
            path: ".gitignore".into(),
            bytes: read_file(&repo.root.join(".gitignore")),
        },
        ContentFile {
            path: "test.sh".into(),
            bytes: read_file(&repo.root.join("test.sh")),
        },
    ]);
    checks.push(ProbeCheck {
        name: "empty-commit-not-progress",
        passed: empty && candidate_progress(&new_digest, &after_empty) == Progress::NoChange,
        detail: format!(
            "empty commit digest unchanged={}",
            new_digest == after_empty
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

    // Cleanup.
    let cleaned =
        std::fs::remove_dir_all(&repo.root).is_ok() && std::fs::remove_file(&repo.bundle).is_ok();
    checks.push(ProbeCheck {
        name: "cleanup",
        passed: cleaned,
        detail: "example repo and bundle removed".into(),
    });

    finish(&checks, started.elapsed())
}
