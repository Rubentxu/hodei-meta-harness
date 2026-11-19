//! One-shot command execution over the execd pipe: ATTACH-FIRST is a
//! requirement, not an optimization — the execd drops unheld sessions
//! before their process can even start, so long commands (extraction of
//! large bundles, installs) never run without an immediate holder. Short
//! commands survive the polling-first style, which is why the bug stayed
//! hidden until a slow one-shot needed it.

use std::time::{Duration, Instant};

use super::api::{self, CreateSessionRequest};
use super::endpoint::ExecdEndpoint;
use super::frames::ServerEvent;
use super::pipe;

/// How a one-shot ended when no terminal status was observed. The
/// distinction matters: `NotAccepted` means the execd REFUSED the session,
/// so the command never ran and it is safe to try again, while `TimedOut`
/// means the command ran and never reported a terminal status — retrying
/// it would run the command twice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OneshotEnd {
    /// A terminal status was observed: the command ran.
    Completed,
    /// The execd refused the session: the command NEVER ran.
    NotAccepted,
    /// The command ran but no terminal status arrived before the deadline.
    TimedOut,
}

/// What a one-shot produced. `exit_code: None` means no terminal status was
/// observed (timeout, signal or lost session) — never a success.
#[derive(Debug, Clone, PartialEq)]
pub struct OneshotOutcome {
    pub exit_code: Option<i32>,
    pub output: String,
    /// How the attempt ended when there is no terminal status.
    pub end: OneshotEnd,
    /// Bytes the execd says the session produced: the yardstick for the
    /// completeness of the output. `None` when the execd did not say.
    pub expected_bytes: Option<usize>,
    /// Bytes of the output that never arrived: a truncated output is
    /// never announced as a complete one.
    pub missing_bytes: usize,
    /// Bytes that arrived and were DISCARDED because the collection was
    /// already full. They are not missing: they were received.
    pub dropped_bytes: usize,
    /// Whether the collection was cut by the quota. A truncated output
    /// is never announced as a complete one.
    pub truncated: bool,
    /// Whether the output was completed from the execd's replay.
    pub replayed: bool,
}

impl OneshotOutcome {
    pub fn succeeded(&self) -> bool {
        self.exit_code == Some(0)
    }

    /// The execd refused the attempt: the command never ran.
    pub fn not_accepted(&self) -> bool {
        self.end == OneshotEnd::NotAccepted
    }
}

/// Deadline of a single readiness attempt: it must fail fast while the
/// sandbox is still booting.
const READY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(3);
/// Interval between readiness attempts.
const READY_POLL_INTERVAL: Duration = Duration::from_millis(250);
/// Grace window to drain the output the execd flushes AFTER the exit
/// frame.
const OUTPUT_DRAIN_GRACE: Duration = Duration::from_millis(500);

/// How many bytes of command output are kept. A command that prints
/// without end does not grow the memory of the harness: what does not
/// fit is DROPPED and COUNTED, and the result says so.
pub const MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// Wait (BOUNDED) until the execd accepts sessions.
///
/// A freshly created sandbox publishes its execd port before the server
/// inside is able to answer: the first attempts are refused. Polling
/// here is what turns that boot delay into a wait instead of a false
/// "the command failed".
pub async fn wait_until_ready(endpoint: &ExecdEndpoint, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if api::session_accepted(endpoint, READY_ATTEMPT_TIMEOUT).await {
            // Accepting a session is not enough: the pipe must attach
            // too. The probe command is harmless, so it can be repeated
            // as many times as needed.
            let probe = run(endpoint, "true", Duration::from_secs(10)).await;
            if probe.end == OneshotEnd::Completed {
                return true;
            }
        }
        if Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(READY_POLL_INTERVAL).await;
    }
}

