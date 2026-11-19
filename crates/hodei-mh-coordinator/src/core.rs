//! Pure coordinator domain: claims, epoch fencing, effect intents.
//! Every transition takes a state and returns a new state — no I/O, no
//! interior mutability. Persistence and the actor are adapters.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// A claim: the single writer of a ticket, and the epoch of that attempt.
/// The epoch is the fencing token that every event and every effect of the
/// attempt must carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Claim {
    pub ticket_id: String,
    pub holder: String,
    pub epoch: u64,
}

/// How far an effect intent has got.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectStatus {
    /// Recorded before the effect runs: it may be running already.
    Pending,
    /// The outcome is unknown: it must be reconciled against the observed
    /// state before anyone says anything about it.
    Uncertain,
    /// Observed NOT applied: the effect never ran, so it may be emitted
    /// again.
    NotApplied,
    /// Confirmed applied by observation: it must never be emitted again.
    Applied,
}

/// A durable effect intent (outbox entry): written BEFORE the effect runs
/// and kept until it is reconciled against the observed state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectIntent {
    pub effect_id: String,
    pub ticket_id: String,
    pub epoch: u64,
    pub description: String,
    pub status: EffectStatus,
    /// The facts of the effect, as the effect's author states them. The
    /// coordinator does not read them: it stores them, fences them to the
    /// attempt and gives them back. An effect without facts carries none.
    pub payload: Option<String>,
    /// What the attempt did, written when it settles. It is NOT part of
    /// the identity of the intent: the facts stay the facts, and the
    /// attempt tells its own story beside them.
    pub outcome: Option<String>,
}

/// The durable state of ONE project.
///
/// A ticket id is unique INSIDE a project: the same id in another project
/// is a different ticket, and the two never collide.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CoordinatorState {
    /// The project this state coordinates.
    pub project: String,
    /// At most one active claim per ticket: the single-writer invariant.
    pub claims: BTreeMap<String, Claim>,
    /// Per-ticket monotonic epoch for idempotency of late/duplicated events.
    pub epochs: BTreeMap<String, u64>,
    /// Durable effect intents (outbox): survive crashes.
    pub effects: BTreeMap<String, EffectIntent>,
}

/// The outcome of a claim request.
#[derive(Debug, Clone, PartialEq)]
pub enum ClaimResult {
    Granted(Claim),
    /// Denied with the current holder — never silently stolen.
    Denied {
        holder: String,
    },
}

/// A refusal by the domain rules: nothing changed.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum Refusal {
    #[error("ticket '{ticket_id}' has no claim: there is no attempt to fence to")]
    NotClaimed { ticket_id: String },
    #[error("epoch {epoch} is not the current attempt of '{ticket_id}' (current: {current})")]
    StaleEpoch {
        ticket_id: String,
        epoch: u64,
        current: u64,
    },
    #[error("effect '{effect_id}' already exists with another intent")]
    EffectConflict { effect_id: String },
}

/// The outcome of recording an effect intent.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectRecord {
    pub intent: EffectIntent,
    /// Whether the effect MAY RUN because of this call: a new intent, or
    /// one that was reconciled as never applied. A duplicate is recorded
    /// already and must not run again.
    pub may_run: bool,
}

impl CoordinatorState {
    /// The state of a project. The project is part of the identity: two
    /// projects coordinate different tickets, even when the ids match.
    pub fn new(project: impl Into<String>) -> Self {
        Self {
            project: project.into(),
            claims: BTreeMap::new(),
            epochs: BTreeMap::new(),
            effects: BTreeMap::new(),
        }
    }

    /// Request a claim. Two concurrent requests for the same ticket yield at
    /// most one `Granted`. The epoch of the ticket advances with every
    /// grant: it is the fencing token of the attempt.
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

    /// The claim that fences the current attempt of a ticket.
    pub fn claim_of(&self, ticket_id: &str) -> Option<&Claim> {
        self.claims.get(ticket_id)
    }

    /// The epoch of the current attempt: the token an event or an effect
    /// must carry to belong to it.
    pub fn attempt_epoch(&self, ticket_id: &str) -> Option<u64> {
        self.claims.get(ticket_id).map(|claim| claim.epoch)
    }

    /// Whether a run event belongs to the current attempt.
    ///
    /// Only the holder of the claim may apply an event, and only with the
    /// epoch of that claim: an OLD epoch is a late duplicate and a NEWER
    /// one is not ours to grant. Both are inert, and neither of them
    /// changes the attempt in flight.
    pub fn accepts_event(&self, ticket_id: &str, event_epoch: u64, holder: &str) -> bool {
        match self.claims.get(ticket_id) {
            Some(claim) => claim.holder == holder && claim.epoch == event_epoch,
            None => false,
        }
    }

