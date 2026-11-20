//! The reusable cleanup: a resource that is not released disappears from
//! the record, and a residue nobody knows about is a residue nobody
//! reconciles.
//!
//! Every cleanup is a GUARD: it is confirmed when the resource is really
//! gone, and REGISTERED when it is not. The registration is eager (a
//! failure is registered as soon as it happens) and it also happens when
//! the guard is dropped without a confirmation: a cancelled operation
//! never leaves an unrecorded residue.
//!
//! The ledger is the input of a reconciliation: it says what is still
//! there and why it was not released. A productive path opens it from its
//! private journal BEFORE creating a sandbox, releases what it can and
//! keeps — journaled — what it cannot.
//!
//! Only a SANDBOX is a residue of the host. An execd session lives inside
//! a sandbox and dies with it, so it is never a reconciliation target: a
//! ledger of sessions could never become empty.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use thiserror::Error;

use crate::control::ControlPlane;

/// A residue: a sandbox whose cleanup was not confirmed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingCleanup {
    /// The sandbox that is still there.
    pub sandbox_id: String,
    /// Why it was not released.
    pub reason: String,
}

impl PendingCleanup {
    /// The residue as one line of text: what it is and why it is there.
    pub fn describe(&self) -> String {
        format!("sandbox {} ({})", self.sandbox_id, self.reason)
    }
}

/// The ledger of the residues: what a reconciliation has to work on.
///
/// A registry without a journal keeps the residues in memory (the
/// transport of one operation). A registry opened from a journal (`durable`)
/// loads what a previous process registered and flushes every change, so
/// the ledger survives the process that wrote it.
#[derive(Debug, Clone, Default)]
pub struct CleanupRegistry {
    ledger: Arc<Mutex<Ledger>>,
}

/// The state behind the ledger: the residues and where they are flushed.
#[derive(Debug, Default)]
struct Ledger {
    entries: Vec<PendingCleanup>,
    journal: Option<PathBuf>,
}

impl CleanupRegistry {
    /// An empty, in-memory ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// A ledger backed by the private journal at `journal`: the residues
    /// a previous process wrote are loaded, and every registration and
    /// reconciliation is flushed, so nothing is lost when the process
    /// exits.
    pub fn durable(journal: impl Into<PathBuf>) -> Self {
        let journal = journal.into();
        let entries = read_journal(&journal);
        Self {
            ledger: Arc::new(Mutex::new(Ledger {
                entries,
                journal: Some(journal),
            })),
        }
    }

    /// Where the productive paths keep the ledger of the residues.
    pub fn default_journal_path() -> PathBuf {
        let state_home = std::env::var_os("XDG_STATE_HOME").map(PathBuf::from);
        let base = state_home
            .or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| PathBuf::from(home).join(".local").join("state"))
            })
            .unwrap_or_else(std::env::temp_dir);
        base.join("hodei-mh").join("cleanup-journal.json")
    }

    /// The ledger behind the registry. A poisoned lock is RECOVERED: the
    /// residues of the operation are worth more than the panic that
    /// poisoned the ledger.
    fn ledger(&self) -> MutexGuard<'_, Ledger> {
        self.ledger.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Register a residue. Registering the same sandbox twice does not
    /// duplicate it: the ledger holds one entry per sandbox with the
    /// LATEST reason.
    pub fn register(&self, pending: PendingCleanup) {
        let mut ledger = self.ledger();
        match ledger
            .entries
            .iter_mut()
            .find(|entry| entry.sandbox_id == pending.sandbox_id)
        {
            Some(entry) => entry.reason = pending.reason,
            None => ledger.entries.push(pending),
        }
        flush(&ledger);
    }

    /// The residues that are still registered.
    pub fn pending(&self) -> Vec<PendingCleanup> {
        self.ledger().entries.clone()
    }

    /// Whether every registered resource was released.
    pub fn is_empty(&self) -> bool {
        self.ledger().entries.is_empty()
    }

    /// Try to release every registered sandbox: the ones that cannot be
    /// released stay registered — and journaled — and are returned.
    pub async fn reconcile_sandboxes(&self, client: &ControlPlane) -> Vec<PendingCleanup> {
        let mut remaining = Vec::new();
        for pending in self.pending() {
            let mut cleanup = CleanupGuard::sandbox(pending.sandbox_id.clone(), self.clone());
            cleanup.fail(pending.reason.clone());
            if cleanup_sandbox(client, &mut cleanup).await.is_ok() {
                continue;
            }
            remaining.push(cleanup.pending());
        }
        self.replace(&remaining);
        remaining
    }

    /// Replace the ledger with the residues that are still there.
    fn replace(&self, entries: &[PendingCleanup]) {
        let mut ledger = self.ledger();
        ledger.entries = entries.to_vec();
        flush(&ledger);
    }
}

