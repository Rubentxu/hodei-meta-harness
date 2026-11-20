//! The live runtime: select, record, claim, work, verify, publish — for
//! real.
//!
//! The runtime owns the ORDER of an attempt. Selection, the durable
//! record and the claim happen before any effect, so idle, ineligible
//! and already-claimed work never reaches the agent and a crash between
//! the record and the claim has an owner to recover. The attempt ends in
//! ONE terminal value, and the claim is released exactly once whichever
//! way it ends.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use run::runtime::{
    CustomObjective, ObjectiveHandle, ObjectiveOutcome, ObjectiveRequest, ObjectiveRuntime, RunId,
    RuntimeError, RuntimeIdentity,
};
use tracker::core::TrackerRead;

use crate::core::{GoalRunner, RunnerError};
use crate::ports::{
    AgentPort, Checkpointed, Closed, Closes, Objective, ObjectiveCheck, PublisherPort,
    RequirementsPort, VerifierPort,
};

/// The claim this runtime holds on the work it selected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Claimed {
    /// The epoch of the claim. It is the value the release must state, so
    /// a stale attempt cannot release a claim it no longer owns.
    pub epoch: u64,
}

/// Why the claim was not taken.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ClaimError {
    #[error("the work is already claimed by {holder}")]
    Denied { holder: String },
    #[error("the claim store could not be reached: {reason}")]
    Unavailable { reason: String },
}

/// The store could not give the work back.
///
/// A claim that stays held is not a silent fact: whoever reads the run
/// must see that the work may still belong to this runtime, so the
/// refusal names the work, the epoch and the store's own reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the claim of {work_ref} (epoch {epoch}) could not be released: {reason}")]
pub struct ClaimReleaseError {
    pub work_ref: String,
    pub epoch: u64,
    pub reason: String,
}

/// The claim store the live runtime talks to.
pub trait ClaimPort: Send + Sync {
    /// Take the work for this runtime. One holder per work: a second
    /// claim is denied with the holder's name.
    fn claim(&self, work_ref: &str) -> Result<Claimed, ClaimError>;

    /// Give the work back. Called once per successful claim, whatever the
    /// attempt ended with. A store that could not take the work back is
    /// reported, never ignored: the attempt must say the claim may still
    /// be held.
    fn release(&self, work_ref: &str, epoch: u64) -> Result<(), ClaimReleaseError>;

    /// The claims held right now, as work ref → holder.
    ///
    /// Selection reads it so work somebody else holds is skipped instead
    /// of selected and denied afterwards. The default is empty: a store
    /// that models no other holder has nothing to report.
    fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
        Ok(BTreeMap::new())
    }
}

/// The goal the durable attempt record describes: what the attempt is
/// about, so a recovery can say it out loud.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptGoal {
    pub work_ref: String,
    pub instruction: String,
    pub run: u64,
}

/// Why the durable record of the attempt could not be opened.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the attempt record of {work_ref} could not be opened: {reason}")]
pub struct AttemptRecordError {
    pub work_ref: String,
    pub reason: String,
}

/// The durable record of the attempt, written BEFORE the claim: a crash
/// between the record and the claim then has an owner, and the recovery
/// of the record can give the claim back.
pub trait AttemptRecordPort: Send + Sync {
    /// Ask what the previous attempt left behind, and resolve it: a
    /// pending record releases the dead run's claim, a confirmed one is
    /// never redone. An unrecovered record refuses the next attempt.
    fn recover(&self) -> Result<(), AttemptRecordError>;

    /// Open the Pending record of this attempt, with its handoff.
    fn begin_attempt(&self, goal: &AttemptGoal) -> Result<(), AttemptRecordError>;
}

/// The record nobody keeps: for runtimes that own no durable state (the
/// offline tests). The live runtime never uses it — the product wires
/// the durable one.
pub struct NoAttemptRecord;

impl AttemptRecordPort for NoAttemptRecord {
    fn recover(&self) -> Result<(), AttemptRecordError> {
        Ok(())
    }

    fn begin_attempt(&self, _goal: &AttemptGoal) -> Result<(), AttemptRecordError> {
        Ok(())
    }
}

/// How a live attempt ended, before it is read as an objective outcome.
///
/// The vocabulary is TOTAL: every way an attempt can stop has a value,
/// so no path ends without a recorded verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttemptEnd {
    /// The candidate was published; `closure` says how the closure ended.
    Published {
        work_ref: String,
        closure: Closed,
        detail: String,
    },
    /// The verification did not cover every required check.
    ChecksNotHeld {
        work_ref: String,
        missing: Vec<String>,
    },
    /// The named work was not eligible in the observed snapshot.
    NamedNotEligible { work_ref: String, reason: String },
    /// The candidate could not be verified.
    Unverifiable { work_ref: String, reason: String },
    /// Somebody else holds the work.
    ClaimDenied { work_ref: String, holder: String },
    /// The attempt was stopped before its effects could be published.
    Cancelled { work_ref: String },
    /// The agent could not run the objective. An agent that ends its turn
    /// with no candidate changes arrives HERE, carrying the stage's own
    /// cause: the port refuses before the runner ever sees a diff, so the
    /// vocabulary keeps no separate no-change state — it could only
    /// duplicate a cause that already travels with the reason.
    AgentFailed { work_ref: String, reason: String },
    /// The infrastructure failed around the attempt.
    InfraFailed { reason: String },
    /// The candidate could not be promoted.
    PromotionFailed { work_ref: String, reason: String },
    /// The candidate WAS promoted and the durable record could not be
    /// settled: the effect is a fact, only the record is uncertain, and
    /// the effect is never applied a second time.
    PromotedUnconfirmed { work_ref: String, detail: String },
    /// There was no eligible work at all.
    NoEligibleWork { reason: String },
    /// The attempt ended as `end`, but the claim release did not
    /// complete: the verdict travels with the release failure so neither
    /// half is lost, and the work may still be held.
    UncertainRelease {
        release: ClaimReleaseError,
        end: Box<AttemptEnd>,
    },
}

/// The outcome the attempt ended with.
///
/// Effects that moved without a confirmed closure are a FAILURE, never a
/// confirmation: the record must not claim the work was closed.
pub fn outcome_of(end: AttemptEnd) -> ObjectiveOutcome {
    match end {
        AttemptEnd::Published {
            work_ref,
            closure: Closed::Confirmed | Closed::NotRequired,
            detail,
        } => ObjectiveOutcome::Confirmed { work_ref, detail },
        AttemptEnd::Published {
            work_ref,
            closure: Closed::Unconfirmed,
            detail,
        } => ObjectiveOutcome::Failed {
            reason: format!(
                "the effects of {work_ref} were moved but the closure was not confirmed: {detail}"
            ),
        },
        AttemptEnd::ChecksNotHeld { work_ref, missing } => ObjectiveOutcome::Rejected {
            work_ref,
            reason: format!("the evidence does not satisfy {}", missing.join(", ")),
        },
        AttemptEnd::NamedNotEligible { work_ref, reason } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} is not eligible: {reason}"),
            work_ref,
        },
        AttemptEnd::Unverifiable { work_ref, reason } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} could not be verified: {reason}"),
            work_ref,
        },
        AttemptEnd::ClaimDenied { work_ref, holder } => ObjectiveOutcome::Rejected {
            reason: format!("{work_ref} is claimed by {holder}"),
            work_ref,
        },
        AttemptEnd::Cancelled { work_ref } => ObjectiveOutcome::Failed {
            reason: format!("{work_ref} was cancelled before it was published"),
        },
        AttemptEnd::AgentFailed { work_ref, reason } => ObjectiveOutcome::Failed {
            reason: format!("the agent could not run {work_ref}: {reason}"),
        },
        AttemptEnd::InfraFailed { reason } => ObjectiveOutcome::Failed { reason },
        AttemptEnd::PromotionFailed { work_ref, reason } => ObjectiveOutcome::Failed {
            reason: format!("the candidate of {work_ref} could not be promoted: {reason}"),
        },
        AttemptEnd::PromotedUnconfirmed { work_ref, detail } => ObjectiveOutcome::Failed {
            reason: format!(
                "the candidate of {work_ref} was promoted but the durable record is unconfirmed: {detail}; the effect is durable and must never be re-applied to settle the record"
            ),
        },
        AttemptEnd::NoEligibleWork { reason } => ObjectiveOutcome::Idle { reason },
        // The release failure is the headline: an operator must see that
        // the work may still be held. The attempt's own verdict travels
        // with it, so the end is never lost behind the store's refusal.
        AttemptEnd::UncertainRelease { release, end } => ObjectiveOutcome::Failed {
            reason: format!(
                "the claim release did not complete: {release}; the attempt itself ended as: {}",
                outcome_of(*end).describe()
            ),
        },
    }
}

