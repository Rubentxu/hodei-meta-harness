//! The claim store of the live runtime: the coordinator's durable state.
//!
//! The claim is the single-writer fact of an attempt. It is taken BEFORE
//! any effect and given back exactly once, whatever the attempt ends
//! with; the runtime's guard owns the release, so an aborted attempt
//! releases it too.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hodei_mh_coordinator::core::ClaimResult;
use hodei_mh_coordinator::store::{CoordinatorStore, StoreError};
use runner::live::{ClaimError, ClaimPort, ClaimReleaseError, Claimed};

/// The holder the live runtime claims work as.
pub const LIVE_HOLDER: &str = "supervisor-run";

/// Why the claim store could not be opened.
#[derive(Debug, thiserror::Error)]
pub enum ClaimsError {
    #[error("the state directory could not be created: {0}")]
    State(#[from] std::io::Error),
    #[error("the coordinator state could not be opened: {0}")]
    Store(#[from] StoreError),
}

/// The claims of a project, over the coordinator's durable store.
pub struct StoreClaims {
    store: Mutex<CoordinatorStore>,
    holder: String,
}

impl StoreClaims {
    /// Open the durable claims of `project` at `path`.
    pub fn open(project: &str, path: &Path, holder: &str) -> Result<Self, ClaimsError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            store: Mutex::new(CoordinatorStore::open(project, path)?),
            holder: holder.to_string(),
        })
    }
}

impl ClaimPort for StoreClaims {
    fn claim(&self, work_ref: &str) -> Result<Claimed, ClaimError> {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        let result = store
            .transact(|state| Ok(state.try_claim(work_ref, &self.holder)))
            .map_err(|error| ClaimError::Unavailable {
                reason: error.to_string(),
            })?;
        match result {
            ClaimResult::Granted(claim) => Ok(Claimed { epoch: claim.epoch }),
            ClaimResult::Denied { holder } => Err(ClaimError::Denied { holder }),
        }
    }

    /// Give the work back. A store that cannot transact the release is
    /// reported, never ignored: the work would stay held under this
    /// holder with no record saying so.
    fn release(&self, work_ref: &str, epoch: u64) -> Result<(), ClaimReleaseError> {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        // Only the attempt that owns the claim releases it: a stale
        // attempt must not give back work somebody else now holds. Such
        // a mismatch is a no-op, not a failure: the claim of THIS epoch
        // was never ours to give back.
        store
            .transact(|state| {
                if state.claim_of(work_ref).map(|claim| claim.epoch) == Some(epoch) {
                    state.release(work_ref, &self.holder);
                }
                Ok(())
            })
            .map_err(|error| ClaimReleaseError {
                work_ref: work_ref.to_string(),
                epoch,
                reason: error.to_string(),
            })
    }

    /// The claims the coordinator holds right now.
    ///
    /// A store that cannot be read is an ERROR, never an empty snapshot:
    /// selection over unseen claims would pick work somebody else owns.
    fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
        let store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        let state = store.load().map_err(|error| ClaimError::Unavailable {
            reason: error.to_string(),
        })?;
        Ok(state
            .claims
            .iter()
            .map(|(ticket, claim)| (ticket.clone(), claim.holder.clone()))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .join(format!("hodei-live-claims-{}-{tag}", std::process::id()))
            .join("coordinator.db")
    }

    fn claims(tag: &str) -> (StoreClaims, std::path::PathBuf) {
        let path = state_path(tag);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
        let claims = StoreClaims::open("project", &path, LIVE_HOLDER).expect("the store opens");
        (claims, path)
    }

    #[test]
    fn a_claimed_work_is_denied_to_the_next_claim_and_released_once() {
        let (claims, path) = claims("release");
        let first = claims.claim("T-1").expect("the work is free");
        assert_eq!(first.epoch, 1);
        assert!(matches!(
            claims.claim("T-1"),
            Err(ClaimError::Denied { ref holder }) if holder == LIVE_HOLDER
        ));
        claims
            .release("T-1", first.epoch)
            .expect("the release completes");
        let second = claims
            .claim("T-1")
            .expect("the released work is free again");
        assert_eq!(second.epoch, 2, "every grant advances the fencing epoch");
        claims
            .release("T-1", second.epoch)
            .expect("the release completes");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn the_held_claims_are_reported_for_selection() {
        let (claims, path) = claims("snapshot");
        claims.claim("T-1").expect("the work is free");
        let held = claims.claims().expect("the snapshot is readable");
        assert_eq!(held.get("T-1").map(String::as_str), Some(LIVE_HOLDER));
        assert!(!held.contains_key("T-2"), "{held:?}");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_restart_that_finds_a_claim_does_not_duplicate_it() {
        let path = state_path("restart");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
        let first = StoreClaims::open("project", &path, LIVE_HOLDER).expect("the store opens");
        let held = first.claim("T-3").expect("the work is free");
        assert_eq!(held.epoch, 1);
        // The process goes away without releasing: the state dir is the
        // only thing the next boot has.
        drop(first);
        let booted = StoreClaims::open("project", &path, LIVE_HOLDER).expect("the store reopens");
        assert!(
            matches!(booted.claim("T-3"), Err(ClaimError::Denied { ref holder }) if holder == LIVE_HOLDER),
            "a restart must not grant a second claim on held work"
        );
        // The held epoch is still the fencing token of the attempt.
        assert_eq!(
            booted
                .claims()
                .expect("the held claims")
                .get("T-3")
                .map(String::as_str),
            Some(LIVE_HOLDER)
        );
        booted
            .release("T-3", held.epoch)
            .expect("the release completes");
        assert_eq!(booted.claim("T-3").expect("the released work").epoch, 2);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_stale_epoch_does_not_release_the_current_claim() {
        let (claims, path) = claims("stale");
        let first = claims.claim("T-2").expect("the work is free");
        claims
            .release("T-2", first.epoch)
            .expect("the release completes");
        let second = claims.claim("T-2").expect("the work is free again");
        claims
            .release("T-2", first.epoch)
            .expect("a stale epoch is a no-op, not a failure");
        assert!(
            matches!(claims.claim("T-2"), Err(ClaimError::Denied { .. })),
            "a stale attempt must not release the current one"
        );
        claims
            .release("T-2", second.epoch)
            .expect("the release completes");
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }
}
