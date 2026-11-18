//! SQLite persistence: the coordinator's state and effect intents survive
//! crashes. The database file is the durability boundary; every mutation is
//! committed BEFORE the caller is told that it happened.
//!
//! The state of a project lives in its OWN file, and the file records the
//! project it belongs to: opening it as another project is refused, because
//! two projects must never share one identity.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, Transaction, TransactionBehavior, params};

use crate::core::{Claim, CoordinatorState, EffectIntent, EffectStatus, Refusal};

/// How long a writer waits for the write lock before giving up: the
/// exclusion between processes is that lock, not a convention.
const BUSY_TIMEOUT: Duration = Duration::from_secs(10);

const SCHEMA: &str = "\
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS claims (
    ticket_id TEXT PRIMARY KEY,
    holder TEXT NOT NULL,
    epoch INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS epochs (
    ticket_id TEXT PRIMARY KEY,
    epoch INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS effects (
    effect_id TEXT PRIMARY KEY,
    ticket_id TEXT NOT NULL,
    epoch INTEGER NOT NULL,
    description TEXT NOT NULL,
    status TEXT NOT NULL
);";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(
        "the state file of project '{file}' was opened as project '{requested}': two projects must not share one identity"
    )]
    IdentityMismatch { file: String, requested: String },
}

/// A mutation that did not happen: the domain refused it, or the store
/// could not persist it. Nothing was committed in either case.
#[derive(Debug, thiserror::Error)]
pub enum TransactError {
    #[error("refused: {0}")]
    Refused(#[from] Refusal),
    #[error("store: {0}")]
    Store(#[from] StoreError),
}

#[derive(Debug)]
pub struct CoordinatorStore {
    conn: Connection,
    project: String,
}

impl CoordinatorStore {
    /// Open (creating the schema when needed) the durable state of a
    /// project.
    pub fn open(project: &str, path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(BUSY_TIMEOUT)?;
        conn.execute_batch(SCHEMA)?;
        let store = Self {
            conn,
            project: project.to_string(),
        };
        store.bind_identity()?;
        Ok(store)
    }

    /// The project this state coordinates.
    pub fn project(&self) -> &str {
        &self.project
    }

    /// Run a mutation over the committed state inside an EXCLUSIVE write
    /// transaction.
    ///
    /// The mutation sees the state as committed — the write lock is already
    /// held, so no other coordinator can change it under the mutation's
    /// feet — and its changes are persisted BEFORE the transaction
    /// commits. A failure persists NOTHING: the caller is told that the
    /// mutation did not happen.
    pub fn transact<T>(
        &mut self,
        mutate: impl FnOnce(&mut CoordinatorState) -> Result<T, Refusal>,
    ) -> Result<T, TransactError> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StoreError::from)?;
        let before = load_state(&tx, &self.project)?;
        let mut after = before.clone();
        let outcome = mutate(&mut after)?;
        persist_delta(&tx, &before, &after)?;
        tx.commit().map_err(StoreError::from)?;
        Ok(outcome)
    }

    /// The committed state.
    pub fn load(&self) -> Result<CoordinatorState, StoreError> {
        load_state(&self.conn, &self.project)
    }

    /// Recover after a restart: every intent that was PENDING comes back as
    /// UNCERTAIN, because nobody may say whether its effect ran. The
    /// returned ids are the ones to reconcile against the observed state.
    pub fn recover_pending_effects(&mut self) -> Result<Vec<String>, TransactError> {
        self.transact(|state| {
            let mut recovered = Vec::new();
            for (effect_id, intent) in state.effects.iter_mut() {
                if intent.status == EffectStatus::Pending {
                    intent.status = EffectStatus::Uncertain;
                    recovered.push(effect_id.clone());
                }
            }
            Ok(recovered)
        })
    }

    /// The effect ids that await reconciliation from the observed state.
    pub fn uncertain_effects(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self.conn.prepare(
            "SELECT effect_id FROM effects WHERE status = 'uncertain' ORDER BY effect_id",
        )?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    /// Bind the file to its project: the file remembers the project it
    /// belongs to, and opening it as another one is refused.
    fn bind_identity(&self) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES ('project', ?1)",
            params![self.project],
        )?;
        let owner: String =
            tx.query_row("SELECT value FROM meta WHERE key = 'project'", [], |row| {
                row.get(0)
            })?;
        if owner != self.project {
            return Err(StoreError::IdentityMismatch {
                file: owner,
                requested: self.project.clone(),
            });
        }
        tx.commit()?;
        Ok(())
    }
}

