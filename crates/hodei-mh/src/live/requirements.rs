//! The requirements of a work item, read from the tracker.
//!
//! The tracker states what the work is (its title and description) and
//! which executable checks decide it: every `check:` line is a command
//! the clean verifier runs. A task that declares no check is
//! UNVERIFIABLE — it is refused instead of being published on trust.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use hodei_mh_tracker::core::WorkItem;
use serde::Deserialize;
use verify::runner::Check;

use runner::ports::{Objective, ObjectiveCheck, RequirementsError, RequirementsPort};

/// The prefix of a line that declares an executable check.
pub const CHECK_PREFIX: &str = "check:";

/// The requirements of a work item, read with the official tracker CLI.
pub struct BacklogRequirements {
    bin: PathBuf,
    working_dir: PathBuf,
}

impl BacklogRequirements {
    /// Read the task documents of the project at `working_dir`.
    pub fn new(bin: impl Into<PathBuf>, working_dir: &Path) -> Self {
        Self {
            bin: bin.into(),
            working_dir: working_dir.to_path_buf(),
        }
    }

    fn view(&self, work_ref: &str) -> Result<String, String> {
        let output = Command::new(&self.bin)
            .args(["task", "view", work_ref, "--json"])
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .output()
            .map_err(|error| format!("spawn {}: {error}", self.bin.display()))?;
        if !output.status.success() {
            return Err(format!(
                "{} task view {work_ref} exited {}: {}",
                self.bin.display(),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }
}

impl RequirementsPort for BacklogRequirements {
    fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
        let raw = self.view(&item.id).map_err(|reason| RequirementsError {
            work_ref: item.id.clone(),
            reason,
        })?;
        requirements_from_view(&raw, item)
    }
}

/// The versioned task document the official CLI answers with.
#[derive(Debug, Deserialize)]
struct TaskView {
    kind: String,
    task: TaskDocument,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TaskDocument {
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    acceptance_criteria: Vec<Criterion>,
}

#[derive(Debug, Deserialize)]
struct Criterion {
    /// The number the tracker states for the criterion. It is the index a
    /// closure checks; it is never derived from the position in the list.
    #[serde(default)]
    index: Option<u64>,
    #[serde(default)]
    text: String,
}

/// A declared `check:` line and the acceptance criterion that declares
/// it, when one does.
type DeclaredLine<'a> = (Option<usize>, &'a str);

/// The objective a task view states, or the reason it states none.
///
/// The instruction is the title and the description; the checks are the
/// `check:` lines of the description and of the acceptance criteria. A
/// check declared under a criterion carries the NUMBER the tracker states
/// for it, so the closure checks the criteria the checks belong to.
pub fn requirements_from_view(raw: &str, item: &WorkItem) -> Result<Objective, RequirementsError> {
    let view: TaskView = serde_json::from_str(raw).map_err(|error| RequirementsError {
        work_ref: item.id.clone(),
        reason: format!("the task document is not JSON: {error}"),
    })?;
    if view.kind != "task-view" {
        return Err(RequirementsError {
            work_ref: item.id.clone(),
            reason: format!("the task document is a {:?}, not a task view", view.kind),
        });
    }
    let instruction = format!(
        "{}\n\n{}",
        view.task.title.trim(),
        view.task.description.trim()
    );
    // A line of the description belongs to no criterion; a line of a
    // criterion belongs to the number the tracker states for it. A
    // criterion the tracker does not number maps to none: the index is
    // not guessed from a position.
    let mut declared: Vec<DeclaredLine<'_>> = Vec::new();
    for line in view.task.description.lines() {
        declared.push((None, line));
    }
    for criterion in &view.task.acceptance_criteria {
        let ac = criterion
            .index
            .and_then(|index| usize::try_from(index).ok())
            .filter(|index| *index > 0);
        for line in criterion.text.lines() {
            declared.push((ac, line));
        }
    }
    let checks = checks_of(&declared).map_err(|reason| RequirementsError {
        work_ref: item.id.clone(),
        reason,
    })?;
    if checks.is_empty() {
        return Err(RequirementsError {
            work_ref: item.id.clone(),
            reason: format!(
                "the task declares no '{CHECK_PREFIX}' line: nothing can decide whether it held"
            ),
        });
    }
    Ok(Objective {
        work_ref: item.id.clone(),
        instruction,
        checks,
    })
}

/// The executable checks a set of declared lines states, in order.
///
/// A check keeps the criterion it was declared under. A line that is not
/// a `check:` line is not a check, and a check the description declares
/// is attached to no acceptance criterion.
fn checks_of(declared: &[DeclaredLine<'_>]) -> Result<Vec<ObjectiveCheck>, String> {
    let mut checks = Vec::new();
    for (ac, line) in declared {
        let trimmed = line.trim();
        let Some(command) = trimmed.strip_prefix(CHECK_PREFIX) else {
            continue;
        };
        let command = command.trim();
        if command.is_empty() {
            return Err(format!(
                "the line '{trimmed}' declares a check without a command"
            ));
        }
        let check = Check {
            id: format!("check-{}", checks.len() + 1),
            command: command.to_string(),
        };
        checks.push(match ac {
            Some(index) => ObjectiveCheck::in_ac(check, *index),
            None => ObjectiveCheck::free(check),
        });
    }
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodei_mh_tracker::core::WorkStatus;

    fn item(id: &str) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: "the work".into(),
            status: WorkStatus::Open,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        }
    }

    fn view(description: &str, criteria: &[&str]) -> String {
        serde_json::json!({
            "schemaVersion": 1,
            "kind": "task-view",
            "task": {
                "id": "T-1",
                "title": "Do the work",
                "description": description,
                "acceptanceCriteria": criteria.iter().enumerate().map(|(index, text)| {
                    serde_json::json!({"index": index + 1, "text": text, "checked": false})
                }).collect::<Vec<_>>(),
            }
        })
        .to_string()
    }

    #[test]
    fn every_check_line_becomes_an_executable_check() {
        let objective = requirements_from_view(
            &view(
                "Do it.\ncheck: test -f out.txt\ncheck: grep -q ok out.txt",
                &[],
            ),
            &item("T-1"),
        )
        .expect("the task declares checks");
        assert_eq!(objective.work_ref, "T-1");
        assert!(objective.instruction.contains("Do the work"));
        assert_eq!(
            objective.required(),
            vec!["check-1".to_string(), "check-2".to_string()]
        );
        assert_eq!(objective.checks[1].check.command, "grep -q ok out.txt");
        assert!(
            objective.check_ac().is_empty(),
            "a check the description declares belongs to no criterion"
        );
    }

    /// A view whose criteria are numbered by the tracker, interleaving
    /// criteria that declare no check.
    fn numbered_view() -> String {
        serde_json::json!({
            "schemaVersion": 1,
            "kind": "task-view",
            "task": {
                "id": "T-1",
                "title": "Do the work",
                "description": "Do it.",
                "acceptanceCriteria": [
                    {"index": 1, "text": "the file exists", "checked": false},
                    {"index": 2, "text": "check: test -f a.txt", "checked": false},
                    {"index": 3, "text": "the code is documented", "checked": false},
                    {"index": 4, "text": "check: test -f b.txt", "checked": false},
                ],
            }
        })
        .to_string()
    }

    #[test]
    fn a_check_carries_the_criterion_that_declares_it_not_its_position() {
        // The criteria that declare checks are 2 and 4; marking the first
        // two criteria by COUNT would check the wrong ones.
        let objective = requirements_from_view(&numbered_view(), &item("T-1"))
            .expect("the task declares checks");
        assert_eq!(
            objective
                .checks
                .iter()
                .map(|declared| declared.check.command.as_str())
                .collect::<Vec<_>>(),
            vec!["test -f a.txt", "test -f b.txt"]
        );
        assert_eq!(objective.check_ac(), vec![2, 4]);
    }

    #[test]
    fn a_criterion_the_tracker_does_not_number_maps_to_no_index() {
        let view = serde_json::json!({
            "schemaVersion": 1,
            "kind": "task-view",
            "task": {
                "id": "T-1",
                "title": "Do the work",
                "description": "Do it.",
                "acceptanceCriteria": [{"text": "check: cargo test", "checked": false}],
            }
        })
        .to_string();
        let objective =
            requirements_from_view(&view, &item("T-1")).expect("the task declares a check");
        assert_eq!(objective.checks[0].check.command, "cargo test");
        assert_eq!(
            objective.check_ac(),
            Vec::<usize>::new(),
            "a criterion without a stated number is not guessed"
        );
    }

    #[test]
    fn a_check_line_in_an_acceptance_criterion_counts_too() {
        let objective = requirements_from_view(
            &view("Do it.", &["the workspace has a `check: test -d src` line"]),
            &item("T-1"),
        )
        .expect_err("the criterion line does not start with the prefix");
        assert!(objective.to_string().contains("declares no"), "{objective}");
        let objective =
            requirements_from_view(&view("Do it.", &["check: cargo test"]), &item("T-1"))
                .expect("the criterion declares a check");
        assert_eq!(objective.checks[0].check.command, "cargo test");
    }

    #[test]
    fn a_task_without_a_check_is_unverifiable_instead_of_trusted() {
        let error = requirements_from_view(&view("Just do it.", &["it works"]), &item("T-1"))
            .expect_err("nothing decides whether the task held");
        assert!(error.to_string().contains("nothing can decide"), "{error}");
    }

    #[test]
    fn a_check_without_a_command_is_refused() {
        let error = requirements_from_view(&view("check:   ", &[]), &item("T-1"))
            .expect_err("a check without a command decides nothing");
        assert!(error.to_string().contains("without a command"), "{error}");
    }

    #[test]
    fn a_document_that_is_not_a_task_view_is_refused() {
        let error = requirements_from_view(r#"{"kind":"task-list","task":{}}"#, &item("T-1"))
            .expect_err("another document is not a task view");
        assert!(error.to_string().contains("not a task view"), "{error}");
    }
}
