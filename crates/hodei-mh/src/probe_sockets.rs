//! The supervisor socket files a probe creates, and their removal.
//!
//! A probe that starts one supervisor per project names a socket file for
//! each of them. The supervisor unlinks its OWN socket only when its serve
//! loop ends normally; a probe that KILLS the supervisor leaves the file
//! behind, and every run of the probe then adds more orphans to the host.
//!
//! The probe owns the files it names, so the probe removes them: the
//! cleanup here touches EXACTLY the paths the probe registered, never a
//! file it did not create, and it reports what it could not remove.

use std::io;
use std::path::{Path, PathBuf};

/// The prefix of every supervisor socket a multiproject probe names.
const SOCKET_PREFIX: &str = "hodei-sup-mproj-";

/// The suffix of every supervisor socket a multiproject probe names.
const SOCKET_SUFFIX: &str = ".sock";

/// What happened to one socket file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Removal {
    /// The file was there and is gone now.
    Removed(PathBuf),
    /// There was nothing to remove: the file was already gone.
    AlreadyGone(PathBuf),
    /// The file is still there: the removal failed.
    Failed {
        /// The file that survived.
        path: PathBuf,
        /// Why the removal failed.
        kind: io::ErrorKind,
    },
}

/// The result of removing every socket file a probe registered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Cleanup {
    outcomes: Vec<Removal>,
}

impl Cleanup {
    /// What happened to each file, in registration order.
    pub fn outcomes(&self) -> &[Removal] {
        &self.outcomes
    }

    /// How many files were removed by this cleanup.
    pub fn removed(&self) -> usize {
        self.outcomes
            .iter()
            .filter(|outcome| matches!(outcome, Removal::Removed(_)))
            .count()
    }

    /// The files that are still on disk after the cleanup.
    pub fn leftovers(&self) -> Vec<&Path> {
        self.outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                Removal::Failed { path, .. } => Some(path.as_path()),
                _ => None,
            })
            .collect()
    }

    /// True when no registered file survived the cleanup.
    pub fn is_complete(&self) -> bool {
        self.leftovers().is_empty()
    }
}

/// The socket files a probe registered: the files it is responsible for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SocketFiles {
    paths: Vec<PathBuf>,
}

impl SocketFiles {
    /// A registry with no files yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a socket file the probe named for a supervisor.
    pub fn track(&mut self, path: impl Into<PathBuf>) {
        let path = path.into();
        if !self.paths.contains(&path) {
            self.paths.push(path);
        }
    }

    /// The registered files.
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Remove every registered file and forget them.
    ///
    /// A file that is already gone is not a failure: the cleanup is
    /// idempotent, so the supervisor's own removal (the normal exit path)
    /// and the probe's removal can happen in either order.
    pub fn remove_all(&mut self) -> Cleanup {
        let outcomes: Vec<Removal> = self.paths.iter().map(|path| remove_one(path)).collect();
        self.paths.clear();
        Cleanup { outcomes }
    }
}

impl Drop for SocketFiles {
    /// The safety net: a probe that dies before its cleanup still removes
    /// the files it registered. Best effort by nature — a `Drop` cannot
    /// report — which is why the probe ALSO removes them explicitly and
    /// reports the outcome.
    fn drop(&mut self) {
        let _ = self.remove_all();
    }
}

/// The socket file a probe names for the supervisor of `project`.
pub fn supervisor_socket_path(run_id: u32, project: &str) -> PathBuf {
    PathBuf::from(format!(
        "/tmp/{SOCKET_PREFIX}{run_id}-{project}{SOCKET_SUFFIX}"
    ))
}

/// The supervisor socket files that live inside `dir`.
pub fn sockets_in(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with(SOCKET_PREFIX) && name.ends_with(SOCKET_SUFFIX)
                })
        })
        .collect();
    found.sort();
    found
}