/// Read the committed state of a project.
fn load_state(conn: &Connection, project: &str) -> Result<CoordinatorState, StoreError> {
    let mut state = CoordinatorState::new(project);
    let mut stmt = conn.prepare("SELECT ticket_id, holder, epoch FROM claims")?;
    let rows = stmt.query_map([], |row| {
        Ok(Claim {
            ticket_id: row.get(0)?,
            holder: row.get(1)?,
            epoch: row.get::<_, i64>(2)? as u64,
        })
    })?;
    for claim in rows {
        let claim = claim?;
        state.claims.insert(claim.ticket_id.clone(), claim);
    }
    let mut stmt = conn.prepare("SELECT ticket_id, epoch FROM epochs")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
    })?;
    for row in rows {
        let (ticket, epoch) = row?;
        state.epochs.insert(ticket, epoch);
    }
    let mut stmt =
        conn.prepare("SELECT effect_id, ticket_id, epoch, description, status FROM effects")?;
    let rows = stmt.query_map([], |row| {
        let status: String = row.get(4)?;
        Ok(EffectIntent {
            effect_id: row.get(0)?,
            ticket_id: row.get(1)?,
            epoch: row.get::<_, i64>(2)? as u64,
            description: row.get(3)?,
            status: status_from_text(&status),
        })
    })?;
    for effect in rows {
        let effect = effect?;
        state.effects.insert(effect.effect_id.clone(), effect);
    }
    Ok(state)
}

/// Persist the DIFFERENCE between two states: the collections are never
/// rewritten, only the rows that changed.
fn persist_delta(
    tx: &Transaction,
    before: &CoordinatorState,
    after: &CoordinatorState,
) -> Result<(), StoreError> {
    for (ticket, claim) in &after.claims {
        if before.claims.get(ticket) != Some(claim) {
            tx.execute(
                "INSERT OR REPLACE INTO claims (ticket_id, holder, epoch) VALUES (?1, ?2, ?3)",
                params![claim.ticket_id, claim.holder, claim.epoch as i64],
            )?;
        }
    }
    for ticket in before.claims.keys() {
        if !after.claims.contains_key(ticket) {
            tx.execute("DELETE FROM claims WHERE ticket_id = ?1", params![ticket])?;
        }
    }
    for (ticket, epoch) in &after.epochs {
        if before.epochs.get(ticket) != Some(epoch) {
            tx.execute(
                "INSERT OR REPLACE INTO epochs (ticket_id, epoch) VALUES (?1, ?2)",
                params![ticket, *epoch as i64],
            )?;
        }
    }
    for ticket in before.epochs.keys() {
        if !after.epochs.contains_key(ticket) {
            tx.execute("DELETE FROM epochs WHERE ticket_id = ?1", params![ticket])?;
        }
    }
    for (effect_id, effect) in &after.effects {
        if before.effects.get(effect_id) != Some(effect) {
            tx.execute(
                "INSERT OR REPLACE INTO effects (effect_id, ticket_id, epoch, description, status)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    effect.effect_id,
                    effect.ticket_id,
                    effect.epoch as i64,
                    effect.description,
                    status_to_text(&effect.status)
                ],
            )?;
        }
    }
    for effect_id in before.effects.keys() {
        if !after.effects.contains_key(effect_id) {
            tx.execute(
                "DELETE FROM effects WHERE effect_id = ?1",
                params![effect_id],
            )?;
        }
    }
    Ok(())
}

fn status_to_text(status: &EffectStatus) -> &'static str {
    match status {
        EffectStatus::Pending => "pending",
        EffectStatus::Uncertain => "uncertain",
        EffectStatus::NotApplied => "not_applied",
        EffectStatus::Applied => "applied",
    }
}

