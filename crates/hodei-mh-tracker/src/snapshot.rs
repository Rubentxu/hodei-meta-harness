//! Backlog.md `task list --json` envelope parsing into provider-agnostic
//! work items. Tolerant by contract: unknown fields ignored, unknown states
//! preserved, missing data parsed as "not ready" rather than guessed.

use serde_json::Value;

use crate::core::{WorkItem, WorkStatus};

/// One envelope from the Backlog CLI (kind=task-list, schemaVersion=1).
/// Returns items plus every unresolved dependency reference (archived,
/// ambiguous or deleted tasks referenced by `dependencies`).
pub fn parse_task_list_envelope(
    text: &str,
    open_aliases: &[String],
) -> Result<ParsedEnvelope, String> {
    let envelope: Value =
        serde_json::from_str(text).map_err(|e| format!("invalid envelope JSON: {e}"))?;
    let kind = envelope["kind"].as_str().unwrap_or_default();
    if kind != "task-list" {
        return Err(format!("unexpected envelope kind {kind:?}"));
    }
    let mut items = Vec::new();
    for task in envelope["tasks"].as_array().cloned().unwrap_or_default() {
        let id = task["id"].as_str().unwrap_or_default().to_string();
        if id.is_empty() {
            continue;
        }
        let status = task["status"].as_str().unwrap_or_default();
        let dependencies: Vec<String> = task["dependencies"]
            .as_array()
            .map(|deps| {
                deps.iter()
                    .filter_map(|d| d.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        items.push(WorkItem {
            id: id.clone(),
            title: task["title"].as_str().unwrap_or_default().to_string(),
            status: WorkStatus::parse(status, open_aliases),
            provider_ready: task.get("isReady").and_then(|v| v.as_bool()),
            dependencies,
        });
    }
    let known: std::collections::HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    let unresolved: Vec<String> = items
        .iter()
        .flat_map(|i| i.dependencies.iter())
        .filter(|dep| !known.contains(*dep))
        .cloned()
        .collect();
    Ok(ParsedEnvelope {
        items,
        unresolved_dependencies: unresolved,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedEnvelope {
    pub items: Vec<WorkItem>,
    /// Dependency ids referenced but not present in this snapshot
    /// (archived tasks, ambiguous ids, deletions).
    pub unresolved_dependencies: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const FIXTURE: &str = r#"{
        "kind": "task-list",
        "schemaVersion": 1,
        "tasks": [
            {"id": "TASK-1", "title": "one", "status": "In Progress", "isReady": true, "dependencies": []},
            {"id": "TASK-4", "title": "four", "status": "To Do", "isReady": true, "dependencies": []},
            {"id": "TASK-5", "title": "five", "status": "To Do", "isReady": false, "dependencies": ["TASK-1", "TASK-4"]},
            {"id": "TASK-9", "title": "nine", "status": "To Do", "isReady": true, "dependencies": ["TASK-ARCHIVED"]},
            {"id": "TASK-10", "title": "ten", "status": "Waiting Review", "isReady": true, "dependencies": []}
        ]
    }"#;

    #[test]
    fn envelope_parses_items_preserving_unknown_states() {
        let parsed = parse_task_list_envelope(FIXTURE, &[]).unwrap();
        assert_eq!(parsed.items.len(), 5);
        assert_eq!(parsed.items[0].status, WorkStatus::InProgress);
        assert_eq!(
            parsed.items[4].status,
            WorkStatus::Unknown("Waiting Review".into())
        );
    }

    #[test]
    fn archived_dependency_is_reported_unresolved_and_blocks_eligibility() {
        let parsed = parse_task_list_envelope(FIXTURE, &[]).unwrap();
        assert_eq!(
            parsed.unresolved_dependencies,
            vec!["TASK-ARCHIVED".to_string()]
        );
        let eligible: Vec<String> = crate::core::select_eligible(&parsed.items)
            .into_iter()
            .map(|i| i.id.clone())
            .collect();
        // TASK-9 blocked by unresolved dep; TASK-5 blocked by isReady=false.
        assert_eq!(eligible, vec!["TASK-4".to_string()]);
    }

    #[test]
    fn empty_tasks_array_is_a_valid_snapshot() {
        let parsed =
            parse_task_list_envelope(r#"{"kind":"task-list","schemaVersion":1,"tasks":[]}"#, &[])
                .unwrap();
        assert!(parsed.items.is_empty());
        assert!(parsed.unresolved_dependencies.is_empty());
    }

    #[test]
    fn wrong_kind_is_an_error_not_an_empty_snapshot() {
        assert!(parse_task_list_envelope(r#"{"kind":"board","tasks":[]}"#, &[]).is_err());
        assert!(parse_task_list_envelope("not json", &[]).is_err());
    }

    #[test]
    fn missing_is_ready_and_status_fields_parse_conservatively() {
        let parsed = parse_task_list_envelope(
            r#"{"kind":"task-list","schemaVersion":1,"tasks":[{"id":"t","title":"x"}]}"#,
            &[],
        )
        .unwrap();
        let item = &parsed.items[0];
        assert_eq!(item.status, WorkStatus::Unknown("".into()));
        assert_eq!(item.provider_ready, None);
        assert!(!crate::core::compute_readiness(
            item,
            &std::collections::HashSet::from(["t".to_string()])
        ));
    }
}