/// Remove one file and report what happened.
fn remove_one(path: &Path) -> Removal {
    match std::fs::remove_file(path) {
        Ok(()) => Removal::Removed(path.to_path_buf()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Removal::AlreadyGone(path.to_path_buf())
        }
        Err(error) => Removal::Failed {
            path: path.to_path_buf(),
            kind: error.kind(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A private directory for one test: the test owns it and removes it.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hodei-probe-sockets-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create the scratch directory");
        dir
    }

    #[test]
    fn the_cleanup_removes_the_registered_files_and_nothing_else() {
        let dir = scratch("only-registered");
        let mine = dir.join("hodei-sup-mproj-1-alpha.sock");
        let foreign = dir.join("hodei-sup-mproj-9-omega.sock");
        std::fs::write(&mine, b"").expect("write the registered socket");
        std::fs::write(&foreign, b"").expect("write the foreign socket");

        let mut sockets = SocketFiles::new();
        sockets.track(&mine);
        let cleanup = sockets.remove_all();

        assert_eq!(cleanup.removed(), 1);
        assert!(cleanup.is_complete());
        assert!(!mine.exists(), "the registered socket must be gone");
        assert!(
            foreign.exists(),
            "a socket the probe did not create must survive the cleanup"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_cleanup_is_idempotent() {
        let dir = scratch("idempotent");
        let socket = dir.join("hodei-sup-mproj-1-beta.sock");
        std::fs::write(&socket, b"").expect("write the socket");

        let mut sockets = SocketFiles::new();
        sockets.track(&socket);
        let first = sockets.remove_all();
        let second = sockets.remove_all();

        assert_eq!(first.removed(), 1);
        assert!(
            second.outcomes().is_empty(),
            "a second cleanup has nothing left to do"
        );
        assert!(!socket.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_that_cannot_be_removed_is_reported_not_swallowed() {
        let dir = scratch("not-removable");
        // A directory cannot be unlinked as a file: the removal fails and
        // the failure has to be visible, not silently ignored.
        let socket = dir.join("hodei-sup-mproj-1-gamma.sock");
        std::fs::create_dir_all(&socket).expect("create the undeletable path");

        let mut sockets = SocketFiles::new();
        sockets.track(&socket);
        let cleanup = sockets.remove_all();

        assert_eq!(cleanup.removed(), 0);
        assert!(!cleanup.is_complete());
        assert_eq!(cleanup.leftovers(), vec![socket.as_path()]);
        assert!(matches!(
            cleanup.outcomes().first(),
            Some(Removal::Failed { kind, .. }) if *kind == io::ErrorKind::IsADirectory
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_run_leaves_no_supervisor_socket_behind() {
        let dir = scratch("no-leftovers");
        // The socket of an EARLIER run: it is not the probe's business.
        let earlier = dir.join("hodei-sup-mproj-111-zeta.sock");
        std::fs::write(&earlier, b"").expect("write the earlier socket");
        let before = sockets_in(&dir).len();

        let mut sockets = SocketFiles::new();
        for project in ["alpha", "beta", "gamma", "delta"] {
            let path = dir.join(format!("hodei-sup-mproj-222-{project}.sock"));
            std::fs::write(&path, b"").expect("write the socket of the run");
            sockets.track(&path);
        }
        assert_eq!(
            sockets_in(&dir).len(),
            before + 4,
            "the run created four supervisor sockets"
        );

        let cleanup = sockets.remove_all();

        assert!(cleanup.is_complete());
        assert_eq!(
            sockets_in(&dir).len(),
            before,
            "the number of supervisor sockets must be the same before and after the run"
        );
        assert!(earlier.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_named_socket_follows_the_documented_pattern() {
        let path = supervisor_socket_path(4242, "alpha");
        assert_eq!(path, PathBuf::from("/tmp/hodei-sup-mproj-4242-alpha.sock"));
        let dir = scratch("pattern");
        let named = dir.join(path.file_name().expect("the file name"));
        std::fs::write(&named, b"").expect("write the named socket");
        assert_eq!(sockets_in(&dir), vec![named]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
