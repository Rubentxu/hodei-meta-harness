//! The measurement of the transport against a local fake execd: how many
//! calls it makes, how many connections it opens and how long a command
//! takes.
//!
//! The numbers are the evidence of the transport's cost: the readiness
//! that is observed once, the sessions that are opened once and the
//! deadline that bounds the whole thing.

use std::time::{Duration, Instant};

use super::client::ExecdClient;
use super::deadline::Deadline;
use super::endpoint::ExecdEndpoint;
use super::fake::{FakeBehavior, FakeExecd};
use super::oneshot::{self, READINESS_PROBE_COMMAND};

/// How many commands the measurement runs.
const COMMANDS: usize = 20;

/// The budget of one command.
const COMMAND_BUDGET: Duration = Duration::from_secs(5);

/// The most HTTP requests one session may cost: create, session state,
/// delete and the WebSocket upgrade of the attach.
const MAX_HTTP_REQUESTS_PER_SESSION: usize = 4;

/// The most pipe attachments one session may cost: the live stream. A
/// replay is only attached when the live stream lost bytes, and the
/// cooperative fake delivers all of them.
const MAX_PIPE_ATTACHES_PER_SESSION: usize = 1;

/// The baseline of the transport BEFORE the readiness was cached.
///
/// It comes from the previous `run_when_ready`: every call ran the
/// bounded readiness wait (`wait_until_ready`) and then the command, so
/// the readiness was paid once per command — `COMMANDS` readiness probes
/// and two sessions (the probe and the command) for each of the
/// `COMMANDS` commands.
#[derive(Debug, Clone, Copy)]
struct Baseline {
    /// The readiness probes of the previous transport.
    probes: usize,
    /// The sessions of the previous transport.
    creates: usize,
    /// The deletions of the previous transport.
    deletes: usize,
}

/// One readiness probe and one command session per command.
const BASELINE: Baseline = Baseline {
    probes: COMMANDS,
    creates: COMMANDS * 2,
    deletes: COMMANDS * 2,
};

/// What the measurement observed.
#[derive(Debug, Clone)]
pub struct Measurement {
    /// The commands that were run.
    pub commands: usize,
    /// The readiness probes that were run.
    pub probes: usize,
    /// The HTTP requests the transport made.
    pub http_requests: usize,
    /// The TCP connections the transport opened.
    pub connections: usize,
    /// The sessions the transport created.
    pub creates: usize,
    /// The sessions the transport deleted.
    pub deletes: usize,
    /// The session states the transport asked for.
    pub statuses: usize,
    /// The pipes the transport attached to.
    pub pipes: usize,
    /// The median duration of a command.
    pub p50_ms: f64,
    /// The ninety-fifth percentile of the duration of a command.
    pub p95_ms: f64,
}

impl Measurement {
    /// The measurement as one line of text.
    pub fn summary(&self) -> String {
        format!(
            "commands={} probes={} http_requests={} connections={} creates={} deletes={} \
             statuses={} pipes={} p50_ms={:.2} p95_ms={:.2}",
            self.commands,
            self.probes,
            self.http_requests,
            self.connections,
            self.creates,
            self.deletes,
            self.statuses,
            self.pipes,
            self.p50_ms,
            self.p95_ms
        )
    }
}

/// The percentile of a sorted list of durations, nearest rank.
fn percentile(sorted: &[Duration], fraction: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (fraction * sorted.len() as f64).ceil().max(1.0) as usize;
    let index = rank.min(sorted.len()) - 1;
    sorted[index].as_secs_f64() * 1000.0
}

/// The command of one index of the measurement.
fn command_of(index: usize) -> String {
    format!("command-{index}")
}

/// The transport of one sandbox: ONE client for every command of it.
fn client_of(fake: &FakeExecd) -> ExecdClient {
    ExecdClient::new(ExecdEndpoint::new(fake.base_url.clone()))
}

