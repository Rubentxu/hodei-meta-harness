//! Interaction lifecycle (AC-2): an answer that arrives after the
//! interaction was stopped is DISCARDED — late results never apply.

use serde::{Deserialize, Serialize};

use crate::response::ValidAnswer;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InteractionState {
    /// Waiting for an answer.
    Pending,
    /// The run stopped: the interaction is closed for good.
    Stopped,
}

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("late answer discarded: the interaction was already stopped")]
pub struct LateDiscarded;

/// Apply a validated answer to an interaction. Pending accepts; Stopped
/// rejects with LateDiscarded — the answer exists but has NO effect (AC-2).
pub fn apply(state: &InteractionState, answer: ValidAnswer) -> Result<ValidAnswer, LateDiscarded> {
    match state {
        InteractionState::Pending => Ok(answer),
        InteractionState::Stopped => Err(LateDiscarded),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer() -> ValidAnswer {
        ValidAnswer {
            json: serde_json::json!({"outcome": {"outcome": "selected", "optionId": "a1"}}),
        }
    }

    #[test]
    fn pending_accepts_stopped_discards() {
        assert!(apply(&InteractionState::Pending, answer()).is_ok());
        assert_eq!(
            apply(&InteractionState::Stopped, answer()),
            Err(LateDiscarded)
        );
    }
}
