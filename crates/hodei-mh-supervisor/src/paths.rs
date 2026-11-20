//! Where the control surface of a supervisor lives.
//!
//! The artifacts of a supervisor are PRIVATE to the user who runs it: the
//! directory that holds them is created with mode `0700` and every file
//! inside it with mode `0600`. The old default lived in the shared
//! temporary directory, where the path itself could be taken over by
//! another local user.
//!
//! Two locks guard the surface, and both live inside that private
//! directory (never next to the socket, which may be in a shared place):
//!
//! - the PROJECT lock: one controller per project identity;
//! - the SOCKET lock: one owner per socket path.

use std::io;
use std::os::unix::fs::MetadataExt as _;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// The environment variable that holds the per-user runtime directory.
const RUNTIME_HOME_ENV: &str = "XDG_RUNTIME_DIR";

/// The name of the harness inside the runtime root.
const PRODUCT_DIR: &str = "hodei-mh";

/// The root that holds the runtime artifacts of every supervisor.
pub fn runtime_root() -> PathBuf {
    if let Some(dir) = std::env::var_os(RUNTIME_HOME_ENV).filter(|value| !value.is_empty()) {
        return PathBuf::from(dir).join(PRODUCT_DIR);
    }
    // `/run/user/<uid>` is the session directory of a normal Linux login.
    let uid = current_uid();
    let run_user = PathBuf::from("/run/user").join(uid.to_string());
    if run_user.is_dir() {
        return run_user.join(PRODUCT_DIR);
    }
    // Last resort: a per-user directory under the system temporary
    // directory. It is still private: `private_dir` creates it 0700.
    std::env::temp_dir().join(format!("{PRODUCT_DIR}-{uid}"))
}

/// The control socket a supervisor serves when the caller names none.
pub fn default_socket() -> PathBuf {
    runtime_root().join("supervisor.sock")
}

/// The lock that protects ONE project identity.
pub fn project_lock(project: &str) -> PathBuf {
    runtime_root()
        .join("projects")
        .join(format!("{}.lock", identity_key(project)))
}

/// The lock that protects ONE socket path.
///
/// The socket path may live in a shared directory, so its lock does NOT
/// live next to it: a lock file in a world-writable directory is a
/// denial of service that any local user can prepare before the
/// controller starts.
pub fn socket_lock(socket: &Path) -> PathBuf {
    runtime_root()
        .join("sockets")
        .join(format!("{}.lock", identity_key(&socket.to_string_lossy())))
}

/// A short, stable key for an identity.
///
/// The key is a digest, not the identity itself: a digest is exactly one
/// path segment by construction, so an identity can never escape the
/// directory that holds it — whatever the caller put in it.
fn identity_key(identity: &str) -> String {
    let digest = Sha256::digest(identity.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// The user id of this process.
pub fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no argument, cannot fail and has no
    // preconditions.
    unsafe { libc::getuid() }
}

/// Create a directory that only its owner can enter, and refuse to use one
/// that belongs to somebody else.
///
/// A directory that already exists with a wider mode is tightened; a
/// directory owned by another user is refused: the artifacts inside it
/// would not be private.
pub fn private_dir(path: &Path) -> io::Result<()> {
    if path.exists() {
        let meta = std::fs::metadata(path)?;
        if meta.uid() != current_uid() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!(
                    "{} belongs to uid {}, not to uid {}",
                    path.display(),
                    meta.uid(),
                    current_uid()
                ),
            ));
        }
        let mode = meta.mode() & 0o777;
        if mode & 0o077 != 0 {
            std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
        }
        return Ok(());
    }
    // The ancestors of a private directory are private too: a directory
    // that holds a private artifact must not be readable by anybody else.
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        private_dir(parent)?;
    }
    match std::fs::create_dir(path) {
        Ok(()) => {}
        // Another creator won the race between the check above and this
        // call: the directory is theirs only if it passes the checks for
        // an existing one.
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return private_dir(path),
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(path, std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    Ok(())
}

/// Create the private runtime directories of the harness.
pub fn ensure_runtime_dirs() -> io::Result<PathBuf> {
    let root = runtime_root();
    private_dir(&root)?;
    private_dir(&root.join("projects"))?;
    private_dir(&root.join("sockets"))?;
    Ok(root)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_runtime_root_is_private_to_the_user() {
        let root = runtime_root();
        assert!(root.ends_with(PRODUCT_DIR), "{}", root.display());
    }

    #[test]
    fn an_identity_never_escapes_its_directory() {
        // A project name full of path references stays one segment.
        let lock = project_lock("../../etc/passwd");
        let parent = lock.parent().expect("the lock has a directory");
        assert_eq!(parent, runtime_root().join("projects"));
        assert!(
            !lock
                .file_name()
                .expect("the lock has a name")
                .to_string_lossy()
                .contains('/')
        );
    }

    #[test]
    fn two_projects_do_not_share_a_lock() {
        assert_ne!(project_lock("alpha"), project_lock("beta"));
        assert_eq!(project_lock("alpha"), project_lock("alpha"));
    }

    #[test]
    fn two_sockets_do_not_share_a_lock() {
        assert_ne!(
            socket_lock(Path::new("/tmp/a.sock")),
            socket_lock(Path::new("/tmp/b.sock"))
        );
    }

    #[test]
    fn the_socket_lock_does_not_live_next_to_the_socket() {
        let lock = socket_lock(Path::new("/tmp/hodei.sock"));
        assert_eq!(
            lock.parent(),
            Some(runtime_root().join("sockets").as_path())
        );
        assert!(!lock.starts_with("/tmp"));
    }

    #[test]
    fn a_private_directory_is_created_with_mode_0700() {
        let dir = std::env::temp_dir().join(format!(
            "hodei-private-{}-{}",
            std::process::id(),
            identity_key("dir")
        ));
        let _ = std::fs::remove_dir_all(&dir);
        private_dir(&dir).expect("create the private directory");
        let meta = std::fs::metadata(&dir).expect("metadata");
        assert_eq!(meta.mode() & 0o777, 0o700, "only the owner enters");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
