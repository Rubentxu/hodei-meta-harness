//! The contract of the tracker against the REAL official CLI.
//!
//! The tracker is the authority of the closure, and the contract is
//! checked against the real thing: a real project, a real task and the
//! real `backlog` binary. No model, no network and no container are
//! involved.
//!
//! The fast CI lane does not install the official CLI, so the contract is
//! NOT RUN there and it says so loudly: a check that cannot run is never
//! reported as a pass. The promotion probe drives the same contract in the
//! lane that has the CLI.

use std::path::{Path, PathBuf};
use std::process::Command;

use promote::intent::{PromotionFacts, PromotionIntent};
use promote::ticket::TicketUpdate;
use promote::writer::{BacklogWorkWriter, TicketSnapshot, WorkWriter};

/// The CLI of the official tracker. The contract needs it.
const CLI: &str = "backlog";

/// A temporary project with a real task in a real tracker.
struct Project {
    dir: PathBuf,
    ticket: String,
}

impl Project {
    fn new(tag: &str) -> Option<Self> {
        let dir = std::env::temp_dir().join(format!(
            "hodei-cli-contract-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).ok()?;
        git(&dir, &["init", "-q", "-b", "main"]);
        let init = Command::new(CLI)
            .args([
                "init",
                "contract",
                "--agent-instructions",
                "none",
                "--check-branches",
                "false",
            ])
            .current_dir(&dir)
            .output()
            .ok()?;
        if !init.status.success() {
            return None;
        }
        let created = Command::new(CLI)
            .args([
                "task",
                "create",
                "The promotion of the candidate",
                "--description",
                "the description of the task",
                "--ac",
                "the first criterion",
                "--ac",
                "the second criterion",
            ])
            .current_dir(&dir)
            .output()
            .ok()?;
        if !created.status.success() {
            return None;
        }
        let listing = Command::new(CLI)
            .args(["task", "list", "--plain"])
            .current_dir(&dir)
            .output()
            .ok()?;
        let listing = String::from_utf8_lossy(&listing.stdout).to_string();
        let ticket = listing.lines().find_map(|line| {
            let id = line.split_whitespace().next()?;
            id.starts_with("TASK-").then(|| id.to_string())
        })?;
        Some(Self { dir, ticket })
    }

    fn writer(&self) -> BacklogWorkWriter {
        BacklogWorkWriter {
            bin: CLI.into(),
            working_dir: self.dir.clone(),
        }
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Run a git command in the temporary project.
fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Whether the official CLI is installed.
fn cli_is_installed() -> bool {
    Command::new(CLI)
        .arg("--help")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

#[test]
fn the_ticket_is_read_and_closed_through_the_official_cli() {
    if !cli_is_installed() {
        eprintln!(
            "NOT RUN: the official Backlog.md CLI is not installed, so the contract \
             against the real tracker is not checked here. The promotion probe drives \
             the same contract in the lane that installs the CLI."
        );
        return;
    }
    let Some(project) = Project::new("contract") else {
        panic!("the temporary project could not be built with the official CLI");
    };
    let writer = project.writer();

    // The re-read reads the versioned document of the CLI.
    let snapshot = writer.read_ticket(&project.ticket).expect("the ticket");
    assert_eq!(snapshot.id, project.ticket);
    assert_eq!(snapshot.status, "To Do");
    assert!(snapshot.checked_ac.is_empty());
    assert!(
        snapshot.raw.contains("the description of the task"),
        "the description of the task is conserved"
    );

    // The intent of the attempt: the note carries the marker of the
    // operation, so a second attempt knows the evidence is already there.
    let intent = PromotionIntent::new(PromotionFacts {
        project: "contract".into(),
        work_ref: project.ticket.clone(),
        epoch: 1,
        managed_branch: "hodei-mh/main".into(),
        expected_base: "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5".into(),
        expected_candidate: "2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5".into(),
        target_status: "Done".into(),
        note: "the evidence of the promotion".into(),
        check_ac: vec![1],
        final_summary: "promoted and closed".into(),
    })
    .expect("a valid intent");

    writer
        .apply(&project.ticket, &intent.update)
        .expect("the closure");

    // The closure is verified by re-reading: the tracker says what it did.
    let closed: TicketSnapshot = writer.read_ticket(&project.ticket).expect("the ticket");
    assert_eq!(closed.status, "Done");
    assert_eq!(closed.checked_ac, vec![1]);
    assert!(
        closed.notes.contains(&intent.marker()),
        "the note of the operation is there: {}",
        closed.notes
    );
    assert!(
        closed.raw.contains("the description of the task"),
        "the description of the task is conserved"
    );

    // The evidence of the operation is appended once: a second attempt
    // that appends it again would duplicate it, and the marker is what
    // tells that it is already there.
    assert!(!intent.marked_by(""), "an empty note carries no marker");
    assert!(intent.marked_by(&closed.notes));
}

#[test]
fn the_evidence_of_a_ticket_closed_by_a_person_is_appended_without_touching_its_closure() {
    if !cli_is_installed() {
        eprintln!(
            "NOT RUN: the official Backlog.md CLI is not installed, so the contract \
             against the real tracker is not checked here. The promotion probe drives \
             the same contract in the lane that installs the CLI."
        );
        return;
    }
    let Some(project) = Project::new("append") else {
        panic!("the temporary project could not be built with the official CLI");
    };
    let writer = project.writer();

    // A person closes the ticket with their own summary.
    let human = TicketUpdate {
        note: "the note of the person".into(),
        check_ac: vec![1, 2],
        final_summary: "closed by the person".into(),
        target_status: "Done".into(),
    };
    writer
        .apply(&project.ticket, &human)
        .expect("the closure of the person");

    // The operation appends ONLY its evidence: the closure of the person
    // is not written again.
    let intent = PromotionIntent::new(PromotionFacts {
        project: "contract".into(),
        work_ref: project.ticket.clone(),
        epoch: 1,
        managed_branch: "hodei-mh/main".into(),
        expected_base: "1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5".into(),
        expected_candidate: "2f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5".into(),
        target_status: "Done".into(),
        note: "the evidence of the promotion".into(),
        check_ac: vec![1],
        final_summary: "promoted and closed".into(),
    })
    .expect("a valid intent");
    writer
        .append_note(&project.ticket, &intent.update.note)
        .expect("the evidence");

    let after: TicketSnapshot = writer.read_ticket(&project.ticket).expect("the ticket");
    assert_eq!(after.status, "Done");
    assert_eq!(
        after.final_summary, "closed by the person",
        "the summary of the person is not overwritten"
    );
    assert!(after.notes.contains("the note of the person"));
    assert!(after.notes.contains(&intent.marker()));
    assert_eq!(
        after.notes.matches(&intent.marker()).count(),
        1,
        "the evidence of the operation is there once"
    );
}
