//! Supervisor actor: a single mailbox serializes state mutations, while
//! external effects run detached — Stop and queries keep being answered
//! even when an effect is blocked.
//!
//! Durability comes BEFORE the answer: every mutation is committed to the
//! store before the caller is told it happened. A write that fails returns
//! the failure — never a claim that was not persisted, never a release that
//! was not, and never an effect whose intent was not recorded.

use std::path::Path;
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};

use crate::core::{ClaimResult, CoordinatorState, Refusal};
use crate::paths;
use crate::store::{CoordinatorStore, StoreError, TransactError};

#[derive(Debug, thiserror::Error)]
pub enum ActorError {
    #[error("store: {0}")]
    Store(#[from] StoreError),
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("actor mailbox closed")]
    MailboxClosed,
}

impl From<TransactError> for ActorError {
    fn from(error: TransactError) -> Self {
        match error {
            TransactError::Refused(refusal) => ActorError::Refused(refusal),
            TransactError::Store(store) => ActorError::Store(store),
        }
    }
}

/// What the actor did with an effect request.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectOutcome {
    pub effect_id: String,
    /// Whether the effect may run: a duplicate id is recorded already and
    /// is NOT run again.
    pub may_run: bool,
}

/// Requests the actor serves. Long-running effects are executed detached;
/// control messages (`Stop`, queries) never wait behind them.
pub enum Request {
    Claim {
        ticket_id: String,
        holder: String,
        reply: oneshot::Sender<Result<ClaimResult, ActorError>>,
    },
    Release {
        ticket_id: String,
        holder: String,
        reply: oneshot::Sender<Result<bool, ActorError>>,
    },
    /// Launch a durable effect: the intent is committed first, then the
    /// effect closure builds and runs the effect in its own task.
    RunEffect {
        effect_id: String,
        ticket_id: String,
        epoch: u64,
        description: String,
        /// Builds the effect. It is called ONLY when the intent is durable
        /// and the effect may run: a duplicate id never builds it, so the
        /// effect does not exist to run.
        effect: Box<dyn FnOnce() -> tokio::task::JoinHandle<bool> + Send + 'static>,
        reply: oneshot::Sender<Result<EffectOutcome, ActorError>>,
    },
    /// Apply a run event against the fencing of the current attempt.
    ApplyEvent {
        ticket_id: String,
        event_epoch: u64,
        holder: String,
        reply: oneshot::Sender<Result<bool, ActorError>>,
    },
    /// Control plane: always answered promptly.
    Stop { reply: oneshot::Sender<bool> },
    Query {
        reply: oneshot::Sender<Result<CoordinatorState, ActorError>>,
    },
}

pub struct CoordinatorActor {
    mailbox: mpsc::Sender<Request>,
    handle: tokio::task::JoinHandle<Result<CoordinatorState, ActorError>>,
    /// The effect ids that the restart recovered as reconciliable.
    recovered: Vec<String>,
}

impl CoordinatorActor {
    /// Start the actor over the durable state of a project.
    ///
    /// The state is RECOVERED, not assumed: the claims are the ones that
    /// were granted, and every intent that was pending comes back as
    /// reconciliable, because nobody may say whether its effect ran.
    pub fn start(project: &str, db_path: &Path) -> Result<Self, ActorError> {
        let mut store = CoordinatorStore::open(project, db_path)?;
        let recovered = store.recover_pending_effects()?;
        Ok(Self::spawn(store, recovered))
    }

    /// Start the actor over the state directory of a project, OUTSIDE the
    /// workspace: the tree of the user never carries harness state.
    pub fn start_in_state_dir(project: &str) -> Result<Self, ActorError> {
        let directory = paths::project_state_dir(project);
        std::fs::create_dir_all(&directory)?;
        Self::start(project, &paths::project_state_db(project))
    }

