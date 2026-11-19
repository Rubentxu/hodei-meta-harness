//! The promotion and the closure, as ONE attempt that survives a crash.
//!
//! The fixtures are real: a temporary git repository with a real bundle and
//! a real compare-and-swap, and the real durable store of the coordinator.
//! The tracker is a fake, so the crash boundaries can be provoked exactly
//! where they are provoked in production: the tracker fails, the store
//! fails, the branch moves, the attempt is fenced out.
//!
//! No model, no network and no container are involved.

use std::path::{Path, PathBuf};
use std::process::Command;

use coordinator::store::CoordinatorStore;
use promote::flow::{PromotionError, PromotionRequest, execute};
use promote::intent::{Conflict, PromotionFacts, PromotionIntent, PromotionOutcome};
use promote::journal::{CoordinatorJournal, IntentJournal, JournalError};
use promote::writer::{FakeWorkWriter, TicketSnapshot, WorkWriter};
use serde_json::json;
use verify::evidence::EvidenceSet;
use verify::transport::TransportDigest;

const TICKET: &str = "TASK-1";
const PROJECT: &str = "alpha";
const BRANCH: &str = "hodei-mh/main";

/// A temporary project with a base, a candidate and the bundle that
/// carries the candidate.
struct Fixture {
    dir: PathBuf,
    repo: PathBuf,
    bundle: PathBuf,
    db: PathBuf,
    base: String,
    candidate: String,
    tree: String,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "hodei-promote-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|since| since.as_nanos())
                .unwrap_or_default()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("the temporary directory");
        let repo = dir.join("project");
        hodei_mh_workspace::git::init_fixture_repo(&repo).expect("a fixture repository");

        let base = commit(&repo, "base.txt", "the base");
        git(&repo, &["checkout", "-q", "-b", "candidate"]);
        let candidate = commit(&repo, "candidate.txt", "the candidate work");
        let tree = git(&repo, &["rev-parse", "candidate^{tree}"]);
        let bundle = dir.join("candidate.bundle");
        git(
            &repo,
            &["bundle", "create", bundle.to_str().unwrap(), "--all"],
        );
        git(&repo, &["checkout", "-q", "main"]);
        git(&repo, &["branch", "-q", BRANCH, &base]);

        Self {
            db: dir.join("coordinator.db"),
            dir,
            repo,
            bundle,
            base,
            candidate,
            tree,
        }
    }

    /// The intent of the attempt: the facts of the promotion.
    fn intent(&self) -> PromotionIntent {
        PromotionIntent::new(PromotionFacts {
            project: PROJECT.into(),
            work_ref: TICKET.into(),
            epoch: 1,
            managed_branch: BRANCH.into(),
            expected_base: self.base.clone(),
            expected_candidate: self.candidate.clone(),
            target_status: "Done".into(),
            note: "evidence of the promotion".into(),
            check_ac: vec![1],
            final_summary: "promoted and closed".into(),
        })
        .expect("a valid intent")
    }

    /// The evidence that speaks for the candidate.
    fn evidence(&self) -> EvidenceSet {
        let bytes = std::fs::read(&self.bundle).expect("the bundle");
        evidence(TICKET, &self.candidate, &self.tree, &bytes)
    }

    /// The tracker, with the ticket open.
    fn tracker(&self) -> FakeWorkWriter {
        let tracker = FakeWorkWriter::default();
        tracker
            .tickets
            .lock()
            .expect("lock")
            .insert(TICKET.into(), ("To Do".into(), Vec::new(), Vec::new()));
        tracker
    }

    /// The store, with the ticket claimed by the harness.
    fn store(&self) -> CoordinatorStore {
        let mut store = CoordinatorStore::open(PROJECT, &self.db).expect("the durable store");
        let claim = store
            .transact(|state| Ok(state.try_claim(TICKET, "hodei-mh")))
            .expect("the claim");
        match claim {
            coordinator::core::ClaimResult::Granted(claim) => assert_eq!(claim.epoch, 1),
            coordinator::core::ClaimResult::Denied { holder } => {
                panic!("the claim was denied by {holder}")
            }
        }
        store
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// The evidence document, as it arrives from the store.
fn evidence(work_ref: &str, revision: &str, tree: &str, bundle: &[u8]) -> EvidenceSet {
    let digest = TransportDigest::of(bundle);
    let document = json!({
        "work_ref": work_ref,
        "binding": {
            "revision": revision,
            "tree": tree,
            "transport_digest": digest.as_str(),
            "required_checks": [{"id": "AC-1", "command": "make check"}],
            "image_id": "sha256:image",
            "environment": "sha256:image::sandbox-1"
        },
        "entries": [{
            "result": {
                "check_id": "AC-1",
                "environment": "sha256:image::sandbox-1",
                "image_id": "sha256:image",
                "tree": tree,
                "transport_digest": digest.as_str(),
                "command": "make check",
                "record": {
                    "command": "make check",
                    "exit_code": 0,
                    "output": "ok",
                    "output_truncated": false,
                    "stream_completed": true
                }
            },
            "status": "Valid"
        }]
    });
    serde_json::from_value(document).expect("the evidence document is well formed")
}

/// The journal that fails when the attempt settles: the crash that happens
/// after the effects and before their record.
struct FaultyJournal<'a> {
    inner: CoordinatorJournal<'a>,
    fail_on_settle: bool,
}

