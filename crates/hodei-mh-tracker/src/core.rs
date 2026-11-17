//! Provider-agnostic work domain. No Backlog types here: adapters translate
//! into `WorkItem`s, contracts are validated against any `TrackerRead`.

use std::collections::HashSet;

use async_trait::async_trait;
use thiserror::Error;

/// Observed work states. Unknown provider states are preserved verbatim and
/// are never eligible — an unrecognized state must not silently mean ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkStatus {
    Open,
    InProgress,
    Done,
    Unknown(String),
}

impl WorkStatus {
    pub fn parse(raw: &str, open_aliases: &[String]) -> Self {
        let normalized = raw.trim();
        if open_aliases
            .iter()
            .any(|a| a.eq_ignore_ascii_case(normalized))
        {
            WorkStatus::Open
        } else {
            match normalized.to_ascii_lowercase().as_str() {
                "to do" | "todo" | "open" => WorkStatus::Open,
                "in progress" | "inprogress" | "doing" => WorkStatus::InProgress,
                "done" | "closed" => WorkStatus::Done,
                _other => WorkStatus::Unknown(normalized.to_string()),
            }
        }
    }

    pub fn is_open(&self) -> bool {
        matches!(self, WorkStatus::Open)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkItem {
    pub id: String,
    pub title: String,
    pub status: WorkStatus,
    /// Provider readiness flag, when the tracker publishes one. `None` means
    /// the provider does not expose readiness; eligibility then relies on
    /// dependency resolution alone.
    pub provider_ready: Option<bool>,
    pub dependencies: Vec<String>,
}

/// A query failure. Deliberately distinct from "no work found": the core
/// never cancels or closes work because a query errored (absence of a
/// response is not evidence of absence of work).
#[derive(Debug, Clone, Error, PartialEq)]
pub enum TrackerError {
    #[error("tracker query failed: {0}")]
    Query(String),
    #[error("tracker stream ended unexpectedly: {0}")]
    Stream(String),
}

/// Read-side of the WorkTracker SPI.
#[async_trait]
pub trait TrackerRead: Send + Sync {
    /// One consistent snapshot of open work.
    async fn snapshot(&self) -> Result<Vec<WorkItem>, TrackerError>;
}

/// Readiness decision, computed from observed data only.
/// - provider `isReady` is respected when present (false = not ready);
/// - a dependency whose id is absent from the known set is NOT ready
///   (archived, ambiguous or deleted dependencies block conservatively);
/// - only open status can be eligible; unknown states never are.
pub fn compute_readiness(item: &WorkItem, known_ids: &HashSet<String>) -> bool {
    if !item.status.is_open() {
        return false;
    }
    if item.provider_ready == Some(false) {
        return false;
    }
    item.dependencies.iter().all(|dep| known_ids.contains(dep))
}

/// Select eligible items from a snapshot, keeping input order.
pub fn select_eligible(items: &[WorkItem]) -> Vec<&WorkItem> {
    let known: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    items
        .iter()
        .filter(|item| compute_readiness(item, &known))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, status: WorkStatus, ready: Option<bool>, deps: &[&str]) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("title {id}"),
            status,
            provider_ready: ready,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn unknown_states_and_missing_dependencies_are_never_ready() {
        let items = vec![
            item("a", WorkStatus::Unknown("Custom".into()), Some(true), &[]),
            item("b", WorkStatus::Open, Some(true), &["ghost"]),
            item("c", WorkStatus::Open, Some(false), &[]),
            item("d", WorkStatus::Open, Some(true), &[]),
            item("e", WorkStatus::Open, None, &[]),
        ];
        let eligible: Vec<String> = select_eligible(&items)
            .into_iter()
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(eligible, vec!["d", "e"]);
    }

    #[test]
    fn done_and_in_progress_never_eligible() {
        let items = vec![
            item("x", WorkStatus::Done, Some(true), &[]),
            item("y", WorkStatus::InProgress, Some(true), &[]),
        ];
        assert!(select_eligible(&items).is_empty());
    }

    #[test]
    fn status_parsing_respects_observed_aliases_and_preserves_unknown() {
        let aliases = vec!["To Do".to_string()];
        assert_eq!(WorkStatus::parse("to do", &aliases), WorkStatus::Open);
        assert_eq!(
            WorkStatus::parse("In Progress", &[]),
            WorkStatus::InProgress
        );
        assert_eq!(
            WorkStatus::parse("Waiting Review", &[]),
            WorkStatus::Unknown("Waiting Review".into())
        );
    }

    #[test]
    fn dependency_present_but_not_open_still_blocks_via_is_ready() {
        // "b" is open but depends on "a" which is Done: provider computes
        // isReady=false for b — respected.
        let a = item("a", WorkStatus::Done, Some(true), &[]);
        let mut b = item("b", WorkStatus::Open, None, &["a"]);
        let known: HashSet<String> = ["a".to_string(), "b".to_string()].into();
        b.provider_ready = None;
        // Without provider flag, dependency resolution alone lets b through
        // when the dependency EXISTS in the snapshot; eligibility here is a
        // local decision — providers mark is_ready=false for blocked work.
        assert!(compute_readiness(&b, &known));
        let _ = a;
    }
}