impl Drop for CleanupRegistry {
    fn drop(&mut self) {
        // The residues the operation still holds are flushed when the
        // ledger goes away: a process that exits without a reconciliation
        // does not lose them.
        flush(&self.ledger());
    }
}

/// The journal of the residues: a private JSON file.
///
/// It is written atomically (a temporary file renamed over the journal),
/// so a crash never leaves a half-written ledger: the reconciliation
/// would have nothing to read.
fn write_journal(journal: &Path, entries: &[PendingCleanup]) -> std::io::Result<()> {
    if let Some(parent) = journal.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::to_vec_pretty(entries)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let temporary = journal.with_extension("json.tmp");
    std::fs::write(&temporary, body)?;
    std::fs::rename(&temporary, journal)
}

/// The residues a journal carries.
///
/// A journal that cannot be read is not a ledger this registry can trust:
/// it starts empty and the next registration rewrites it. The failure is
/// reported, never hidden.
fn read_journal(journal: &Path) -> Vec<PendingCleanup> {
    let bytes = match std::fs::read(journal) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            tracing::warn!(
                journal = %journal.display(),
                %error,
                "the cleanup journal could not be read: the ledger starts empty"
            );
            return Vec::new();
        }
    };
    match serde_json::from_slice(&bytes) {
        Ok(entries) => entries,
        Err(error) => {
            tracing::warn!(
                journal = %journal.display(),
                %error,
                "the cleanup journal is not a ledger: it starts empty and is rewritten"
            );
            Vec::new()
        }
    }
}

/// Flush the ledger to its journal. A ledger that cannot be flushed is
/// reported; the residue is still in memory, so the operation does not
/// pretend it is gone.
fn flush(ledger: &Ledger) {
    let Some(journal) = &ledger.journal else {
        return;
    };
    if let Err(error) = write_journal(journal, &ledger.entries) {
        tracing::warn!(
            journal = %journal.display(),
            %error,
            "the cleanup journal could not be written"
        );
    }
}

/// A cleanup in progress.
///
/// The guard is confirmed when the resource is gone. While it is not
/// confirmed, dropping it registers the residue: an operation that was
/// cancelled never leaves a resource behind without a record.
#[derive(Debug)]
pub struct CleanupGuard {
    sandbox_id: String,
    registry: CleanupRegistry,
    reason: Option<String>,
    confirmed: bool,
    registered: bool,
}

impl CleanupGuard {
    /// The guard of a sandbox of the control plane.
    pub fn sandbox(id: impl Into<String>, registry: CleanupRegistry) -> Self {
        Self {
            sandbox_id: id.into(),
            registry,
            reason: None,
            confirmed: false,
            registered: false,
        }
    }

    /// The residue this guard would register.
    pub fn pending(&self) -> PendingCleanup {
        PendingCleanup {
            sandbox_id: self.sandbox_id.clone(),
            reason: self
                .reason
                .clone()
                .unwrap_or_else(|| "the cleanup was not confirmed".to_string()),
        }
    }