    fn spawn(store: CoordinatorStore, recovered: Vec<String>) -> Self {
        let (mailbox, mut rx) = mpsc::channel::<Request>(64);
        // Completed detached effects report back here; the mailbox never
        // waits behind an effect.
        let (outcome_tx, mut outcome_rx) = mpsc::channel::<(String, Option<bool>)>(64);
        let handle = tokio::spawn(async move {
            let mut store = store;
            let mut stopped = false;
            // A persistence failure with no caller to report it to: it is
            // returned when the actor ends.
            let mut deferred: Option<ActorError> = None;
            loop {
                let request = tokio::select! {
                    request = rx.recv() => request,
                    outcome = outcome_rx.recv() => {
                        match outcome {
                            Some((effect_id, applied)) => {
                                let settled = store.transact(|state| {
                                    match applied {
                                        Some(applied) => Ok(state.reconcile(&effect_id, applied)),
                                        None => Ok(state.mark_uncertain(&effect_id)),
                                    }
                                });
                                if let Err(error) = settled {
                                    // The outcome stays unrecorded: the
                                    // intent is still reconciliable and a
                                    // restart will find it as such.
                                    deferred = Some(ActorError::from(error));
                                }
                            }
                            None => break,
                        }
                        continue;
                    }
                };
                let Some(request) = request else { break };
                match request {
                    Request::Claim {
                        ticket_id,
                        holder,
                        reply,
                    } => {
                        let granted =
                            store.transact(|state| Ok(state.try_claim(&ticket_id, &holder)));
                        let _ = reply.send(granted.map_err(ActorError::from));
                    }
                    Request::Release {
                        ticket_id,
                        holder,
                        reply,
                    } => {
                        let released =
                            store.transact(|state| Ok(state.release(&ticket_id, &holder)));
                        let _ = reply.send(released.map_err(ActorError::from));
                    }
                    Request::RunEffect {
                        effect_id,
                        ticket_id,
                        epoch,
                        description,
                        effect,
                        reply,
                    } => {
                        let recorded = store.transact(|state| {
                            state.record_effect(&effect_id, &ticket_id, epoch, &description)
                        });
                        match recorded {
                            Ok(record) if record.may_run => {
                                let _ = reply.send(Ok(EffectOutcome {
                                    effect_id: effect_id.clone(),
                                    may_run: true,
                                }));
                                // Detached supervision: the outcome is
                                // reported back through the completion
                                // channel; timeout, panic or dropped handle
                                // leaves the intent Uncertain.
                                let outcome_tx = outcome_tx.clone();
                                tokio::spawn(async move {
                                    let handle = effect();
                                    let outcome =
                                        tokio::time::timeout(Duration::from_secs(30), handle).await;
                                    let applied = match outcome {
                                        Ok(Ok(applied)) => Some(applied),
                                        _ => None,
                                    };
                                    let _ = outcome_tx.send((effect_id, applied)).await;
                                });
                            }
                            Ok(record) => {
                                // The intent is recorded already: the
                                // effect is not built, so it does not run.
                                let _ = reply.send(Ok(EffectOutcome {
                                    effect_id: record.intent.effect_id,
                                    may_run: false,
                                }));
                            }
                            Err(error) => {
                                let _ = reply.send(Err(ActorError::from(error)));
                            }
                        }
                    }
                    Request::ApplyEvent {
                        ticket_id,
                        event_epoch,
                        holder,
                        reply,
                    } => {
                        let accepted = store.transact(|state| {
                            Ok(state.accepts_event(&ticket_id, event_epoch, &holder))
                        });
                        let _ = reply.send(accepted.map_err(ActorError::from));
                    }
                    Request::Stop { reply } => {
                        stopped = true;
                        let _ = reply.send(true);
                    }
                    Request::Query { reply } => {
                        let state = store.load();
                        let _ = reply.send(state.map_err(ActorError::from));
                    }
                }
                if stopped {
                    break;
                }
            }
            match deferred {
                Some(error) => Err(error),
                None => Ok(store.load()?),
            }
        });
        Self {
            mailbox,
            handle,
            recovered,
        }
    }

    /// The effect ids that the restart recovered as reconciliable.
    pub fn recovered_effects(&self) -> &[String] {
        &self.recovered
    }

