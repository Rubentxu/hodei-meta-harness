//! FakeTracker: the same read contracts, verified without any Backlog types
//! in the core (AC-4).

use async_trait::async_trait;

use crate::core::{TrackerError, TrackerRead, WorkItem};

#[derive(Default)]
pub struct FakeTracker {
    pub items: Vec<WorkItem>,
    pub fail_with: Option<TrackerError>,
}

impl FakeTracker {
    pub fn with_items(items: Vec<WorkItem>) -> Self {
        Self {
            items,
            fail_with: None,
        }
    }

    pub fn failing(message: &str) -> Self {
        Self {
            items: Vec::new(),
            fail_with: Some(TrackerError::Query(message.to_string())),
        }
    }
}

#[async_trait]
impl TrackerRead for FakeTracker {
    async fn snapshot(&self) -> Result<Vec<WorkItem>, TrackerError> {
        match &self.fail_with {
            Some(err) => Err(err.clone()),
            None => Ok(self.items.clone()),
        }
    }
}

/// Contracts every TrackerRead implementation must satisfy — executed against
/// FakeTracker here and against BacklogTracker in integration tests.
pub mod contracts {
    use std::collections::HashSet;

    use crate::core::{
        TrackerError, TrackerRead, WorkItem, WorkStatus, compute_readiness, select_eligible,
    };

    pub fn item(id: &str, status: WorkStatus, ready: Option<bool>, deps: &[&str]) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("work {id}"),
            status,
            provider_ready: ready,
            dependencies: deps.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Distinct identities and states drive the same contracts everywhere.
    pub async fn verify_read_contracts(tracker: &dyn TrackerRead) {
        // Contract 1: snapshot returns identity-stable items.
        let snapshot = tracker.snapshot().await.expect("snapshot");
        assert_eq!(snapshot.len(), 3);
        assert_eq!(snapshot[0].id, "A-1");
        assert_eq!(
            snapshot[2].status,
            WorkStatus::Unknown("Blocked Externally".into())
        );

        // Contract 2: eligibility respects observed state and readiness.
        let eligible: Vec<String> = select_eligible(&snapshot)
            .into_iter()
            .map(|i| i.id.clone())
            .collect();
        assert_eq!(eligible, vec!["A-1"]);

        // Contract 3: unresolved dependency blocks even with isReady=true.
        let ghost = item("A-3", WorkStatus::Open, Some(true), &["NOPE"]);
        let known: HashSet<String> = HashSet::new();
        assert!(!compute_readiness(&ghost, &known));
    }

    /// Contract 4: a query error is an error, never an empty snapshot.
    pub async fn verify_error_contracts(tracker: &dyn TrackerRead) {
        let result = tracker.snapshot().await;
        assert!(matches!(result, Err(TrackerError::Query(_))));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::WorkStatus;

    #[tokio::test]
    async fn fake_tracker_passes_the_shared_contracts() {
        let tracker = FakeTracker::with_items(vec![
            contracts::item("A-1", WorkStatus::Open, Some(true), &[]),
            contracts::item("A-2", WorkStatus::InProgress, Some(true), &[]),
            contracts::item(
                "A-3",
                WorkStatus::Unknown("Blocked Externally".into()),
                Some(true),
                &[],
            ),
        ]);
        contracts::verify_read_contracts(&tracker).await;

        let failing = FakeTracker::failing("simulated outage");
        contracts::verify_error_contracts(&failing).await;
    }
}