impl<'a> FaultyJournal<'a> {
    fn new(store: &'a mut CoordinatorStore) -> Self {
        Self {
            inner: CoordinatorJournal::new(store),
            fail_on_settle: false,
        }
    }
}

impl IntentJournal for FaultyJournal<'_> {
    fn project(&self) -> &str {
        self.inner.project()
    }

    fn load(
        &self,
        operation_id: &str,
    ) -> Result<Option<promote::intent::JournalRecord>, JournalError> {
        self.inner.load(operation_id)
    }

    fn record(&mut self, intent: &PromotionIntent) -> Result<(), JournalError> {
        self.inner.record(intent)
    }

    fn current_epoch(&self, work_ref: &str) -> Result<Option<u64>, JournalError> {
        self.inner.current_epoch(work_ref)
    }

    fn settle(
        &mut self,
        operation_id: &str,
        outcome: &PromotionOutcome,
    ) -> Result<(), JournalError> {
        if self.fail_on_settle {
            return Err(JournalError::Store("the process died".into()));
        }
        self.inner.settle(operation_id, outcome)
    }
}

/// Run the attempt with the real store and the given tracker.
fn run(
    fixture: &Fixture,
    tracker: &FakeWorkWriter,
    store: &mut CoordinatorStore,
) -> Result<promote::flow::PromotionReport, PromotionError> {
    let intent = fixture.intent();
    let evidence = fixture.evidence();
    let mut journal = CoordinatorJournal::new(store);
    execute(
        PromotionRequest {
            intent: &intent,
            evidence: &evidence,
            repo: &fixture.repo,
            bundle: &fixture.bundle,
            candidate_branch: "candidate",
        },
        tracker,
        &mut journal,
    )
}

/// The state of the ticket, as the tracker answers it.
fn ticket_of(tracker: &FakeWorkWriter) -> TicketSnapshot {
    tracker.read_ticket(TICKET).expect("the ticket")
}

/// The head of the managed branch.
fn head_of(fixture: &Fixture) -> String {
    git(&fixture.repo, &["rev-parse", BRANCH])
}

/// The commits the managed branch adds on top of the base.
fn added_commits(fixture: &Fixture) -> Vec<String> {
    let range = format!("{}..{BRANCH}", fixture.base);
    let listing = git(&fixture.repo, &["rev-list", &range]);
    listing.lines().map(str::to_string).collect()
}

