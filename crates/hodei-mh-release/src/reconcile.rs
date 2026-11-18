//! Create reconciliation (AC-1): a timeout or disconnection AFTER a create
//! leaves the effect UNKNOWN. Before trying again, the request tag is
//! reconciled by metadata/labels: an existing sandbox with OUR tag is
//! adopted, never duplicated. Orphans are cleaned only when they carry the
//! hodei-mh ownership tag.

use serde::{Deserialize, Serialize};

/// The ownership tag every hodei-mh environment carries. Cleaning respects
/// it strictly: untagged foreign environments are NEVER touched.
pub const OWNERSHIP_TAG: &str = "managed-by=hodei-mh";

/// The metadata KEY whose value names the owner. The value is
/// [`OWNER_VALUE`]; the pair is what identifies a hodei-mh environment.
pub const OWNERSHIP_KEY: &str = "managed-by";

/// The owner value written into the metadata of every hodei-mh sandbox.
pub const OWNER_VALUE: &str = "hodei-mh";

/// The metadata key carrying the per-request tag.
pub const REQUEST_TAG_KEY: &str = "request-tag";

/// Whether a metadata/labels map belongs to a hodei-mh environment.
pub fn is_owned_by_us(metadata: &std::collections::BTreeMap<String, String>) -> bool {
    metadata.get(OWNERSHIP_KEY).map(String::as_str) == Some(OWNER_VALUE)
}

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

/// The result of looking the request tag up.
#[derive(Debug, Clone, PartialEq)]
pub enum TagLookup {
    /// The search worked: this is the sandbox that carries the tag.
    Found { id: String },
    /// The search worked and no sandbox carries the tag: the effect
    /// really did not happen.
    NotFound,
    /// The search itself FAILED. "I could not ask" is NOT "it is not
    /// there": treating it as such licenses a duplicate.
    Failed { reason: String },
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
///
/// A FAILED search is never a license to retry: the effect may well have
/// happened, and a retry would duplicate it.
pub fn reconcile_create(
    outcome: CreateOutcome,
    find_by_tag: impl Fn(&RequestTag) -> TagLookup,
) -> ReconcileDecision {
    match outcome {
        CreateOutcome::Confirmed { id } => ReconcileDecision::AdoptExisting { id },
        CreateOutcome::Unknown { tag } => match find_by_tag(&tag) {
            TagLookup::Found { id } => ReconcileDecision::AdoptExisting { id },
            TagLookup::NotFound => ReconcileDecision::RetrySafe,
            TagLookup::Failed { reason } => ReconcileDecision::Abort {
                reason: format!("could not search for the request tag: {reason}"),
            },
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
    /// Age of the last activity, derived from the environment's real
    /// creation time. A caller that cannot compute it does NOT pass 0
    /// (which would look instantly idle): it uses [`UnknownIdle`].
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
    use std::collections::BTreeMap;

    #[test]
    fn unknown_create_adopts_existing_effect_by_tag() {
        let decision = reconcile_create(
            CreateOutcome::Unknown {
                tag: RequestTag("req-7".into()),
            },
            |tag| {
                if tag.0 == "req-7" {
                    TagLookup::Found {
                        id: "sbx-found".into(),
                    }
                } else {
                    TagLookup::NotFound
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
            |_| TagLookup::NotFound,
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
    fn a_failed_search_never_licenses_a_duplicate() {
        let decision = reconcile_create(
            CreateOutcome::Unknown {
                tag: RequestTag("req-9".into()),
            },
            |_| TagLookup::Failed {
                reason: "server restarting".into(),
            },
        );
        match decision {
            ReconcileDecision::Abort { reason } => {
                assert!(reason.contains("could not search"), "{reason}");
            }
            other => panic!("a failed search must abort, got {other:?}"),
        }
    }

    #[test]
    fn ownership_is_read_from_the_metadata_pair() {
        let mut ours = BTreeMap::new();
        ours.insert(OWNERSHIP_KEY.to_string(), OWNER_VALUE.to_string());
        assert!(is_owned_by_us(&ours));

        let mut foreign = BTreeMap::new();
        foreign.insert(OWNERSHIP_KEY.to_string(), "someone-else".to_string());
        assert!(!is_owned_by_us(&foreign));

        let mut impostor = BTreeMap::new();
        impostor.insert("managed-by-hodei-mh".to_string(), String::new());
        assert!(!is_owned_by_us(&impostor));

        assert!(!is_owned_by_us(&BTreeMap::new()));
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
