//! WorkWriter (GC-10): host-side ticket updates ONLY through the official
//! Backlog.md CLI — Markdown files are never edited directly. `read_ticket`
//! performs the real re-read that gates closure; `apply` appends evidence,
//! marks ACs and sets the configured status in one CLI invocation. The
//! trait is synchronous on purpose: the CLI is blocking process IO, and
//! callers on an async runtime wrap it in `spawn_blocking` if needed.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use thiserror::Error;

use crate::ticket::TicketUpdate;

#[derive(Debug, Error)]
#[error("work writer failed: {0}")]
pub struct WriterError(pub String);

/// In-memory ticket record used by the fake writer.
pub type FakeTicket = (String, Vec<usize>, Vec<String>);

/// What a re-read of the ticket shows. Parsed from the CLI's plain output.
#[derive(Debug, Clone, PartialEq)]
pub struct TicketSnapshot {
    pub status: String,
    /// 1-based indexes of ACs that read back as checked.
    pub checked_ac: Vec<usize>,
    /// The full plain text, kept verbatim so callers can verify that prior
    /// notes and ACs are conserved.
    pub raw: String,
}

impl TicketSnapshot {
    pub fn parse(raw: &str) -> Self {
        let mut status = String::new();
        let mut checked_ac = Vec::new();
        for line in raw.lines() {
            if let Some(rest) = line.strip_prefix("Status:") {
                // Format: "Status: ○ To Do" — drop the glyph.
                let mut parts = rest.split_whitespace();
                let glyph = parts.next();
                let name = parts.collect::<Vec<_>>().join(" ");
                if glyph.is_some() && !name.is_empty() {
                    status = name;
                } else {
                    status = rest.trim().to_string();
                }
            }
            if let Some(rest) = line.strip_prefix("- [x] #")
                && let Some(Ok(index)) = rest.split_whitespace().next().map(|head| head.parse())
            {
                checked_ac.push(index);
            }
        }
        Self {
            status,
            checked_ac,
            raw: raw.to_string(),
        }
    }
}

pub trait WorkWriter: Send + Sync {
    /// Real re-read of the ticket before and after closure.
    fn read_ticket(&self, work_ref: &str) -> Result<TicketSnapshot, WriterError>;
    /// Apply the closure update through the official CLI.
    fn apply(&self, work_ref: &str, update: &TicketUpdate) -> Result<(), WriterError>;
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
        let raw = self.run(&["task", "view", work_ref])?;
        Ok(TicketSnapshot::parse(&raw))
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
            Some((status, checked, _notes)) => Ok(TicketSnapshot {
                status: status.clone(),
                checked_ac: checked.clone(),
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "Task TASK-10 - Promote\n==================================================\n\nStatus: ○ To Do\nOrdinal: 10000\n\nAcceptance Criteria:\n--------------------------------------------------\n- [ ] #1 first\n- [x] #2 second\n";

    #[test]
    fn snapshot_parses_status_and_checked_acs() {
        let snapshot = TicketSnapshot::parse(SAMPLE);
        assert_eq!(snapshot.status, "To Do");
        assert_eq!(snapshot.checked_ac, vec![2]);
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