fn status_from_text(text: &str) -> EffectStatus {
    match text {
        "applied" => EffectStatus::Applied,
        "uncertain" => EffectStatus::Uncertain,
        "not_applied" => EffectStatus::NotApplied,
        _ => EffectStatus::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::ClaimResult;

    fn temp_db(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("hodei-store-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("state.db")
    }

    fn claim(store: &mut CoordinatorStore, ticket: &str, holder: &str) -> u64 {
        let claim = store
            .transact(|state| Ok(state.try_claim(ticket, holder)))
            .unwrap();
        match claim {
            ClaimResult::Granted(claim) => claim.epoch,
            ClaimResult::Denied { .. } => panic!("the claim was denied"),
        }
    }

    #[test]
    fn state_and_intents_survive_crash_roundtrip() {
        let db = temp_db("roundtrip");
        {
            let mut store = CoordinatorStore::open("alpha", &db).unwrap();
            claim(&mut store, "T1", "client-a");
            let epoch = claim(&mut store, "T2", "client-b");
            store
                .transact(|state| {
                    state
                        .record_effect("e1", "T2", epoch, "sandbox create")
                        .map(|r| r.may_run)
                })
                .unwrap();
        } // "crash": store dropped

        let store = CoordinatorStore::open("alpha", &db).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored.claims.len(), 2);
        assert_eq!(restored.epochs.get("T2"), Some(&1));
        assert_eq!(
            restored.effects["e1"].status,
            EffectStatus::Pending,
            "the intent is written before the effect runs"
        );
        assert_eq!(restored.project, "alpha");
    }

    #[test]
    fn a_pending_intent_comes_back_as_reconciliable() {
        let db = temp_db("recover");
        {
            let mut store = CoordinatorStore::open("alpha", &db).unwrap();
            let epoch = claim(&mut store, "T1", "client-a");
            store
                .transact(|state| {
                    state
                        .record_effect("e-pending", "T1", epoch, "git push")
                        .map(|r| r.may_run)
                })
                .unwrap();
        }

        let mut store = CoordinatorStore::open("alpha", &db).unwrap();
        let recovered = store.recover_pending_effects().unwrap();
        assert_eq!(recovered, vec!["e-pending".to_string()]);
        assert_eq!(
            store.load().unwrap().effects["e-pending"].status,
            EffectStatus::Uncertain
        );
        assert_eq!(
            store.uncertain_effects().unwrap(),
            vec!["e-pending".to_string()]
        );
    }

    #[test]
    fn the_state_file_refuses_another_project() {
        let db = temp_db("identity");
        let store = CoordinatorStore::open("alpha", &db).unwrap();
        assert_eq!(store.project(), "alpha");
        let error = CoordinatorStore::open("beta", &db).unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::IdentityMismatch {
                    ref file,
                    ref requested
                } if file == "alpha" && requested == "beta"
            ),
            "the file of one project is not the file of another: {error}"
        );
    }

    #[test]
    fn a_write_failure_persists_nothing() {
        let db = temp_db("failure");
        {
            let mut store = CoordinatorStore::open("alpha", &db).unwrap();
            claim(&mut store, "T1", "client-a");
        }
        // A REAL write failure, injected from outside: SQLite refuses the
        // INSERT of the next claim.
        let injector = Connection::open(&db).unwrap();
        injector
            .execute_batch(
                "CREATE TRIGGER fail_claims BEFORE INSERT ON claims
                 BEGIN SELECT RAISE(ABORT, 'injected write failure'); END;",
            )
            .unwrap();

        let mut store = CoordinatorStore::open("alpha", &db).unwrap();
        let error = store
            .transact(|state| Ok(state.try_claim("T2", "client-b")))
            .unwrap_err();
        assert!(matches!(error, TransactError::Store(_)), "{error}");

        // Nothing was committed: the claim is not there and the epoch of
        // T2 did not move.
        let state = store.load().unwrap();
        assert!(!state.claims.contains_key("T2"));
        assert!(!state.epochs.contains_key("T2"));
        assert!(state.claims.contains_key("T1"));
    }

    #[test]
    fn two_stores_on_one_file_serialize_their_writes() {
        let db = temp_db("serialized");
        let mut first = CoordinatorStore::open("alpha", &db).unwrap();
        let mut second = CoordinatorStore::open("alpha", &db).unwrap();
        let a = first
            .transact(|state| Ok(state.try_claim("T1", "client-a")))
            .unwrap();
        let b = second
            .transact(|state| Ok(state.try_claim("T1", "client-b")))
            .unwrap();
        let grants = [a, b]
            .into_iter()
            .filter(|result| matches!(result, ClaimResult::Granted(_)))
            .count();
        assert_eq!(
            grants, 1,
            "the write lock is the exclusion, not a convention"
        );
    }

    #[test]
    fn the_second_coordinator_waits_for_the_decision_of_the_first() {
        let db = temp_db("excluded");
        // The first coordinator decides INSIDE its transaction, and keeps it
        // open while it decides: the second one asks for the same ticket
        // meanwhile and must wait for the decision.
        let (inside_tx, inside_rx) = std::sync::mpsc::channel::<()>();
        let (decide, decide_rx) = std::sync::mpsc::channel::<()>();
        let first = std::thread::spawn({
            let db = db.clone();
            move || {
                let mut store = CoordinatorStore::open("alpha", &db).unwrap();
                store
                    .transact(|state| {
                        inside_tx.send(()).unwrap();
                        decide_rx.recv().unwrap();
                        Ok(state.try_claim("T1", "client-a"))
                    })
                    .unwrap()
            }
        });
        inside_rx.recv().unwrap();

        let db_for_second = db.clone();
        let second = std::thread::spawn(move || {
            let mut store = CoordinatorStore::open("alpha", &db_for_second).unwrap();
            store
                .transact(|state| Ok(state.try_claim("T1", "client-b")))
                .unwrap()
        });
        // The second coordinator had the time to read the state while the
        // first one was deciding: it must still be denied.
        std::thread::sleep(Duration::from_millis(200));
        decide.send(()).unwrap();

        let first_result = first.join().unwrap();
        let second_result = second.join().unwrap();
        let grants = [first_result, second_result]
            .into_iter()
            .filter(|result| matches!(result, ClaimResult::Granted(_)))
            .count();
        assert_eq!(
            grants, 1,
            "the second coordinator must see the decision of the first"
        );
    }
}