/// A claim that is released exactly once, whichever way the attempt ends.
///
/// The guard is owned by the attempt itself, so an attempt whose task is
/// ABORTED (a Stop) still releases its claim when the future is dropped:
/// the release does not depend on the attempt reaching its end.
///
/// `Drop` cannot return the store's refusal, so a failed release is
/// stowed in the slot instead of dropped: the attempt drains the slot at
/// its terminal step and wraps its verdict as an uncertain release.
struct ClaimGuard {
    claims: Arc<dyn ClaimPort>,
    work_ref: String,
    epoch: u64,
    released: bool,
    release_failed: Arc<Mutex<Option<ClaimReleaseError>>>,
}

impl ClaimGuard {
    fn new(claims: Arc<dyn ClaimPort>, work_ref: &str, epoch: u64) -> Self {
        Self {
            claims,
            work_ref: work_ref.to_string(),
            epoch,
            released: false,
            release_failed: Arc::new(Mutex::new(None)),
        }
    }

    /// Give the work back now. Calling it twice releases once. A release
    /// the store refused is stowed in the slot, never discarded.
    fn release_now(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        if let Err(error) = self.claims.release(&self.work_ref, self.epoch) {
            let mut slot = self
                .release_failed
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *slot = Some(error);
        }
    }

    /// The release failure the store left in the slot, if any. Taking it
    /// is the terminal step: a drained slot means the work went back.
    fn take_release_error(&self) -> Option<ClaimReleaseError> {
        self.release_failed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

impl Drop for ClaimGuard {
    fn drop(&mut self) {
        self.release_now();
    }
}

/// The cancellation state of the runtime: the attempt in flight and the
/// stops that arrived before their attempts installed their flags.
///
/// The state is shared through an `Arc` so the attempt and whoever raises
/// the stop read the SAME answer: a stop is never addressed to an attempt
/// that has not started, and never to the one that came after it.
///
/// Every pending stop is remembered by the run it names, so stops for
/// different runs never overwrite each other. A pending stop lives until
/// the run it names installs its flag or a newer run supersedes it.
#[derive(Debug, Default)]
struct Cancellations {
    state: Mutex<CancellationState>,
}

#[derive(Debug, Default)]
struct CancellationState {
    /// The stops of the runs whose attempts have not installed their flag
    /// yet. They are held here so those attempts begin already cancelled
    /// instead of running work nobody wants anymore.
    early: BTreeSet<RunId>,
    /// The run whose attempt is in flight, with the flag it reads.
    current: Option<(RunId, Arc<AtomicBool>)>,
}

impl Cancellations {
    /// Install the flag of `run`'s attempt and return it.
    ///
    /// The flag REPLACES the previous one, so a stop raised for an earlier
    /// attempt cannot be read by this one. It starts true when its own
    /// stop arrived before the attempt did: a stop raised while the run
    /// was starting is never lost. An attempt that installs late never
    /// steals the slot from a newer one.
    ///
    /// The pending stops of OLDER runs are superseded here, and that is
    /// provably inert: a stop never starts an attempt — it can only mark
    /// the flag of the run it names — so a superseded stop neither
    /// reaches the newer attempt nor revives the older one. The test
    /// `a_pending_stop_superseded_by_a_newer_install_starts_and_cancels_nothing`
    /// keeps that invariant observable.
    fn install(&self, run: RunId) -> Arc<AtomicBool> {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        // One attempt per run installs its flag once; a second install
        // keeps the flag a stop may already have reached.
        if let Some((current, flag)) = &state.current
            && *current == run
        {
            return flag.clone();
        }
        // The pending stops are addressed by run, so this install consumes
        // only its own. The stops of older runs are history: this install
        // supersedes them. The stops of newer runs stay pending.
        let stopped_early = state.early.remove(&run);
        state.early.retain(|pending| *pending > run);
        let cancelled = Arc::new(AtomicBool::new(stopped_early));
        if state
            .current
            .as_ref()
            .is_none_or(|(current, _)| *current < run)
        {
            state.current = Some((run, cancelled.clone()));
        }
        cancelled
    }

    /// Deliver a stop to the attempt of `run`, or keep it for the attempt
    /// that has not started yet.
    ///
    /// A stop that belongs to an older run than the attempt in flight is
    /// history: it is dropped instead of reaching an attempt it was never
    /// raised for.
    fn cancel(&self, run: RunId) {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        match &state.current {
            Some((current, flag)) if *current == run => {
                flag.store(true, Ordering::SeqCst);
            }
            Some((current, _)) if *current > run => {}
            _ => {
                state.early.insert(run);
            }
        }
    }
}

/// The live runtime: the product's ports, wired in the product's order.
pub struct LiveRuntime {
    identity: RuntimeIdentity,
    project: String,
    tracker: Arc<dyn TrackerRead>,
    requirements: Arc<dyn RequirementsPort>,
    agent: Arc<dyn AgentPort>,
    verifier: Arc<dyn VerifierPort>,
    publisher: Arc<dyn PublisherPort>,
    claims: Arc<dyn ClaimPort>,
    records: Arc<dyn AttemptRecordPort>,
    cancellations: Arc<Cancellations>,
}

impl LiveRuntime {
    /// A runtime that serves `project` through the given ports.
    ///
    /// The attempt record of such a runtime is the one nobody keeps:
    /// `with_records` wires the durable one, and the LIVE product must
    /// not run without it.
    pub fn new(
        project: impl Into<String>,
        tracker: Arc<dyn TrackerRead>,
        requirements: Arc<dyn RequirementsPort>,
        agent: Arc<dyn AgentPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<dyn PublisherPort>,
        claims: Arc<dyn ClaimPort>,
    ) -> Self {
        let project = project.into();
        Self {
            identity: RuntimeIdentity::live(project.clone()),
            project,
            tracker,
            requirements,
            agent,
            verifier,
            publisher,
            claims,
            records: Arc::new(NoAttemptRecord),
            cancellations: Arc::new(Cancellations::default()),
        }
    }

    /// The runtime that keeps its attempts in the durable record `records`.
    pub fn with_records(mut self, records: Arc<dyn AttemptRecordPort>) -> Self {
        self.records = records;
        self
    }

    /// Ask the attempt of `run` to stop after the agent returns. The
    /// attempt then releases its claim without verifying or publishing.
    ///
    /// A stop for a run whose attempt has not started yet is REMEMBERED by
    /// that run: the attempt begins already cancelled, so a stop raised
    /// while the run was starting is never lost. A stop for an older run
    /// than the attempt in flight never reaches it.
    pub fn cancel(&self, run: RunId) {
        self.cancellations.cancel(run);
    }

    fn runner(&self) -> GoalRunner {
        GoalRunner::new(
            self.tracker.clone(),
            self.requirements.clone(),
            self.agent.clone(),
            self.verifier.clone(),
            self.publisher.clone(),
        )
    }

    /// Ask the durable record before the attempt runs: the recovery
    /// resolves what the last attempt left (and gives back the claim of
    /// a dead one), and the Pending record of this attempt is opened.
    fn open_record(&self, objective: &Objective, run: RunId) -> Result<(), AttemptRecordError> {
        self.records.recover()?;
        self.records.begin_attempt(&AttemptGoal {
            work_ref: objective.work_ref.clone(),
            instruction: objective.instruction.clone(),
            run: run.number(),
        })
    }
}

#[async_trait::async_trait]
impl ObjectiveRuntime for LiveRuntime {
    fn identity(&self) -> RuntimeIdentity {
        self.identity.clone()
    }

    async fn start(
        &self,
        run: RunId,
        request: ObjectiveRequest,
    ) -> Result<ObjectiveHandle, RuntimeError> {
        if request.project != self.project {
            return Err(RuntimeError(format!(
                "this runtime serves '{}', not '{}'",
                self.project, request.project
            )));
        }
        // A cancellation belongs to the run it was raised for: the attempt
        // gets a FRESH flag, and a stop that arrived before the attempt
        // did starts it cancelled instead of being lost.
        let cancelled = self.cancellations.install(run);
        // A custom objective owns no ticket: it selects nothing, claims
        // nothing and writes no tracker, so no selection runs for it.
        // The durable record still comes first: an effect without an
        // owner record is exactly what a crash must not leave behind.
        if let Some(custom) = &request.objective {
            if request.work_ref.is_some() {
                return Err(RuntimeError(
                    "a custom objective owns no ticket: name a ticket or an objective, not both"
                        .into(),
                ));
            }
            let objective = custom_objective(custom, run);
            if let Err(error) = self.open_record(&objective, run) {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::InfraFailed {
                        reason: error.to_string(),
                    },
                )));
            }
            return Ok(self.handle(objective, None, cancelled));
        }
        // Work somebody else holds is not selected at all: it is not
        // eligible, and the chain skips it.
        let held = match self.claims.claims() {
            Ok(held) => held,
            Err(error) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::InfraFailed {
                        reason: format!("the held claims could not be read: {error}"),
                    },
                )));
            }
        };
        let selected = self
            .runner()
            .select_in(request.work_ref.as_deref(), &held)
            .await;
        let item = match selected {
            Ok(Some(item)) => item,
            Ok(None) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::NoEligibleWork {
                        reason: no_eligible_work(&held),
                    },
                )));
            }
            Err(error) => {
                return Ok(ObjectiveHandle::finished(outcome_of(early_selection_end(
                    request.work_ref.as_deref(),
                    error,
                ))));
            }
        };
        // The requirements are read BEFORE the claim: they are a pure
        // read, and work whose contract cannot be read was never worth
        // claiming. The durable record is written BEFORE the claim too:
        // a crash between the two then has an owner to recover.
        let mut objective = match self.requirements.requirements(&item) {
            Ok(objective) => objective,
            Err(error) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::Unverifiable {
                        work_ref: item.id,
                        reason: error.to_string(),
                    },
                )));
            }
        };
        objective.run = Some(run.number());
        if let Err(error) = self.open_record(&objective, run) {
            return Ok(ObjectiveHandle::finished(outcome_of(
                AttemptEnd::InfraFailed {
                    reason: error.to_string(),
                },
            )));
        }
        let epoch = match self.claims.claim(&item.id) {
            Ok(claimed) => claimed.epoch,
            Err(ClaimError::Denied { holder }) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::ClaimDenied {
                        work_ref: item.id,
                        holder,
                    },
                )));
            }
            Err(ClaimError::Unavailable { reason }) => {
                return Ok(ObjectiveHandle::finished(outcome_of(
                    AttemptEnd::InfraFailed { reason },
                )));
            }
        };
        // The guard is created with the claim and travels with the attempt:
        // a dropped attempt (aborted) still gives the work back.
        let guard = ClaimGuard::new(self.claims.clone(), &item.id, epoch);
        Ok(self.handle(objective, Some(guard), cancelled))
    }

    /// Signal the attempt of `run`: it ends after the agent returns,
    /// without verifying or publishing, and releases everything it owns.
    fn cancel(&self, run: RunId) {
        LiveRuntime::cancel(self, run)
    }
}

