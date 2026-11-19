//! `hodei-mh promote-probe`: the promotion of a candidate and the closure
//! of its ticket as ONE attempt, against a real example project (a real git
//! repo, a real tracker and the real durable store).
//!
//! What it proves: the intent is written BEFORE the effect, a tracker that
//! fails does not lose the promoted work, a crash is resumed by observing
//! the two authorities (the same commit, no second one, no duplicated note),
//! the closure goes through the official CLI, and a change of a person is
//! never overwritten.
//!
//! The evidence of the candidate is DECLARED here with the shape the
//! verifier mints: this probe is about the promotion and the closure, not
//! about the verification, which is the verify-probe's subject.

use std::path::{Path, PathBuf};
use std::process::Command;

use hodei_mh_coordinator::core::ClaimResult;
use hodei_mh_coordinator::store::CoordinatorStore;
use hodei_mh_workspace::git as workspace_git;
use promote::flow::{PromotionRequest, execute};
use promote::git;
use promote::intent::{Conflict, PromotionFacts, PromotionIntent};
use promote::journal::{CoordinatorJournal, IntentJournal};
use promote::writer::{BacklogWorkWriter, WorkWriter};
use serde_json::json;
use verify::evidence::EvidenceSet;
use verify::transport::TransportDigest;

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
    base: String,
    candidate: String,
    tree: String,
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
    let candidate = commit_file(&root, "feature.txt", "candidate work")?;
    let tree = git_out(&root, &["rev-parse", "candidate^{tree}"])?
        .1
        .trim()
        .to_string();
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
    Some(ExampleProject {
        root,
        ticket_id,
        base,
        candidate,
        tree,
    })
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

/// The evidence of the candidate, declared with the shape the verifier
/// mints: the promotion needs evidence that speaks for the candidate, and
/// this probe is about what happens with it, not about minting it.
fn declared_evidence(work_ref: &str, revision: &str, tree: &str, bundle: &[u8]) -> EvidenceSet {
    let digest = TransportDigest::of(bundle);
    let document = json!({
        "work_ref": work_ref,
        "binding": {
            "revision": revision,
            "tree": tree,
            "transport_digest": digest.as_str(),
            "required_checks": [{"id": "AC-1", "command": "make check"}],
            "image_id": "sha256:image",
            "environment": "sha256:image::sandbox-1"
        },
        "entries": [{
            "result": {
                "check_id": "AC-1",
                "environment": "sha256:image::sandbox-1",
                "image_id": "sha256:image",
                "tree": tree,
                "transport_digest": digest.as_str(),
                "command": "make check",
                "record": {
                    "command": "make check",
                    "exit_code": 0,
                    "output": "ok",
                    "output_truncated": false,
                    "stream_completed": true
                }
            },
            "status": "Valid"
        }]
    });
    serde_json::from_value(document).expect("the evidence document is well formed")
}