    /// Record an effect intent BEFORE the effect runs (write-ahead).
    ///
    /// The intent is fenced to the current attempt: the epoch must be the
    /// one of the claim. An effect id that already exists with the SAME
    /// intent is a duplicate (`may_run == false`) and must not run the
    /// effect again; an id that exists with ANOTHER intent is refused and
    /// leaves the attempt untouched.
    pub fn record_effect(
        &mut self,
        effect_id: &str,
        ticket_id: &str,
        epoch: u64,
        description: &str,
    ) -> Result<EffectRecord, Refusal> {
        self.record_effect_with_payload(effect_id, ticket_id, epoch, description, None)
    }

    /// Record an effect intent WITH the facts of the effect.
    ///
    /// The payload is what the effect is about: the expected revision, the
    /// base it applies to, whatever the effect needs to be resumed exactly.
    /// Two records of the same id with different facts are two different
    /// intents: the second one is refused, because adopting the facts of
    /// another attempt is how a stale effect gets applied.
    pub fn record_effect_with_payload(
        &mut self,
        effect_id: &str,
        ticket_id: &str,
        epoch: u64,
        description: &str,
        payload: Option<&str>,
    ) -> Result<EffectRecord, Refusal> {
        let current = self
            .attempt_epoch(ticket_id)
            .ok_or_else(|| Refusal::NotClaimed {
                ticket_id: ticket_id.to_string(),
            })?;
        if current != epoch {
            return Err(Refusal::StaleEpoch {
                ticket_id: ticket_id.to_string(),
                epoch,
                current,
            });
        }
        if let Some(existing) = self.effects.get(effect_id) {
            let same_intent = existing.ticket_id == ticket_id
                && existing.epoch == epoch
                && existing.description == description
                && existing.payload.as_deref() == payload;
            if !same_intent {
                return Err(Refusal::EffectConflict {
                    effect_id: effect_id.to_string(),
                });
            }
            // The same intent, recorded again. It may run again only when it
            // was reconciled as never applied; otherwise it is a duplicate
            // and it keeps its history untouched.
            if existing.status != EffectStatus::NotApplied {
                return Ok(EffectRecord {
                    intent: existing.clone(),
                    may_run: false,
                });
            }
        }
        let intent = EffectIntent {
            effect_id: effect_id.to_string(),
            ticket_id: ticket_id.to_string(),
            epoch,
            description: description.to_string(),
            status: EffectStatus::Pending,
            payload: payload.map(str::to_string),
            outcome: None,
        };
        self.effects.insert(effect_id.to_string(), intent.clone());
        Ok(EffectRecord {
            intent,
            may_run: true,
        })
    }

    /// The facts of a recorded intent, as its author stated them.
    pub fn effect_payload(&self, effect_id: &str) -> Option<&str> {
        self.effects
            .get(effect_id)
            .and_then(|intent| intent.payload.as_deref())
    }

    /// An effect whose outcome is unknown becomes Uncertain: it must be
    /// reconciled from observed state before anyone re-emits it.
    pub fn mark_uncertain(&mut self, effect_id: &str) -> bool {
        self.with_effect(effect_id, |e| e.status = EffectStatus::Uncertain)
    }

    /// Reconcile an effect intent with observed reality: applied, or never
    /// applied (which is the only state that may be emitted again).
    pub fn reconcile(&mut self, effect_id: &str, applied: bool) -> bool {
        self.settle_effect(effect_id, None, applied)
    }

    /// Settle a recorded intent: the attempt says what it did.
    ///
    /// An applied effect is a FACT and it is never downgraded: a later
    /// observation that contradicts it (a branch that moved, a ticket that
    /// changed) does not un-apply it. Saying "it may run again" about an
    /// effect that already ran is how the same effect happens twice.
    pub fn settle_effect(&mut self, effect_id: &str, outcome: Option<&str>, applied: bool) -> bool {
        self.with_effect(effect_id, |effect| {
            if effect.status == EffectStatus::Applied && !applied {
                return;
            }
            effect.status = if applied {
                EffectStatus::Applied
            } else {
                EffectStatus::NotApplied
            };
            if let Some(outcome) = outcome {
                effect.outcome = Some(outcome.to_string());
            }
        })
    }

    /// Whether an effect may be emitted again. Only an intent that was
    /// observed NOT applied may: a confirmed one would be applied twice and
    /// an unknown one must be reconciled first.
    pub fn can_reemit(&self, effect_id: &str) -> bool {
        matches!(
            self.effects.get(effect_id).map(|effect| &effect.status),
            Some(EffectStatus::NotApplied)
        )
    }

