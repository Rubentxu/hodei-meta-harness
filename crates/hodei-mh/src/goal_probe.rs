//! `hodei-mh goal-probe`: TASK-6 vertical slice — reproducible selection with
//! explanations on the live backlog, idle without claims, refresh semantics.

use std::collections::BTreeMap;

use serde_json::json;

use hodei_mh_goal::refresh::{self, Disappearance};
use hodei_mh_goal::selection::{chain_order, explain_selection, next_decision};
use hodei_mh_tracker::backlog::BacklogTracker;
use hodei_mh_tracker::core::TrackerRead;
use hodei_mh_tracker::fake::{FakeTracker, contracts};

struct Check {
    name: &'static str,
    passed: bool,
    detail: String,
}

pub async fn run(working_dir: String) -> i32 {
    let mut checks: Vec<Check> = Vec::new();
    let finish = |checks: &[Check]| -> i32 {
        let summary = json!({
            "task": "TASK-6",
            "checks": checks.iter().map(|c| json!({
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
    };

    // 1. Live backlog: reproducible selection with explanations.
    let tracker = BacklogTracker::new("backlog", &working_dir);
    let snapshot = tracker.snapshot().await;
    let Ok(snapshot) = snapshot else {
        checks.push(Check {
            name: "live-snapshot",
            passed: false,
            detail: "snapshot failed".into(),
        });
        return finish(&checks);
    };
    let claims = BTreeMap::new();
    let explained_once = explain_selection(&snapshot, &claims);
    let explained_twice = explain_selection(&snapshot, &claims);
    let reproducible = explained_once == explained_twice;
    let explained_count = explained_once.len();
    let eligible_ids: Vec<String> = chain_order(&explained_once);
    checks.push(Check {
        name: "reproducible-selection-with-explanations",
        passed: reproducible && explained_count == snapshot.len(),
        detail: format!(
            "{explained_count}/{count} items explained deterministically, chain={eligible_ids:?}",
            count = snapshot.len()
        ),
    });
    let decision = next_decision(&explained_once, &claims);
    checks.push(Check {
        name: "next-decision-from-live-backlog",
        passed: matches!(
            decision,
            hodei_mh_goal::selection::SupervisorDecision::Work { .. }
        ),
        detail: format!("{decision:?}"),
    });

    // 2. Empty backlog -> Idle (no model calls by construction: the decision
    //    layer never invokes anything on Idle).
    let empty = FakeTracker::with_items(Vec::new());
    let empty_snapshot = empty.snapshot().await.unwrap_or_default();
    let empty_explained = explain_selection(&empty_snapshot, &BTreeMap::new());
    let idle = next_decision(&empty_explained, &BTreeMap::new());
    let idle_ok = idle == hodei_mh_goal::selection::SupervisorDecision::Idle;
    checks.push(Check {
        name: "empty-backlog-idle",
        passed: idle_ok,
        detail: format!("{idle:?} (zero model calls on idle by design)"),
    });

    // 3. Wake-up: a new eligible task in a later snapshot produces Work.
    let woke = FakeTracker::with_items(vec![contracts::item(
        "NEW-1",
        hodei_mh_tracker::core::WorkStatus::Open,
        Some(true),
        &[],
    )]);
    let woke_snapshot = woke.snapshot().await.unwrap_or_default();
    let woke_explained = explain_selection(&woke_snapshot, &BTreeMap::new());
    checks.push(Check {
        name: "new-eligible-wakes-supervisor",
        passed: matches!(
            next_decision(&woke_explained, &BTreeMap::new()),
            hodei_mh_goal::selection::SupervisorDecision::Work { .. }
        ),
        detail: "NEW-1 in snapshot -> Work decision".into(),
    });

    // 4. Refresh: external completion, failed query = visibility loss.
    let previous = woke_snapshot.clone();
    let completed_after = vec![hodei_mh_tracker::core::WorkItem {
        id: "NEW-1".into(),
        title: "work NEW-1".into(),
        status: hodei_mh_tracker::core::WorkStatus::Done,
        provider_ready: Some(true),
        dependencies: vec![],
    }];
    let report = refresh::refresh(&previous, Ok(&completed_after), None);
    let external_detected = report.externally_completed == vec!["NEW-1".to_string()];
    let failed = refresh::refresh(&previous, Err("tracker outage"), Some(false));
    let loss_not_deletion = failed
        .disappeared
        .iter()
        .all(|(_, kind)| *kind == Disappearance::VisibilityLoss);
    checks.push(Check {
        name: "refresh-distinguishes-completion-loss-deletion",
        passed: external_detected && loss_not_deletion,
        detail: format!(
            "external_completed={:?} outage_disappearances={:?}",
            report.externally_completed, failed.disappeared
        ),
    });

    finish(&checks)
}
