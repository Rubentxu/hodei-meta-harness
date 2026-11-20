//! The durable checkpoint of the live pipeline.
//!
//! One JSON record per project survives a restart: the goal identity, the
//! run and the session that carried it, the observed candidate and whether
//! the attempt was still pending or already confirmed. A rollover always
//! opens a NEW session, and a confirmed effect is never re-applied; a
//! pending record is reported as uncertain instead of being executed
//! again, and it refuses the next attempt until it is resolved. The
//! handoff is rendered from the checkpoint (never from a conversation)
//! and travels WITH the Pending record, so a route the boot announces
//! exists from the moment the record does. The stored instruction is
//! never the raw text: the handoff's own redactor passes over it before
//! it is persisted, and the record and the directory are private to the
//! owner (mode `0600` and `0700`).

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};

use rollover::checkpoint::{Checkpoint, RolloverLedger};
use rollover::handoff::{redact_secrets, render_handoff};
use runner::live::{AttemptGoal, AttemptRecordError, AttemptRecordPort};
use serde::{Deserialize, Serialize};

/// The file the durable record lives in, inside the project state dir.
pub const CHECKPOINT_FILE: &str = "live-checkpoint.json";

/// The rendered handoff, inside the project state dir.
pub const HANDOFF_FILE: &str = "HANDOFF.md";

/// Write a file that only this user can read, and get the bytes onto the
/// media before the caller publishes them (with a rename, or as the last
/// word of the attempt).
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

/// How far the attempt the record describes got.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LivePhase {
    /// The turn may have run: its effects are not known to be applied.
    Pending,
    /// The attempt reached a terminal effect: nothing must be redone.
    Confirmed,
}

/// The candidate an attempt observed, as the record keeps it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateRef {
    pub base: String,
    pub head: String,
    pub bundle_digest: String,
}

/// The durable record of one project.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LiveRecord {
    /// The rollover checkpoint: goal identity, epoch and session.
    pub checkpoint: Checkpoint,
    /// The controller's run that carried the objective, when it named one.
    pub run: Option<u64>,
    pub phase: LivePhase,
    pub candidate: Option<CandidateRef>,
}

/// How a record read at boot is to be treated.
#[derive(Debug, Clone, PartialEq)]
pub enum Recovery {
    /// Nothing was ever recorded: there is nothing to recover.
    Fresh,
    /// A confirmed attempt: nothing to redo, and its effect is not applied
    /// a second time.
    NothingToRedo(LiveRecord),
    /// A pending attempt: it is uncertain whether it happened, and it is
    /// never silently executed again.
    Uncertain(LiveRecord),
}

/// Read a record (or its absence) as a recovery classification.
pub fn classify(record: Option<&LiveRecord>) -> Recovery {
    match record {
        None => Recovery::Fresh,
        Some(record) => match record.phase {
            LivePhase::Confirmed => Recovery::NothingToRedo(record.clone()),
            LivePhase::Pending => Recovery::Uncertain(record.clone()),
        },
    }
}

/// The goal identity a record keeps across rollovers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalIdentity {
    pub goal_id: String,
    pub work_ref: String,
    pub next_action: String,
    pub references: Vec<String>,
}

impl GoalIdentity {
    /// The goal of one objective: its own reference, and the run as the
    /// next action a restart reads.
    pub fn of(work_ref: &str, instruction: &str, run: Option<u64>) -> Self {
        Self {
            goal_id: work_ref.to_string(),
            work_ref: work_ref.to_string(),
            next_action: match run {
                Some(run) => format!("run {run}: {instruction}"),
                None => instruction.to_string(),
            },
            references: vec![work_ref.to_string()],
        }
    }
}

/// Why the durable record could not be kept.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("the live checkpoint {path} could not be read: {reason}")]
    Read { path: String, reason: String },
    #[error("the live checkpoint {path} could not be written: {reason}")]
    Write { path: String, reason: String },
    #[error(
        "a rollover always opens a NEW session: {session} is the session of the record it would replace"
    )]
    SameSession { session: String },
    #[error("the handoff of {goal} could not be rendered: {reason}")]
    Handoff { goal: String, reason: String },
}

/// The instruction as it is persisted: the handoff's own redactor passes
/// over it BEFORE the record is written, so what survives on the media is
/// never the raw text.
fn redacted(text: &str) -> String {
    redact_secrets(text).trim_end_matches('\n').to_string()
}