/// Run a command once the execd is actually answering.
///
/// The readiness wait runs a HARMLESS probe, so the real command is
/// executed EXACTLY ONCE: a command that ran and failed is returned as
/// it is, never repeated.
pub async fn run_when_ready(
    endpoint: &ExecdEndpoint,
    command: &str,
    timeout: Duration,
    ready_timeout: Duration,
) -> OneshotOutcome {
    if !wait_until_ready(endpoint, ready_timeout).await {
        return OneshotOutcome {
            exit_code: None,
            output: format!("[execd did not accept a session within {ready_timeout:?}]"),
            end: OneshotEnd::NotAccepted,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
    }
    run(endpoint, command, timeout).await
}

/// The state of one-shot output collection.
///
/// The collection has a QUOTA: a command that prints without end does not
/// grow the memory of the harness. Three counters keep the truth apart:
///
/// - `received_bytes`: how many bytes of the stream arrived;
/// - the length of the collected bytes: how many of them were KEPT;
/// - `missing_bytes`: how many never arrived.
///
/// The difference between the first two is what the quota discarded, and
/// it is never reported as "missing": a byte that arrived and was
/// dropped is not a byte that was lost.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Collector {
    /// The terminal status, once it has been observed.
    pub terminal_code: Option<i32>,
    /// Whether the exit frame was already seen: the execd may still be
    /// flushing the PTY buffer after it.
    pub draining: bool,
    /// Bytes of the session output that were never received: a hole in
    /// the stream, measured against what the execd says it produced.
    pub missing_bytes: usize,
    /// Whether a replay frame actually contributed bytes: the output
    /// was completed from the execd's replay.
    pub replayed: bool,
    /// Bytes that arrived and were discarded by the quota.
    pub dropped_bytes: usize,
    /// Whether the collection was cut by the quota.
    pub truncated: bool,
    /// How many bytes of the session output arrived, kept or not.
    pub received_bytes: usize,
    /// How many bytes the collection may keep.
    pub limit: usize,
}

impl Default for Collector {
    fn default() -> Self {
        Self::with_limit(MAX_OUTPUT_BYTES)
    }
}

impl Collector {
    /// A collector that keeps at most `limit` bytes.
    pub fn with_limit(limit: usize) -> Self {
        Self {
            terminal_code: None,
            draining: false,
            missing_bytes: 0,
            replayed: false,
            dropped_bytes: 0,
            truncated: false,
            received_bytes: 0,
            limit,
        }
    }
}

/// Apply a replay frame to the collection.
///
/// The execd replays the output it kept, starting at `offset` in the
/// session's output stream. The collection is kept as a PREFIX of that
/// stream, so it always describes the output faithfully:
///
/// - bytes already collected are not duplicated;
/// - a replay that starts BEYOND what arrived leaves a gap: those
///   bytes belong at their own position in the stream, so appending
///   them at the end would build a string that is not the session
///   output. The gap is counted and stays visible.
///
/// The position in the stream is the number of bytes that ARRIVED, not
/// the number that were kept: a collection cut by the quota still knows
/// where the stream is.
///
/// A truncated output is never announced as a complete one.
pub fn apply_replay(output: &mut Vec<u8>, collector: &mut Collector, offset: i64, data: &[u8]) {
    let start = offset.max(0) as usize;
    let have = collector.received_bytes;
    if start > have {
        // A GAP: the bytes between were never forwarded. The replayed
        // bytes belong at `start`, not at the end of what we have.
        collector.missing_bytes += start - have;
        return;
    }
    // The replay covers the stream positions [start, start + data.len()):
    // only the positions the collection does not have yet are new.
    let from = have - start;
    if from < data.len() {
        append_bytes(output, collector, &data[from..]);
        collector.replayed = true;
    }
}