    /// The recorded intent of an effect.
    pub fn effect(&self, effect_id: &str) -> Option<&EffectIntent> {
        self.effects.get(effect_id)
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

    fn claimed(project: &str, ticket: &str, holder: &str) -> CoordinatorState {
        let mut state = CoordinatorState::new(project);
        let claim = match state.try_claim(ticket, holder) {
            ClaimResult::Granted(claim) => claim,
            _ => unreachable!(),
        };
        assert_eq!(claim.epoch, 1);
        state
    }

    #[test]
    fn concurrent_claims_yield_at_most_one_grant() {
        let mut state = CoordinatorState::new("alpha");
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
    fn the_same_ticket_id_in_two_projects_does_not_collide() {
        let mut alpha = CoordinatorState::new("alpha");
        let mut beta = CoordinatorState::new("beta");
        assert!(matches!(
            alpha.try_claim("TASK-1", "orchestrator"),
            ClaimResult::Granted(_)
        ));
        assert!(matches!(
            beta.try_claim("TASK-1", "orchestrator"),
            ClaimResult::Granted(_)
        ));
        // Each project keeps its own epoch for its own ticket.
        assert_eq!(alpha.attempt_epoch("TASK-1"), Some(1));
        assert_eq!(beta.attempt_epoch("TASK-1"), Some(1));
    }

    #[test]
    fn only_the_holder_of_the_current_attempt_applies_its_events() {
        let state = claimed("alpha", "T1", "client-a");
        assert!(state.accepts_event("T1", 1, "client-a"));
        // A late event of an older attempt: inert.
        assert!(!state.accepts_event("T1", 0, "client-a"));
        // An event of a newer attempt: not ours to grant, inert.
        assert!(!state.accepts_event("T1", 2, "client-a"));
        // Another holder is not authorized: inert.
        assert!(!state.accepts_event("T1", 1, "client-b"));
        // An event of a ticket nobody claimed: inert.
        assert!(!state.accepts_event("T9", 1, "client-a"));
    }

    #[test]
    fn an_inert_event_leaves_the_attempt_untouched() {
        let state = claimed("alpha", "T1", "client-a");
        assert!(!state.accepts_event("T1", 2, "client-a"));
        assert!(!state.accepts_event("T1", 1, "client-b"));
        // The claim of the current attempt is exactly as it was.
        assert_eq!(state.attempt_epoch("T1"), Some(1));
        assert_eq!(
            state.claim_of("T1").map(|c| c.holder.as_str()),
            Some("client-a")
        );
    }

    #[test]
    fn an_effect_is_fenced_to_the_current_attempt() {
        let mut state = claimed("alpha", "T1", "client-a");
        // No claim: there is no attempt to fence to.
        assert_eq!(
            state.record_effect("e1", "T2", 1, "write file"),
            Err(Refusal::NotClaimed {
                ticket_id: "T2".into()
            })
        );
        // An epoch of another attempt: refused, and nothing was recorded.
        assert_eq!(
            state.record_effect("e1", "T1", 7, "write file"),
            Err(Refusal::StaleEpoch {
                ticket_id: "T1".into(),
                epoch: 7,
                current: 1
            })
        );
        assert!(state.effect("e1").is_none());
    }

    #[test]
    fn a_duplicate_effect_id_does_not_run_again() {
        let mut state = claimed("alpha", "T1", "client-a");
        let first = state.record_effect("e1", "T1", 1, "write file").unwrap();
        assert!(first.may_run);
        // The same intent, recorded again: a duplicate.
        let second = state.record_effect("e1", "T1", 1, "write file").unwrap();
        assert!(!second.may_run);
        assert_eq!(state.effects.len(), 1);
        // The duplicate did not alter the recorded intent.
        assert_eq!(
            state.effect("e1").map(|e| &e.status),
            Some(&EffectStatus::Pending)
        );
    }

    #[test]
    fn a_conflicting_effect_id_is_refused_without_altering_the_attempt() {
        let mut state = claimed("alpha", "T1", "client-a");
        assert!(matches!(
            state.try_claim("T2", "client-b"),
            ClaimResult::Granted(_)
        ));
        state.record_effect("e1", "T1", 1, "write file").unwrap();
        // The same id for another ticket: refused.
        assert_eq!(
            state.record_effect("e1", "T2", 1, "write file"),
            Err(Refusal::EffectConflict {
                effect_id: "e1".into()
            })
        );
        // The same id for another effect of the same ticket: refused.
        assert_eq!(
            state.record_effect("e1", "T1", 1, "delete file"),
            Err(Refusal::EffectConflict {
                effect_id: "e1".into()
            })
        );
        // The recorded intent is exactly as it was.
        assert_eq!(
            state.effect("e1").map(|e| e.description.as_str()),
            Some("write file")
        );
        assert_eq!(state.effects.len(), 1);
    }

    #[test]
    fn only_an_effect_observed_not_applied_may_be_emitted_again() {
        let mut state = claimed("alpha", "T1", "client-a");
        state.record_effect("e1", "T1", 1, "git push").unwrap();
        // Recorded and maybe running: it is not emitted again.
        assert!(!state.can_reemit("e1"));
        // The outcome is unknown: it must be reconciled first.
        state.mark_uncertain("e1");
        assert!(!state.can_reemit("e1"));
        // Observed: it never ran. Now it may be emitted again.
        state.reconcile("e1", false);
        assert!(state.can_reemit("e1"));
        let again = state.record_effect("e1", "T1", 1, "git push").unwrap();
        assert!(again.may_run);
        // Observed: it was applied. It is never emitted again.
        state.reconcile("e1", true);
        assert!(!state.can_reemit("e1"));
        let duplicate = state.record_effect("e1", "T1", 1, "git push").unwrap();
        assert!(!duplicate.may_run);
        // An unknown effect is not something to re-emit.
        assert!(!state.can_reemit("missing"));
    }

    #[test]
    fn the_facts_of_an_intent_are_part_of_its_identity() {
        let mut state = claimed("alpha", "T1", "client-a");
        state
            .record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"abc\"}"))
            .unwrap();
        // The same facts: the same intent, recorded again.
        let same = state
            .record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"abc\"}"))
            .unwrap();
        assert!(!same.may_run);
        // Another set of facts under the same id: another intent, refused.
        assert_eq!(
            state.record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"def\"}")),
            Err(Refusal::EffectConflict {
                effect_id: "e1".into()
            })
        );
        assert_eq!(
            state.effect_payload("e1"),
            Some("{\"sha\":\"abc\"}"),
            "the recorded facts are the ones that were written"
        );
    }

    #[test]
    fn a_settled_intent_says_what_it_did_and_is_not_emitted_again() {
        let mut state = claimed("alpha", "T1", "client-a");
        state
            .record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"abc\"}"))
            .unwrap();
        assert!(state.settle_effect("e1", Some("{\"outcome\":\"promoted\"}"), true));
        assert_eq!(
            state.effects["e1"].status,
            EffectStatus::Applied,
            "an applied effect is never emitted again"
        );
        assert_eq!(
            state.effects["e1"].outcome.as_deref(),
            Some("{\"outcome\":\"promoted\"}")
        );
        assert_eq!(
            state.effect_payload("e1"),
            Some("{\"sha\":\"abc\"}"),
            "the facts of the intent do not change when the attempt settles"
        );
        assert!(!state.can_reemit("e1"));
        let again = state
            .record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"abc\"}"))
            .unwrap();
        assert!(!again.may_run);
    }

    #[test]
    fn an_applied_effect_is_never_un_applied_by_a_later_observation() {
        let mut state = claimed("alpha", "T1", "client-a");
        state
            .record_effect_with_payload("e1", "T1", 1, "promote", Some("{\"sha\":\"abc\"}"))
            .unwrap();
        state.settle_effect("e1", Some("{\"outcome\":\"closed\"}"), true);
        assert_eq!(state.effects["e1"].status, EffectStatus::Applied);
        // A later observation that contradicts it does not un-apply it.
        state.settle_effect("e1", Some("{\"outcome\":\"conflicted\"}"), false);
        assert_eq!(
            state.effects["e1"].status,
            EffectStatus::Applied,
            "an effect that ran is never un-applied: it would be run again"
        );
        assert_eq!(
            state.effects["e1"].outcome.as_deref(),
            Some("{\"outcome\":\"closed\"}"),
            "the story of an applied effect is not rewritten by a later refusal"
        );
        assert!(!state.can_reemit("e1"));
    }

    #[test]
    fn release_is_holder_scoped() {
        let mut state = CoordinatorState::new("alpha");
        state.try_claim("T1", "a");
        assert!(!state.release("T1", "intruder"));
        assert!(state.release("T1", "a"));
        assert!(matches!(
            state.try_claim("T1", "b"),
            ClaimResult::Granted(_)
        ));
        // The next attempt of the same ticket carries a newer epoch.
        assert_eq!(state.attempt_epoch("T1"), Some(2));
    }
}