impl LiveRuntime {
    /// The handle of an objective about to run: a stop that arrived while
    /// the attempt was being set up ends it before any effect, and the
    /// claim, when there is one, goes back with the attempt.
    ///
    /// The release is the last thing the attempt does before its verdict
    /// is read: a store that refused the release wraps the verdict as an
    /// uncertain release, so a held claim is never a silent fact.
    fn handle(
        &self,
        objective: Objective,
        mut guard: Option<ClaimGuard>,
        cancelled: Arc<AtomicBool>,
    ) -> ObjectiveHandle {
        let agent = self.agent.clone();
        let verifier = self.verifier.clone();
        let publisher = self.publisher.clone();
        ObjectiveHandle::new(async move {
            let end = if cancelled.load(Ordering::SeqCst) {
                AttemptEnd::Cancelled {
                    work_ref: objective.work_ref.clone(),
                }
            } else {
                attempt(&objective, agent, verifier, publisher, cancelled).await
            };
            let release = match &mut guard {
                Some(guard) => {
                    guard.release_now();
                    guard.take_release_error()
                }
                None => None,
            };
            drop(guard);
            let end = match release {
                Some(release) => AttemptEnd::UncertainRelease {
                    release,
                    end: Box::new(end),
                },
                None => end,
            };
            outcome_of(end)
        })
    }
}

/// The objective a custom request carries: free-form work that owns no
/// ticket, so it selects nothing, claims nothing and closes nothing. It
/// IS judged — by the one check the operator's termination criterion
/// becomes. An objective without a criterion could never produce the
/// evidence the promotion demands, so the CLI refuses one outright. Its
/// identity carries the run: two custom objectives never share a goal
/// record.
pub(crate) fn custom_objective(custom: &CustomObjective, run: RunId) -> Objective {
    Objective {
        work_ref: format!("custom-objective-{}", run.number()),
        instruction: custom.instruction.trim().to_string(),
        checks: vec![ObjectiveCheck::free(accept_check(&custom.accept))],
        run: Some(run.number()),
        closes: Closes::Nothing,
    }
}

/// The operator's criterion as the executable check the verification
/// demands: the check holds exactly when the command ends with the exit
/// code that means fulfilled. An exit code other than zero must survive
/// the shell, so the expectation is compiled into the command itself.
fn accept_check(criterion: &goal::source::TerminationCriterion) -> verify::runner::Check {
    let command = if criterion.expect_exit == 0 {
        criterion.command.clone()
    } else {
        format!(
            "( {} ); rc=$?; if [ \"$rc\" = {} ]; then exit 0; else echo \"the criterion exited $rc, expected {}\" >&2; exit 1; fi",
            criterion.command, criterion.expect_exit, criterion.expect_exit
        )
    };
    verify::runner::Check {
        id: "accept".to_string(),
        command,
    }
}

/// Why the selection found nothing eligible. Work somebody else holds is
/// NAMED: a bare "no work" would hide that the queue is held, not empty.
fn no_eligible_work(held: &BTreeMap<String, String>) -> String {
    if held.is_empty() {
        return "no eligible work".to_string();
    }
    format!(
        "no eligible work: {} claim(s) held: {}",
        held.len(),
        held.keys().cloned().collect::<Vec<_>>().join(", ")
    )
}

/// The attempt that ended before the agent ran: the selection itself
/// refused, either because the named work is not eligible or because the
/// tracker could not be read.
fn early_selection_end(work_ref: Option<&str>, error: RunnerError) -> AttemptEnd {
    match error {
        RunnerError::Requirements(reason) => AttemptEnd::NamedNotEligible {
            work_ref: work_ref.unwrap_or_default().to_string(),
            reason,
        },
        RunnerError::Tracker(error) => AttemptEnd::InfraFailed {
            reason: error.to_string(),
        },
    }
}

