//! The application-level goal runner.
//!
//! It selects the work and executes it through PORTS: the agent that
//! works, the verifier that judges and the publisher that promotes. The
//! runner keeps no phase of its own — the state machine lives in the
//! supervisor's engine — and it only reports what it did.

pub mod core;
pub mod live;
pub mod ports;
pub mod rehearsal;

pub use core::{GoalRunner, RunnerError};
pub use live::{AttemptEnd, ClaimError, ClaimPort, Claimed, LiveRuntime, outcome_of};