/// Keep what fits in the quota and COUNT what does not.
fn append_bytes(output: &mut Vec<u8>, collector: &mut Collector, bytes: &[u8]) {
    let room = collector.limit.saturating_sub(output.len());
    let keep = bytes.len().min(room);
    output.extend_from_slice(&bytes[..keep]);
    if keep < bytes.len() {
        // The quota is full: the bytes that do not fit are DROPPED, and
        // the fact that they arrived is remembered.
        collector.dropped_bytes += bytes.len() - keep;
        collector.truncated = true;
    }
    collector.received_bytes += bytes.len();
}

/// Apply one event to the collection.
///
/// The exit frame does NOT stop the collection: the execd drains the PTY
/// buffer after the process ends, so the tail of the output arrives
/// after the exit frame. Dropping it loses the last lines of the
/// command — a short command can lose its whole output.
pub fn apply_event(output: &mut Vec<u8>, collector: &mut Collector, event: ServerEvent) {
    match event {
        ServerEvent::Stdout(bytes) => append_bytes(output, collector, &bytes),
        ServerEvent::Replay { offset, data } => apply_replay(output, collector, offset, &data),
        ServerEvent::Stderr(bytes) => append_bytes(output, collector, &bytes),
        ServerEvent::Exit { exit_code } => {
            collector.terminal_code = exit_code;
            collector.draining = true;
        }
        _ => {}
    }
}

