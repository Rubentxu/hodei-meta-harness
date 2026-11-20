//! The claim store of the live runtime: the coordinator's durable state.
//!
//! The claim is the single-writer fact of an attempt. It is taken BEFORE
//! any effect and given back exactly once, whatever the attempt ends
//! with; the runtime's guard owns the release, so an aborted attempt
//! releases it too.

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use hodei_mh_coordinator::core::ClaimResult;
use hodei_mh_coordinator::store::{CoordinatorStore, StoreError};
use runner::live::{ClaimError, ClaimPort, Claimed};

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

    fn release(&self, work_ref: &str, epoch: u64) {
        let mut store = self.store.lock().unwrap_or_else(PoisonError::into_inner);
        // Only the attempt that owns the claim releases it: a stale
        // attempt must not give back work somebody else now holds.
        let _ = store.transact(|state| {
            if state.claim_of(work_ref).map(|claim| claim.epoch) == Some(epoch) {
                state.release(work_ref, &self.holder);
            }
            Ok(())
        });
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
        claims.release("T-1", first.epoch);
        let second = claims
            .claim("T-1")
            .expect("the released work is free again");
        assert_eq!(second.epoch, 2, "every grant advances the fencing epoch");
        claims.release("T-1", second.epoch);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }

    #[test]
    fn a_stale_epoch_does_not_release_the_current_claim() {
        let (claims, path) = claims("stale");
        let first = claims.claim("T-2").expect("the work is free");
        claims.release("T-2", first.epoch);
        let second = claims.claim("T-2").expect("the work is free again");
        claims.release("T-2", first.epoch);
        assert!(
            matches!(claims.claim("T-2"), Err(ClaimError::Denied { .. })),
            "a stale attempt must not release the current one"
        );
        claims.release("T-2", second.epoch);
        let _ = std::fs::remove_dir_all(path.parent().expect("a parent"));
    }
}
