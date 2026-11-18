//! `hodei-mh promote-probe`: TASK-10 vertical slice — CAS git promotion with
//! a write-ahead ledger, crash recovery between git and the official CLI,
//! re-read verification, and tracker-failure conservation, all against a
//! real example project (real git repo + real backlog).

use std::path::{Path, PathBuf};
use std::process::Command;

use hodei_mh_workspace::git as workspace_git;
use promote::git::{self, PromoteError};
use promote::ledger::{LedgerPhase, PromotionLedger, PromotionPlan, RecoveryAction};
use promote::ticket::TicketUpdate;
use promote::writer::{BacklogWorkWriter, WorkWriter};

struct ProbeCheck {
    name: &'static str,
    passed: bool,
    detail: String,
}

fn git_out(repo: &Path, args: &[&str]) -> Option<(bool, String, String)> {
    Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .ok()
        .map(|o| {
            (
                o.status.success(),
                String::from_utf8_lossy(&o.stdout).to_string(),
                String::from_utf8_lossy(&o.stderr).to_string(),
            )
        })
}

fn commit_file(repo: &Path, name: &str, message: &str) -> Option<String> {
    std::fs::write(repo.join(name), format!("content {name}\n")).ok()?;
    // The fixture commits with git's PLUMBING: automation must not
    // depend on the host's `git commit` path, which can be governed by a
    // policy that refuses repositories it does not classify.
    let ok = git_out(repo, &["add", name])?.0;
    if !ok {
        return None;
    }
    workspace_git::commit_index(repo, message).ok()
}

/// Build the example project: a git repo with a real backlog inside.
struct ExampleProject {
    root: PathBuf,
    ticket_id: String,
}

fn make_example_project(base_dir: &Path) -> Option<ExampleProject> {
    let root = base_dir.join("project");
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).ok()?;
    workspace_git::init_fixture_repo(&root).ok()?;

    let init = Command::new("backlog")
        .arg("init")
        .arg("ejemplo")
        .args(["--agent-instructions", "none", "--check-branches", "false"])
        .current_dir(&root)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .output()
        .ok()?;
    if !init.status.success() {
        eprintln!(
            "backlog init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        return None;
    }
    let created = Command::new("backlog")
        .args([
            "task",
            "create",
            "Ticket de promoción",
            "--description",
            " creado por el probe",
            "--ac",
            "El candidato validado integra la rama gestionada por CAS",
            "--ac",
            "El cierre no crea doble verdad ni pisa contenido previo",
        ])
        .current_dir(&root)
        .output()
        .ok()?;
    if !created.status.success() {
        eprintln!(
            "task create failed: {}",
            String::from_utf8_lossy(&created.stderr)
        );
        return None;
    }
    // The CLI assigns TASK-1 to the first task of a fresh backlog.
    let listing = Command::new("backlog")
        .args(["task", "list", "--plain"])
        .current_dir(&root)
        .output()
        .ok()?;
    let listing = String::from_utf8_lossy(&listing.stdout).to_string();
    let ticket_id = listing.lines().find_map(|line| {
        let id = line.split_whitespace().next()?;
        id.starts_with("TASK-").then(|| id.to_string())
    })?;

    // Base commit and managed branch at the base.
    let base = commit_file(&root, "README.md", "base")?;
    git_out(&root, &["branch", "-q", "hodei-mh/main", &base])?;
    // Candidate branch: one commit on top of base, exported as a bundle.
    git_out(&root, &["checkout", "-q", "-b", "candidate"])?;
    let _head = commit_file(&root, "feature.txt", "candidate work")?;
    let bundle = base_dir.join("candidate.bundle");
    if !git_out(
        &root,
        &["bundle", "create", &bundle.display().to_string(), "--all"],
    )?
    .0
    {
        return None;
    }
    git_out(&root, &["checkout", "-q", "main"])?;
    Some(ExampleProject { root, ticket_id })
}

