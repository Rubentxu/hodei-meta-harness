//! Goal sources (GC-14): a goal either lives inside a tracker scope (the
//! default) or is an explicit custom objective with a verifiable
//! termination criterion. A tracker failure NEVER silently rewrites the
//! default scope into a custom objective, and completing a custom objective
//! creates nothing new — only a new authorized edition can reactivate it.

use serde::{Deserialize, Serialize};

use verify::result::{ExecutionRecord, Verdict, verdict_of};

/// Where the goal's work comes from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GoalSource {
    /// Default: read work from the configured tracker (e.g. Backlog.md).
    TrackerScope { provider: String },
    /// Explicit custom objective: no tickets are written; termination is
    /// defined by the user's criterion and each restart is a new edition.
    CustomObjective {
        edition: u64,
        criterion: TerminationCriterion,
    },
}

/// A verifiable termination condition: a command plus the exit code that
/// means "fulfilled".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TerminationCriterion {
    pub command: String,
    pub expect_exit: i32,
}

/// Lifecycle of a custom objective. `Fulfilled` is terminal on its own —
/// completing never spawns a next objective.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CustomStatus {
    Active,
    Fulfilled,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CustomGoalState {
    pub edition: u64,
    pub status: CustomStatus,
}

/// What the supervisor does with a custom objective after one evaluation.
#[derive(Debug, Clone, PartialEq)]
pub enum CustomDecision {
    /// Criterion met: the objective is fulfilled and NOTHING is created —
    /// the supervisor goes idle for this goal (finite objective, no
    /// invented recurrence).
    Fulfilled { edition: u64 },
    /// Criterion not met yet: keep the objective active.
    Continue { edition: u64 },
    /// A fulfilled objective with a NEW authorized edition reactivates it.
    Reactivated { edition: u64 },
    /// A fulfilled objective without a new edition stays fulfilled.
    StayFulfilled { edition: u64 },
}

/// Advance a custom objective. `authorized_edition` is Some only when the
/// user explicitly authorizes a new run of a fulfilled objective (AC-2:
/// nothing reactivates automatically).
pub fn advance_custom(
    state: &CustomGoalState,
    authorized_edition: Option<u64>,
    record: &ExecutionRecord,
    criterion: &TerminationCriterion,
) -> CustomDecision {
    match state.status {
        CustomStatus::Active => {
            if verdict_of(record) == Verdict::Pass
                && record.exit_code == Some(criterion.expect_exit)
            {
                CustomDecision::Fulfilled {
                    edition: state.edition,
                }
            } else {
                CustomDecision::Continue {
                    edition: state.edition,
                }
            }
        }
        CustomStatus::Fulfilled => match authorized_edition {
            Some(new_edition) if new_edition > state.edition => CustomDecision::Reactivated {
                edition: new_edition,
            },
            _ => CustomDecision::StayFulfilled {
                edition: state.edition,
            },
        },
    }
}

/// Resolution of which scope a run operates in. The tracker failure is a
/// VISIBLE outcome, not a silent fallback (AC-3).
#[derive(Debug, Clone, PartialEq)]
pub enum ScopeResolution {
    Tracker {
        provider: String,
    },
    Custom {
        objective: GoalSource,
    },
    /// The default scope requires the tracker and the tracker failed: the
    /// run must fail visibly, never rewrite itself into a custom objective.
    TrackerFailure {
        reason: String,
    },
}

pub fn resolve_scope(
    configured: Option<GoalSource>,
    tracker_health: Result<(), String>,
) -> ScopeResolution {
    match configured {
        Some(GoalSource::TrackerScope { provider }) => match tracker_health {
            Ok(()) => ScopeResolution::Tracker { provider },
            Err(reason) => ScopeResolution::TrackerFailure { reason },
        },
        Some(custom @ GoalSource::CustomObjective { .. }) => {
            ScopeResolution::Custom { objective: custom }
        }
        None => match tracker_health {
            Ok(()) => ScopeResolution::Tracker {
                provider: "backlog".into(),
            },
            Err(reason) => ScopeResolution::TrackerFailure { reason },
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass_record(exit: i32) -> ExecutionRecord {
        ExecutionRecord::terminal("criterion", exit, "done")
    }

    fn criterion() -> TerminationCriterion {
        TerminationCriterion {
            command: "make check".into(),
            expect_exit: 0,
        }
    }

    fn active(edition: u64) -> CustomGoalState {
        CustomGoalState {
            edition,
            status: CustomStatus::Active,
        }
    }

    #[test]
    fn fulfilling_creates_nothing_new_and_goes_terminal() {
        let decision = advance_custom(&active(3), None, &pass_record(0), &criterion());
        assert_eq!(decision, CustomDecision::Fulfilled { edition: 3 });
        // Without a new authorized edition it stays fulfilled forever.
        let fulfilled = CustomGoalState {
            edition: 3,
            status: CustomStatus::Fulfilled,
        };
        assert_eq!(
            advance_custom(&fulfilled, None, &pass_record(0), &criterion()),
            CustomDecision::StayFulfilled { edition: 3 }
        );
    }

    #[test]
    fn unmet_criterion_keeps_the_objective_active() {
        let decision = advance_custom(&active(1), None, &pass_record(1), &criterion());
        assert_eq!(decision, CustomDecision::Continue { edition: 1 });
    }

    #[test]
    fn only_a_new_authorized_edition_reactivates() {
        let fulfilled = CustomGoalState {
            edition: 2,
            status: CustomStatus::Fulfilled,
        };
        // Same edition is not a reactivation.
        assert_eq!(
            advance_custom(&fulfilled, Some(2), &pass_record(0), &criterion()),
            CustomDecision::StayFulfilled { edition: 2 }
        );
        // A strictly newer authorized edition reactivates.
        assert_eq!(
            advance_custom(&fulfilled, Some(3), &pass_record(0), &criterion()),
            CustomDecision::Reactivated { edition: 3 }
        );
    }

    #[test]
    fn criterion_exit_code_must_match_expectation() {
        // exit 0 is Pass for the record, but the criterion expects exit 2.
        let criterion = TerminationCriterion {
            command: "special".into(),
            expect_exit: 2,
        };
        let decision = advance_custom(&active(1), None, &pass_record(0), &criterion);
        assert_eq!(decision, CustomDecision::Continue { edition: 1 });
    }

    #[test]
    fn tracker_failure_is_visible_and_never_becomes_custom() {
        // Default scope + failing tracker: explicit failure.
        assert_eq!(
            resolve_scope(None, Err("backlog not installed".into())),
            ScopeResolution::TrackerFailure {
                reason: "backlog not installed".into()
            }
        );
        // Explicit tracker scope + failing tracker: same.
        assert!(matches!(
            resolve_scope(
                Some(GoalSource::TrackerScope {
                    provider: "backlog".into()
                }),
                Err("timeout".into())
            ),
            ScopeResolution::TrackerFailure { .. }
        ));
        // An explicit custom objective ignores tracker health: the user
        // asked for it directly.
        let custom = GoalSource::CustomObjective {
            edition: 1,
            criterion: criterion(),
        };
        assert_eq!(
            resolve_scope(Some(custom.clone()), Err("down".into())),
            ScopeResolution::Custom { objective: custom }
        );
        // Default scope + healthy tracker: tracker scope.
        assert_eq!(
            resolve_scope(None, Ok(())),
            ScopeResolution::Tracker {
                provider: "backlog".into()
            }
        );
    }
}