/// One live attempt from the agent to the publisher, ending in one
/// terminal value. Every effect it performs is a port call.
async fn attempt(
    objective: &Objective,
    agent: Arc<dyn AgentPort>,
    verifier: Arc<dyn VerifierPort>,
    publisher: Arc<dyn PublisherPort>,
    cancelled: Arc<AtomicBool>,
) -> AttemptEnd {
    let work_ref = objective.work_ref.clone();
    let candidate = match agent.execute(objective).await {
        Ok(candidate) => candidate,
        Err(error) => {
            return AttemptEnd::AgentFailed {
                work_ref,
                reason: error.to_string(),
            };
        }
    };
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
    }
    let verification = match verifier.verify(objective, &candidate).await {
        Ok(verification) => verification,
        Err(error) => {
            return AttemptEnd::Unverifiable {
                work_ref,
                reason: error.to_string(),
            };
        }
    };
    // A stop observed while verifying ends the attempt BEFORE its verdict
    // is turned into a rejection: the work was not judged, it was stopped.
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
    }
    let required = objective.required();
    if !verification.covers(&required) {
        return AttemptEnd::ChecksNotHeld {
            work_ref,
            missing: verification.missing(&required),
        };
    }
    // The last gate: verifying is harmless, publishing is not.
    if cancelled.load(Ordering::SeqCst) {
        return AttemptEnd::Cancelled { work_ref };
    }
    match publisher
        .publish(objective, &candidate, &verification)
        .await
    {
        // A publication whose record settled ends as Published: the
        // closure dimension says how the tracker reads. A publication
        // whose record could NOT settle ends as PromotedUnconfirmed: the
        // effect moved, so it is never mistaken for a promotion failure
        // and never re-attempted.
        Ok(publication) => match publication.checkpoint {
            Checkpointed::Settled => AttemptEnd::Published {
                work_ref,
                closure: publication.closure,
                detail: publication.detail,
            },
            Checkpointed::Uncertain => AttemptEnd::PromotedUnconfirmed {
                work_ref,
                detail: publication.detail,
            },
        },
        Err(error) => AttemptEnd::PromotionFailed {
            work_ref,
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use super::*;
    use crate::ports::{
        AgentError, Closed, Closes, ObjectiveCheck, Publication, PublisherError, RequirementsError,
        Verification, VerifierError,
    };
    use tracker::core::{WorkItem, WorkStatus};
    use tracker::fake::FakeTracker;
    use verify::git::{GitRevision, GitTree};
    use verify::runner::{Candidate, Check};
    use verify::transport::TransportDigest;

    struct FixedRequirements;

    impl RequirementsPort for FixedRequirements {
        fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
            Ok(Objective {
                work_ref: item.id.clone(),
                instruction: format!("do {}", item.id),
                checks: vec![ObjectiveCheck::free(Check {
                    id: "AC-1".into(),
                    command: "true".into(),
                })],
                run: None,
                closes: Closes::Ticket,
            })
        }
    }

    struct FailingRequirements;

    impl RequirementsPort for FailingRequirements {
        fn requirements(&self, item: &WorkItem) -> Result<Objective, RequirementsError> {
            Err(RequirementsError {
                work_ref: item.id.clone(),
                reason: "the contract could not be read".into(),
            })
        }
    }

    #[derive(Default)]
    struct CountingAgent {
        calls: AtomicUsize,
        seen: Mutex<Vec<String>>,
    }

    impl CountingAgent {
        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn seen(&self) -> Vec<String> {
            self.seen
                .lock()
                .map(|seen| seen.clone())
                .unwrap_or_default()
        }
    }

    #[async_trait::async_trait]
    impl AgentPort for CountingAgent {
        async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if let Ok(mut seen) = self.seen.lock() {
                seen.push(objective.work_ref.clone());
            }
            Ok(candidate())
        }
    }

    struct HoldingVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for HoldingVerifier {
        async fn verify(
            &self,
            objective: &Objective,
            _candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            Ok(Verification::satisfying(&objective.required()))
        }
    }

    struct EmptyVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for EmptyVerifier {
        async fn verify(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            Ok(Verification {
                satisfied: Vec::new(),
                evidence: None,
            })
        }
    }

    struct CountingPublisher {
        calls: AtomicUsize,
        closure: Closed,
    }

    impl CountingPublisher {
        fn confirm_closing() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                closure: Closed::Confirmed,
            }
        }

        fn confirm_without_closure() -> Self {
            Self {
                calls: AtomicUsize::new(0),
                closure: Closed::Unconfirmed,
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl PublisherPort for CountingPublisher {
        async fn publish(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
            _verification: &Verification,
        ) -> Result<Publication, PublisherError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Publication {
                revision: "1f0c3a5b".into(),
                closure: self.closure,
                checkpoint: Checkpointed::Settled,
                detail: "the publisher moved the effects".into(),
            })
        }
    }

    struct FakeClaims {
        grant: Option<Claimed>,
        claim_calls: AtomicUsize,
        releases: Mutex<Vec<(String, u64)>>,
        held: BTreeMap<String, String>,
        release_fails: bool,
    }

    impl FakeClaims {
        fn granting(epoch: u64) -> Self {
            Self {
                grant: Some(Claimed { epoch }),
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: BTreeMap::new(),
                release_fails: false,
            }
        }

        fn denying() -> Self {
            Self {
                grant: None,
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: BTreeMap::new(),
                release_fails: false,
            }
        }

        /// A claim store that already reports `held` and grants `epoch`.
        fn holding(epoch: u64, held: &[(&str, &str)]) -> Self {
            Self {
                grant: Some(Claimed { epoch }),
                claim_calls: AtomicUsize::new(0),
                releases: Mutex::new(Vec::new()),
                held: held
                    .iter()
                    .map(|(work_ref, holder)| (work_ref.to_string(), holder.to_string()))
                    .collect(),
                release_fails: false,
            }
        }

        /// A store that grants the claim but REFUSES the release: the
        /// work stays held whatever the attempt ended with.
        fn granting_but_refusing_release(epoch: u64) -> Self {
            Self {
                release_fails: true,
                ..Self::granting(epoch)
            }
        }

        fn claim_calls(&self) -> usize {
            self.claim_calls.load(Ordering::SeqCst)
        }

        fn releases(&self) -> Vec<(String, u64)> {
            self.releases
                .lock()
                .map(|releases| releases.clone())
                .unwrap_or_default()
        }
    }

    impl ClaimPort for FakeClaims {
        fn claim(&self, _work_ref: &str) -> Result<Claimed, ClaimError> {
            self.claim_calls.fetch_add(1, Ordering::SeqCst);
            match self.grant {
                Some(claimed) => Ok(claimed),
                None => Err(ClaimError::Denied {
                    holder: "another-agent".into(),
                }),
            }
        }

        fn release(&self, work_ref: &str, epoch: u64) -> Result<(), ClaimReleaseError> {
            if let Ok(mut releases) = self.releases.lock() {
                releases.push((work_ref.to_string(), epoch));
            }
            if self.release_fails {
                return Err(ClaimReleaseError {
                    work_ref: work_ref.to_string(),
                    epoch,
                    reason: "the release transaction was refused".into(),
                });
            }
            Ok(())
        }

        fn claims(&self) -> Result<BTreeMap<String, String>, ClaimError> {
            Ok(self.held.clone())
        }
    }

    fn candidate() -> Candidate {
        Candidate {
            revision: GitRevision::parse("1f0c3a5b7d9e246183a5c7e9b0d3f5a7c9e1b3d5")
                .expect("a revision"),
            tree: GitTree::parse("3d5f1b7c9e1b3d51f0c3a5b7d9e246183a5c7e9b").expect("a tree"),
            transport_digest: TransportDigest::of(b"the bundle"),
            bundle: b"the bundle".to_vec(),
        }
    }

    fn item(id: &str, status: WorkStatus) -> WorkItem {
        WorkItem {
            id: id.into(),
            title: format!("work {id}"),
            status,
            provider_ready: Some(true),
            dependencies: Vec::new(),
        }
    }

    fn runtime_with(
        items: Vec<WorkItem>,
        requirements: Arc<dyn RequirementsPort>,
        verifier: Arc<dyn VerifierPort>,
        publisher: Arc<CountingPublisher>,
        claims: Arc<FakeClaims>,
        agent: Arc<CountingAgent>,
    ) -> LiveRuntime {
        LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(items)),
            requirements,
            agent,
            verifier,
            publisher,
            claims,
        )
    }

    fn start(
        runtime: &LiveRuntime,
        run: RunId,
        work_ref: Option<&str>,
    ) -> impl std::future::Future<Output = Result<ObjectiveHandle, RuntimeError>> {
        runtime.start(
            run,
            ObjectiveRequest {
                project: "project".into(),
                work_ref: work_ref.map(str::to_string),
                objective: None,
            },
        )
    }

    async fn outcome_of_start(runtime: &LiveRuntime, work_ref: Option<&str>) -> ObjectiveOutcome {
        start(runtime, RunId(1), work_ref)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await
    }

    #[test]
    fn every_attempt_end_maps_to_its_outcome() {
        assert_eq!(
            outcome_of(AttemptEnd::Published {
                work_ref: "T-1".into(),
                closure: Closed::Confirmed,
                detail: "published".into(),
            }),
            ObjectiveOutcome::Confirmed {
                work_ref: "T-1".into(),
                detail: "published".into(),
            }
        );
        let unconfirmed = outcome_of(AttemptEnd::Published {
            work_ref: "T-1".into(),
            closure: Closed::Unconfirmed,
            detail: "moved".into(),
        });
        assert!(
            matches!(unconfirmed, ObjectiveOutcome::Failed { ref reason } if reason.contains("closure")),
            "{unconfirmed:?}"
        );
        for end in [
            AttemptEnd::ChecksNotHeld {
                work_ref: "T-1".into(),
                missing: vec!["AC-1".into()],
            },
            AttemptEnd::NamedNotEligible {
                work_ref: "T-1".into(),
                reason: "done".into(),
            },
            AttemptEnd::Unverifiable {
                work_ref: "T-1".into(),
                reason: "no evidence".into(),
            },
            AttemptEnd::ClaimDenied {
                work_ref: "T-1".into(),
                holder: "another-agent".into(),
            },
        ] {
            assert!(
                matches!(outcome_of(end.clone()), ObjectiveOutcome::Rejected { .. }),
                "{end:?} must be rejected"
            );
        }
        for end in [
            AttemptEnd::AgentFailed {
                work_ref: "T-1".into(),
                reason: "the session ended".into(),
            },
            AttemptEnd::InfraFailed {
                reason: "the sandbox could not be created".into(),
            },
            AttemptEnd::PromotionFailed {
                work_ref: "T-1".into(),
                reason: "the ref moved concurrently".into(),
            },
        ] {
            assert!(
                matches!(outcome_of(end.clone()), ObjectiveOutcome::Failed { .. }),
                "{end:?} must fail"
            );
        }
        // A release the store refused is visible in the outcome text,
        // whatever the attempt itself ended with: the release failure is
        // the headline, and the inner verdict travels with it.
        for end in [
            AttemptEnd::Published {
                work_ref: "T-1".into(),
                closure: Closed::Confirmed,
                detail: "published".into(),
            },
            AttemptEnd::Cancelled {
                work_ref: "T-1".into(),
            },
        ] {
            let uncertain = outcome_of(AttemptEnd::UncertainRelease {
                release: ClaimReleaseError {
                    work_ref: "T-1".into(),
                    epoch: 3,
                    reason: "the state directory is read-only".into(),
                },
                end: Box::new(end),
            });
            match uncertain {
                ObjectiveOutcome::Failed { ref reason } => {
                    assert!(
                        reason.contains("the claim release did not complete"),
                        "{reason}"
                    );
                    assert!(
                        reason.contains("read-only"),
                        "the store's reason travels: {reason}"
                    );
                    assert!(reason.contains("epoch 3"), "the epoch travels: {reason}");
                }
                other => panic!("a refused release must fail the run, got {other:?}"),
            }
        }
        // A promoted effect with an unsettled record fails WITHOUT
        // inviting a re-promotion: the reason says the effect moved.
        let promoted_unconfirmed = outcome_of(AttemptEnd::PromotedUnconfirmed {
            work_ref: "T-1".into(),
            detail: "promoted 1f0c3a5b; the checkpoint was not confirmed".into(),
        });
        match promoted_unconfirmed {
            ObjectiveOutcome::Failed { ref reason } => {
                assert!(reason.contains("was promoted"), "{reason}");
                assert!(
                    !reason.contains("could not be promoted"),
                    "an unsettled record is never a promotion failure: {reason}"
                );
                assert!(reason.contains("never be re-applied"), "{reason}");
            }
            other => panic!("a moved effect with an uncertain record must fail, got {other:?}"),
        }
        assert_eq!(
            outcome_of(AttemptEnd::NoEligibleWork {
                reason: "none".into(),
            }),
            ObjectiveOutcome::Idle {
                reason: "none".into(),
            }
        );
    }

    #[tokio::test]
    async fn idle_work_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(7));
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Idle { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0, "idle work costs no agent call");
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.claim_calls(), 0, "idle work takes no claim");
        assert!(claims.releases().is_empty());
    }

    #[tokio::test]
    async fn a_recurring_goal_with_no_work_waits_twice_without_any_effect() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(7));
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        // Two consecutive starts of a goal whose queue is empty: each one
        // waits, and neither opens a sandbox, a claim or an agent turn.
        for run in 1..=2u64 {
            let outcome = start(&runtime, RunId(run), None)
                .await
                .expect("the runtime accepts the objective")
                .completion()
                .await;
            assert!(
                matches!(outcome, ObjectiveOutcome::Idle { .. }),
                "run {run}: {outcome:?}"
            );
        }
        assert_eq!(agent.calls(), 0, "waiting costs no agent turn");
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.claim_calls(), 0, "waiting takes no claim");
        assert!(claims.releases().is_empty());
    }

    /// A tracker whose reads are counted: a custom objective must not
    /// consult it, and must not write the directory it lives in.
    struct CountingTracker {
        snapshot_calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl tracker::core::TrackerRead for CountingTracker {
        async fn snapshot(&self) -> Result<Vec<WorkItem>, tracker::core::TrackerError> {
            self.snapshot_calls.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        }
    }

    /// A publisher that runs the REAL evidence gate: the same
    /// `require_evidence` the product's promotion applies before a
    /// candidate moves. A verdict without evidence that speaks for the
    /// candidate is refused here exactly as it is refused in production.
    struct GatedPublisher {
        closures: Mutex<Vec<Closes>>,
    }

    #[async_trait::async_trait]
    impl PublisherPort for GatedPublisher {
        async fn publish(
            &self,
            objective: &Objective,
            candidate: &Candidate,
            verification: &Verification,
        ) -> Result<Publication, PublisherError> {
            if let Ok(mut closures) = self.closures.lock() {
                closures.push(objective.closes);
            }
            let evidence = verification
                .evidence
                .as_ref()
                .ok_or_else(|| PublisherError {
                    work_ref: objective.work_ref.clone(),
                    reason: "the verdict carries no evidence".into(),
                })?;
            promote::evidence::require_evidence(
                evidence,
                &promote::evidence::PromotionCandidate {
                    work_ref: objective.work_ref.clone(),
                    revision: candidate.revision.clone(),
                    transport_digest: candidate.transport_digest.clone(),
                },
            )
            .map_err(|error| PublisherError {
                work_ref: objective.work_ref.clone(),
                reason: error.to_string(),
            })?;
            let closure = match objective.closes {
                Closes::Ticket => Closed::Confirmed,
                Closes::Nothing => Closed::NotRequired,
            };
            Ok(Publication {
                revision: candidate.revision.as_str().to_string(),
                closure,
                checkpoint: Checkpointed::Settled,
                detail: "the evidence gate held and the effects moved".into(),
            })
        }
    }

    /// A verifier that mirrors the shape of the real verifier's output:
    /// one result per declared check, bound to the candidate as observed.
    /// The measurement itself belongs to the sandbox; THE GATE UNDER TEST
    /// is the real `require_evidence`, which this feeds.
    struct MeasuringVerifier;

    #[async_trait::async_trait]
    impl VerifierPort for MeasuringVerifier {
        async fn verify(
            &self,
            objective: &Objective,
            candidate: &Candidate,
        ) -> Result<Verification, VerifierError> {
            let binding = verify::evidence::EvidenceBinding {
                revision: candidate.revision.clone(),
                tree: candidate.tree.clone(),
                transport_digest: candidate.transport_digest.clone(),
                required_checks: objective
                    .checks
                    .iter()
                    .map(|declared| verify::evidence::CheckBinding {
                        id: declared.check.id.clone(),
                        command: declared.check.command.clone(),
                    })
                    .collect(),
                image_id: "sha256:image".to_string(),
                environment: "env-1".to_string(),
            };
            let entries = objective
                .checks
                .iter()
                .map(|declared| verify::evidence::EvidenceEntry {
                    result: verify::evidence::BoundResult {
                        check_id: declared.check.id.clone(),
                        environment: "env-1".to_string(),
                        image_id: "sha256:image".to_string(),
                        tree: candidate.tree.clone(),
                        transport_digest: candidate.transport_digest.clone(),
                        command: declared.check.command.clone(),
                        record: verify::result::ExecutionRecord::terminal(
                            &declared.check.command,
                            0,
                            "the criterion held",
                        ),
                    },
                    status: verify::evidence::EvidenceStatus::Valid,
                })
                .collect();
            Ok(Verification {
                satisfied: objective.required(),
                evidence: Some(verify::evidence::EvidenceSet {
                    work_ref: objective.work_ref.clone(),
                    binding,
                    entries,
                }),
            })
        }
    }

    fn custom(text: &str) -> CustomObjective {
        CustomObjective {
            instruction: text.into(),
            accept: goal::source::TerminationCriterion {
                command: "test -f SUMMARY.md".into(),
                expect_exit: 0,
            },
        }
    }

    fn directory_snapshot(dir: &std::path::Path) -> Vec<(std::path::PathBuf, Vec<u8>)> {
        let mut files = Vec::new();
        let Ok(entries) = std::fs::read_dir(dir) else {
            return files;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(directory_snapshot(&path));
            } else if let Ok(bytes) = std::fs::read(&path) {
                files.push((path, bytes));
            }
        }
        files.sort();
        files
    }

    #[tokio::test]
    async fn a_custom_objective_never_reads_claims_or_the_tracker() {
        let tracker_dir = std::env::temp_dir().join(format!(
            "hodei-custom-objective-{}-{}",
            std::process::id(),
            "tracker"
        ));
        let _ = std::fs::remove_dir_all(&tracker_dir);
        std::fs::create_dir_all(&tracker_dir).expect("the tracker fixture");
        std::fs::write(tracker_dir.join("BACKLOG.md"), "# Backlog\n\n- keep me\n")
            .expect("the backlog fixture");
        let before = directory_snapshot(&tracker_dir);

        let agent = Arc::new(CountingAgent::default());
        let claims = Arc::new(FakeClaims::granting(7));
        let publisher = Arc::new(GatedPublisher {
            closures: Mutex::new(Vec::new()),
        });
        let tracker = Arc::new(CountingTracker {
            snapshot_calls: AtomicUsize::new(0),
        });
        let runtime = LiveRuntime::new(
            "project",
            tracker.clone(),
            Arc::new(FixedRequirements),
            agent.clone(),
            Arc::new(MeasuringVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let outcome = runtime
            .start(
                RunId(3),
                ObjectiveRequest {
                    project: "project".into(),
                    work_ref: None,
                    objective: Some(custom("summarise the docs")),
                },
            )
            .await
            .expect("the runtime accepts a custom objective")
            .completion()
            .await;

        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "custom-objective-3"),
            "{outcome:?}"
        );
        assert_eq!(agent.seen(), vec!["custom-objective-3".to_string()]);
        assert_eq!(claims.claim_calls(), 0, "a custom objective takes no claim");
        assert!(claims.releases().is_empty());
        assert_eq!(
            tracker.snapshot_calls.load(Ordering::SeqCst),
            0,
            "a custom objective never reads the tracker"
        );
        assert_eq!(
            publisher
                .closures
                .lock()
                .map(|c| c.clone())
                .unwrap_or_default(),
            vec![Closes::Nothing],
            "the publisher was told there is no ticket to close"
        );
        assert_eq!(
            directory_snapshot(&tracker_dir),
            before,
            "the tracker directory is byte-identical"
        );
        let _ = std::fs::remove_dir_all(&tracker_dir);
    }

    #[tokio::test]
    async fn a_custom_objective_that_also_names_a_ticket_is_refused() {
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            Arc::new(FakeClaims::granting(7)),
            Arc::new(CountingAgent::default()),
        );
        let refused = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "project".into(),
                    work_ref: Some("T-1".into()),
                    objective: Some(custom("summarise the docs")),
                },
            )
            .await;
        assert!(
            refused.is_err(),
            "an objective owns no ticket or one ticket"
        );
    }

    #[tokio::test]
    async fn work_that_is_not_eligible_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(7));
        let runtime = runtime_with(
            vec![item("T-2", WorkStatus::Done)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, Some("T-2")).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.claim_calls(), 0);
    }

    #[tokio::test]
    async fn held_claims_are_skipped_by_selection_and_not_eligible_by_name() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::holding(7, &[("T-1", "another-agent")]));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open), item("T-2", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        // Unnamed: the claimed work is skipped, not selected and denied
        // afterwards.
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-2"),
            "{outcome:?}"
        );
        assert_eq!(agent.seen(), vec!["T-2".to_string()]);
        assert_eq!(claims.releases(), vec![("T-2".to_string(), 7)]);

        // Named: the claimed work is not eligible, and nothing runs.
        let named = outcome_of_start(&runtime, Some("T-1")).await;
        assert!(
            matches!(named, ObjectiveOutcome::Rejected { ref reason, .. } if reason.contains("ClaimedBy")),
            "{named:?}"
        );
        assert_eq!(agent.calls(), 1, "named claimed work costs no agent call");
    }

    #[tokio::test]
    async fn a_denied_claim_never_reaches_the_agent() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::denying());
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { ref reason, .. } if reason.contains("another-agent")),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert!(
            claims.releases().is_empty(),
            "a claim that was never taken is not released"
        );
    }

    #[tokio::test]
    async fn an_unverifiable_objective_never_reaches_the_agent_nor_the_claim() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(3));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FailingRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        // The contract is a pure read and runs before the claim: work
        // whose contract cannot be read was never claimed, so there is
        // nothing to give back.
        assert_eq!(claims.claim_calls(), 0);
        assert!(
            claims.releases().is_empty(),
            "work that was never claimed releases nothing"
        );
    }

    #[tokio::test]
    async fn checks_that_do_not_hold_are_rejected_and_never_published() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(5));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(EmptyVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Rejected { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(
            publisher.calls(),
            0,
            "a rejected objective is never published"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 5)]);
    }

    #[tokio::test]
    async fn a_published_objective_is_confirmed_and_releases_the_claim_once() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(11));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 11)]);
    }

    #[tokio::test]
    async fn moved_effects_without_a_confirmed_closure_are_a_failure() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_without_closure());
        let claims = Arc::new(FakeClaims::granting(13));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("closure")),
            "{outcome:?}"
        );
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 13)]);
    }

    /// A publisher that moves the effect and then reports the durable
    /// record UNCERTAIN: the effect stands, the record does not.
    struct UncertainRecordPublisher {
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl PublisherPort for UncertainRecordPublisher {
        async fn publish(
            &self,
            _objective: &Objective,
            _candidate: &Candidate,
            _verification: &Verification,
        ) -> Result<Publication, PublisherError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(Publication {
                revision: "1f0c3a5b".into(),
                closure: Closed::Confirmed,
                checkpoint: Checkpointed::Uncertain,
                detail: "promoted 1f0c3a5b; the checkpoint was not confirmed".into(),
            })
        }
    }

    #[tokio::test]
    async fn a_promoted_effect_with_an_uncertain_record_never_invites_a_re_promotion() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(UncertainRecordPublisher {
            calls: AtomicUsize::new(0),
        });
        let claims = Arc::new(FakeClaims::granting(47));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            agent,
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        match outcome {
            ObjectiveOutcome::Failed { ref reason } => {
                assert!(reason.contains("was promoted"), "{reason}");
                assert!(
                    !reason.contains("could not be promoted"),
                    "the effect moved: this is not a promotion failure: {reason}"
                );
            }
            other => panic!("an unsettled record is a failure, got {other:?}"),
        }
        assert_eq!(
            publisher.calls.load(Ordering::SeqCst),
            1,
            "the attempt ends in ONE verdict: the effect is not retried"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 47)]);
    }

    /// A tracker whose snapshot MOVES: the closing publisher marks the
    /// work it published as done, the way the official tracker does when
    /// the closure lands. The chained selection then advances instead of
    /// being offered the same work again.
    #[derive(Clone)]
    struct SharedTracker {
        items: Arc<Mutex<Vec<WorkItem>>>,
    }

    #[async_trait::async_trait]
    impl TrackerRead for SharedTracker {
        async fn snapshot(&self) -> Result<Vec<WorkItem>, tracker::core::TrackerError> {
            Ok(self
                .items
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone())
        }
    }

    struct ClosingPublisher {
        tracker: SharedTracker,
        calls: AtomicUsize,
    }

    #[async_trait::async_trait]
    impl PublisherPort for ClosingPublisher {
        async fn publish(
            &self,
            objective: &Objective,
            _candidate: &Candidate,
            _verification: &Verification,
        ) -> Result<Publication, PublisherError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut items = self
                .tracker
                .items
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            if let Some(item) = items.iter_mut().find(|item| item.id == objective.work_ref) {
                item.status = WorkStatus::Done;
            }
            Ok(Publication {
                revision: "1f0c3a5b".into(),
                closure: Closed::Confirmed,
                checkpoint: Checkpointed::Settled,
                detail: "the closure moved the ticket".into(),
            })
        }
    }

    fn closing_runtime(
        items: Vec<WorkItem>,
        claims: Arc<FakeClaims>,
        agent: Arc<CountingAgent>,
    ) -> (LiveRuntime, Arc<ClosingPublisher>) {
        let tracker = SharedTracker {
            items: Arc::new(Mutex::new(items)),
        };
        let publisher = Arc::new(ClosingPublisher {
            tracker,
            calls: AtomicUsize::new(0),
        });
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(publisher.tracker.clone()),
            Arc::new(FixedRequirements),
            agent,
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims,
        );
        (runtime, publisher)
    }

    #[tokio::test]
    async fn a_confirmed_objective_within_one_run_selects_the_next_ticket() {
        // The runtime half of the chain: the supervisor asks the SAME run
        // again with no named work, and the selection has moved on — the
        // closed ticket is gone from the eligible set.
        let agent = Arc::new(CountingAgent::default());
        let claims = Arc::new(FakeClaims::granting(41));
        let (runtime, publisher) = closing_runtime(
            vec![item("T-1", WorkStatus::Open), item("T-2", WorkStatus::Open)],
            claims.clone(),
            agent.clone(),
        );

        let first = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(&first, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "T-1"),
            "{first:?}"
        );
        let second = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(&second, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "T-2"),
            "the chained selection takes the NEXT ticket: {second:?}"
        );
        assert_eq!(agent.seen(), vec!["T-1".to_string(), "T-2".to_string()]);
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 41), ("T-2".to_string(), 41)],
            "each attempt gives its claim back"
        );
    }

    #[tokio::test]
    async fn a_chained_selection_with_nothing_eligible_ends_idle_without_effects() {
        // The refusal path at the runtime level: once the only ticket is
        // closed, the next unnamed selection costs no claim, no agent
        // call and no effect — the chain has nothing left to run.
        let agent = Arc::new(CountingAgent::default());
        let claims = Arc::new(FakeClaims::granting(43));
        let (runtime, publisher) = closing_runtime(
            vec![item("T-1", WorkStatus::Open)],
            claims.clone(),
            agent.clone(),
        );

        let first = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(&first, ObjectiveOutcome::Confirmed { .. }),
            "{first:?}"
        );
        let second = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(&second, ObjectiveOutcome::Idle { .. }),
            "{second:?}"
        );
        assert_eq!(agent.calls(), 1, "idle selection costs no agent call");
        assert_eq!(publisher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(claims.claim_calls(), 1, "nothing new is claimed");
    }

    #[tokio::test]
    async fn a_cancelled_attempt_is_not_verified_or_published() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(17));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        runtime.cancel(RunId(1));
        let outcome = handle.completion().await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stop that precedes the attempt costs no effect"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 17)]);
    }

    #[tokio::test]
    async fn a_stop_that_arrives_before_the_attempt_starts_is_not_lost() {
        // The window between the engine applying a Start and the runtime
        // installing its attempt: the stop lands with no attempt to signal.
        // The attempt must then BEGIN cancelled instead of running work
        // nobody wants anymore.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(17));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        runtime.cancel(RunId(1));
        let outcome = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stopped attempt never reaches the agent"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 17)],
            "the claim goes back instead of staying held"
        );
    }

    #[tokio::test]
    async fn every_pending_stop_is_remembered_by_its_run() {
        // Two stops raised before their attempts start, each naming a
        // different run: the second one must not overwrite the first, so
        // run 1's attempt still begins cancelled when it finally starts.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(19));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        runtime.cancel(RunId(1));
        runtime.cancel(RunId(2));
        let outcome = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "the stop of run 1 survives the stop of run 2: {outcome:?}"
        );
        assert_eq!(
            agent.calls(),
            0,
            "a stopped attempt never reaches the agent"
        );
        assert_eq!(publisher.calls(), 0);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 19)],
            "the claim goes back instead of staying held"
        );
    }

    #[tokio::test]
    async fn a_pending_stop_does_not_cancel_a_different_run() {
        // A pending stop names the run it was raised for: the attempt of
        // another run is not cancelled by it.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(23));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        runtime.cancel(RunId(1));
        let outcome = start(&runtime, RunId(2), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "a stop for run 1 must not cancel run 2: {outcome:?}"
        );
        assert_eq!(agent.calls(), 1);
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 23)]);
    }

    #[tokio::test]
    async fn a_stop_during_verification_never_reaches_the_publisher() {
        struct GatedVerifier {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
            verified: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl VerifierPort for GatedVerifier {
            async fn verify(
                &self,
                objective: &Objective,
                _candidate: &Candidate,
            ) -> Result<Verification, VerifierError> {
                self.entered.notify_one();
                self.gate.notified().await;
                self.verified.store(true, Ordering::SeqCst);
                Ok(Verification::satisfying(&objective.required()))
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let verified = Arc::new(AtomicBool::new(false));
        let claims = Arc::new(FakeClaims::granting(29));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(CountingAgent::default()),
            Arc::new(GatedVerifier {
                entered: entered.clone(),
                gate: gate.clone(),
                verified: verified.clone(),
            }),
            publisher.clone(),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The attempt is inside the verifier when the stop arrives.
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert!(
            verified.load(Ordering::SeqCst),
            "the verification that was already running completed"
        );
        assert_eq!(
            publisher.calls(),
            0,
            "a stop observed after verification publishes nothing"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 29)]);
    }

    #[tokio::test]
    async fn a_stop_during_publication_leaves_the_completed_effect_confirmed() {
        struct GatedPublisher {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl PublisherPort for GatedPublisher {
            async fn publish(
                &self,
                _objective: &Objective,
                _candidate: &Candidate,
                _verification: &Verification,
            ) -> Result<Publication, PublisherError> {
                self.entered.notify_one();
                self.gate.notified().await;
                Ok(Publication {
                    revision: "1f0c3a5b".into(),
                    closure: Closed::Confirmed,
                    checkpoint: Checkpointed::Settled,
                    detail: "the publisher moved the effects".into(),
                })
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let claims = Arc::new(FakeClaims::granting(31));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(CountingAgent::default()),
            Arc::new(HoldingVerifier),
            Arc::new(GatedPublisher {
                entered: entered.clone(),
                gate: gate.clone(),
            }),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The stop lands while the effect is being moved: the effect
        // completes, so it is a FACT and the attempt reports it.
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "{outcome:?}"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 31)]);
    }

    #[tokio::test]
    async fn a_stop_for_one_run_does_not_cancel_the_next() {
        // A stop that belongs to a run that already ended must never reach
        // the attempt of the run that followed it.
        struct GatedAgent {
            entered: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl AgentPort for GatedAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.entered.notify_one();
                self.gate.notified().await;
                Ok(candidate())
            }
        }

        let entered = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let claims = Arc::new(FakeClaims::granting(37));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(GatedAgent {
                entered: entered.clone(),
                gate: gate.clone(),
            }),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        // Run 1 is stopped before its attempt begins: it never runs.
        runtime.cancel(RunId(1));
        let first = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        assert!(
            matches!(first, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{first:?}"
        );

        // Run 2 is in flight when the stop of run 1 is repeated.
        let second = start(&runtime, RunId(2), None)
            .await
            .expect("the runtime accepts the next objective");
        let completion = tokio::spawn(async move { second.completion().await });
        entered.notified().await;
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Confirmed { ref work_ref, .. } if work_ref == "T-1"),
            "a stop for run 1 must not cancel run 2: {outcome:?}"
        );
        assert_eq!(publisher.calls(), 1);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 37), ("T-1".to_string(), 37)],
            "both runs gave their claim back"
        );
    }

    #[tokio::test]
    async fn idle_with_held_claims_names_the_held_work() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::holding(7, &[("T-1", "another-agent")]));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        match outcome {
            ObjectiveOutcome::Idle { reason } => {
                assert!(reason.contains("T-1"), "the held work is named: {reason}");
                assert!(reason.contains("claim"), "the reason says why: {reason}");
            }
            other => panic!("expected idle, got {other:?}"),
        }
        assert_eq!(agent.calls(), 0);
        assert_eq!(claims.claim_calls(), 0);
    }

    #[tokio::test]
    async fn a_cooperative_stop_lets_the_attempt_end_and_releases_its_claim() {
        // An agent that winds down when the stop reaches it — the way the
        // real one closes its session and releases its sandbox — instead
        // of being dropped mid-flight.
        struct WindingDownAgent {
            started: Arc<tokio::sync::Notify>,
            gate: Arc<tokio::sync::Notify>,
            closed: Arc<AtomicBool>,
        }

        #[async_trait::async_trait]
        impl AgentPort for WindingDownAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.started.notify_one();
                self.gate.notified().await;
                self.closed.store(true, Ordering::SeqCst);
                Ok(candidate())
            }
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let gate = Arc::new(tokio::sync::Notify::new());
        let closed = Arc::new(AtomicBool::new(false));
        let claims = Arc::new(FakeClaims::granting(23));
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(WindingDownAgent {
                started: started.clone(),
                gate: gate.clone(),
                closed: closed.clone(),
            }),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        // The attempt is inside the agent when the stop arrives.
        started.notified().await;
        // The stop is a SIGNAL: the attempt returns and closes what it owns
        // instead of being dropped.
        runtime.cancel(RunId(1));
        gate.notify_one();
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { ref reason } if reason.contains("cancelled")),
            "{outcome:?}"
        );
        assert!(
            closed.load(Ordering::SeqCst),
            "the attempt reached its own end and closed its session"
        );
        assert_eq!(
            publisher.calls(),
            0,
            "a cancelled attempt publishes nothing"
        );
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 23)]);
    }

    #[tokio::test]
    async fn an_aborted_attempt_still_releases_its_claim() {
        // A Stop aborts the execution task: the attempt future is dropped
        // mid-agent. The claim must be given back even then — a cancelled
        // attempt that keeps the work claimed starves the next one.
        struct BlockingAgent {
            started: Arc<tokio::sync::Notify>,
        }

        #[async_trait::async_trait]
        impl AgentPort for BlockingAgent {
            async fn execute(&self, _objective: &Objective) -> Result<Candidate, AgentError> {
                self.started.notify_one();
                std::future::pending::<()>().await;
                Ok(candidate())
            }
        }

        let started = Arc::new(tokio::sync::Notify::new());
        let agent = Arc::new(BlockingAgent {
            started: started.clone(),
        });
        let claims = Arc::new(FakeClaims::granting(19));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            agent,
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            claims.clone(),
        );
        let handle = start(&runtime, RunId(1), None)
            .await
            .expect("the runtime accepts the objective");
        let completion = tokio::spawn(async move { handle.completion().await });
        started.notified().await;
        completion.abort();
        let _ = completion.await;
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 19)],
            "an aborted attempt releases its claim exactly once"
        );
    }

    #[tokio::test]
    async fn the_live_runtime_does_not_claim_a_ttl_it_cannot_renew() {
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            Arc::new(FakeClaims::granting(1)),
            Arc::new(CountingAgent::default()),
        );
        assert!(
            matches!(
                runtime.renew(300).await,
                run::runtime::RenewOutcome::Unsupported { .. }
            ),
            "the lifecycle backend has no ttl renewal"
        );
    }

    #[tokio::test]
    async fn a_project_mismatch_is_refused_instead_of_served() {
        let runtime = runtime_with(
            Vec::new(),
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            Arc::new(FakeClaims::granting(1)),
            Arc::new(CountingAgent::default()),
        );
        let refused = runtime
            .start(
                RunId(1),
                ObjectiveRequest {
                    project: "another-project".into(),
                    work_ref: None,
                    objective: None,
                },
            )
            .await;
        assert!(refused.is_err(), "the runtime serves its own project only");
    }

    #[tokio::test]
    async fn a_tracker_that_cannot_be_read_is_an_infrastructure_failure() {
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(1));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::failing("simulated outage")),
            Arc::new(FixedRequirements),
            agent.clone(),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        assert!(
            matches!(outcome, ObjectiveOutcome::Failed { .. }),
            "{outcome:?}"
        );
        assert_eq!(agent.calls(), 0);
        assert_eq!(publisher.calls(), 0);
        assert!(claims.releases().is_empty());
    }

    #[tokio::test]
    async fn a_refused_claim_release_ends_the_run_as_an_uncertain_release() {
        // The whole attempt succeeds and only the store's release is
        // refused: the verdict must say the release did not complete,
        // never silently leave the work held.
        let claims = Arc::new(FakeClaims::granting_but_refusing_release(43));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            Arc::new(CountingPublisher::confirm_closing()),
            claims.clone(),
            Arc::new(CountingAgent::default()),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        match &outcome {
            ObjectiveOutcome::Failed { reason } => {
                assert!(
                    reason.contains("the claim release did not complete"),
                    "the release failure is the headline: {reason}"
                );
                assert!(reason.contains("T-1"), "the held work is named: {reason}");
                assert!(
                    reason.contains("the release transaction was refused"),
                    "the store's reason travels: {reason}"
                );
                assert!(
                    reason.contains("confirmed T-1"),
                    "the attempt's own verdict is not lost: {reason}"
                );
            }
            other => panic!("a refused release must fail the run, got {other:?}"),
        }
        assert_eq!(
            claims.releases().len(),
            1,
            "the release is attempted exactly once even when the store refuses"
        );
    }

    /// The agent port of a turn that changed nothing: the refusal the
    /// productive stage raises when the staged worktree is still the
    /// base tree.
    struct NoChangesAgent;

    #[async_trait::async_trait]
    impl AgentPort for NoChangesAgent {
        async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError> {
            Err(AgentError {
                work_ref: objective.work_ref.clone(),
                reason: "the agent produced no changes to the managed state: the staged worktree is still the base tree".into(),
            })
        }
    }

    #[tokio::test]
    async fn an_agent_with_no_candidate_changes_carries_its_cause_and_never_promotes() {
        // The shape of a no-change turn after the dead no-commit state
        // was removed: NOT a timeout, NEVER promoted, and the cause
        // ("no changes") travels in the outcome text.
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(5));
        let runtime = LiveRuntime::new(
            "project",
            Arc::new(FakeTracker::with_items(vec![item("T-1", WorkStatus::Open)])),
            Arc::new(FixedRequirements),
            Arc::new(NoChangesAgent),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
        );
        let outcome = outcome_of_start(&runtime, None).await;
        match &outcome {
            ObjectiveOutcome::Failed { reason } => {
                assert!(reason.contains("no changes"), "the cause travels: {reason}");
                assert!(
                    !reason.contains("timeout"),
                    "a no-change turn is never mistaken for a timeout: {reason}"
                );
            }
            other => panic!("a refused turn without effects must fail the run, got {other:?}"),
        }
        assert_eq!(
            publisher.calls(),
            0,
            "a candidate that did not move is never promoted"
        );
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 5)],
            "the claim goes back whatever the refusal was"
        );
    }

    #[tokio::test]
    async fn a_pending_stop_superseded_by_a_newer_install_starts_and_cancels_nothing() {
        // The superseded stop of a run whose attempt never installed a
        // flag is dropped by the newer install. That is inert by proof,
        // not by promise: the newer attempt runs untouched, and a stop
        // addressed to the older run afterwards reaches neither one.
        let agent = Arc::new(CountingAgent::default());
        let publisher = Arc::new(CountingPublisher::confirm_closing());
        let claims = Arc::new(FakeClaims::granting(41));
        let runtime = runtime_with(
            vec![item("T-1", WorkStatus::Open), item("T-2", WorkStatus::Open)],
            Arc::new(FixedRequirements),
            Arc::new(HoldingVerifier),
            publisher.clone(),
            claims.clone(),
            agent.clone(),
        );
        // A stop for run 1 arrives before any attempt of run 1 installs.
        runtime.cancel(RunId(1));
        // Run 2 installs: its own flag starts false, so the superseded
        // stop neither cancels run 2 nor revives anything of run 1.
        let second = start(&runtime, RunId(2), None)
            .await
            .expect("the runtime accepts the objective")
            .completion()
            .await;
        // The unnamed selection takes the first eligible ticket.
        assert!(
            matches!(&second, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "T-1"),
            "the superseded stop does not cancel the newer run: {second:?}"
        );
        assert_eq!(agent.calls(), 1, "the superseded stop starts no attempt");
        assert_eq!(publisher.calls(), 1);
        assert_eq!(claims.releases(), vec![("T-1".to_string(), 41)]);

        // Now a run is IN FLIGHT and the older stop is raised again: it
        // is history, and the attempt in flight reads only its own flag.
        let third = start(&runtime, RunId(3), Some("T-2"))
            .await
            .expect("the runtime accepts the next objective");
        let completion = tokio::spawn(async move { third.completion().await });
        runtime.cancel(RunId(1));
        let outcome = completion.await.expect("the attempt ends");
        assert!(
            matches!(&outcome, ObjectiveOutcome::Confirmed { work_ref, .. } if work_ref == "T-2"),
            "a stop for an older run never reaches the attempt in flight: {outcome:?}"
        );
        assert_eq!(agent.calls(), 2, "nothing else ran");
        assert_eq!(publisher.calls(), 2);
        assert_eq!(
            claims.releases(),
            vec![("T-1".to_string(), 41), ("T-2".to_string(), 41)]
        );
    }
}