/// Run one command to its terminal state or to the timeout, whichever comes
/// first. The pipe attaches IMMEDIATELY after session creation.
pub async fn run(endpoint: &ExecdEndpoint, command: &str, timeout: Duration) -> OneshotOutcome {
    let session = match api::create_session(
        endpoint,
        CreateSessionRequest {
            cwd: None,
            command: Some(command),
        },
    )
    .await
    {
        Ok(session) => session,
        Err(err) => {
            return OneshotOutcome {
                exit_code: None,
                output: format!("[create failed: {err}]"),
                end: OneshotEnd::NotAccepted,
                expected_bytes: None,
                missing_bytes: 0,
                dropped_bytes: 0,
                truncated: false,
                replayed: false,
            };
        }
    };

    let ws_url = endpoint.ws_url(&session.session_id);
    let mut output: Vec<u8> = Vec::new();
    let mut collector = Collector::default();

    let attach_outcome = pipe::attach(&ws_url, endpoint.attach_options()).await;
    let attach_error = attach_outcome.as_ref().err().map(|e| e.to_string());
    if let Ok(mut pipe_session) = attach_outcome {
        let started = Instant::now();
        let deadline = started + timeout;
        // The exit frame does not close the stream: a short grace window
        // keeps the output the execd flushes after it.
        let mut wait_until = deadline;
        loop {
            let now = Instant::now();
            if now >= wait_until {
                break;
            }
            let wait = wait_until.saturating_duration_since(now);
            match tokio::time::timeout(wait, pipe_session.next_event()).await {
                Ok(Some(event)) => {
                    apply_event(&mut output, &mut collector, event);
                    if collector.draining {
                        // The exit frame arrived, or more output is still
                        // arriving: keep the window open.
                        wait_until = std::cmp::min(deadline, Instant::now() + OUTPUT_DRAIN_GRACE);
                    }
                }
                // The pipe closed: no more events will ever arrive.
                Ok(None) => break,
                // A silent tick is NOT the deadline: long quiet commands
                // (large extractions, installs) keep running. Only the
                // wait deadline ends the wait.
                Err(_) => continue,
            }
        }
        if collector.terminal_code.is_none() {
            // Still running at the deadline: kill and record absence. A
            // negative code means death by signal — no terminal status.
            let _ = pipe_session.send_signal("SIGKILL").await;
            let grace = Instant::now() + Duration::from_secs(2);
            while Instant::now() < grace && collector.terminal_code.is_none() {
                match tokio::time::timeout(Duration::from_millis(800), pipe_session.next_event())
                    .await
                {
                    Ok(Some(event)) => apply_event(&mut output, &mut collector, event),
                    Ok(None) | Err(_) => break,
                }
            }
        }
        let _ = pipe_session.close().await;
    }

    // COMPLETION FROM THE REPLAY. The live stream can lose the tail of
    // the output: the execd forwards what it read from the PTY, and the
    // bytes written just before the process ended can be left behind.
    // The execd keeps what it produced and replays it on a new attach,
    // and it says how many bytes it produced: a shorter collection is a
    // truncated output, never a complete one.
    let expected = api::session_output_offset(endpoint, &session.session_id).await;
    // Two ways to know the output is short: the execd says it produced
    // more bytes than arrived, or the execd says nothing and the
    // command ended with no output at all — the signature of a tail
    // that was never forwarded.
    //
    // The measure is the bytes that ARRIVED, not the bytes that were
    // kept: a collection cut by the quota received everything the execd
    // sent, and a replay cannot add to a collection that cannot grow.
    let short = match expected {
        Some(produced) => produced > collector.received_bytes as i64,
        None => collector.terminal_code.is_some() && output.is_empty(),
    };
    let mut replay_error: Option<String> = None;
    if short && !collector.truncated {
        match pipe::attach(&ws_url, endpoint.attach_options()).await {
            Ok(mut replay_pipe) => {
                replay_missing(
                    &mut replay_pipe,
                    &mut output,
                    &mut collector,
                    expected.map(|value| value.max(0) as usize),
                    timeout,
                )
                .await;
                let _ = replay_pipe.close().await;
            }
            Err(err) => {
                replay_error = Some(format!("[replay attach failed: {err}]"));
            }
        }
    }
    let _ = api::delete_session(endpoint, &session.session_id).await;

    // The completeness of the output, measured against what the execd
    // says it produced. The collection is a PREFIX of the session
    // output, so the bytes that never arrived are the ones beyond what
    // arrived — and a gap in the middle is part of that count. The bytes
    // the quota discarded are NOT missing: they arrived.
    let expected_bytes = expected.map(|value| value.max(0) as usize);
    let missing_bytes = match expected_bytes {
        Some(expected) => expected.saturating_sub(collector.received_bytes),
        // The execd never said how much it produced: the gaps the
        // replay exposed are the only measure available.
        None => collector.missing_bytes,
    };
    let dropped_bytes = collector.dropped_bytes;
    let truncated = collector.truncated;
    // The diagnostics travel with the text, never inside the collected
    // bytes: the length of the output is what the completeness is
    // measured against.
    let mut text = String::from_utf8_lossy(&output).to_string();
    if let Some(err) = attach_error {
        text.push_str(&format!("[attach failed: {err}]"));
    }
    if let Some(err) = replay_error {
        text.push_str(&err);
    }
    if truncated {
        // The truncation is SAID, not hidden: a reader that only looks
        // at the text still learns that the output is not the whole one.
        text.push_str(&format!(
            "\n[output truncated: {dropped_bytes} bytes exceeded the quota of {} bytes]",
            collector.limit
        ));
    }
    let replayed = collector.replayed;
    match collector.terminal_code {
        Some(code) if code >= 0 => OneshotOutcome {
            exit_code: Some(code),
            output: text,
            end: OneshotEnd::Completed,
            expected_bytes,
            missing_bytes,
            dropped_bytes,
            truncated,
            replayed,
        },
        _ => OneshotOutcome {
            exit_code: None,
            output: text,
            // The session EXISTED: the command may have started, so it is
            // NOT safe to run it again.
            end: OneshotEnd::TimedOut,
            expected_bytes,
            missing_bytes,
            dropped_bytes,
            truncated,
            replayed,
        },
    }
}

