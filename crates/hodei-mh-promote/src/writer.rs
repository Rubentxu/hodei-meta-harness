//! WorkWriter (GC-10): host-side ticket updates ONLY through the official
//! Backlog.md CLI - Markdown files are never edited directly. `read_ticket`
//! performs the real re-read that gates closure, and it reads the VERSIONED
//! document of the CLI (`task view --json`), not its plain text: the plain
//! text is a rendering that can change without notice, while the document
//! names the fields this contract needs. `apply` appends evidence, marks ACs
//! and sets the configured status in one CLI invocation; `append_note` writes
//! ONLY the evidence, so the closure of a person is never overwritten. The
//! trait is synchronous on purpose: the CLI is blocking process IO, and
//! callers on an async runtime wrap it in `spawn_blocking` if needed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;
use thiserror::Error;

use crate::ticket::TicketUpdate;

#[derive(Debug, Error)]
#[error("work writer failed: {0}")]
pub struct WriterError(pub String);

/// In-memory ticket record used by the fake writer.
pub type FakeTicket = (String, Vec<usize>, Vec<String>);

/// What a re-read of the ticket shows, read from the versioned document.
#[derive(Debug, Clone, PartialEq)]
pub struct TicketSnapshot {
    /// The id of the ticket, as the tracker names it.
    pub id: String,
    pub status: String,
    /// 1-based indexes of ACs that read back as checked.
    pub checked_ac: Vec<usize>,
    /// The notes of the ticket, verbatim and in the order they were added.
    pub notes: String,
    /// The closure summary, verbatim.
    pub final_summary: String,
    /// The whole document, kept verbatim so callers can verify that prior
    /// notes and ACs are conserved.
    pub raw: String,
}