    /// The resource this guard watches.
    pub fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    /// The resource is gone: there is no residue.
    pub fn confirm(&mut self) {
        self.confirmed = true;
        self.reason = None;
    }

    /// The cleanup failed: the residue is registered with its reason, so
    /// it is reconciled even if the guard is never dropped.
    pub fn fail(&mut self, reason: impl Into<String>) {
        self.reason = Some(reason.into());
        self.registered = true;
        let pending = self.pending();
        self.registry.register(pending);
    }
}

impl Drop for CleanupGuard {
    fn drop(&mut self) {
        if self.confirmed || self.registered {
            return;
        }
        // The guard was dropped without a confirmation: the operation was
        // cancelled or it returned early. The residue is registered.
        let pending = self.pending();
        self.registry.register(pending);
    }
}

/// The failure of a cleanup.
#[derive(Debug, Error)]
pub enum CleanupError {
    #[error("the control plane refused the deletion: {0}")]
    Refused(String),
    #[error("the control plane could not be reached: {0}")]
    Unreachable(String),
}

/// Release a sandbox and keep the residue when the deletion is not
/// confirmed.
pub async fn cleanup_sandbox(
    client: &ControlPlane,
    guard: &mut CleanupGuard,
) -> Result<(), CleanupError> {
    let id = guard.sandbox_id().to_string();
    match client.delete(&ControlPlane::sandbox_path(&id)).send().await {
        Ok(response) if response.status().is_success() => {
            guard.confirm();
            Ok(())
        }
        Ok(response) => {
            let reason = format!("the control plane answered {}", response.status());
            guard.fail(reason.clone());
            Err(CleanupError::Refused(reason))
        }
        Err(error) => {
            let reason = error.to_string();
            guard.fail(reason.clone());
            Err(CleanupError::Unreachable(reason))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A local HTTP server that answers every request with `status` and
    /// records the request line, one request per connection.
    async fn control_plane_answering(status: &'static str) -> (String, Arc<Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the listener binds");
        let addr = listener.local_addr().expect("the listener has an address");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = seen.clone();
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                let recorded = recorded.clone();
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let read = socket.read(&mut buffer).await.unwrap_or(0);
                    let request = String::from_utf8_lossy(&buffer[..read]).to_string();
                    let line = request.lines().next().unwrap_or_default().to_string();
                    recorded
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(line);
                    let response = format!(
                        "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), seen)
    }

    /// A factory of journals inside the temporary directory.
    fn journal_of(name: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("hodei-mh-cleanup-{}-{name}", std::process::id()))
            .join("cleanup-journal.json")
    }

    fn clear_journal(journal: &Path) {
        let _ = std::fs::remove_dir_all(journal.parent().unwrap_or(journal));
    }

    #[tokio::test]
    async fn a_failed_cleanup_stays_registered_for_reconciliation() {
        let registry = CleanupRegistry::new();
        let (origin, _) = control_plane_answering("500 Internal Server Error").await;
        let control = ControlPlane::new(origin);
        let mut guard = CleanupGuard::sandbox("sbx-1", registry.clone());
        let error = cleanup_sandbox(&control, &mut guard)
            .await
            .expect_err("the deletion is refused");
        assert!(matches!(error, CleanupError::Refused(_)), "{error}");
        let pending = registry.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sandbox_id, "sbx-1");
    }

    #[tokio::test]
    async fn a_cancelled_cleanup_stays_registered_for_reconciliation() {
        // The operation was cancelled: the guard was dropped without
        // ever confirming the deletion. The residue must be visible.
        let registry = CleanupRegistry::new();
        drop(CleanupGuard::sandbox("sbx-2", registry.clone()));
        let pending = registry.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sandbox_id, "sbx-2");
    }

    #[tokio::test]
    async fn a_confirmed_cleanup_leaves_no_residue() {
        let registry = CleanupRegistry::new();
        let mut guard = CleanupGuard::sandbox("sbx-3", registry.clone());
        guard.confirm();
        drop(guard);
        assert!(registry.is_empty());
    }

    #[tokio::test]
    async fn the_reconciliation_releases_what_it_can_and_keeps_the_rest() {
        let registry = CleanupRegistry::new();
        registry.register(PendingCleanup {
            sandbox_id: "sbx-4".to_string(),
            reason: "the control plane was unreachable".to_string(),
        });
        let (origin, _) = control_plane_answering("500 Internal Server Error").await;
        let control = ControlPlane::new(origin);
        let remaining = registry.reconcile_sandboxes(&control).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(registry.pending().len(), 1);
    }

    #[tokio::test]
    async fn the_reconciliation_empties_the_ledger_when_the_sandbox_is_gone() {
        // A ledger that can never become empty is a dead end: a released
        // sandbox leaves the ledger, and the release is asked for.
        let registry = CleanupRegistry::new();
        registry.register(PendingCleanup {
            sandbox_id: "sbx-5".to_string(),
            reason: "the control plane was unreachable".to_string(),
        });
        let (origin, requests) = control_plane_answering("200 OK").await;
        let control = ControlPlane::new(origin);
        let remaining = registry.reconcile_sandboxes(&control).await;
        assert!(remaining.is_empty(), "{remaining:?}");
        assert!(
            registry.is_empty(),
            "the released sandbox leaves the ledger"
        );
        let requests = requests.lock().unwrap_or_else(PoisonError::into_inner);
        assert!(
            requests
                .iter()
                .any(|line| line == "DELETE /v1/sandboxes/sbx-5 HTTP/1.1"),
            "the reconciliation asked for the deletion: {requests:?}"
        );
    }

    #[tokio::test]
    async fn a_residue_survives_the_process_that_registered_it() {
        // The ledger is durable: the residues of a process that exited are
        // loaded by the next one, which is what makes a reconciliation
        // possible at all.
        let journal = journal_of("survives");
        clear_journal(&journal);
        drop(CleanupGuard::sandbox(
            "sbx-6",
            CleanupRegistry::durable(&journal),
        ));
        let next = CleanupRegistry::durable(&journal);
        let pending = next.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].sandbox_id, "sbx-6");
        assert!(
            next.pending()[0].reason.contains("not confirmed"),
            "{pending:?}"
        );
        // The ledger is flushed when it goes away: it must be dropped
        // before its journal is removed.
        drop(next);
        clear_journal(&journal);
    }

    #[tokio::test]
    async fn a_released_residue_disappears_from_the_journal_after_reconciliation() {
        let journal = journal_of("released");
        clear_journal(&journal);
        let registry = CleanupRegistry::durable(&journal);
        registry.register(PendingCleanup {
            sandbox_id: "sbx-7".to_string(),
            reason: "the control plane was unreachable".to_string(),
        });
        let (origin, _) = control_plane_answering("200 OK").await;
        let control = ControlPlane::new(origin);
        registry.reconcile_sandboxes(&control).await;
        assert!(CleanupRegistry::durable(&journal).is_empty());
        drop(registry);
        clear_journal(&journal);
    }

    #[test]
    fn a_poisoned_ledger_still_records_the_residue() {
        // A panic while the ledger was held must not make the residues
        // unreachable: the lock is recovered, not re-panicked on.
        let registry = CleanupRegistry::new();
        let poisoned = registry.clone();
        let _ = std::thread::spawn(move || {
            let _held = poisoned.ledger();
            panic!("the owner of the ledger died");
        })
        .join();
        registry.register(PendingCleanup {
            sandbox_id: "sbx-8".to_string(),
            reason: "registered after the poison".to_string(),
        });
        assert_eq!(registry.pending().len(), 1);
        assert!(!registry.is_empty());
    }
}