/// The durable checkpoints of one project.
#[derive(Debug, Clone)]
pub struct LiveCheckpoints {
    dir: PathBuf,
}

impl LiveCheckpoints {
    /// The checkpoints kept under `dir`.
    pub fn under(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The checkpoints of `project`, under its private state directory.
    pub fn for_project(project: &str) -> Self {
        Self::under(hodei_mh_coordinator::paths::project_state_dir(project))
    }

    fn path(&self) -> PathBuf {
        self.dir.join(CHECKPOINT_FILE)
    }

    /// The handoff file of this project.
    pub fn handoff_path(&self) -> PathBuf {
        self.dir.join(HANDOFF_FILE)
    }

    /// The record as it was last persisted, or none.
    pub fn load(&self) -> Result<Option<LiveRecord>, CheckpointError> {
        let path = self.path();
        let raw = match std::fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(CheckpointError::Read {
                    path: path.display().to_string(),
                    reason: error.to_string(),
                });
            }
        };
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|error| CheckpointError::Read {
                path: path.display().to_string(),
                reason: error.to_string(),
            })
    }

    /// Classify the persisted record for a boot.
    pub fn recover(&self) -> Result<Recovery, CheckpointError> {
        let record = self.load()?;
        Ok(classify(record.as_ref()))
    }

    /// Write the Pending record of a turn that is about to run.
    ///
    /// The goal identity survives the rollover; the epoch advances and the
    /// session id must be NEW, because a rollover never resumes the
    /// conversation of the session it replaces.
    pub fn begin(
        &self,
        goal: &GoalIdentity,
        run: Option<u64>,
        session_id: &str,
    ) -> Result<LiveRecord, CheckpointError> {
        let previous = self.load()?;
        if let Some(previous) = &previous
            && previous.checkpoint.session_id == session_id
        {
            return Err(CheckpointError::SameSession {
                session: session_id.to_string(),
            });
        }
        // What the record keeps is the redacted instruction, never the
        // raw text: a secret that reaches the objective never reaches
        // the media through this path.
        let next_action = redacted(&goal.next_action);
        let references: Vec<String> = goal.references.iter().map(|r| redacted(r)).collect();
        let mut ledger = match &previous {
            Some(record) => RolloverLedger::initial(record.checkpoint.clone()),
            None => RolloverLedger::initial(Checkpoint {
                goal_id: goal.goal_id.clone(),
                work_ref: goal.work_ref.clone(),
                epoch: 0,
                session_id: String::new(),
                next_action: next_action.clone(),
                decisions: Vec::new(),
                references: references.clone(),
                evidences: Vec::new(),
                failed_strategies: Vec::new(),
            }),
        };
        ledger.begin();
        ledger.record_session(session_id);
        let mut checkpoint = ledger.confirm();
        checkpoint.goal_id = goal.goal_id.clone();
        checkpoint.work_ref = goal.work_ref.clone();
        checkpoint.next_action = next_action;
        checkpoint.references = references;
        let record = LiveRecord {
            checkpoint,
            run,
            phase: LivePhase::Pending,
            candidate: None,
        };
        self.persist(&record)?;
        Ok(record)
    }

    /// Record the candidate an attempt observed, still Pending.
    pub fn observe(&self, candidate: CandidateRef) -> Result<LiveRecord, CheckpointError> {
        let Some(mut record) = self.load()? else {
            return Err(CheckpointError::Write {
                path: self.path().display().to_string(),
                reason: "there is no pending checkpoint to record the candidate on".into(),
            });
        };
        record.candidate = Some(candidate);
        self.persist(&record)?;
        Ok(record)
    }

    /// Confirm the attempt: the effect is settled and must not be redone.
    ///
    /// Confirming an absent or already confirmed record is idempotent: a
    /// confirmed effect stays confirmed.
    pub fn confirm(&self, candidate: Option<CandidateRef>) -> Result<LiveRecord, CheckpointError> {
        let mut record = self.load()?.ok_or_else(|| CheckpointError::Write {
            path: self.path().display().to_string(),
            reason: "there is no checkpoint to confirm".into(),
        })?;
        record.phase = LivePhase::Confirmed;
        if candidate.is_some() {
            record.candidate = candidate;
        }
        self.persist(&record)?;
        Ok(record)
    }

    /// Render the handoff of `record` and return its path.
    ///
    /// The handoff travels WITH the record: it is written before the
    /// turn starts, from the checkpoint alone, and never from a
    /// conversation.
    pub fn write_handoff(&self, record: &LiveRecord) -> Result<PathBuf, CheckpointError> {
        let rendered =
            render_handoff(&record.checkpoint).map_err(|error| CheckpointError::Handoff {
                goal: record.checkpoint.goal_id.clone(),
                reason: error.to_string(),
            })?;
        let path = self.handoff_path();
        let failure = |reason: String| CheckpointError::Write {
            path: path.display().to_string(),
            reason,
        };
        supervisor::paths::private_dir(&self.dir).map_err(|error| failure(error.to_string()))?;
        write_private(&path, rendered.as_bytes()).map_err(|error| failure(error.to_string()))?;
        Ok(path)
    }

    /// Replace the record atomically: a crash leaves either the previous
    /// record or the new one, never a torn file. The directory is private
    /// (`0700`) and the bytes reach the media (`fsync`) before the rename.
    fn persist(&self, record: &LiveRecord) -> Result<(), CheckpointError> {
        let path = self.path();
        let failure = |reason: String| CheckpointError::Write {
            path: path.display().to_string(),
            reason,
        };
        supervisor::paths::private_dir(&self.dir).map_err(|error| failure(error.to_string()))?;
        let encoded =
            serde_json::to_string_pretty(record).map_err(|error| failure(error.to_string()))?;
        // A stable temporary name: a crash leaves one orphan `.tmp` that
        // the next write truncates, never a growing pile of suffixes.
        let temporary = path.with_extension("json.tmp");
        write_private(&temporary, encoded.as_bytes())
            .map_err(|error| failure(error.to_string()))?;
        if let Err(error) = std::fs::rename(&temporary, &path) {
            let _ = std::fs::remove_file(&temporary);
            return Err(failure(error.to_string()));
        }
        Ok(())
    }
}