#[tokio::test]
async fn the_transport_is_measured_against_a_local_fake_execd() {
    let fake = FakeExecd::start(FakeBehavior::Cooperative).await;
    let client = client_of(&fake);
    let mut durations = Vec::new();
    for index in 0..COMMANDS {
        let started = Instant::now();
        let outcome = oneshot::run_when_ready(
            &client,
            &command_of(index),
            Deadline::in_budget(COMMAND_BUDGET),
        )
        .await;
        durations.push(started.elapsed());
        assert!(
            outcome.succeeded(),
            "the command {index} did not succeed: {}",
            outcome.output
        );
    }
    let counts = fake.counts();
    let mut sorted = durations.clone();
    sorted.sort();
    let probes = counts.times(READINESS_PROBE_COMMAND);
    let measurement = Measurement {
        commands: counts.commands.len() - probes,
        probes,
        http_requests: counts.http_requests,
        connections: counts.connections,
        creates: counts.creates,
        deletes: counts.deletes,
        statuses: counts.statuses,
        pipes: counts.pipes,
        p50_ms: percentile(&sorted, 0.50),
        p95_ms: percentile(&sorted, 0.95),
    };
    println!("MEASUREMENT {}", measurement.summary());
    println!(
        "BASELINE probes={} creates={} deletes={} (the previous one probe per command)",
        BASELINE.probes, BASELINE.creates, BASELINE.deletes
    );
    assert_eq!(
        measurement.commands, COMMANDS,
        "every command was run exactly once"
    );
    assert_eq!(
        measurement.probes, 1,
        "the readiness of the generation is observed once"
    );
    assert!(
        measurement.probes < BASELINE.probes,
        "the readiness is no longer paid per command: {} probes, baseline {}",
        measurement.probes,
        BASELINE.probes
    );
    assert_eq!(
        measurement.creates,
        measurement.commands + measurement.probes,
        "one session per command, plus the single readiness probe"
    );
    assert_eq!(
        measurement.deletes, measurement.creates,
        "every session the transport opened was deleted"
    );
    assert!(
        measurement.http_requests <= measurement.creates * MAX_HTTP_REQUESTS_PER_SESSION,
        "{} HTTP requests for {} sessions exceed the bound of {} per session",
        measurement.http_requests,
        measurement.creates,
        MAX_HTTP_REQUESTS_PER_SESSION
    );
    assert!(
        measurement.pipes <= measurement.creates * MAX_PIPE_ATTACHES_PER_SESSION,
        "{} pipe attaches for {} sessions exceed the bound of {} per session",
        measurement.pipes,
        measurement.creates,
        MAX_PIPE_ATTACHES_PER_SESSION
    );
}

#[tokio::test]
async fn a_spent_budget_is_refused_even_when_the_readiness_is_cached() {
    // The readiness of the generation is already observed: a cache hit
    // must NOT buy a command a budget that is already spent.
    let fake = FakeExecd::start(FakeBehavior::Cooperative).await;
    let client = client_of(&fake);
    let first =
        oneshot::run_when_ready(&client, &command_of(0), Deadline::in_budget(COMMAND_BUDGET)).await;
    assert!(first.succeeded(), "the first command did not succeed");
    assert!(
        client.readiness_is_current(),
        "the readiness of the generation is cached"
    );
    let before = fake.counts();

    let outcome =
        oneshot::run_when_ready(&client, &command_of(1), Deadline::in_budget(Duration::ZERO)).await;
    assert!(
        outcome.not_accepted(),
        "a spent budget is refused: {outcome:?}"
    );
    assert!(
        !outcome.uncertain(),
        "the command never ran, so the result is not uncertain"
    );

    let after = fake.counts();
    assert_eq!(
        after.creates, before.creates,
        "no session was opened with a spent budget"
    );
    assert_eq!(
        after.times(&command_of(1)),
        0,
        "the real command never ran with a spent budget"
    );
}

