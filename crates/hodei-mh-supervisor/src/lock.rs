//! The exclusive lock that decides WHO owns a control surface.
//!
//! The old decision was made by asking whether a file with a given NAME
//! existed and whether somebody answered on it. Both questions are racy:
//! two controllers could both see "nobody there", and the second one
//! unlinked the socket the first one was serving. The result was TWO
//! controllers with two states and clients that disagreed.
//!
//! An advisory lock on a file is the atomic answer: the kernel decides
//! who gets it, and it releases it when the holder dies. The lock file
//! records the holder so a refusal can NAME it.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

/// Why a lock could not be taken.
#[derive(Debug, thiserror::Error)]
pub enum LockError {
    #[error("cannot open the lock {path}: {source}")]
    Open { path: String, source: io::Error },
    #[error("the lock {path} belongs to another user: refusing to use it")]
    NotOwner { path: String },
    #[error("another controller already holds {path}{holder}")]
    Held { path: String, holder: String },
}

/// An exclusive lock on an identity. It is held for as long as this
/// value lives: dropping it releases the lock.
#[derive(Debug)]
pub struct IdentityLock {
    file: File,
    path: PathBuf,
}

impl IdentityLock {
    /// Take the lock of `path` for `holder`.
    ///
    /// The lock file is private (`0600`) and is never followed when it is
    /// a symlink: a lock in a shared directory must not be something
    /// another local user can point somewhere else.
    pub fn acquire(path: &Path, holder: &str) -> Result<Self, LockError> {
        if let Some(parent) = path.parent() {
            crate::paths::private_dir(parent).map_err(|source| LockError::Open {
                path: path.display().to_string(),
                source,
            })?;
        }
        let file = OpenOptions::new()
            .write(true)
            .read(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)
            .map_err(|source| LockError::Open {
                path: path.display().to_string(),
                source,
            })?;
        let meta = file.metadata().map_err(|source| LockError::Open {
            path: path.display().to_string(),
            source,
        })?;
        if meta.uid() != current_uid() {
            return Err(LockError::NotOwner {
                path: path.display().to_string(),
            });
        }
        // Tighten the mode: a lock file that others can read tells them
        // which controller is running.
        let _ = file.set_permissions(PermissionsExt::from_mode(0o600));
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => {
                return Err(LockError::Held {
                    path: path.display().to_string(),
                    holder: holder_of(path),
                });
            }
            Err(std::fs::TryLockError::Error(source)) => {
                return Err(LockError::Open {
                    path: path.display().to_string(),
                    source,
                });
            }
        }
        let lock = IdentityLock {
            file,
            path: path.to_path_buf(),
        };
        lock.record(holder);
        Ok(lock)
    }

    /// The path of the lock this value holds.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Write WHO holds the lock. The record is a convenience for the
    /// operator: the EXCLUSION is the kernel's lock, not this text.
    fn record(&self, holder: &str) {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs())
            .unwrap_or_default();
        let record = format!(
            "holder={holder}\npid={}\nstarted={started}\n",
            std::process::id()
        );
        let mut file = &self.file;
        let _ = file.set_len(0);
        let _ = file.write_all(record.as_bytes());
        let _ = file.flush();
    }
}

/// Who holds the lock at `path`, as far as the file says. Empty when the
/// file says nothing: the exclusion never depends on this text.
pub fn holder_of(path: &Path) -> String {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return String::new();
    };
    let holder = contents
        .lines()
        .find_map(|line| line.strip_prefix("holder="))
        .unwrap_or_default()
        .trim();
    if holder.is_empty() {
        String::new()
    } else {
        format!(" (held by {holder})")
    }
}

/// The user id of this process.
fn current_uid() -> u32 {
    // SAFETY: `getuid` takes no argument, cannot fail and has no
    // preconditions.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Remove the private directory of a lock.
    fn cleanup(path: &Path) {
        if let Some(dir) = path.parent() {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    /// A lock inside a PRIVATE directory: the locks of the product live
    /// in the private runtime directory, never in a shared one.
    fn lock_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hodei-lock-dir-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        crate::paths::private_dir(&dir).expect("the private directory of the lock");
        dir.join("identity.lock")
    }

    #[test]
    fn a_second_holder_of_the_same_lock_is_refused() {
        let path = lock_path("exclusive");
        cleanup(&path);
        let first = IdentityLock::acquire(&path, "project alpha").expect("first lock");
        let second = IdentityLock::acquire(&path, "project beta");
        match second {
            Err(LockError::Held { holder, .. }) => {
                assert!(holder.contains("project alpha"), "{holder}");
            }
            other => panic!("the second holder must be refused, got {other:?}"),
        }
        drop(first);
        cleanup(&path);
    }

    #[test]
    fn a_released_lock_can_be_taken_again() {
        let path = lock_path("released");
        cleanup(&path);
        {
            let _first = IdentityLock::acquire(&path, "first").expect("first lock");
        }
        let _second = IdentityLock::acquire(&path, "second").expect("the lock is free again");
        cleanup(&path);
    }

    #[test]
    fn the_lock_file_is_private() {
        let path = lock_path("private");
        cleanup(&path);
        let lock = IdentityLock::acquire(&path, "holder").expect("lock");
        let mode = std::fs::metadata(lock.path()).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o600, "only the owner reads the lock");
        drop(lock);
        cleanup(&path);
    }
}