    pub async fn claim(&self, ticket_id: &str, holder: &str) -> Result<ClaimResult, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::Claim {
                ticket_id: ticket_id.to_string(),
                holder: holder.to_string(),
                reply,
            })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)?
    }

    pub async fn release(&self, ticket_id: &str, holder: &str) -> Result<bool, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::Release {
                ticket_id: ticket_id.to_string(),
                holder: holder.to_string(),
                reply,
            })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)?
    }

    /// Record the intent of an effect and let it run.
    ///
    /// The effect is built only when its intent is durable and the effect
    /// may run: an id that is recorded already is a duplicate and is not
    /// run again.
    pub async fn run_effect(
        &self,
        effect_id: &str,
        ticket_id: &str,
        epoch: u64,
        description: &str,
        effect: impl FnOnce() -> tokio::task::JoinHandle<bool> + Send + 'static,
    ) -> Result<EffectOutcome, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::RunEffect {
                effect_id: effect_id.to_string(),
                ticket_id: ticket_id.to_string(),
                epoch,
                description: description.to_string(),
                effect: Box::new(effect),
                reply,
            })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)?
    }

    /// Whether a run event belongs to the current attempt of a ticket. An
    /// event of an older or newer epoch, or of another holder, is inert.
    pub async fn apply_event(
        &self,
        ticket_id: &str,
        event_epoch: u64,
        holder: &str,
    ) -> Result<bool, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::ApplyEvent {
                ticket_id: ticket_id.to_string(),
                event_epoch,
                holder: holder.to_string(),
                reply,
            })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)?
    }

    pub async fn stop(&self) -> Result<bool, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::Stop { reply })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)
    }

    /// The committed state.
    pub async fn query(&self) -> Result<CoordinatorState, ActorError> {
        let (reply, rx) = oneshot::channel();
        self.mailbox
            .send(Request::Query { reply })
            .await
            .map_err(|_| ActorError::MailboxClosed)?;
        rx.await.map_err(|_| ActorError::MailboxClosed)?
    }

    /// Final state after the actor loop ends. A persistence failure that
    /// had no caller to report it to is returned here.
    pub async fn join(self) -> Result<CoordinatorState, ActorError> {
        self.handle.await.map_err(|error| {
            ActorError::Store(StoreError::Sqlite(rusqlite::Error::ToSqlConversionFailure(
                Box::new(error),
            )))
        })?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::{ClaimResult, EffectStatus};

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hodei-actor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    /// A REAL write failure, injected from outside the coordinator: SQLite
    /// refuses the writes of the named table.
    fn inject_write_failure(db: &std::path::Path, table: &str) {
        let injector = rusqlite::Connection::open(db).unwrap();
        injector
            .execute_batch(&format!(
                "CREATE TRIGGER fail_{table} BEFORE INSERT ON {table}
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;"
            ))
            .unwrap();
    }

    #[tokio::test]
    async fn stop_is_answered_while_effect_is_blocked() {
        let actor = CoordinatorActor::start("alpha", &temp_db("stop")).unwrap();
        // Effect blocked for a long time.
        let blocked = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(120)).await;
            true
        });
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        let outcome = actor
            .run_effect("e-blocked", "T1", claim.epoch, "blocked effect", || blocked)
            .await
            .unwrap();
        assert!(outcome.may_run);
        // Control plane still responsive while the effect hangs.
        let stopped = tokio::time::timeout(Duration::from_secs(2), actor.stop()).await;
        assert!(matches!(stopped, Ok(Ok(true))));
        drop(actor); // ends the detached effect task too
    }

    #[tokio::test]
    async fn concurrent_claim_race_grants_exactly_once() {
        let actor = CoordinatorActor::start("alpha", &temp_db("race")).unwrap();
        let (a, b) = tokio::join!(actor.claim("T9", "client-a"), actor.claim("T9", "client-b"));
        let grants = [a.unwrap(), b.unwrap()]
            .into_iter()
            .filter(|r| matches!(r, ClaimResult::Granted(_)))
            .count();
        assert_eq!(grants, 1);
        let _ = actor.stop().await;
    }

    #[tokio::test]
    async fn a_write_failure_does_not_grant_a_claim() {
        let db = temp_db("fail-claim");
        let actor = CoordinatorActor::start("alpha", &db).unwrap();
        inject_write_failure(&db, "claims");
        let error = actor.claim("T1", "client-a").await.unwrap_err();
        assert!(matches!(error, ActorError::Store(_)), "{error}");
        // The claim was NOT granted: nobody holds it, on disk either.
        assert_eq!(actor.query().await.unwrap().claims.len(), 0);
    }

    #[tokio::test]
    async fn a_write_failure_does_not_release_a_claim() {
        let db = temp_db("fail-release");
        let actor = CoordinatorActor::start("alpha", &db).unwrap();
        assert!(matches!(
            actor.claim("T1", "client-a").await.unwrap(),
            ClaimResult::Granted(_)
        ));
        // The DELETE of the claim is the write that fails.
        let injector = rusqlite::Connection::open(&db).unwrap();
        injector
            .execute_batch(
                "CREATE TRIGGER fail_claims BEFORE DELETE ON claims
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
            )
            .unwrap();
        let error = actor.release("T1", "client-a").await.unwrap_err();
        assert!(matches!(error, ActorError::Store(_)), "{error}");
        // The claim is still held: the release did not happen.
        assert!(actor.query().await.unwrap().claims.contains_key("T1"));
    }

    #[tokio::test]
    async fn a_write_failure_does_not_run_an_effect() {
        let db = temp_db("fail-effect");
        let actor = CoordinatorActor::start("alpha", &db).unwrap();
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        inject_write_failure(&db, "effects");
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = ran.clone();
        let error = actor
            .run_effect("e1", "T1", claim.epoch, "sandbox create", move || {
                flag.store(true, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async { true })
            })
            .await
            .unwrap_err();
        assert!(matches!(error, ActorError::Store(_)), "{error}");
        // The effect never ran: its intent was not durable.
        assert!(!ran.load(std::sync::atomic::Ordering::SeqCst));
        assert!(actor.query().await.unwrap().effects.is_empty());
    }

    #[tokio::test]
    async fn two_coordinators_on_one_identity_grant_the_claim_once() {
        let db = temp_db("two-coordinators");
        let first = CoordinatorActor::start("alpha", &db).unwrap();
        let second = CoordinatorActor::start("alpha", &db).unwrap();
        let (a, b) = tokio::join!(
            first.claim("T1", "client-a"),
            second.claim("T1", "client-b")
        );
        let grants = [a.unwrap(), b.unwrap()]
            .into_iter()
            .filter(|result| matches!(result, ClaimResult::Granted(_)))
            .count();
        assert_eq!(grants, 1, "the write lock excludes the coordinators");
        let _ = first.stop().await;
        let _ = second.stop().await;
    }

    #[tokio::test]
    async fn the_same_ticket_id_in_two_projects_does_not_collide() {
        let alpha = CoordinatorActor::start("alpha", &temp_db("project-alpha")).unwrap();
        let beta = CoordinatorActor::start("beta", &temp_db("project-beta")).unwrap();
        assert!(matches!(
            alpha.claim("TASK-1", "orchestrator").await.unwrap(),
            ClaimResult::Granted(_)
        ));
        assert!(matches!(
            beta.claim("TASK-1", "orchestrator").await.unwrap(),
            ClaimResult::Granted(_)
        ));
    }

    #[tokio::test]
    async fn an_unauthorized_event_leaves_the_attempt_untouched() {
        let actor = CoordinatorActor::start("alpha", &temp_db("events")).unwrap();
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        // The event of the current attempt is accepted.
        assert!(
            actor
                .apply_event("T1", claim.epoch, "client-a")
                .await
                .unwrap()
        );
        // A late event of an older epoch: inert.
        assert!(
            !actor
                .apply_event("T1", claim.epoch - 1, "client-a")
                .await
                .unwrap()
        );
        // An event of a newer epoch: not ours to grant, inert.
        assert!(
            !actor
                .apply_event("T1", claim.epoch + 1, "client-a")
                .await
                .unwrap()
        );
        // Another holder is not authorized: inert.
        assert!(
            !actor
                .apply_event("T1", claim.epoch, "client-b")
                .await
                .unwrap()
        );
        // The attempt in flight is exactly as it was.
        let state = actor.query().await.unwrap();
        assert_eq!(state.attempt_epoch("T1"), Some(claim.epoch));
        assert_eq!(
            state.claim_of("T1").map(|c| c.holder.as_str()),
            Some("client-a")
        );
    }

    #[tokio::test]
    async fn a_duplicate_effect_id_is_not_run_again() {
        let actor = CoordinatorActor::start("alpha", &temp_db("duplicate")).unwrap();
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        let runs = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = runs.clone();
        let first = actor
            .run_effect("e1", "T1", claim.epoch, "sandbox create", move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async { true })
            })
            .await
            .unwrap();
        assert!(first.may_run);
        let counter = runs.clone();
        let second = actor
            .run_effect("e1", "T1", claim.epoch, "sandbox create", move || {
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                tokio::spawn(async { true })
            })
            .await
            .unwrap();
        assert!(!second.may_run, "a duplicate id is not run again");
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(runs.load(std::sync::atomic::Ordering::SeqCst), 1);
        // The recorded intent was not altered.
        let state = actor.query().await.unwrap();
        assert_eq!(state.effects.len(), 1);
    }

    #[tokio::test]
    async fn a_conflicting_effect_id_is_refused() {
        let actor = CoordinatorActor::start("alpha", &temp_db("conflict")).unwrap();
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        actor
            .run_effect("e1", "T1", claim.epoch, "sandbox create", || {
                tokio::spawn(async { true })
            })
            .await
            .unwrap();
        // The same id for another effect: refused, and the attempt keeps
        // its recorded intent.
        let error = actor
            .run_effect("e1", "T1", claim.epoch, "delete file", || {
                tokio::spawn(async { true })
            })
            .await
            .unwrap_err();
        assert!(matches!(error, ActorError::Refused(_)), "{error}");
        let state = actor.query().await.unwrap();
        assert_eq!(state.effects.len(), 1);
        assert_eq!(state.effects["e1"].description.as_str(), "sandbox create");
    }

    /// The environment that turns this very test into the second process.
    const CHILD_DB_ENV: &str = "HODEI_COORDINATOR_CHILD_DB";

    #[tokio::test]
    async fn two_processes_on_one_identity_grant_the_claim_once() {
        let db = temp_db("cross-process");
        if let Some(child_db) = std::env::var_os(CHILD_DB_ENV) {
            // The second PROCESS: it claims the same ticket of the same
            // identity, in its own process, with its own connection.
            let actor = CoordinatorActor::start("alpha", Path::new(&child_db)).unwrap();
            let granted = matches!(
                actor.claim("T1", "child").await.unwrap(),
                ClaimResult::Granted(_)
            );
            println!("CHILD_GRANTED={granted}");
            return;
        }
        // The first process: it spawns the second one and claims the same
        // ticket at the same time.
        let child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "actor::tests::two_processes_on_one_identity_grant_the_claim_once",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(CHILD_DB_ENV, &db)
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let actor = CoordinatorActor::start("alpha", &db).unwrap();
        let mine = matches!(
            actor.claim("T1", "parent").await.unwrap(),
            ClaimResult::Granted(_)
        );
        let output = child.wait_with_output().await.unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("CHILD_GRANTED="),
            "the second process did not report its verdict: {stdout}"
        );
        let child_granted = stdout.contains("CHILD_GRANTED=true");
        assert_eq!(
            usize::from(mine) + usize::from(child_granted),
            1,
            "the write lock excludes the processes, not the conventions"
        );
    }

    #[tokio::test]
    async fn a_restart_recovers_the_claims_and_the_pending_intents() {
        let db = temp_db("restart");
        let actor = CoordinatorActor::start("alpha", &db).unwrap();
        let claim = match actor.claim("T1", "client-a").await.unwrap() {
            ClaimResult::Granted(claim) => claim,
            ClaimResult::Denied { .. } => unreachable!(),
        };
        // An effect that never reports its outcome.
        actor
            .run_effect("e-pending", "T1", claim.epoch, "sandbox create", || {
                tokio::spawn(async {
                    tokio::time::sleep(Duration::from_secs(120)).await;
                    true
                })
            })
            .await
            .unwrap();
        let _ = actor.stop().await;
        drop(actor);

        // The restart: the claim is recovered and the pending intent comes
        // back as reconciliable.
        let restarted = CoordinatorActor::start("alpha", &db).unwrap();
        assert_eq!(restarted.recovered_effects(), ["e-pending".to_string()]);
        let state = restarted.query().await.unwrap();
        assert_eq!(
            state.claim_of("T1").map(|c| c.holder.as_str()),
            Some("client-a")
        );
        assert_eq!(state.epochs.get("T1"), Some(&claim.epoch));
        assert_eq!(
            state.effects["e-pending"].status,
            EffectStatus::Uncertain,
            "nobody may say whether the effect ran"
        );
    }
}