#[tokio::test]
async fn several_sessions_in_a_ready_sandbox_probe_the_readiness_once() {
    let fake = FakeExecd::start(FakeBehavior::Cooperative).await;
    let client = client_of(&fake);
    for index in 0..3 {
        let outcome = oneshot::run_when_ready(
            &client,
            &command_of(index),
            Deadline::in_budget(COMMAND_BUDGET),
        )
        .await;
        assert!(outcome.succeeded(), "the command {index} did not succeed");
    }
    let counts = fake.counts();
    assert_eq!(
        counts.times(READINESS_PROBE_COMMAND),
        1,
        "the real probe runs once for the generation, not once per command"
    );
    for index in 0..3 {
        assert_eq!(
            counts.times(&command_of(index)),
            1,
            "the command {index} ran exactly once"
        );
    }
}

#[tokio::test]
async fn a_lost_session_invalidates_the_readiness() {
    // The first session is lost: the readiness observed for it does not
    // hold anymore, so it is observed again.
    let fake = FakeExecd::start(FakeBehavior::LosesTheFirstSession).await;
    let client = client_of(&fake);
    let outcome =
        oneshot::run_when_ready(&client, &command_of(0), Deadline::in_budget(COMMAND_BUDGET)).await;
    println!("OUTCOME {outcome:?}");
    println!("COUNTS {:?}", fake.counts());
    assert!(outcome.succeeded(), "the command did not succeed");
    let counts = fake.counts();
    assert_eq!(
        counts.times(READINESS_PROBE_COMMAND),
        2,
        "the readiness is observed again after the loss"
    );
    assert_eq!(counts.times(&command_of(0)), 1, "the command ran once");
}

#[tokio::test]
async fn a_blocked_call_respects_the_budget_and_is_not_repeated() {
    // The execd receives the create and never answers: the call must
    // return inside the budget, and the command must not run again.
    let fake = FakeExecd::start(FakeBehavior::SilentOnCreate).await;
    let client = client_of(&fake);
    let budget = Duration::from_secs(1);
    let started = Instant::now();
    let outcome = oneshot::run(&client, &command_of(0), Deadline::in_budget(budget)).await;
    let elapsed = started.elapsed();
    assert!(
        elapsed <= budget + Duration::from_millis(500),
        "the call respected the budget: {elapsed:?}"
    );
    assert!(
        outcome.uncertain(),
        "the session may exist: the result is uncertain, not a refusal"
    );
    assert!(
        !outcome.repeatable(),
        "an uncertain command is not repeated"
    );
    let counts = fake.counts();
    assert_eq!(
        counts.times(&command_of(0)),
        1,
        "the command was asked for once"
    );
}

#[tokio::test]
async fn a_lost_session_leaves_a_result_that_is_not_repeated() {
    // The session existed and its pipe closed without an exit frame:
    // the command may have run, so it must not run again.
    let fake = FakeExecd::start(FakeBehavior::LosesTheSession).await;
    let client = client_of(&fake);
    let outcome = oneshot::run(&client, &command_of(0), Deadline::in_budget(COMMAND_BUDGET)).await;
    assert!(outcome.uncertain(), "the result is uncertain");
    assert!(
        !outcome.repeatable(),
        "an uncertain command is not repeated"
    );
    let counts = fake.counts();
    assert_eq!(counts.times(&command_of(0)), 1, "the command ran once");
}

#[tokio::test]
async fn a_refused_session_is_not_accepted_and_may_be_repeated() {
    // The execd refuses the session: the command never ran.
    let fake = FakeExecd::start(FakeBehavior::RefusesCreate).await;
    let client = client_of(&fake);
    let outcome = oneshot::run(&client, &command_of(0), Deadline::in_budget(COMMAND_BUDGET)).await;
    assert!(outcome.not_accepted(), "the session was refused");
    assert!(outcome.repeatable(), "a refused command may be repeated");
    assert!(!outcome.succeeded());
}