/// The attempt, against a tracker and a durable store.
fn attempt(
    intent: &PromotionIntent,
    evidence: &EvidenceSet,
    project: &ExampleProject,
    bundle: &Path,
    writer: &BacklogWorkWriter,
    store: &mut CoordinatorStore,
) -> Result<promote::flow::PromotionReport, promote::flow::PromotionError> {
    let mut journal = CoordinatorJournal::new(store);
    execute(
        PromotionRequest {
            intent,
            evidence,
            repo: &project.root,
            bundle,
            candidate_branch: "candidate",
        },
        writer,
        &mut journal,
    )
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
    let bundle = base_dir.join("candidate.bundle");
    checks.push(ProbeCheck {
        name: "example-project-setup",
        passed: project.base.len() == 40 && project.candidate.len() == 40 && bundle.exists(),
        detail: format!(
            "ticket {} target '{target_status}' base {}.. candidate {}..",
            project.ticket_id,
            &project.base[..8],
            &project.candidate[..8]
        ),
    });

    // The durable store of the project, and the claim of the ticket: the
    // attempt is fenced to it.
    let mut store = match CoordinatorStore::open("promote-probe", &base_dir.join("state.db")) {
        Ok(store) => store,
        Err(error) => {
            checks.push(ProbeCheck {
                name: "durable-store-opens",
                passed: false,
                detail: error.to_string(),
            });
            return finish(&checks);
        }
    };
    let epoch =
        match store.transact(|state| Ok(state.try_claim(&project.ticket_id, "promote-probe"))) {
            Ok(ClaimResult::Granted(claim)) => claim.epoch,
            Ok(ClaimResult::Denied { holder }) => {
                checks.push(ProbeCheck {
                    name: "durable-store-opens",
                    passed: false,
                    detail: format!("the ticket is claimed by {holder}"),
                });
                return finish(&checks);
            }
            Err(error) => {
                checks.push(ProbeCheck {
                    name: "durable-store-opens",
                    passed: false,
                    detail: error.to_string(),
                });
                return finish(&checks);
            }
        };

    // The intent of the attempt: the candidate it names, the base it
    // applies to, the ticket and the epoch.
    let intent = match PromotionIntent::new(PromotionFacts {
        project: "promote-probe".into(),
        work_ref: project.ticket_id.clone(),
        epoch,
        managed_branch: "hodei-mh/main".into(),
        expected_base: project.base.clone(),
        expected_candidate: project.candidate.clone(),
        target_status: target_status.clone(),
        note: "Evidencia: candidato promovido por promote-probe".into(),
        check_ac: vec![1],
        final_summary: "Candidato integrado y ticket cerrado sin doble verdad.".into(),
    }) {
        Ok(intent) => intent,
        Err(error) => {
            checks.push(ProbeCheck {
                name: "the-intent-states-the-candidate",
                passed: false,
                detail: error.to_string(),
            });
            return finish(&checks);
        }
    };
    let bytes = std::fs::read(&bundle).unwrap_or_default();
    let evidence = declared_evidence(
        &project.ticket_id,
        &project.candidate,
        &project.tree,
        &bytes,
    );
    let writer = BacklogWorkWriter {
        bin: "backlog".into(),
        working_dir: project.root.clone(),
    };

    // The tracker fails: the promotion happens and the closure does not.
    // The attempt must not lose the promoted work.
    let broken_writer = BacklogWorkWriter {
        bin: "backlog".into(),
        working_dir: base_dir.clone(), // no backlog here -> the CLI fails
    };
    let failed = attempt(
        &intent,
        &evidence,
        &project,
        &bundle,
        &broken_writer,
        &mut store,
    );
    let tracker_failed = failed.is_err();
    let work_intact =
        git::ensure_ref(&project.root, "hodei-mh/main", &project.candidate).unwrap_or(false);
    checks.push(ProbeCheck {
        name: "tracker-failure-conserves-work",
        passed: tracker_failed && work_intact,
        detail: format!(
            "the tracker failed as expected ({:?}); the promoted commit is intact={work_intact}",
            failed.as_ref().err().map(|error| error.to_string())
        ),
    });

    // The intent was written BEFORE the effect, so it is there even though
    // the closure did not happen.
    let recorded = {
        let journal = CoordinatorJournal::new(&mut store);
        journal.load(&intent.operation_id)
    };
    let recorded = match recorded {
        Ok(Some(record)) => record,
        Ok(None) => {
            checks.push(ProbeCheck {
                name: "the-intent-is-written-before-the-effect",
                passed: false,
                detail: "the attempt was not recorded".into(),
            });
            return finish(&checks);
        }
        Err(error) => {
            checks.push(ProbeCheck {
                name: "the-intent-is-written-before-the-effect",
                passed: false,
                detail: error.to_string(),
            });
            return finish(&checks);
        }
    };
    checks.push(ProbeCheck {
        name: "the-intent-is-written-before-the-effect",
        passed: recorded.intent.same_attempt(&intent)
            && recorded
                .outcome
                .as_ref()
                .is_some_and(|outcome| outcome.is_applied()),
        detail: format!(
            "operation {} recorded with the candidate {}.. and the outcome {:?}",
            intent.operation_id,
            &project.candidate[..8],
            recorded.outcome
        ),
    });

    // CRASH RESUMED: the attempt runs again. It observes what happened and
    // does only what is left to do: the candidate is not promoted again
    // and the note is not duplicated.
    let resumed = attempt(&intent, &evidence, &project, &bundle, &writer, &mut store);
    let branch_after = git_out(&project.root, &["rev-parse", "hodei-mh/main"])
        .map(|(_, out, _)| out.trim().to_string())
        .unwrap_or_default();
    let notes_after = writer
        .read_ticket(&project.ticket_id)
        .map(|snapshot| snapshot.notes)
        .unwrap_or_default();
    let marker_count = notes_after.matches(&intent.marker()).count();
    checks.push(ProbeCheck {
        name: "crash-resumes-the-same-commit-without-duplicating",
        passed: resumed.as_ref().is_ok_and(|report| !report.promoted && report.closed)
            && branch_after == project.candidate
            && marker_count == 1,
        detail: format!(
            "the branch is at {}.., the note of the operation appears {marker_count} time(s), the resumed attempt promoted={} closed={}",
            &branch_after[..8.min(branch_after.len())],
            resumed.as_ref().map(|report| report.promoted).unwrap_or(true),
            resumed.as_ref().map(|report| report.closed).unwrap_or(false),
        ),
    });

    // The record of the attempt carries the facts it was written with, and
    // it says that the operation finished.
    let recorded = {
        let journal = CoordinatorJournal::new(&mut store);
        journal.load(&intent.operation_id)
    };
    let recorded = match recorded {
        Ok(Some(record)) => record,
        Ok(None) => {
            checks.push(ProbeCheck {
                name: "the-record-carries-the-facts",
                passed: false,
                detail: "the attempt was not recorded".into(),
            });
            return finish(&checks);
        }
        Err(error) => {
            checks.push(ProbeCheck {
                name: "the-record-carries-the-facts",
                passed: false,
                detail: error.to_string(),
            });
            return finish(&checks);
        }
    };
    checks.push(ProbeCheck {
        name: "the-record-carries-the-facts",
        passed: recorded.intent.same_attempt(&intent)
            && recorded.outcome.is_some_and(|outcome| outcome.is_applied()),
        detail: format!(
            "operation {} recorded with the candidate {}..",
            intent.operation_id,
            &project.candidate[..8]
        ),
    });

    // Real re-read of the ticket: the closure is verified, the prior
    // content is conserved.
    let snapshot = writer.read_ticket(&project.ticket_id);
    let ticket_ok = snapshot.as_ref().is_ok_and(|snap| {
        snap.status == target_status
            && snap.checked_ac.contains(&1)
            && snap.raw.contains("creado por el probe")
    });
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

    // A change of a person on the managed branch: the attempt is refused,
    // and nothing of that person is touched.
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
    let moved = attempt(&intent, &evidence, &project, &bundle, &writer, &mut store);
    let refused = moved
        .as_ref()
        .is_ok_and(|report| matches!(report.conflict, Some(Conflict::RefMoved { .. })));
    let human_intact = human
        .as_deref()
        .map(|h| git::ensure_ref(&project.root, "hodei-mh/main", h).unwrap_or(false))
        .unwrap_or(false);
    let notes_still_there = writer
        .read_ticket(&project.ticket_id)
        .map(|s| s.notes.contains("Evidencia"))
        .unwrap_or(false);
    checks.push(ProbeCheck {
        name: "base-move-forces-revalidation-conserving-humans",
        passed: refused && human_intact && notes_still_there,
        detail: "the attempt is refused (the branch moved); the human commit is intact; the notes are conserved"
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