/// The goal identity an attempt record describes: the run is part of it,
/// so two attempts at the same reference never share a goal identity.
fn identity(goal: &AttemptGoal) -> GoalIdentity {
    GoalIdentity::of(&goal.work_ref, &goal.instruction, Some(goal.run))
}

/// The durable attempt record the live runtime opens before every claim:
/// the same JSON checkpoint, seen through the port the runtime speaks.
/// A confirmed record settles with nothing to redo; a pending one is
/// uncertain and refuses the next attempt until it is resolved — the
/// effect it describes is never re-applied to settle it.
#[derive(Debug, Clone)]
pub struct CheckpointRecords {
    checkpoints: LiveCheckpoints,
}

impl CheckpointRecords {
    /// The records of `project`, under its private state directory.
    pub fn for_project(project: &str) -> Self {
        Self {
            checkpoints: LiveCheckpoints::for_project(project),
        }
    }

    /// The records kept under a private state directory: the injection
    /// point of the tests, and of any caller that owns its state dir.
    #[allow(dead_code)]
    pub fn under(dir: impl Into<PathBuf>) -> Self {
        Self {
            checkpoints: LiveCheckpoints::under(dir),
        }
    }
}

impl AttemptRecordPort for CheckpointRecords {
    fn recover(&self) -> Result<(), AttemptRecordError> {
        match self.checkpoints.recover() {
            Ok(Recovery::Fresh) | Ok(Recovery::NothingToRedo(_)) => Ok(()),
            Ok(Recovery::Uncertain(record)) => Err(AttemptRecordError {
                work_ref: record.checkpoint.work_ref.clone(),
                reason: "the previous attempt is recorded as pending: its effects are unknown, \
                        so it is neither redone nor overwritten — resolve the record \
                        explicitly before starting another attempt"
                    .into(),
            }),
            Err(error) => Err(AttemptRecordError {
                work_ref: String::new(),
                reason: error.to_string(),
            }),
        }
    }

    fn begin_attempt(&self, goal: &AttemptGoal) -> Result<(), AttemptRecordError> {
        // A session id no previous record can carry: pid, run and clock,
        // so the same-session refusal can never fire on a fresh attempt
        // and every rollover really opens a NEW session.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.subsec_nanos())
            .unwrap_or(0);
        let session = format!("run-{}-{}-{nanos}", std::process::id(), goal.run);
        let record = self
            .checkpoints
            .begin(&identity(goal), Some(goal.run), &session)
            .map_err(|error| AttemptRecordError {
                work_ref: goal.work_ref.clone(),
                reason: error.to_string(),
            })?;
        // The handoff travels WITH the Pending record: a boot that
        // advertises a route finds the file already on the media.
        self.checkpoints
            .write_handoff(&record)
            .map_err(|error| AttemptRecordError {
                work_ref: goal.work_ref.clone(),
                reason: error.to_string(),
            })?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    fn dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "hodei-live-checkpoint-{}-{tag}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    fn goal(work_ref: &str) -> GoalIdentity {
        GoalIdentity::of(work_ref, "do the work", Some(7))
    }

