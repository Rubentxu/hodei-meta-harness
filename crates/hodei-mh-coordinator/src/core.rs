//! Pure coordinator domain: claims, epoch idempotency, effect intents.
//! Every transition takes a state and returns a new state — no I/O, no
//! interior mutability. Persistence and the actor are adapters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub ticket_id: String,
    pub holder: String,
    pub epoch: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectStatus {
    /// Recorded before the effect runs.
    Pending,
    /// Confirmed applied by observation.
    Applied,
    /// Ran or may have run without confirmation — must be reconciled from
    /// observed state before any re-emission.
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectIntent {
    pub effect_id: String,
    pub ticket_id: String,
    pub epoch: u64,
    pub description: String,
    pub status: EffectStatus,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct CoordinatorState {
    /// At most one active claim per ticket: the single-writer invariant.
    pub claims: BTreeMap<String, Claim>,
    /// Per-ticket monotonic epoch for idempotency of late/duplicated events.
    pub epochs: BTreeMap<String, u64>,
    /// Durable effect intents (outbox): survive crashes.
    pub effects: BTreeMap<String, EffectIntent>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ClaimResult {
    Granted(Claim),
    /// Denied with the current holder — never silently stolen.
    Denied {
        holder: String,
    },
}

impl CoordinatorState {
    /// Request a claim. Two concurrent requests for the same ticket yield at
    /// most one `Granted`.
    pub fn try_claim(&mut self, ticket_id: &str, holder: &str) -> ClaimResult {
        if let Some(existing) = self.claims.get(ticket_id) {
            return ClaimResult::Denied {
                holder: existing.holder.clone(),
            };
        }
        let epoch = self.epochs.get(ticket_id).copied().unwrap_or(0) + 1;
        self.epochs.insert(ticket_id.to_string(), epoch);
        let claim = Claim {
            ticket_id: ticket_id.to_string(),
            holder: holder.to_string(),
            epoch,
        };
        self.claims.insert(ticket_id.to_string(), claim.clone());
        ClaimResult::Granted(claim)
    }

    /// Release a claim (only its holder may release it).
    pub fn release(&mut self, ticket_id: &str, holder: &str) -> bool {
        match self.claims.get(ticket_id) {
            Some(claim) if claim.holder == holder => {
                self.claims.remove(ticket_id);
                true
            }
            _ => false,
        }
    }

    /// Apply a run event. Events with an epoch older than the ticket's
    /// current epoch are dropped without producing operations and without
    /// touching any other run — late/duplicated events are inert.
    pub fn apply_event(&mut self, ticket_id: &str, event_epoch: u64) -> Option<u64> {
        let current = self.epochs.get(ticket_id).copied().unwrap_or(0);
        if event_epoch < current {
            return None;
        }
        self.epochs
            .insert(ticket_id.to_string(), event_epoch.max(current));
        Some(event_epoch)
    }

    /// Record an effect intent BEFORE running it (write-ahead).
    pub fn record_effect(
        &mut self,
        effect_id: &str,
        ticket_id: &str,
        epoch: u64,
        description: &str,
    ) -> EffectIntent {
        let intent = EffectIntent {
            effect_id: effect_id.to_string(),
            ticket_id: ticket_id.to_string(),
            epoch,
            description: description.to_string(),
            status: EffectStatus::Pending,
        };
        self.effects.insert(effect_id.to_string(), intent.clone());
        intent
    }

    /// An effect whose outcome is unknown becomes Uncertain: it must be
    /// reconciled from observed state before anyone re-emits it.
    pub fn mark_uncertain(&mut self, effect_id: &str) -> bool {
        self.with_effect(effect_id, |e| e.status = EffectStatus::Uncertain)
    }

    /// Reconcile an uncertain effect with observed reality.
    pub fn reconcile(&mut self, effect_id: &str, applied: bool) -> bool {
        self.with_effect(effect_id, |e| {
            e.status = if applied {
                EffectStatus::Applied
            } else {
                EffectStatus::Pending
            }
        })
    }

    /// Uncertain effects must be reconciled before re-emission is legal.
    pub fn can_reemit(&self, effect_id: &str) -> bool {
        match self.effects.get(effect_id) {
            Some(effect) => !matches!(effect.status, EffectStatus::Uncertain),
            None => false,
        }
    }

    fn with_effect(&mut self, effect_id: &str, mutate: impl FnOnce(&mut EffectIntent)) -> bool {
        match self.effects.get_mut(effect_id) {
            Some(effect) => {
                mutate(effect);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn concurrent_claims_yield_at_most_one_grant() {
        let mut state = CoordinatorState::default();
        let first = state.try_claim("T1", "client-a");
        let second = state.try_claim("T1", "client-b");
        assert!(matches!(first, ClaimResult::Granted(_)));
        assert_eq!(
            second,
            ClaimResult::Denied {
                holder: "client-a".into()
            }
        );
        // Different tickets are independent single writers.
        assert!(matches!(
            state.try_claim("T2", "client-b"),
            ClaimResult::Granted(_)
        ));
    }

    #[test]
    fn stale_and_duplicate_epochs_are_inert() {
        let mut state = CoordinatorState::default();
        state.try_claim("T1", "a"); // epoch 1
        assert_eq!(state.apply_event("T1", 1), Some(1)); // duplicate current: fine
        assert_eq!(state.apply_event("T1", 0), None); // stale: dropped
        assert_eq!(state.apply_event("T1", 2), Some(2)); // newer: applies
        // Epochs of other runs untouched by the stale event.
        assert_eq!(state.epochs.get("T1"), Some(&2));
        assert!(!state.epochs.contains_key("T9"));
    }

    #[test]
    fn effects_require_reconciliation_before_reemission() {
        let mut state = CoordinatorState::default();
        let claim = match state.try_claim("T1", "a") {
            ClaimResult::Granted(claim) => claim,
            _ => unreachable!(),
        };
        state.record_effect("e1", "T1", claim.epoch, "write file");
        assert!(state.can_reemit("e1")); // pending, never ran: re-emission legal

        // Outcome unknown -> uncertain -> re-emission blocked.
        state.mark_uncertain("e1");
        assert!(!state.can_reemit("e1"));
        state.reconcile("e1", false); // observed: never applied
        assert!(state.can_reemit("e1"));
        state.reconcile("e1", true); // observed: already applied
        assert!(state.can_reemit("e1")); // idempotent completion path

        assert!(!state.can_reemit("missing"));
    }

    #[test]
    fn release_is_holder_scoped() {
        let mut state = CoordinatorState::default();
        state.try_claim("T1", "a");
        assert!(!state.release("T1", "intruder"));
        assert!(state.release("T1", "a"));
        assert!(matches!(
            state.try_claim("T1", "b"),
            ClaimResult::Granted(_)
        ));
    }
}
