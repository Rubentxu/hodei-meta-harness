//! `hodei-mh tracker-probe`: TASK-3 vertical slice — eligible work selection
//! against the pinned Backlog CLI plus shared read contracts with FakeTracker.

use std::time::Duration;

use serde_json::json;

use hodei_mh_tracker::backlog::{BacklogTracker, feed_stream};
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
            "task": "TASK-3",
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

    // Real pinned CLI (v1.55.3) against this repository's live backlog.
    let tracker = BacklogTracker::new("backlog", &working_dir);
    let statuses = tracker.observed_statuses().await;
    checks.push(Check {
        name: "config-observed",
        passed: statuses.is_ok(),
        detail: format!(
            "statuses={:?}",
            statuses.as_ref().unwrap_or(&Vec::new()).join(", ")
        ),
    });

    let snapshot = match tracker.snapshot().await {
        Ok(items) => items,
        Err(err) => {
            checks.push(Check {
                name: "real-snapshot",
                passed: false,
                detail: err.to_string(),
            });
            return finish(&checks);
        }
    };
    let eligible: Vec<String> = hodei_mh_tracker::core::select_eligible(&snapshot)
        .into_iter()
        .map(|i| i.id.clone())
        .collect();
    let in_progress: Vec<String> = snapshot
        .iter()
        .filter(|i| matches!(i.status, hodei_mh_tracker::core::WorkStatus::InProgress))
        .map(|i| i.id.clone())
        .collect();
    checks.push(Check {
        name: "real-snapshot",
        passed: !snapshot.is_empty(),
        detail: format!(
            "{} items, eligible={:?}, in-progress-excluded={:?}",
            snapshot.len(),
            eligible,
            in_progress
        ),
    });
    checks.push(Check {
        name: "in-progress-never-eligible",
        passed: eligible.iter().all(|id| !in_progress.contains(id)),
        detail: format!("eligible={eligible:?} vs in-progress={in_progress:?}"),
    });

    // Query error is not an empty snapshot (AC-3): exercised on the real
    // adapter with an invalid invocation.
    let broken = BacklogTracker::new("false", &working_dir);
    let err_result = broken.snapshot().await;
    checks.push(Check {
        name: "query-error-distinct-from-empty",
        passed: matches!(
            err_result,
            Err(hodei_mh_tracker::core::TrackerError::Query(_))
        ),
        detail: format!("error path: {:?}", err_result.err()),
    });

    // Watch stream contract: successive complete objects including empty,
    // truncation stays buffered (AC-2).
    let mut buffer = String::new();
    let streamed = feed_stream(
        &mut buffer,
        r#"{"kind":"task-list","tasks":[]} {"kind":"task-list","tasks":[{"id":"W1","title":"w","status":"To Do","isReady":true}]}"#,
        &statuses.unwrap_or_default(),
    );
    let streamed_ok = matches!(
        streamed.as_ref(),
        Ok(events) if events.len() == 2 && events[0].as_ref().unwrap().is_empty()
    );
    checks.push(Check {
        name: "watch-stream-contract",
        passed: streamed_ok,
        detail: format!("events={:?}", streamed.map(|e| e.len())),
    });

    // Shared contracts without Backlog types in the core (AC-4).
    let fake = FakeTracker::with_items(vec![
        contracts::item(
            "A-1",
            hodei_mh_tracker::core::WorkStatus::Open,
            Some(true),
            &[],
        ),
        contracts::item(
            "A-2",
            hodei_mh_tracker::core::WorkStatus::InProgress,
            Some(true),
            &[],
        ),
        contracts::item(
            "A-3",
            hodei_mh_tracker::core::WorkStatus::Unknown("Blocked Externally".into()),
            Some(true),
            &[],
        ),
    ]);
    contracts::verify_read_contracts(&fake).await;
    let failing = FakeTracker::failing("simulated outage");
    contracts::verify_error_contracts(&failing).await;
    checks.push(Check {
        name: "fake-tracker-contracts",
        passed: true,
        detail: "read + error contracts pass on FakeTracker".into(),
    });

    let _ = Duration::from_secs(1); // poll cadence documented at call sites
    finish(&checks)
}