    fn candidate() -> CandidateRef {
        CandidateRef {
            base: "base-1".into(),
            head: "head-1".into(),
            bundle_digest: "digest-1".into(),
        }
    }

    #[test]
    fn a_restart_opens_a_new_session_and_keeps_the_goal_identity() {
        let dir = dir("restart");
        let checkpoints = LiveCheckpoints::under(&dir);
        let first = checkpoints
            .begin(&goal("TASK-7"), Some(7), "session-1")
            .expect("the first pending record");
        assert_eq!(first.checkpoint.epoch, 1);
        assert_eq!(first.phase, LivePhase::Pending);

        // The runtime is torn down and booted again on the same state dir.
        let booted = LiveCheckpoints::under(&dir);
        let second = booted
            .begin(&goal("TASK-7"), Some(8), "session-2")
            .expect("the restart record");
        assert_eq!(second.checkpoint.session_id, "session-2");
        assert_ne!(second.checkpoint.session_id, "session-1");
        assert_eq!(second.checkpoint.epoch, 2, "the epoch fences the attempt");
        assert_eq!(second.checkpoint.goal_id, "TASK-7");
        assert_eq!(second.checkpoint.work_ref, "TASK-7");
        assert_eq!(second.run, Some(8));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_same_session_is_never_reused_by_a_rollover() {
        let dir = dir("same-session");
        let checkpoints = LiveCheckpoints::under(&dir);
        checkpoints
            .begin(&goal("TASK-7"), Some(1), "session-1")
            .expect("the first record");
        let reused = checkpoints.begin(&goal("TASK-7"), Some(2), "session-1");
        assert!(
            matches!(reused, Err(CheckpointError::SameSession { ref session }) if session == "session-1"),
            "{reused:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_confirmed_attempt_is_reported_as_nothing_to_redo() {
        let dir = dir("confirmed");
        let checkpoints = LiveCheckpoints::under(&dir);
        checkpoints
            .begin(&goal("TASK-7"), Some(1), "session-1")
            .expect("the pending record");
        checkpoints.observe(candidate()).expect("the candidate");
        checkpoints
            .confirm(Some(candidate()))
            .expect("the confirmed record");

        // Booted twice: the same answer, and no effect is re-applied.
        for _ in 0..2 {
            match LiveCheckpoints::under(&dir).recover().expect("recover") {
                Recovery::NothingToRedo(record) => {
                    assert_eq!(record.candidate, Some(candidate()));
                    assert_eq!(record.checkpoint.work_ref, "TASK-7");
                }
                other => panic!("expected nothing to redo, got {other:?}"),
            }
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pending_attempt_is_uncertain_and_never_silently_re_executed() {
        let dir = dir("pending");
        let checkpoints = LiveCheckpoints::under(&dir);
        checkpoints
            .begin(&goal("TASK-7"), Some(1), "session-1")
            .expect("the pending record");
        checkpoints.observe(candidate()).expect("the candidate");

        for _ in 0..2 {
            match LiveCheckpoints::under(&dir).recover().expect("recover") {
                Recovery::Uncertain(record) => {
                    assert_eq!(record.phase, LivePhase::Pending);
                    assert_eq!(record.candidate, Some(candidate()));
                }
                other => panic!("expected uncertainty, got {other:?}"),
            }
        }
        // Recovering never rewrites the record: the uncertainty stands
        // until an explicit confirmation says otherwise.
        let raw = std::fs::read_to_string(dir.join(CHECKPOINT_FILE)).expect("the record");
        assert!(raw.contains("\"pending\""), "{raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_state_dir_has_nothing_to_recover() {
        let dir = dir("fresh");
        assert_eq!(
            LiveCheckpoints::under(&dir).recover().expect("recover"),
            Recovery::Fresh
        );
    }

    #[test]
    fn the_handoff_names_the_goal_and_never_carries_a_secret() {
        let dir = dir("handoff");
        let checkpoints = LiveCheckpoints::under(&dir);
        // The reference avoids the `sk-` shape of an API key prefix, which
        // the handoff redacts on purpose.
        let mut identity = goal("GC-7");
        identity.next_action = "continue GC-7 with token=abc123".into();
        let record = checkpoints
            .begin(&identity, Some(1), "session-1")
            .expect("the record");
        let path = checkpoints.write_handoff(&record).expect("the handoff");
        assert_eq!(path.file_name(), Some(std::ffi::OsStr::new(HANDOFF_FILE)));
        let rendered = std::fs::read_to_string(&path).expect("the handoff file");
        assert!(rendered.contains("GC-7"), "rendered: {rendered:?}");
        assert!(rendered.contains("session-1"));
        assert!(!rendered.contains("abc123"), "{rendered}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_record_and_its_directory_are_private_to_the_owner() {
        let dir = dir("modes");
        let checkpoints = LiveCheckpoints::under(&dir);
        checkpoints
            .begin(&goal("TASK-7"), Some(1), "session-1")
            .expect("the record");
        checkpoints
            .write_handoff(&checkpoints.load().expect("load").expect("record"))
            .expect("the handoff");

        let dir_mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(dir_mode, 0o700, "the state directory is private");
        for name in [CHECKPOINT_FILE, HANDOFF_FILE] {
            let mode = std::fs::metadata(dir.join(name))
                .unwrap_or_else(|_| panic!("{name} exists"))
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "{name} is readable only by its owner");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_wide_directory_mode_is_tightened_before_the_record_lands() {
        let dir = dir("tighten");
        std::fs::create_dir_all(&dir).expect("the loose directory");
        std::fs::set_permissions(&dir, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .expect("loosen");
        let checkpoints = LiveCheckpoints::under(&dir);
        checkpoints
            .begin(&goal("TASK-7"), Some(1), "session-1")
            .expect("the record");
        let mode = std::fs::metadata(&dir)
            .expect("metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700, "the record never lands in a shared directory");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_persisted_record_never_carries_the_raw_instruction() {
        let dir = dir("raw-secret");
        let checkpoints = LiveCheckpoints::under(&dir);
        let mut identity = goal("GC-7");
        identity.next_action = "continue GC-7 with password=hunter2".into();
        identity.references.push("api key sk-live-9988".into());
        checkpoints
            .begin(&identity, Some(1), "session-1")
            .expect("the record");
        let raw = std::fs::read_to_string(dir.join(CHECKPOINT_FILE)).expect("the record");
        assert!(raw.contains("GC-7"), "the identity survives: {raw}");
        assert!(
            !raw.contains("hunter2"),
            "the secret is not on the media: {raw}"
        );
        assert!(!raw.contains("9988"), "the key is not on the media: {raw}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn opening_an_attempt_writes_the_record_and_its_handoff_together() {
        let dir = dir("records-handoff");
        let records = CheckpointRecords::under(&dir);
        records
            .begin_attempt(&AttemptGoal {
                work_ref: "TASK-7".into(),
                instruction: "do the work".into(),
                run: 3,
            })
            .expect("the attempt opens");
        assert!(
            dir.join(CHECKPOINT_FILE).exists(),
            "the pending record is on the media"
        );
        assert!(
            dir.join(HANDOFF_FILE).exists(),
            "the handoff travels with the record, before the turn"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pending_record_refuses_the_next_attempt_and_a_confirmed_one_does_not() {
        let dir = dir("records-refusal");
        let records = CheckpointRecords::under(&dir);
        let goal = AttemptGoal {
            work_ref: "TASK-7".into(),
            instruction: "do the work".into(),
            run: 3,
        };
        records.begin_attempt(&goal).expect("the attempt opens");

        // The Pending record is uncertain until it is resolved: the next
        // attempt is refused, and the effect is never re-applied to settle it.
        let refused = records.recover();
        assert!(
            matches!(&refused, Err(error) if error.work_ref == "TASK-7"),
            "{refused:?}"
        );
        assert!(
            matches!(&refused, Err(error) if error.reason.contains("pending")),
            "{refused:?}"
        );

        // Resolving the record explicitly (confirming it) lets the next
        // attempt open.
        LiveCheckpoints::under(&dir)
            .confirm(None)
            .expect("the record is confirmed");
        records.recover().expect("a confirmed record settles");
        records
            .begin_attempt(&AttemptGoal { run: 4, ..goal })
            .expect("the next attempt opens");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_fresh_project_opens_its_first_attempt_without_a_prior_record() {
        let dir = dir("records-fresh");
        let records = CheckpointRecords::under(&dir);
        records.recover().expect("nothing to recover");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