fn configured_statuses(project: &Path) -> Vec<String> {
    let output = Command::new("backlog")
        .args(["config", "get", "statuses"])
        .current_dir(project)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
        .unwrap_or_default();
    output
        .trim()
        .split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

pub async fn run() -> i32 {
    let mut checks: Vec<ProbeCheck> = Vec::new();
    let base_dir = std::env::temp_dir().join(format!("hodei-promote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base_dir);
    std::fs::create_dir_all(&base_dir).ok();

    // Real example project: git + backlog + candidate bundle.
    let Some(project) = make_example_project(&base_dir) else {
        checks.push(ProbeCheck {
            name: "example-project-setup",
            passed: false,
            detail: "could not build example project".into(),
        });
        return finish(&checks);
    };
    let statuses = configured_statuses(&project.root);
    let Some(target_status) = statuses.last().cloned() else {
        checks.push(ProbeCheck {
            name: "example-project-setup",
            passed: false,
            detail: format!("no configured statuses: {statuses:?}"),
        });
        return finish(&checks);
    };
    let base_ref = git_out(&project.root, &["rev-parse", "hodei-mh/main"])
        .and_then(|(ok, out, _)| ok.then(|| out.trim().to_string()))
        .unwrap_or_default();
    checks.push(ProbeCheck {
        name: "example-project-setup",
        passed: !base_ref.is_empty() && Path::new(&base_dir.join("candidate.bundle")).exists(),
        detail: format!(
            "ticket {} target '{target_status}' base {base}..",
            project.ticket_id,
            base = &base_ref[..8]
        ),
    });

    // Plan written to the outbox BEFORE any effect (write-ahead).
    let ledger_path = base_dir.join("outbox.json");
    let bundle = base_dir.join("candidate.bundle");
    let mut ledger = PromotionLedger::plan(PromotionPlan {
        work_ref: project.ticket_id.clone(),
        managed_branch: "hodei-mh/main".into(),
        base_ref: base_ref.clone(),
        target_status: target_status.clone(),
        update: TicketUpdate {
            note: "Evidencia: candidato promovido por promote-probe".into(),
            check_ac: vec![1],
            final_summary: "Candidato integrado y ticket cerrado sin doble verdad.".into(),
            target_status: target_status.clone(),
        },
    });
    let _ = std::fs::write(&ledger_path, ledger.to_json().unwrap_or_default());

    // CAS promotion against the verified base.
    let promoted = git::promote_ff_only(
        &project.root,
        "hodei-mh/main",
        &bundle,
        "candidate",
        &base_ref,
    );
    let commit = match promoted {
        Ok(commit) => commit,
        Err(err) => {
            checks.push(ProbeCheck {
                name: "cas-promotion",
                passed: false,
                detail: err.to_string(),
            });
            return finish(&checks);
        }
    };
    ledger.record_promoted(&commit);
    let _ = std::fs::write(&ledger_path, ledger.to_json().unwrap_or_default());
    checks.push(ProbeCheck {
        name: "cas-promotion",
        passed: git::ensure_ref(&project.root, "hodei-mh/main", &commit).unwrap_or(false),
        detail: format!("managed branch now at {}..", &commit[..8]),
    });

    // CRASH: drop the ledger, reload from disk, classify recovery.
    let reloaded =
        PromotionLedger::from_json(&std::fs::read_to_string(&ledger_path).unwrap_or_default());
    let Some(reloaded) = reloaded.ok().filter(|l| l == &ledger) else {
        checks.push(ProbeCheck {
            name: "crash-recovers-same-commit",
            passed: false,
            detail: "outbox reload mismatch".into(),
        });
        return finish(&checks);
    };
    let mut ledger = reloaded;
    let reconcile_ok = ledger.recovery()
        == RecoveryAction::ReconcileTracker {
            commit: commit.clone(),
        };
    checks.push(ProbeCheck {
        name: "crash-recovers-same-commit",
        passed: reconcile_ok,
        detail: format!("recovery reconciles THE SAME commit {}..", &commit[..8]),
    });

    // Tracker failure (AC-4): the technical work survives, nothing is lost.
    let broken_writer = BacklogWorkWriter {
        bin: "backlog".into(),
        working_dir: base_dir.clone(), // no backlog here -> CLI fails
    };
    let failed = ledger.plan.update.clone();
    let apply_err = broken_writer.apply(&project.ticket_id, &failed).is_err();
    let work_intact = git::ensure_ref(&project.root, "hodei-mh/main", &commit).unwrap_or(false)
        && ledger.recovery()
            == RecoveryAction::ReconcileTracker {
                commit: commit.clone(),
            };
    checks.push(ProbeCheck {
        name: "tracker-failure-conserves-work",
        passed: apply_err && work_intact,
        detail: format!(
            "apply failed as expected; promoted commit intact={work_intact}; ledger still pending"
        ),
    });

    // Reconcile through the official CLI against the real backlog.
    let writer = BacklogWorkWriter {
        bin: "backlog".into(),
        working_dir: project.root.clone(),
    };
    let update_ok = writer
        .apply(&project.ticket_id, &ledger.plan.update)
        .is_ok();
    ledger.record_tracker_updated();
    checks.push(ProbeCheck {
        name: "official-cli-update",
        passed: update_ok,
        detail: format!(
            "ticket {} updated: notes appended, AC checked, status '{target_status}'",
            project.ticket_id
        ),
    });

    // Real re-read of the ticket: closure is verified, prior content conserved.
    let snapshot = writer.read_ticket(&project.ticket_id);
    let ticket_ok = snapshot
        .as_ref()
        .map(|snap| {
            snap.status == target_status
                && snap.checked_ac.contains(&1)
                && snap.raw.contains("creado por el probe")
        })
        .unwrap_or(false);
    let (reread_status, reread_acs) = snapshot
        .map(|s| (s.status, s.checked_ac))
        .unwrap_or_else(|_| (String::new(), Vec::new()));
    checks.push(ProbeCheck {
        name: "reread-verifies-closure-conserves-notes",
        passed: ticket_ok,
        detail: format!(
            "re-read status={reread_status:?} checked={reread_acs:?} original description conserved"
        ),
    });

    // Real re-read of the branch: the SAME commit, no duplicates.
    let branch_ok = git::ensure_ref(&project.root, "hodei-mh/main", &commit).unwrap_or(false);
    ledger.confirm();
    let idempotent = ledger.recovery()
        == RecoveryAction::AlreadyDone {
            commit: commit.clone(),
        };
    checks.push(ProbeCheck {
        name: "branch-reread-and-idempotent-close",
        passed: branch_ok
            && idempotent
            && ledger.phase
                == LedgerPhase::Confirmed {
                    commit: commit.clone(),
                },
        detail: "branch at persisted commit; repeated recovery is AlreadyDone; no second commit"
            .to_string(),
    });

    // AC-2: a human change on the managed branch after verification forces
    // revalidation; nothing human is lost and no second ticket path is taken.
    let checkouts_ok = git_out(&project.root, &["checkout", "-q", "hodei-mh/main"])
        .map(|(ok, _, _)| ok)
        .unwrap_or(false);
    let human = if checkouts_ok {
        commit_file(
            &project.root,
            "human-late.txt",
            "human lands after verification",
        )
    } else {
        None
    };
    git_out(&project.root, &["checkout", "-q", "main"]);
    let moved = git::promote_ff_only(
        &project.root,
        "hodei-mh/main",
        &bundle,
        "candidate",
        &base_ref,
    );
    let moved_ok = matches!(moved, Err(PromoteError::BaseMoved { .. }));
    let human_intact = human
        .as_deref()
        .map(|h| git::ensure_ref(&project.root, "hodei-mh/main", h).unwrap_or(false))
        .unwrap_or(false);
    let notes_still_there = writer
        .read_ticket(&project.ticket_id)
        .map(|s| s.raw.contains("creado por el probe") && s.raw.contains("Evidencia"))
        .unwrap_or(false);
    checks.push(ProbeCheck {
        name: "base-move-forces-revalidation-conserving-humans",
        passed: moved_ok && human_intact && notes_still_there,
        detail: "stale promotion rejected (BaseMoved); human commit intact; ticket notes conserved"
            .to_string(),
    });

    let cleaned = std::fs::remove_dir_all(&base_dir).is_ok();
    checks.push(ProbeCheck {
        name: "cleanup",
        passed: cleaned,
        detail: "example project and outbox removed".into(),
    });

    finish(&checks)
}

fn finish(checks: &[ProbeCheck]) -> i32 {
    let summary = serde_json::json!({
        "task": "TASK-10",
        "checks": checks.iter().map(|c| serde_json::json!({
            "name": c.name, "passed": c.passed, "detail": c.detail,
        })).collect::<Vec<_>>(),
        "all_passed": checks.iter().all(|c| c.passed),
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
}