/// Read the replay of a finished session until the output is complete.
///
/// The execd keeps what it produced and replays it on a new attach: the
/// replay carries the offset it resumes from, so the bytes already
/// collected are not duplicated and a gap is counted instead of being
/// swallowed.
async fn replay_missing(
    pipe_session: &mut pipe::PipeSession,
    output: &mut Vec<u8>,
    collector: &mut Collector,
    expected: Option<usize>,
    timeout: Duration,
) {
    let deadline = Instant::now() + timeout;
    // The collection cannot grow past its quota: a replay that arrives
    // after that only confirms what is already known.
    while !collector.truncated
        && expected.is_none_or(|expected| collector.received_bytes < expected)
    {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let wait = deadline
            .saturating_duration_since(now)
            .min(Duration::from_millis(500));
        match tokio::time::timeout(wait, pipe_session.next_event()).await {
            Ok(Some(event)) => apply_event(output, collector, event),
            // The pipe closed: no more events will ever arrive.
            Ok(None) => break,
            Err(_) => continue,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_output_that_arrives_after_the_exit_frame_is_kept() {
        // The execd drains the PTY buffer AFTER the exit frame: the tail
        // of the output must survive it.
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"first\n".to_vec()),
        );
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Exit { exit_code: Some(0) },
        );
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"last\n".to_vec()),
        );
        assert_eq!(output, b"first\nlast\n".to_vec());
        assert_eq!(collector.terminal_code, Some(0));
        assert!(collector.draining);
    }

    #[test]
    fn the_exit_frame_ends_the_collection_and_records_the_status() {
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Exit { exit_code: Some(3) },
        );
        assert_eq!(collector.terminal_code, Some(3));
        assert!(collector.draining);
        assert!(output.is_empty());
    }

    #[test]
    fn a_replayed_output_is_collected_too() {
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Replay {
                offset: 0,
                data: b"missed\n".to_vec(),
            },
        );
        assert_eq!(output, b"missed\n".to_vec());
        assert_eq!(collector.terminal_code, None);
    }

    #[test]
    fn a_replay_resumes_at_its_offset_without_duplicating() {
        // The execd replays what it kept, starting at an offset: the
        // bytes already collected are not written twice.
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"first\n".to_vec()),
        );
        apply_replay(&mut output, &mut collector, 0, b"first\nlast\n");
        assert_eq!(output, b"first\nlast\n".to_vec());
        assert_eq!(collector.missing_bytes, 0);
        assert!(collector.replayed);
    }

    #[test]
    fn a_replay_that_starts_beyond_what_arrived_is_a_visible_hole() {
        // A gap in the output is counted, never swallowed: a truncated
        // output is not announced as a complete one. The replayed bytes
        // belong at their own position in the stream, so they are NOT
        // appended at the end of what arrived: the collection stays a
        // faithful PREFIX of the session output.
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"first\n".to_vec()),
        );
        apply_replay(&mut output, &mut collector, 10, b"tail\n");
        assert_eq!(output, b"first\n".to_vec());
        assert_eq!(collector.missing_bytes, 4);
        assert!(!collector.replayed);
    }

    #[test]
    fn a_replay_that_continues_the_stream_completes_it() {
        // The normal case: the live stream stopped at the offset the
        // replay resumes from.
        let mut output = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"first\n".to_vec()),
        );
        apply_replay(&mut output, &mut collector, 6, b"last\n");
        assert_eq!(output, b"first\nlast\n".to_vec());
        assert_eq!(collector.missing_bytes, 0);
        assert!(collector.replayed);
    }

    #[test]
    fn the_completeness_of_the_output_is_measured_against_the_prefix() {
        // What the execd says it produced is the yardstick: the bytes
        // beyond the collected prefix never arrived.
        let expected = 10usize;
        let output = b"first\n".to_vec();
        assert_eq!(expected.saturating_sub(output.len()), 4);
        // A gap in the middle is part of the same count: the collection
        // stops at the gap, so the missing bytes include it.
        let mut gapped = Vec::new();
        let mut collector = Collector::default();
        apply_event(
            &mut gapped,
            &mut collector,
            ServerEvent::Stdout(b"first\n".to_vec()),
        );
        apply_replay(&mut gapped, &mut collector, 8, b"tail\n");
        assert_eq!(expected.saturating_sub(gapped.len()), 4);
        assert_eq!(collector.missing_bytes, 2);
    }

    #[test]
    fn succeeded_requires_zero_exit() {
        let ok = OneshotOutcome {
            exit_code: Some(0),
            output: "ok".into(),
            end: OneshotEnd::Completed,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(ok.succeeded());
        let absent = OneshotOutcome {
            exit_code: None,
            output: String::new(),
            end: OneshotEnd::TimedOut,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(!absent.succeeded());
        let failed = OneshotOutcome {
            exit_code: Some(2),
            output: String::new(),
            end: OneshotEnd::Completed,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(!failed.succeeded());
    }

    #[test]
    fn only_a_refused_session_is_safe_to_repeat() {
        // The execd refused: the command never ran, so the attempt may
        // be repeated without running the command twice.
        let refused = OneshotOutcome {
            exit_code: None,
            output: "[create failed: connection refused]".into(),
            end: OneshotEnd::NotAccepted,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(refused.not_accepted());
        assert!(!refused.succeeded());

        // The session existed and the command ran: repeating it would
        // run the command twice.
        let ran = OneshotOutcome {
            exit_code: Some(1),
            output: "boom".into(),
            end: OneshotEnd::Completed,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(!ran.not_accepted());

        let silent = OneshotOutcome {
            exit_code: None,
            output: String::new(),
            end: OneshotEnd::TimedOut,
            expected_bytes: None,
            missing_bytes: 0,
            dropped_bytes: 0,
            truncated: false,
            replayed: false,
        };
        assert!(!silent.not_accepted());
    }

    #[test]
    fn the_collection_stops_at_the_quota_and_counts_what_it_dropped() {
        // A command that prints without end does not grow the memory of
        // the harness: the collection stops at the quota.
        let mut output = Vec::new();
        let mut collector = Collector::with_limit(8);
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"0123456789".to_vec()),
        );
        assert_eq!(output, b"01234567".to_vec(), "the quota is respected");
        assert_eq!(collector.dropped_bytes, 2, "what did not fit is counted");
        assert!(collector.truncated, "the truncation is announced");
        assert_eq!(collector.received_bytes, 10, "the bytes did arrive");
    }

    #[test]
    fn the_bytes_that_were_dropped_are_not_reported_as_missing() {
        // A byte that arrived and was dropped is NOT a byte that was
        // lost: the two counts must not be mixed.
        let mut output = Vec::new();
        let mut collector = Collector::with_limit(4);
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"0123456789".to_vec()),
        );
        // The execd says it produced ten bytes: all ten arrived, so
        // none of them is missing.
        let expected = 10usize;
        let missing = expected.saturating_sub(collector.received_bytes);
        assert_eq!(missing, 0, "nothing was lost: it was dropped");
        assert_eq!(collector.dropped_bytes, 6);
    }

    #[test]
    fn a_collection_cut_by_the_quota_still_knows_where_the_stream_is() {
        // The position in the stream is the number of bytes that
        // ARRIVED: a collection cut by the quota does not mistake the
        // end of what it kept for the end of the stream.
        let mut output = Vec::new();
        let mut collector = Collector::with_limit(4);
        apply_event(
            &mut output,
            &mut collector,
            ServerEvent::Stdout(b"0123456789".to_vec()),
        );
        // A replay that resumes at the position the stream reached adds
        // bytes that are new to the stream: they arrive, and they do
        // not fit either.
        apply_replay(&mut output, &mut collector, 10, b"more");
        assert_eq!(output, b"0123".to_vec(), "the quota still holds");
        assert_eq!(collector.dropped_bytes, 10);
        assert_eq!(collector.received_bytes, 14);
        assert_eq!(collector.missing_bytes, 0, "no hole: the bytes arrived");
    }
}
