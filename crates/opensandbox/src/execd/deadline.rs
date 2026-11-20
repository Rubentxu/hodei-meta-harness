//! The deadline of an operation, measured on a monotonic clock.
//!
//! A deadline is the moment an operation stops being allowed to spend
//! time. Every step of the transport derives its own timeout from it, so
//! the whole operation — the readiness wait, the session creation, the
//! attach, the replay and the cleanup — shares ONE budget instead of
//! having one per step.

use std::time::{Duration, Instant};

/// The moment an operation stops being allowed to spend time.
#[derive(Debug, Clone, Copy)]
pub struct Deadline {
    at: Instant,
}

impl Deadline {
    /// A deadline `budget` away from now.
    pub fn in_budget(budget: Duration) -> Self {
        Self {
            at: Instant::now() + budget,
        }
    }

    /// The time left before the deadline. It is zero once it has passed:
    /// a step that has no time left does not run.
    pub fn remaining(&self) -> Duration {
        self.at.saturating_duration_since(Instant::now())
    }

    /// Whether the deadline has passed.
    pub fn expired(&self) -> bool {
        Instant::now() >= self.at
    }

    /// The earlier of this deadline and one `budget` away from now: the
    /// share of a budget that fits in what is left of this one.
    pub fn within(&self, budget: Duration) -> Deadline {
        let candidate = Instant::now() + budget;
        if candidate < self.at {
            Deadline { at: candidate }
        } else {
            *self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_deadline_spends_its_budget_and_no_more() {
        let deadline = Deadline::in_budget(Duration::from_millis(50));
        assert!(!deadline.expired());
        assert!(deadline.remaining() <= Duration::from_millis(50));
        assert!(deadline.remaining() > Duration::ZERO);
    }

    #[test]
    fn a_deadline_without_time_left_expires() {
        let deadline = Deadline::in_budget(Duration::ZERO);
        assert!(deadline.expired());
        assert_eq!(deadline.remaining(), Duration::ZERO);
    }

    #[test]
    fn a_share_never_exceeds_the_deadline_it_comes_from() {
        let budget = Duration::from_millis(50);
        let deadline = Deadline::in_budget(budget);
        let share = deadline.within(Duration::from_secs(10));
        assert!(share.remaining() <= budget);
    }

    #[test]
    fn a_share_shorter_than_the_deadline_is_the_earlier_one() {
        let deadline = Deadline::in_budget(Duration::from_secs(10));
        let share = deadline.within(Duration::from_millis(20));
        assert!(share.remaining() <= Duration::from_millis(20));
        assert!(share.remaining() > Duration::ZERO);
    }
}
