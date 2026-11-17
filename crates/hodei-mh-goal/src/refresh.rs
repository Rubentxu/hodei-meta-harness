//! Refresh semantics (AC-4): an externally completed or changed work item
//! triggers revalidation, and a visibility loss is never reported as a
//! deletion.

use serde::{Deserialize, Serialize};

use hodei_mh_tracker::core::WorkItem;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Disappearance {
    /// The provider was queried successfully (snapshot + archive search) and
    /// the item is genuinely gone.
    Deleted,
    /// The provider could not be fully queried: absence is not evidence.
    VisibilityLoss,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RefreshReport {
    /// Present before, Done (or non-open) now.
    pub externally_completed: Vec<String>,
    /// Present before and after but with changed eligibility inputs.
    pub changed: Vec<String>,
    /// Present before, absent now, classified honestly.
    pub disappeared: Vec<(String, Disappearance)>,
}

/// Compare the previous snapshot against the refresh inputs.
/// `current` is the fresh snapshot query result — an `Err` means the query
/// itself failed, so nothing may be classified as deleted.
/// `archive_search_ok` (when `Some`) reports whether an explicit
/// search-in-archive/provider lookup ran and found the item (`false`).
pub fn refresh(
    previous: &[WorkItem],
    current: Result<&[WorkItem], &str>,
    archive_search_ok: Option<bool>,
) -> RefreshReport {
    let mut report = RefreshReport {
        externally_completed: Vec::new(),
        changed: Vec::new(),
        disappeared: Vec::new(),
    };
    let Ok(current_items) = current else {
        // Query failed: every disappearance is a visibility loss.
        report.disappeared = previous
            .iter()
            .map(|item| (item.id.clone(), Disappearance::VisibilityLoss))
            .collect();
        return report;
    };
    let now: std::collections::HashMap<&str, &WorkItem> =
        current_items.iter().map(|i| (i.id.as_str(), i)).collect();
    for before in previous {
        match now.get(before.id.as_str()) {
            Some(after) => {
                let completed = before.status.is_open() && !after.status.is_open();
                if completed {
                    // Completion subsumes "changed": it is the more specific report.
                    report.externally_completed.push(before.id.clone());
                    continue;
                }
                if before.status != after.status
                    || before.provider_ready != after.provider_ready
                    || before.dependencies != after.dependencies
                {
                    report.changed.push(before.id.clone());
                }
            }
            None => match archive_search_ok {
                // Explicit lookup ran and did not find it: really deleted.
                Some(false) => report
                    .disappeared
                    .push((before.id.clone(), Disappearance::Deleted)),
                // Lookup found it (hidden by filters) or never ran.
                _ => report
                    .disappeared
                    .push((before.id.clone(), Disappearance::VisibilityLoss)),
            },
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use hodei_mh_tracker::core::WorkStatus;

    fn item(id: &str, status: WorkStatus, ready: Option<bool>, deps: &[&str]) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("work {id}"),
            status,
            provider_ready: ready,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn external_completion_and_change_are_detected() {
        let previous = vec![
            item("done-by-human", WorkStatus::Open, Some(true), &[]),
            item("changed", WorkStatus::Open, Some(true), &[]),
            item("stable", WorkStatus::Open, Some(true), &[]),
        ];
        let current = vec![
            item("done-by-human", WorkStatus::Done, Some(true), &[]),
            item("changed", WorkStatus::Open, Some(false), &["new-dep"]),
            item("stable", WorkStatus::Open, Some(true), &[]),
        ];
        let report = refresh(&previous, Ok(&current), None);
        assert_eq!(
            report.externally_completed,
            vec!["done-by-human".to_string()]
        );
        assert_eq!(report.changed, vec!["changed".to_string()]);
        assert!(report.disappeared.is_empty());
    }

    #[test]
    fn failed_query_is_visibility_loss_never_deletion() {
        let previous = vec![item("x", WorkStatus::Open, Some(true), &[])];
        let report = refresh(&previous, Err("tracker outage"), Some(false));
        assert_eq!(
            report.disappeared,
            vec![("x".to_string(), Disappearance::VisibilityLoss)]
        );
    }

    #[test]
    fn absence_without_archive_lookup_is_visibility_loss() {
        let previous = vec![item("gone", WorkStatus::Open, Some(true), &[])];
        // No archive search ran (None): not proven deleted.
        let report = refresh(&previous, Ok(&[]), None);
        assert_eq!(
            report.disappeared,
            vec![("gone".to_string(), Disappearance::VisibilityLoss)]
        );
        // Explicit search ran and found nothing: deletion.
        let report = refresh(&previous, Ok(&[]), Some(false));
        assert_eq!(
            report.disappeared,
            vec![("gone".to_string(), Disappearance::Deleted)]
        );
    }
}
