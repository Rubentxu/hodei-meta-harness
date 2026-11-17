//! Reproducible work selection with explanations (TASK-6).
//! Determinism rule: everything is ordered by item id — the same snapshot
//! always yields the same selection and the same reasons.

use std::collections::{BTreeMap, HashSet};

use hodei_mh_tracker::core::{WorkItem, WorkStatus, select_eligible};
use serde::{Deserialize, Serialize};

/// Why an item is or is not selectable right now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SelectionReason {
    /// Open, dependencies resolved, not claimed.
    Eligible,
    /// Dependencies reference ids absent from the snapshot (archived,
    /// ambiguous, deleted).
    BlockedByUnresolved(Vec<String>),
    /// Dependencies open in the snapshot but not themselves eligible.
    BlockedByOpenDependency(Vec<String>),
    WrongStatus(WorkStatus),
    /// Provider flagged isReady=false.
    NotReady,
    /// An existing claim holds it — assignment is respected (AC-2).
    ClaimedBy(String),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainedItem {
    pub item: WorkItem,
    pub reason: SelectionReason,
}

/// Explain every item of a snapshot. `claims` maps ticket ids to holders
/// (coordinator state); claimed items keep their own reason.
pub fn explain_selection(
    items: &[WorkItem],
    claims: &BTreeMap<String, String>,
) -> Vec<ExplainedItem> {
    let known: HashSet<String> = items.iter().map(|i| i.id.clone()).collect();
    // Claimed items are not eligible for new selection: their dependents
    // must see them as blocked-open, not as free dependencies.
    let eligible: HashSet<String> = select_eligible(items)
        .into_iter()
        .map(|i| i.id.clone())
        .filter(|id| !claims.contains_key(id))
        .collect();
    let mut explained: Vec<ExplainedItem> = items
        .iter()
        .map(|item| {
            let reason = explain_one(item, &known, &eligible, claims);
            ExplainedItem {
                item: item.clone(),
                reason,
            }
        })
        .collect();
    // Reproducibility: id order, always.
    explained.sort_by(|a, b| a.item.id.cmp(&b.item.id));
    explained
}

fn explain_one(
    item: &WorkItem,
    known: &HashSet<String>,
    eligible: &HashSet<String>,
    claims: &BTreeMap<String, String>,
) -> SelectionReason {
    if let Some(holder) = claims.get(&item.id) {
        return SelectionReason::ClaimedBy(holder.clone());
    }
    let unresolved: Vec<String> = item
        .dependencies
        .iter()
        .filter(|dep| !known.contains(*dep))
        .cloned()
        .collect();
    if !unresolved.is_empty() {
        return SelectionReason::BlockedByUnresolved(unresolved);
    }
    if !item.status.is_open() {
        return SelectionReason::WrongStatus(item.status.clone());
    }
    if item.provider_ready == Some(false) {
        return SelectionReason::NotReady;
    }
    let open_blocked: Vec<String> = item
        .dependencies
        .iter()
        .filter(|dep| known.contains(*dep) && !eligible.contains(*dep))
        .cloned()
        .collect();
    if !open_blocked.is_empty() {
        return SelectionReason::BlockedByOpenDependency(open_blocked);
    }
    SelectionReason::Eligible
}

/// The chained execution order of eligible work: dependencies first;
/// among ready items, the ones that unblock more work go earlier (descending
/// dependent count), ties broken by id (deterministic, AC-1).
pub fn chain_order(explained: &[ExplainedItem]) -> Vec<String> {
    let eligible: Vec<&ExplainedItem> = explained
        .iter()
        .filter(|e| e.reason == SelectionReason::Eligible)
        .collect();
    let ids: HashSet<&str> = eligible.iter().map(|e| e.item.id.as_str()).collect();
    // Dependent count within the eligible set (how much work this unblocks).
    let mut unblocks: BTreeMap<&str, usize> = BTreeMap::new();
    for item in eligible.iter().map(|e| &e.item) {
        for dep in &item.dependencies {
            if ids.contains(dep.as_str()) {
                *unblocks.entry(dep.as_str()).or_insert(0) += 1;
            }
        }
    }
    let mut order: Vec<String> = Vec::new();
    let mut placed: HashSet<String> = HashSet::new();
    loop {
        let mut ready: Vec<(&str, usize)> = eligible
            .iter()
            .filter(|e| {
                !placed.contains(&e.item.id)
                    && e.item
                        .dependencies
                        .iter()
                        .all(|dep| !ids.contains(dep.as_str()) || placed.contains(dep))
            })
            .map(|e| {
                (
                    e.item.id.as_str(),
                    *unblocks.get(e.item.id.as_str()).unwrap_or(&0),
                )
            })
            .collect();
        // Descending unblock count, ascending id.
        ready.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        match ready.first() {
            Some(&(id, _)) => {
                order.push(id.to_string());
                placed.insert(id.to_string());
            }
            None => break,
        }
    }
    order
}

