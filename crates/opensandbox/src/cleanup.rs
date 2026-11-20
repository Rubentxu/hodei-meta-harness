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
//! there and why it was not released.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use thiserror::Error;

use crate::control::ControlPlane;
use crate::execd::client::ExecdClient;

/// What a cleanup that is not confirmed refers to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CleanupTarget {
    /// A sandbox of the control plane.
    Sandbox { id: String },
    /// A session of an execd.
    ExecdSession {
        endpoint: String,
        session_id: String,
    },
}

impl CleanupTarget {
    /// A human-readable identity of the resource: what a reconciliation
    /// needs to find it again.
    pub fn describe(&self) -> String {
        match self {
            CleanupTarget::Sandbox { id } => format!("sandbox {id}"),
            CleanupTarget::ExecdSession {
                endpoint,
                session_id,
            } => format!("execd session {session_id} at {endpoint}"),
        }
    }
}

/// A residue: a resource whose cleanup was not confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingCleanup {
    /// The resource that is still there.
    pub target: CleanupTarget,
    /// Why it was not released.
    pub reason: String,
}

impl PendingCleanup {
    /// The residue as one line of text: what it is and why it is there.
    pub fn describe(&self) -> String {
        format!("{} ({})", self.target.describe(), self.reason)
    }
}

/// The ledger of the residues: what a reconciliation has to work on.
#[derive(Debug, Clone, Default)]
pub struct CleanupRegistry {
    entries: Arc<Mutex<Vec<PendingCleanup>>>,
}

impl CleanupRegistry {
    /// An empty ledger.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a residue. Registering the same residue twice does not
    /// duplicate it: the ledger holds one entry per resource.
    pub fn register(&self, pending: PendingCleanup) {
        let mut entries = self.entries.lock().expect("the ledger is writable");
        if entries.iter().any(|entry| entry.target == pending.target) {
            return;
        }
        entries.push(pending);
    }

    /// The residues that are still registered.
    pub fn pending(&self) -> Vec<PendingCleanup> {
        let entries = self.entries.lock().expect("the ledger is readable");
        entries.clone()
    }

    /// Whether every registered resource was released.
    pub fn is_empty(&self) -> bool {
        self.pending().is_empty()
    }

    /// Try to release every registered sandbox: the ones that cannot be
    /// released stay registered, and are returned.
    pub async fn reconcile_sandboxes(&self, client: &ControlPlane) -> Vec<PendingCleanup> {
        let mut remaining = Vec::new();
        for pending in self.pending() {
            let CleanupTarget::Sandbox { id } = &pending.target else {
                remaining.push(pending);
                continue;
            };
            let mut cleanup = CleanupGuard::sandbox(id.clone(), self.clone());
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
        let mut ledger = self.entries.lock().expect("the ledger is writable");
        *ledger = entries.to_vec();
    }
}

/// A cleanup in progress.
///
/// The guard is confirmed when the resource is gone. While it is not
/// confirmed, dropping it registers the residue: an operation that was
/// cancelled never leaves a resource behind without a record.
#[derive(Debug)]
pub struct CleanupGuard {
    target: CleanupTarget,
    registry: CleanupRegistry,
    reason: Option<String>,
    confirmed: bool,
    registered: bool,
}

impl CleanupGuard {
    /// The guard of a sandbox of the control plane.
    pub fn sandbox(id: impl Into<String>, registry: CleanupRegistry) -> Self {
        Self::new(CleanupTarget::Sandbox { id: id.into() }, registry)
    }

    /// The guard of a session of an execd.
    pub fn execd_session(
        endpoint: impl Into<String>,
        session_id: impl Into<String>,
        registry: CleanupRegistry,
    ) -> Self {
        Self::new(
            CleanupTarget::ExecdSession {
                endpoint: endpoint.into(),
                session_id: session_id.into(),
            },
            registry,
        )
    }

    fn new(target: CleanupTarget, registry: CleanupRegistry) -> Self {
        Self {
            target,
            registry,
            reason: None,
            confirmed: false,
            registered: false,
        }
    }

    /// The residue this guard would register.
    pub fn pending(&self) -> PendingCleanup {
        PendingCleanup {
            target: self.target.clone(),
            reason: self
                .reason
                .clone()
                .unwrap_or_else(|| "the cleanup was not confirmed".to_string()),
        }
    }

