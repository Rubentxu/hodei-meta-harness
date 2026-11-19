//! The durable home of the intent of an attempt.
//!
//! The promotion does not keep a store of its own: the intent of a
//! promotion IS an intent of the coordinator, fenced to the attempt that
//! owns it, in the same database where the claims live. One store, one
//! authority: the epoch of the ticket, the identity of the project and the
//! facts of the attempt are all in the same committed row.

use coordinator::store::{CoordinatorStore, TransactError};

use crate::intent::{JournalRecord, PromotionIntent, PromotionOutcome};

/// The journal refused to keep the intent of an attempt.
#[derive(Debug, thiserror::Error)]
pub enum JournalError {
    /// The store refused: the attempt is not the current one, or another
    /// intent owns this identity.
    #[error("the store refused the intent: {0}")]
    Refused(String),
    /// The store could not be read or written.
    #[error("the store failed: {0}")]
    Store(String),
    /// The recorded intent cannot be read back.
    #[error("the recorded intent is unreadable: {0}")]
    Unreadable(String),
}

/// The durable home of the intents.
pub trait IntentJournal {
    /// The project this journal belongs to.
    fn project(&self) -> &str;

    /// The record of an operation, if it was recorded.
    fn load(&self, operation_id: &str) -> Result<Option<JournalRecord>, JournalError>;

    /// Record the intent BEFORE any effect runs.
    ///
    /// It is written only when it is not there yet: a record that exists is
    /// never overwritten, because it is the record of an attempt that may
    /// already have had effects.
    fn record(&mut self, intent: &PromotionIntent) -> Result<(), JournalError>;

    /// The epoch of the current attempt of a ticket, if it is claimed.
    fn current_epoch(&self, work_ref: &str) -> Result<Option<u64>, JournalError>;

    /// The attempt settles: it says what it did.
    fn settle(
        &mut self,
        operation_id: &str,
        outcome: &PromotionOutcome,
    ) -> Result<(), JournalError>;
}

/// The journal of the intents: the coordinator's store.
pub struct CoordinatorJournal<'a> {
    store: &'a mut CoordinatorStore,
}

impl<'a> CoordinatorJournal<'a> {
    pub fn new(store: &'a mut CoordinatorStore) -> Self {
        Self { store }
    }
}

impl IntentJournal for CoordinatorJournal<'_> {
    fn project(&self) -> &str {
        self.store.project()
    }

    fn load(&self, operation_id: &str) -> Result<Option<JournalRecord>, JournalError> {
        let state = self
            .store
            .load()
            .map_err(|error| JournalError::Store(error.to_string()))?;
        let Some(effect) = state.effect(operation_id) else {
            return Ok(None);
        };
        let payload = effect.payload.as_deref().ok_or_else(|| {
            JournalError::Unreadable(format!("the intent {operation_id} carries no facts"))
        })?;
        let intent: PromotionIntent = serde_json::from_str(payload)
            .map_err(|error| JournalError::Unreadable(error.to_string()))?;
        let outcome = match effect.outcome.as_deref() {
            Some(raw) => Some(
                serde_json::from_str(raw)
                    .map_err(|error| JournalError::Unreadable(error.to_string()))?,
            ),
            None => None,
        };
        Ok(Some(JournalRecord { intent, outcome }))
    }

    fn record(&mut self, intent: &PromotionIntent) -> Result<(), JournalError> {
        let payload = serde_json::to_string(intent)
            .map_err(|error| JournalError::Store(error.to_string()))?;
        let description = intent.description();
        self.store
            .transact(|state| {
                state
                    .record_effect_with_payload(
                        &intent.operation_id,
                        &intent.work_ref,
                        intent.epoch,
                        &description,
                        Some(&payload),
                    )
                    .map(|_| ())
            })
            .map_err(|error| match error {
                TransactError::Refused(refusal) => JournalError::Refused(refusal.to_string()),
                TransactError::Store(error) => JournalError::Store(error.to_string()),
            })
    }

    fn current_epoch(&self, work_ref: &str) -> Result<Option<u64>, JournalError> {
        let state = self
            .store
            .load()
            .map_err(|error| JournalError::Store(error.to_string()))?;
        Ok(state.attempt_epoch(work_ref))
    }

    fn settle(
        &mut self,
        operation_id: &str,
        outcome: &PromotionOutcome,
    ) -> Result<(), JournalError> {
        let raw = serde_json::to_string(outcome)
            .map_err(|error| JournalError::Store(error.to_string()))?;
        let applied = outcome.is_applied();
        self.store
            .transact(|state| Ok(state.settle_effect(operation_id, Some(&raw), applied)))
            .map(|_| ())
            .map_err(|error| match error {
                TransactError::Refused(refusal) => JournalError::Refused(refusal.to_string()),
                TransactError::Store(error) => JournalError::Store(error.to_string()),
            })
    }
}
