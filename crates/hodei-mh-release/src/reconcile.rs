//! Create reconciliation (AC-1): a timeout or disconnection AFTER a create
//! leaves the effect UNKNOWN. Before trying again, the request tag is
//! reconciled by metadata/labels: an existing sandbox with OUR tag is
//! adopted, never duplicated. Orphans are cleaned only when they carry the
//! hodei-mh ownership tag.

use serde::{Deserialize, Serialize};

/// The ownership tag every hodei-mh environment carries. Cleaning respects
/// it strictly: untagged foreign environments are NEVER touched.
pub const OWNERSHIP_TAG: &str = "managed-by=hodei-mh";

/// Per-request tag stored in the environment metadata at create time,
/// making the effect findable after a lost response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RequestTag(pub String);

#[derive(Debug, Clone, PartialEq)]
pub enum CreateOutcome {
    /// The create response arrived with an id.
    Confirmed { id: String },
    /// The create was sent but the response was lost (timeout/disconnect).
    Unknown { tag: RequestTag },
}

#[derive(Debug, Clone, PartialEq)]
pub enum ReconcileDecision {
    /// The effect already happened: adopt it, do NOT create another.
    AdoptExisting { id: String },
    /// No trace of the effect: a retry is safe (no duplicate possible).
    RetrySafe,
    /// The environment is gone in a way that must not be retried blindly.
    Abort { reason: String },
}

/// Reconcile an unknown create outcome: search by the request tag before
/// any retry (AC-1).
pub fn reconcile_create(
    outcome: CreateOutcome,
    find_by_tag: impl Fn(&RequestTag) -> Option<String>,
) -> ReconcileDecision {
    match outcome {
        CreateOutcome::Confirmed { id } => ReconcileDecision::AdoptExisting { id },
        CreateOutcome::Unknown { tag } => match find_by_tag(&tag) {
            Some(id) => ReconcileDecision::AdoptExisting { id },
            None => ReconcileDecision::RetrySafe,
        },
    }
}

/// One environment observed in the wild.
#[derive(Debug, Clone, PartialEq)]
pub struct EnvironmentMeta {
    pub id: String,
    /// Whether it carries `managed-by=hodei-mh`.
    pub owned_by_us: bool,
    /// Whether an active run claims it right now.
    pub claimed: bool,
    /// Age of the last activity.
    pub idle_for_secs: u64,
}

/// Orphan cleanup: ONLY hodei-mh-owned, unclaimed, long-idle environments
/// are returned for release. Anything else is left alone.
pub fn orphans_to_release(
    environments: &[EnvironmentMeta],
    idle_threshold_secs: u64,
) -> Vec<String> {
    environments
        .iter()
        .filter(|e| e.owned_by_us && !e.claimed && e.idle_for_secs >= idle_threshold_secs)
        .map(|e| e.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_create_adopts_existing_effect_by_tag() {
        let decision = reconcile_create(
            CreateOutcome::Unknown {
                tag: RequestTag("req-7".into()),
            },
            |tag| {
                if tag.0 == "req-7" {
                    Some("sbx-found".into())
                } else {
                    None
                }
            },
        );
        assert_eq!(
            decision,
            ReconcileDecision::AdoptExisting {
                id: "sbx-found".into()
            }
        );
    }

    #[test]
    fn no_trace_makes_retry_safe_confirmed_never_retries() {
        let retry = reconcile_create(
            CreateOutcome::Unknown {
                tag: RequestTag("req-8".into()),
            },
            |_| None,
        );
        assert_eq!(retry, ReconcileDecision::RetrySafe);

        let confirmed = reconcile_create(CreateOutcome::Confirmed { id: "sbx-1".into() }, |_| {
            panic!("confirmed outcomes must not search")
        });
        assert_eq!(
            confirmed,
            ReconcileDecision::AdoptExisting { id: "sbx-1".into() }
        );
    }

    #[test]
    fn orphan_cleanup_never_touches_foreign_or_claimed_or_fresh() {
        let envs = vec![
            EnvironmentMeta {
                id: "ours-idle".into(),
                owned_by_us: true,
                claimed: false,
                idle_for_secs: 3600,
            },
            EnvironmentMeta {
                id: "foreign".into(),
                owned_by_us: false,
                claimed: false,
                idle_for_secs: 3600,
            },
            EnvironmentMeta {
                id: "ours-claimed".into(),
                owned_by_us: true,
                claimed: true,
                idle_for_secs: 3600,
            },
            EnvironmentMeta {
                id: "ours-fresh".into(),
                owned_by_us: true,
                claimed: false,
                idle_for_secs: 10,
            },
        ];
        let targets = orphans_to_release(&envs, 1800);
        assert_eq!(targets, vec!["ours-idle".to_string()]);
    }
}
