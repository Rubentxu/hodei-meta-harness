//! Host pressure reactions (AC-3): a critical PSI signal or an OOM event
//! suspends NEW admissions and applies bounded backoff — the budget is
//! NEVER raised automatically.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PressureSignal {
    /// Pressure/Stall Information below the critical threshold.
    Normal,
    /// PSI `some`/`full` above the configured critical percentage.
    PsiCritical { resource: String },
    /// The kernel killed a process in one of our sandboxes.
    OomEvent { sandbox: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum PressureAction {
    Continue,
    /// New admissions wait until the pressure clears. The budget itself is
    /// untouched — no automatic raise exists.
    SuspendAdmissions,
    /// After an OOM: suspend admissions AND grow the backoff window.
    Backoff {
        after_releases_ms: u64,
    },
}

/// Bounded exponential backoff for OOM events: 1s, 2s, 4s... capped.
pub fn react(signal: &PressureSignal, backoff_step: u32) -> PressureAction {
    match signal {
        PressureSignal::Normal => PressureAction::Continue,
        PressureSignal::PsiCritical { .. } => PressureAction::SuspendAdmissions,
        PressureSignal::OomEvent { .. } => {
            let shift = backoff_step.min(5); // cap: 32s maximum window
            PressureAction::Backoff {
                after_releases_ms: 1000u64 << shift,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn psi_critical_suspends_without_raising_the_budget() {
        let action = react(
            &PressureSignal::PsiCritical {
                resource: "memory".into(),
            },
            0,
        );
        assert_eq!(action, PressureAction::SuspendAdmissions);
    }

    #[test]
    fn oom_backoff_grows_bounded_and_never_raises_budget() {
        let first = react(
            &PressureSignal::OomEvent {
                sandbox: "a".into(),
            },
            0,
        );
        let second = react(
            &PressureSignal::OomEvent {
                sandbox: "a".into(),
            },
            1,
        );
        let capped = react(
            &PressureSignal::OomEvent {
                sandbox: "a".into(),
            },
            99,
        );
        assert_eq!(
            first,
            PressureAction::Backoff {
                after_releases_ms: 1000
            }
        );
        assert_eq!(
            second,
            PressureAction::Backoff {
                after_releases_ms: 2000
            }
        );
        // The cap holds: no unbounded growth, no budget elevation.
        assert_eq!(
            capped,
            PressureAction::Backoff {
                after_releases_ms: 32000
            }
        );
    }

    #[test]
    fn normal_pressure_continues() {
        assert_eq!(react(&PressureSignal::Normal, 0), PressureAction::Continue);
    }
}