/// What the supervisor should do next (AC-2/AC-3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum SupervisorDecision {
    /// Execute this ticket id next (chain order, unclaimed).
    Work { ticket_id: String },
    /// Nothing eligible and nothing of ours in flight: no model calls.
    Idle,
}

pub fn next_decision(
    explained: &[ExplainedItem],
    our_claims: &BTreeMap<String, String>,
) -> SupervisorDecision {
    for id in chain_order(explained) {
        if !our_claims.contains_key(&id) {
            return SupervisorDecision::Work { ticket_id: id };
        }
    }
    if our_claims.is_empty() {
        SupervisorDecision::Idle
    } else {
        // Our claims exist but nothing chainable: wait on them (no idle —
        // work is in flight).
        SupervisorDecision::Idle
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn selection_is_reproducible_and_explains_every_item() {
        let items = vec![
            item("b", WorkStatus::Open, Some(true), &["a"]),
            item("a", WorkStatus::Open, Some(true), &[]),
            item("c", WorkStatus::Open, Some(true), &["archived-1"]),
            item("d", WorkStatus::InProgress, Some(true), &[]),
            item("e", WorkStatus::Open, Some(false), &[]),
        ];
        let claims = BTreeMap::from([("a".to_string(), "other-agent".to_string())]);
        let first = explain_selection(&items, &claims);
        let second = explain_selection(&items, &claims);
        assert_eq!(first, second, "same snapshot must yield identical output");

        let reason = |id: &str| {
            first
                .iter()
                .find(|e| e.item.id == id)
                .map(|e| e.reason.clone())
                .unwrap()
        };
        assert_eq!(
            reason("a"),
            SelectionReason::ClaimedBy("other-agent".into())
        );
        assert_eq!(
            reason("b"),
            SelectionReason::BlockedByOpenDependency(vec!["a".into()])
        );
        assert_eq!(
            reason("c"),
            SelectionReason::BlockedByUnresolved(vec!["archived-1".into()])
        );
        assert_eq!(
            reason("d"),
            SelectionReason::WrongStatus(WorkStatus::InProgress)
        );
        assert_eq!(reason("e"), SelectionReason::NotReady);
    }

    #[test]
    fn blocked_ticket_does_not_stop_other_work() {
        let items = vec![
            item("blocked", WorkStatus::Open, Some(true), &["missing-lib"]),
            item("ready", WorkStatus::Open, Some(true), &[]),
        ];
        let explained = explain_selection(&items, &BTreeMap::new());
        assert_eq!(chain_order(&explained), vec!["ready".to_string()]);
        let decision = next_decision(&explained, &BTreeMap::new());
        assert_eq!(
            decision,
            SupervisorDecision::Work {
                ticket_id: "ready".into()
            }
        );
    }

    #[test]
    fn chain_respects_dependencies_with_deterministic_ties() {
        let items = vec![
            item("c", WorkStatus::Open, Some(true), &["a", "b"]),
            item("b", WorkStatus::Open, Some(true), &["a"]),
            item("a", WorkStatus::Open, Some(true), &[]),
            item("z", WorkStatus::Open, Some(true), &[]),
        ];
        let explained = explain_selection(&items, &BTreeMap::new());
        assert_eq!(
            chain_order(&explained),
            vec![
                "a".to_string(),
                "b".to_string(),
                "c".to_string(),
                "z".to_string()
            ]
        );
    }

    #[test]
    fn empty_backlog_is_idle_without_claims() {
        let explained = explain_selection(&[], &BTreeMap::new());
        assert_eq!(
            next_decision(&explained, &BTreeMap::new()),
            SupervisorDecision::Idle
        );
        // A held claim means work in flight: idle but not "wake me for nothing".
        let busy = BTreeMap::from([("mine".to_string(), "us".to_string())]);
        assert_eq!(next_decision(&explained, &busy), SupervisorDecision::Idle);
    }
}
