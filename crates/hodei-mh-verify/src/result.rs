//! Execution records and verdicts (AC-1): a terminal exit status and
//! truncated output are distinguishable facts of the record, and a missing
//! result (timeout, signal, crash) can never yield PASS.

use serde::{Deserialize, Serialize};

/// Hard cap for stored command output; truncation is recorded, never silent.
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// What actually happened when a verification command ran.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionRecord {
    pub command: String,
    /// `None` means no terminal status was observed: killed by a timeout,
    /// terminated by a signal, or the runner lost the session. Absence of a
    /// result is a fact, and it can never be read as success.
    pub exit_code: Option<i32>,
    pub output: String,
    /// The output hit the cap: what is stored is incomplete by construction.
    pub output_truncated: bool,
    /// True only when the runner observed a terminal status AND read the
    /// stream to its end.
    pub stream_completed: bool,
}

impl ExecutionRecord {
    pub fn terminal(command: impl Into<String>, exit_code: i32, output: impl Into<String>) -> Self {
        let output = output.into();
        let (bounded, truncated) = bound_output(&output);
        Self {
            command: command.into(),
            exit_code: Some(exit_code),
            output: bounded,
            output_truncated: truncated,
            stream_completed: true,
        }
    }

    /// A result that never reached a terminal state. Not eligible for PASS.
    pub fn absent(command: impl Into<String>, reason: &str) -> Self {
        Self {
            command: command.into(),
            exit_code: None,
            output: format!("[no terminal result: {reason}]"),
            output_truncated: false,
            stream_completed: false,
        }
    }
}

fn bound_output(output: &str) -> (String, bool) {
    if output.len() > MAX_OUTPUT_BYTES {
        let mut cut = MAX_OUTPUT_BYTES;
        while !output.is_char_boundary(cut) {
            cut -= 1;
        }
        (output[..cut].to_string(), true)
    } else {
        (output.to_string(), false)
    }
}

/// The only three outcomes a check can have. There is no implicit PASS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Verdict {
    Pass,
    Fail,
    /// No result, non-zero exit or an incomplete stream that cannot support
    /// a judgement. Explicit and visible.
    NotRun,
}

/// AC-1, total: exit status presence decides PASS eligibility; truncation is
/// recorded separately and an unfinished stream downgrades to NotRun.
pub fn verdict_of(record: &ExecutionRecord) -> Verdict {
    match record.exit_code {
        None => Verdict::NotRun,
        Some(0) if record.stream_completed => Verdict::Pass,
        Some(0) => Verdict::NotRun,
        Some(_) => Verdict::Fail,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_zero_exit_with_complete_stream_is_pass() {
        let record = ExecutionRecord::terminal("cargo test", 0, "all green");
        assert_eq!(verdict_of(&record), Verdict::Pass);
        assert!(!record.output_truncated);
    }

    #[test]
    fn non_zero_exit_is_fail_with_status_preserved() {
        let record = ExecutionRecord::terminal("cargo test", 101, "1 failed");
        assert_eq!(verdict_of(&record), Verdict::Fail);
        assert_eq!(record.exit_code, Some(101));
    }

    #[test]
    fn absent_result_is_never_pass() {
        let timeout = ExecutionRecord::absent("cargo test", "timeout after 120s");
        assert_eq!(verdict_of(&timeout), Verdict::NotRun);
        let killed = ExecutionRecord {
            command: "x".into(),
            exit_code: None,
            output: String::new(),
            output_truncated: false,
            stream_completed: false,
        };
        assert_eq!(verdict_of(&killed), Verdict::NotRun);
    }

    #[test]
    fn unfinished_stream_downgrades_zero_exit_to_notrun() {
        let record = ExecutionRecord {
            command: "flaky".into(),
            exit_code: Some(0),
            output: String::new(),
            output_truncated: false,
            stream_completed: false,
        };
        assert_eq!(verdict_of(&record), Verdict::NotRun);
    }

    #[test]
    fn truncation_is_a_visible_fact_and_char_safe() {
        let long = "a".repeat(MAX_OUTPUT_BYTES + 500);
        let record = ExecutionRecord::terminal("big", 0, long);
        assert!(record.output_truncated);
        assert_eq!(record.output.len(), MAX_OUTPUT_BYTES);
        assert!(record.output.chars().all(|c| c == 'a'));

        let multibyte = "ñ".repeat(MAX_OUTPUT_BYTES / 2 + 10);
        let record = ExecutionRecord::terminal("utf8", 0, multibyte);
        assert!(record.output_truncated);
        assert!(record.output.is_char_boundary(record.output.len()));
    }
}