/// Read the versioned task document of the official CLI.
///
/// A document that is not a task view is an error, never a ticket with no
/// fields: an unreadable observation must not look like an empty one.
pub fn parse_task_view(raw: &str) -> Result<TicketSnapshot, WriterError> {
    let document: Value = serde_json::from_str(raw)
        .map_err(|error| WriterError(format!("the task document is not JSON: {error}")))?;
    let kind = document
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if kind != "task-view" {
        return Err(WriterError(format!(
            "the task document is a {kind:?}, not a task view"
        )));
    }
    let task = document
        .get("task")
        .ok_or_else(|| WriterError("the task document carries no task".into()))?;
    let checked_ac = task
        .get("acceptanceCriteria")
        .and_then(Value::as_array)
        .map(|criteria| {
            criteria
                .iter()
                .filter(|criterion| {
                    criterion
                        .get("checked")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
                })
                .filter_map(|criterion| criterion.get("index").and_then(Value::as_u64))
                .map(|index| index as usize)
                .collect()
        })
        .unwrap_or_default();
    Ok(TicketSnapshot {
        id: task
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        status: task
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        checked_ac,
        notes: task
            .get("implementationNotes")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        final_summary: task
            .get("finalSummary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        raw: raw.to_string(),
    })
}

pub trait WorkWriter: Send + Sync {
    /// Real re-read of the ticket before and after closure.
    fn read_ticket(&self, work_ref: &str) -> Result<TicketSnapshot, WriterError>;
    /// Apply the closure update through the official CLI.
    fn apply(&self, work_ref: &str, update: &TicketUpdate) -> Result<(), WriterError>;
    /// Append ONLY the evidence: the closure of a person is never
    /// overwritten, and the evidence of this operation is not lost.
    fn append_note(&self, work_ref: &str, note: &str) -> Result<(), WriterError>;
}

/// The official-CLI implementation.
pub struct BacklogWorkWriter {
    pub bin: PathBuf,
    pub working_dir: PathBuf,
}

impl BacklogWorkWriter {
    fn run(&self, args: &[&str]) -> Result<String, WriterError> {
        let output = Command::new(&self.bin)
            .args(args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| WriterError(format!("spawn {}: {e}", self.bin.display())))?;
        if !output.status.success() {
            return Err(WriterError(format!(
                "{} {:?} exited {}: {}",
                self.bin.display(),
                args,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl WorkWriter for BacklogWorkWriter {
    fn read_ticket(&self, work_ref: &str) -> Result<TicketSnapshot, WriterError> {
        let raw = self.run(&["task", "view", work_ref, "--json"])?;
        parse_task_view(&raw)
    }

    fn apply(&self, work_ref: &str, update: &TicketUpdate) -> Result<(), WriterError> {
        let mut args = vec![
            "task".to_string(),
            "edit".to_string(),
            work_ref.to_string(),
            "--append-notes".to_string(),
            update.note.clone(),
        ];
        for ac in &update.check_ac {
            args.push("--check-ac".to_string());
            args.push(ac.to_string());
        }
        args.push("--status".to_string());
        args.push(update.target_status.clone());
        args.push("--final-summary".to_string());
        args.push(update.final_summary.clone());
        let str_args: Vec<&str> = args.iter().map(String::as_str).collect();
        self.run(&str_args).map(|_| ())
    }

    fn append_note(&self, work_ref: &str, note: &str) -> Result<(), WriterError> {
        self.run(&["task", "edit", work_ref, "--append-notes", note])
            .map(|_| ())
    }
}

/// In-memory implementation for contract tests: notes accumulate, status
/// changes, and failures can be injected at will.
#[derive(Default)]
pub struct FakeWorkWriter {
    pub tickets: std::sync::Mutex<std::collections::BTreeMap<String, FakeTicket>>,
    pub fail_apply_with: std::sync::Mutex<Option<String>>,
}

impl WorkWriter for FakeWorkWriter {
    fn read_ticket(&self, work_ref: &str) -> Result<TicketSnapshot, WriterError> {
        let tickets = self.tickets.lock().unwrap();
        match tickets.get(work_ref) {
            Some((status, checked, notes)) => Ok(TicketSnapshot {
                id: work_ref.to_string(),
                status: status.clone(),
                checked_ac: checked.clone(),
                notes: notes.join("\n\n"),
                final_summary: String::new(),
                raw: String::new(),
            }),
            None => Err(WriterError(format!("ticket {work_ref} not found"))),
        }
    }

    fn apply(&self, work_ref: &str, update: &TicketUpdate) -> Result<(), WriterError> {
        if let Some(reason) = self.fail_apply_with.lock().unwrap().as_ref() {
            return Err(WriterError(reason.clone()));
        }
        let mut tickets = self.tickets.lock().unwrap();
        let entry = tickets
            .entry(work_ref.to_string())
            .or_insert_with(|| ("To Do".to_string(), Vec::new(), Vec::new()));
        entry.0 = update.target_status.clone();
        entry.1.extend(update.check_ac.iter().copied());
        entry.2.push(update.note.clone());
        Ok(())
    }

    fn append_note(&self, work_ref: &str, note: &str) -> Result<(), WriterError> {
        if let Some(reason) = self.fail_apply_with.lock().unwrap().as_ref() {
            return Err(WriterError(reason.clone()));
        }
        let mut tickets = self.tickets.lock().unwrap();
        let entry = tickets
            .entry(work_ref.to_string())
            .or_insert_with(|| ("To Do".to_string(), Vec::new(), Vec::new()));
        entry.2.push(note.to_string());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The versioned document the official CLI answers with.
    const TASK_VIEW: &str = r#"{
      "schemaVersion": 1,
      "kind": "task-view",
      "task": {
        "id": "TASK-10",
        "status": "Done",
        "acceptanceCriteria": [
          {"index": 1, "text": "first", "checked": true},
          {"index": 2, "text": "second", "checked": false}
        ],
        "implementationNotes": "evidencia [hodei-mh:op:promote:TASK-10:1:abc123]",
        "finalSummary": "cierre"
      }
    }"#;

    #[test]
    fn the_ticket_is_read_from_the_versioned_document() {
        let snapshot = parse_task_view(TASK_VIEW).expect("a task view");
        assert_eq!(snapshot.id, "TASK-10");
        assert_eq!(snapshot.status, "Done");
        assert_eq!(snapshot.checked_ac, vec![1]);
        assert!(
            snapshot
                .notes
                .contains("[hodei-mh:op:promote:TASK-10:1:abc123]"),
            "the marker of the operation travels in the notes: {}",
            snapshot.notes
        );
        assert_eq!(snapshot.final_summary, "cierre");
    }

    #[test]
    fn a_document_that_is_not_a_task_view_is_not_read_as_a_ticket() {
        assert!(parse_task_view(r#"{"kind":"task-list","tasks":[]}"#).is_err());
        assert!(parse_task_view("not json").is_err());
    }

    #[test]
    fn a_task_view_without_acceptance_criteria_reads_as_none_checked() {
        let snapshot = parse_task_view(
            r#"{"schemaVersion":1,"kind":"task-view","task":{"id":"T-1","status":"To Do"}}"#,
        )
        .expect("a task view");
        assert!(snapshot.checked_ac.is_empty());
        assert!(snapshot.notes.is_empty());
        assert_eq!(snapshot.status, "To Do");
    }

    #[test]
    fn fake_writer_accumulates_notes_without_overwriting() {
        let writer = FakeWorkWriter::default();
        {
            let mut tickets = writer.tickets.lock().unwrap();
            tickets.insert(
                "TASK-1".into(),
                ("In Progress".into(), vec![1], vec!["prior note".into()]),
            );
        }
        let update = TicketUpdate {
            note: "later evidence".into(),
            check_ac: vec![2],
            final_summary: "closed".into(),
            target_status: "Done".into(),
        };
        writer.apply("TASK-1", &update).unwrap();
        let notes = {
            let tickets = writer.tickets.lock().unwrap();
            tickets["TASK-1"].2.clone()
        };
        assert_eq!(
            notes,
            vec!["prior note".to_string(), "later evidence".to_string()]
        );
        let snapshot = writer.read_ticket("TASK-1").unwrap();
        assert_eq!(snapshot.status, "Done");
        assert_eq!(snapshot.checked_ac, vec![1, 2]);
        assert!(snapshot.notes.contains("later evidence"));
    }

    #[test]
    fn the_note_of_an_operation_can_be_appended_without_closing() {
        let writer = FakeWorkWriter::default();
        {
            let mut tickets = writer.tickets.lock().unwrap();
            tickets.insert(
                "TASK-1".into(),
                ("Done".into(), vec![1], vec!["human note".into()]),
            );
        }
        writer.append_note("TASK-1", "evidence").unwrap();
        let snapshot = writer.read_ticket("TASK-1").unwrap();
        assert_eq!(snapshot.status, "Done");
        assert!(snapshot.notes.contains("human note"));
        assert!(snapshot.notes.contains("evidence"));
    }

    #[test]
    fn injected_failure_surfaces_as_writer_error() {
        let writer = FakeWorkWriter::default();
        *writer.fail_apply_with.lock().unwrap() = Some("tracker unreachable".into());
        let update = TicketUpdate {
            note: "n".into(),
            check_ac: vec![],
            final_summary: "s".into(),
            target_status: "Done".into(),
        };
        let err = writer.apply("TASK-1", &update).unwrap_err();
        assert!(err.to_string().contains("tracker unreachable"));
    }
}
