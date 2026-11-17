//! SQLite persistence: the coordinator's state and effect intents survive
//! crashes. The database file is the durability boundary; every write is
//! committed before the in-memory confirmation returns.

use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};

use crate::core::{Claim, CoordinatorState, EffectIntent, EffectStatus};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
}

pub struct CoordinatorStore {
    conn: Connection,
}

impl CoordinatorStore {
    /// Open (creating schema when needed) a database file.
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS claims (
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
            );",
        )?;
        Ok(Self { conn })
    }

    /// Persist the whole state in one transaction (read-modify-write callers
    /// hold the actor mailbox, so writes are serialized by design).
    pub fn save(&self, state: &CoordinatorState) -> Result<(), StoreError> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM claims", [])?;
        tx.execute("DELETE FROM epochs", [])?;
        tx.execute("DELETE FROM effects", [])?;
        for claim in state.claims.values() {
            tx.execute(
                "INSERT INTO claims (ticket_id, holder, epoch) VALUES (?1, ?2, ?3)",
                params![claim.ticket_id, claim.holder, claim.epoch as i64],
            )?;
        }
        for (ticket, epoch) in &state.epochs {
            tx.execute(
                "INSERT INTO epochs (ticket_id, epoch) VALUES (?1, ?2)",
                params![ticket, *epoch as i64],
            )?;
        }
        for effect in state.effects.values() {
            let status = match effect.status {
                EffectStatus::Pending => "pending",
                EffectStatus::Applied => "applied",
                EffectStatus::Uncertain => "uncertain",
            };
            tx.execute(
                "INSERT INTO effects (effect_id, ticket_id, epoch, description, status)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    effect.effect_id,
                    effect.ticket_id,
                    effect.epoch as i64,
                    effect.description,
                    status
                ],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Rebuild the in-memory state after a crash/restart.
    pub fn load(&self) -> Result<CoordinatorState, StoreError> {
        let mut state = CoordinatorState::default();
        let mut stmt = self
            .conn
            .prepare("SELECT ticket_id, holder, epoch FROM claims")?;
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
        let mut stmt = self.conn.prepare("SELECT ticket_id, epoch FROM epochs")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as u64))
        })?;
        for row in rows {
            let (ticket, epoch) = row?;
            state.epochs.insert(ticket, epoch);
        }
        let mut stmt = self
            .conn
            .prepare("SELECT effect_id, ticket_id, epoch, description, status FROM effects")?;
        let rows = stmt.query_map([], |row| {
            let status: String = row.get(4)?;
            Ok(EffectIntent {
                effect_id: row.get(0)?,
                ticket_id: row.get(1)?,
                epoch: row.get::<_, i64>(2)? as u64,
                description: row.get(3)?,
                status: match status.as_str() {
                    "applied" => EffectStatus::Applied,
                    "uncertain" => EffectStatus::Uncertain,
                    _ => EffectStatus::Pending,
                },
            })
        })?;
        for effect in rows {
            let effect = effect?;
            state.effects.insert(effect.effect_id.clone(), effect);
        }
        Ok(state)
    }

    /// Uncertain effect ids awaiting reconciliation after a restart.
    pub fn uncertain_effects(&self) -> Result<Vec<String>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT effect_id FROM effects WHERE status = 'uncertain'")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.flatten().collect())
    }

    #[allow(dead_code)]
    fn get_claim(&self, ticket_id: &str) -> Result<Option<Claim>, StoreError> {
        let mut stmt = self
            .conn
            .prepare("SELECT holder, epoch FROM claims WHERE ticket_id = ?1")?;
        let claim = stmt
            .query_row(params![ticket_id], |row| {
                Ok(Claim {
                    ticket_id: ticket_id.to_string(),
                    holder: row.get(0)?,
                    epoch: row.get::<_, i64>(1)? as u64,
                })
            })
            .optional()?;
        Ok(claim)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_and_intents_survive_crash_roundtrip() {
        let dir = std::env::temp_dir().join(format!("hodei-coord-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");

        let mut state = CoordinatorState::default();
        state.try_claim("T1", "client-a");
        let claim = match state.try_claim("T2", "client-b") {
            crate::core::ClaimResult::Granted(claim) => claim,
            _ => unreachable!(),
        };
        state.record_effect("e1", "T2", claim.epoch, "sandbox create");
        {
            let store = CoordinatorStore::open(&db).unwrap();
            store.save(&state).unwrap();
        } // "crash": store dropped

        let store = CoordinatorStore::open(&db).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(restored, state);
        assert_eq!(restored.claims.len(), 2);
        assert_eq!(restored.epochs.get("T2"), Some(&1));
        assert_eq!(restored.effects["e1"].status, EffectStatus::Pending);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn uncertain_effects_survive_and_are_listed_for_reconciliation() {
        let dir = std::env::temp_dir().join(format!("hodei-coord-u-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let db = dir.join("state.db");
        let mut state = CoordinatorState::default();
        state.record_effect("e-uncertain", "T1", 1, "git push");
        state.mark_uncertain("e-uncertain");
        {
            let store = CoordinatorStore::open(&db).unwrap();
            store.save(&state).unwrap();
        }
        let store = CoordinatorStore::open(&db).unwrap();
        let restored = store.load().unwrap();
        assert_eq!(
            restored.effects["e-uncertain"].status,
            EffectStatus::Uncertain
        );
        assert_eq!(
            store.uncertain_effects().unwrap(),
            vec!["e-uncertain".to_string()]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