    /// The resource this guard watches, when it is a sandbox.
    pub fn sandbox_id(&self) -> Option<&str> {
        match &self.target {
            CleanupTarget::Sandbox { id } => Some(id),
            CleanupTarget::ExecdSession { .. } => None,
        }
    }

    /// The session this guard watches, when it is an execd session.
    pub fn session_id(&self) -> Option<&str> {
        match &self.target {
            CleanupTarget::ExecdSession { session_id, .. } => Some(session_id),
            CleanupTarget::Sandbox { .. } => None,
        }
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
    #[error("the execd refused the deletion: {0}")]
    SessionRefused(String),
    #[error("the execd could not be reached: {0}")]
    SessionUnreachable(String),
    #[error("the guard does not watch a sandbox")]
    NotASandbox,
    #[error("the guard does not watch a session")]
    NotASession,
}

/// Release a sandbox and keep the residue when the deletion is not
/// confirmed.
pub async fn cleanup_sandbox(
    client: &ControlPlane,
    guard: &mut CleanupGuard,
) -> Result<(), CleanupError> {
    let Some(id) = guard.sandbox_id().map(str::to_string) else {
        return Err(CleanupError::NotASandbox);
    };
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

/// Release an execd session and keep the residue when the deletion is
/// not confirmed. The deletion is bounded by `budget`: a cleanup that
/// hangs does not hang the operation that asked for it.
pub async fn cleanup_session(
    client: &ExecdClient,
    guard: &mut CleanupGuard,
    budget: Duration,
) -> Result<(), CleanupError> {
    let Some(session_id) = guard.session_id().map(str::to_string) else {
        return Err(CleanupError::NotASession);
    };
    match client.delete_session(&session_id, budget).await {
        Ok(()) => {
            guard.confirm();
            Ok(())
        }
        Err(error) => {
            let reason = error.to_string();
            guard.fail(reason.clone());
            Err(CleanupError::SessionUnreachable(reason))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A control plane that refuses every deletion.
    async fn refusing_control_plane() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the listener binds");
        let addr = listener.local_addr().expect("the listener has an address");
        tokio::spawn(async move {
            while let Ok((mut socket, _)) = listener.accept().await {
                tokio::spawn(async move {
                    let mut buffer = [0u8; 1024];
                    let _ = socket.read(&mut buffer).await;
                    let response = "HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn a_failed_cleanup_stays_registered_for_reconciliation() {
        let registry = CleanupRegistry::new();
        let control = ControlPlane::new(refusing_control_plane().await);
        let mut guard = CleanupGuard::sandbox("sbx-1", registry.clone());
        let error = cleanup_sandbox(&control, &mut guard)
            .await
            .expect_err("the deletion is refused");
        assert!(matches!(error, CleanupError::Refused(_)), "{error}");
        let pending = registry.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].target,
            CleanupTarget::Sandbox {
                id: "sbx-1".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_cancelled_cleanup_stays_registered_for_reconciliation() {
        // The operation was cancelled: the guard was dropped without
        // ever confirming the deletion. The residue must be visible.
        let registry = CleanupRegistry::new();
        drop(CleanupGuard::sandbox("sbx-2", registry.clone()));
        let pending = registry.pending();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].target,
            CleanupTarget::Sandbox {
                id: "sbx-2".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_confirmed_cleanup_leaves_no_residue() {
        let registry = CleanupRegistry::new();
        let control = ControlPlane::new(refusing_control_plane().await);
        let mut guard = CleanupGuard::sandbox("sbx-3", registry.clone());
        guard.confirm();
        drop(guard);
        assert!(registry.is_empty());
        let _ = control;
    }

    #[tokio::test]
    async fn the_reconciliation_releases_what_it_can_and_keeps_the_rest() {
        let registry = CleanupRegistry::new();
        registry.register(PendingCleanup {
            target: CleanupTarget::Sandbox {
                id: "sbx-4".to_string(),
            },
            reason: "the control plane was unreachable".to_string(),
        });
        let control = ControlPlane::new(refusing_control_plane().await);
        let remaining = registry.reconcile_sandboxes(&control).await;
        assert_eq!(remaining.len(), 1);
        assert_eq!(registry.pending().len(), 1);
    }
}