/// Run a git command in the fixture repository.
fn git(repo: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .env("GIT_AUTHOR_NAME", "hodei-mh")
        .env("GIT_AUTHOR_EMAIL", "hodei-mh@local")
        .env("GIT_COMMITTER_NAME", "hodei-mh")
        .env("GIT_COMMITTER_EMAIL", "hodei-mh@local")
        .output()
        .expect("git runs");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// Build a commit with git's plumbing.
fn commit(repo: &Path, name: &str, message: &str) -> String {
    std::fs::write(repo.join(name), format!("content of {name}\n")).expect("the file");
    git(repo, &["add", name]);
    hodei_mh_workspace::git::commit_index(repo, message).expect("the commit")
}

#[test]
fn the_attempt_promotes_the_candidate_and_closes_the_ticket_once() {
    let fixture = Fixture::new("happy");
    let tracker = fixture.tracker();
    let mut store = fixture.store();

    let report = run(&fixture, &tracker, &mut store).expect("the attempt");

    assert!(report.promoted, "the candidate was promoted");
    assert!(report.closed, "the ticket was closed");
    assert_eq!(report.commit.as_deref(), Some(fixture.candidate.as_str()));
    assert_eq!(head_of(&fixture), fixture.candidate);
    assert_eq!(
        added_commits(&fixture),
        vec![fixture.candidate.clone()],
        "the managed branch carries the candidate and nothing else"
    );

    let ticket = ticket_of(&tracker);
    assert_eq!(ticket.status, "Done");
    assert_eq!(ticket.checked_ac, vec![1]);
    let marker = fixture.intent().marker();
    assert_eq!(
        ticket.notes.matches(&marker).count(),
        1,
        "the evidence of the operation is there once: {}",
        ticket.notes
    );

    // A second run changes nothing: the attempt is idempotent.
    let again = run(&fixture, &tracker, &mut store).expect("the second attempt");
    assert!(!again.promoted, "the candidate is not promoted twice");
    assert!(!again.closed, "the ticket is not closed twice");
    assert_eq!(
        added_commits(&fixture),
        vec![fixture.candidate.clone()],
        "no second commit"
    );
    let ticket = ticket_of(&tracker);
    assert_eq!(
        ticket.notes.matches(&marker).count(),
        1,
        "the note of the operation is not duplicated"
    );
}

#[test]
fn a_crash_after_the_promotion_recovers_the_same_commit_and_closes_the_ticket() {
    let fixture = Fixture::new("crash-after-cas");
    let tracker = fixture.tracker();
    let mut store = fixture.store();

    // The crash: the tracker fails while the closure is applied. The
    // promotion is already a fact.
    *tracker.fail_apply_with.lock().expect("lock") = Some("the process died".into());
    let failed = run(&fixture, &tracker, &mut store);
    assert!(failed.is_err(), "the attempt died: {failed:?}");
    assert_eq!(
        added_commits(&fixture),
        vec![fixture.candidate.clone()],
        "the candidate was promoted before the crash"
    );
    assert_eq!(ticket_of(&tracker).status, "To Do");

    // The tracker comes back: the attempt resumes from what the branch
    // says, and it promotes nothing again.
    *tracker.fail_apply_with.lock().expect("lock") = None;
    let resumed = run(&fixture, &tracker, &mut store).expect("the resumed attempt");
    assert!(!resumed.promoted, "the candidate is not promoted again");
    assert!(
        resumed.closed,
        "the ticket is closed by the resumed attempt"
    );
    assert_eq!(
        resumed.commit.as_deref(),
        Some(fixture.candidate.as_str()),
        "the SAME commit is recovered"
    );
    assert_eq!(
        added_commits(&fixture),
        vec![fixture.candidate.clone()],
        "there is no second commit"
    );
    let ticket = ticket_of(&tracker);
    assert_eq!(ticket.status, "Done");
    let marker = fixture.intent().marker();
    assert_eq!(
        ticket.notes.matches(&marker).count(),
        1,
        "the note of the operation is there once"
    );
}

#[test]
fn a_crash_after_the_closure_does_not_duplicate_the_closure_nor_the_note() {
    let fixture = Fixture::new("crash-after-closure");
    let tracker = fixture.tracker();
    let mut store = fixture.store();

    // The crash: the attempt dies when it settles its record. The effects
    // are already there.
    let intent = fixture.intent();
    let evidence = fixture.evidence();
    {
        let mut journal = FaultyJournal::new(&mut store);
        journal.fail_on_settle = true;
        let died = execute(
            PromotionRequest {
                intent: &intent,
                evidence: &evidence,
                repo: &fixture.repo,
                bundle: &fixture.bundle,
                candidate_branch: "candidate",
            },
            &tracker,
            &mut journal,
        );
        assert!(died.is_err(), "the attempt died: {died:?}");
    }

    // The record was never written, but the effects are there: the attempt
    // reconciles by observing, and it duplicates nothing.
    let resumed = run(&fixture, &tracker, &mut store).expect("the resumed attempt");
    assert!(!resumed.promoted, "the candidate is not promoted again");
    assert!(!resumed.closed, "the ticket is not closed again");
    assert_eq!(
        added_commits(&fixture),
        vec![fixture.candidate.clone()],
        "there is no second commit"
    );
    let ticket = ticket_of(&tracker);
    assert_eq!(ticket.status, "Done");
    let marker = fixture.intent().marker();
    assert_eq!(
        ticket.notes.matches(&marker).count(),
        1,
        "the note of the operation is not duplicated"
    );
}

#[test]
fn a_branch_that_moved_is_neither_adopted_nor_overwritten() {
    let fixture = Fixture::new("foreign-ref");
    let tracker = fixture.tracker();
    let mut store = fixture.store();

    // A human commit lands on the managed branch after the intent was
    // written.
    git(&fixture.repo, &["checkout", "-q", BRANCH]);
    let human = commit(&fixture.repo, "human.txt", "the human change");
    git(&fixture.repo, &["checkout", "-q", "main"]);

    let report = run(&fixture, &tracker, &mut store).expect("the attempt");

    assert!(report.conflict.is_some(), "the attempt was refused");
    assert!(
        matches!(report.conflict, Some(Conflict::RefMoved { .. })),
        "the conflict names the branch that moved: {:?}",
        report.conflict
    );
    assert!(!report.promoted, "nothing was promoted");
    assert!(!report.closed, "the ticket was not closed");
    assert_eq!(ticket_of(&tracker).status, "To Do");
    assert_eq!(
        head_of(&fixture),
        human,
        "the change of the human is intact"
    );
    assert_eq!(
        added_commits(&fixture),
        vec![human],
        "the candidate of the attempt was not promoted"
    );
}

#[test]
fn an_intent_of_another_attempt_does_not_close_the_ticket() {
    let fixture = Fixture::new("stale-epoch");
    let tracker = fixture.tracker();
    let mut store = fixture.store();

    // The attempt is fenced out: the ticket was claimed again.
    store
        .transact(|state| Ok(state.release(TICKET, "hodei-mh")))
        .expect("the release");
    store
        .transact(|state| Ok(state.try_claim(TICKET, "another-run")))
        .expect("the claim");

    let report = run(&fixture, &tracker, &mut store).expect("the attempt");
    assert!(
        matches!(report.conflict, Some(Conflict::EpochStale { .. })),
        "the attempt is refused: {:?}",
        report.conflict
    );
    assert!(!report.promoted);
    assert!(
        !report.closed,
        "the ticket of another attempt is not closed"
    );
    assert_eq!(ticket_of(&tracker).status, "To Do");
}

#[test]
fn an_applied_promotion_is_never_un_applied_by_a_later_refusal() {
    let fixture = Fixture::new("applied-stays");
    let tracker = fixture.tracker();
    let mut store = fixture.store();
    let intent = fixture.intent();

    // The tracker fails: the promotion is a fact, the closure is not.
    *tracker.fail_apply_with.lock().expect("lock") = Some("the process died".into());
    let failed = run(&fixture, &tracker, &mut store);
    assert!(failed.is_err(), "the attempt died: {failed:?}");

    // The record says that the promotion happened.
    let outcome = {
        let journal = CoordinatorJournal::new(&mut store);
        let record = journal
            .load(&intent.operation_id)
            .expect("the record")
            .expect("a record of the attempt");
        record.outcome
    };
    assert!(
        outcome.as_ref().expect("an outcome").is_applied(),
        "the promotion is a fact even though the closure did not happen: {outcome:?}"
    );

    // A person moves the managed branch: the attempt is refused, and the
    // record does not un-apply what already happened.
    git(&fixture.repo, &["checkout", "-q", BRANCH]);
    let human = commit(&fixture.repo, "human.txt", "the human change");
    git(&fixture.repo, &["checkout", "-q", "main"]);
    let report = run(&fixture, &tracker, &mut store).expect("the attempt");
    assert!(
        matches!(report.conflict, Some(Conflict::RefMoved { .. })),
        "the attempt is refused: {:?}",
        report.conflict
    );

    let outcome = {
        let journal = CoordinatorJournal::new(&mut store);
        let record = journal
            .load(&intent.operation_id)
            .expect("the record")
            .expect("a record of the attempt");
        record.outcome
    };
    assert!(
        outcome.as_ref().expect("an outcome").is_applied(),
        "a later refusal does not un-apply the promotion: {outcome:?}"
    );
    assert_eq!(
        head_of(&fixture),
        human,
        "the change of the human is intact"
    );
}

#[test]
fn the_ticket_closed_by_somebody_else_keeps_its_closure_and_gains_the_evidence() {
    let fixture = Fixture::new("closed-by-other");
    let tracker = fixture.tracker();
    {
        let mut tickets = tracker.tickets.lock().expect("lock");
        tickets.insert(
            TICKET.into(),
            ("Done".into(), vec![1], vec!["the human note".into()]),
        );
    }
    let mut store = fixture.store();

    let report = run(&fixture, &tracker, &mut store).expect("the attempt");
    assert!(report.promoted, "the candidate is promoted");
    assert!(
        report.evidence_appended,
        "only the evidence is appended: {:?}",
        report
    );
    assert!(
        !report.closed,
        "the closure of the human is not written again"
    );

    let ticket = ticket_of(&tracker);
    assert_eq!(ticket.status, "Done");
    assert!(
        ticket.notes.contains("the human note"),
        "the note of the human is conserved"
    );
    let marker = fixture.intent().marker();
    assert_eq!(
        ticket.notes.matches(&marker).count(),
        1,
        "the evidence of the operation is there once"
    );
}
