//! Workspace export: the explicit list of files that leaves the host.
//!
//! Three owners, three responsibilities:
//!
//! - `plan` owns the RULES: which paths must never leave the host.
//! - `git` owns the CLASSIFICATION: which files are versioned and which
//!   carry unversioned changes.
//! - this module owns the PACKING: the bytes that travel to the sandbox
//!   and the honest report of what travelled and what did not.
//!
//! The export is never a copy of the working tree. The host sends the
//! files the plan allows, and the report says which ones were denied and
//! why — an export that cannot show its refusals cannot claim to be
//! secretless.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use flate2::Compression;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use tar::{Builder, EntryType, Header};
use thiserror::Error;

use crate::git::{self, GitError};
use crate::plan::{DeniedFile, EXPORT_DENYLIST, matches_rule, select_files_for_export};

pub use crate::plan::ExportPolicy;

/// The name of the packed export inside its private directory.
const ARCHIVE_NAME: &str = "project.tgz";

/// How deep the tree walk descends. A project deeper than this is not a
/// project; the walk stops instead of running out of the stack.
const MAX_WALK_DEPTH: usize = 32;

/// The rule recorded for a path that exists but cannot be packed: a
/// directory (a gitlink, a submodule), a device, a socket.
const RULE_NOT_A_FILE: &str = "not-a-regular-file";

/// The rule recorded for a tracked path that is not in the working tree
/// (deleted, or never checked out).
const RULE_MISSING: &str = "missing-in-worktree";

/// The rule recorded for a path that could not be read.
const RULE_UNREADABLE: &str = "unreadable";

/// Where the exported file list came from. The report carries it because
/// the classification of a plain directory is NOT git's: the filesystem is
/// the source of truth there, and nobody should read a tree walk as a
/// versioned state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportSource {
    /// The index of the project's own working tree.
    GitIndex,
    /// A plain directory: there is no index, so nothing is a change.
    TreeWalk,
}

/// The honest report of one export: what travelled, what did not, and
/// under which policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectExport {
    pub policy: ExportPolicy,
    pub source: ExportSource,
    pub files_exported: Vec<String>,
    pub denied: Vec<DeniedFile>,
}

/// Everything that can go wrong while staging an export. Each variant says
/// what failed and how to correct it.
#[derive(Debug, Error)]
pub enum StageError {
    #[error("the project path {path} is not a directory")]
    NotADirectory { path: PathBuf },
    #[error("the git working tree of {path} could not be read: {source}")]
    Git {
        path: PathBuf,
        #[source]
        source: GitError,
    },
    #[error("the export is empty: {reason}. {hint}")]
    Empty { reason: String, hint: String },
    #[error("every candidate file was denied by the export rules: {denied:?}")]
    AllDenied { denied: Vec<DeniedFile> },
    #[error("the private temporary directory of the export could not be created")]
    NoPrivateDir,
    #[error("i/o error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// A packed export, ready to be uploaded.
///
/// The bytes live in a private directory that is removed when the value is
/// dropped: the host keeps no copy of what it sent, and a crash between
/// the packing and the upload leaves nothing behind but a `0700` directory
/// in the temporary area.
pub struct StagedExport {
    archive: PathBuf,
    private_dir: PathBuf,
    report: ProjectExport,
}

impl StagedExport {
    /// The path of the packed archive.
    pub fn archive(&self) -> &Path {
        &self.archive
    }

    /// What was exported and what was denied.
    pub fn report(&self) -> &ProjectExport {
        &self.report
    }

    /// The bytes of the packed archive.
    pub fn bytes(&self) -> Result<Vec<u8>, StageError> {
        fs::read(&self.archive).map_err(|source| StageError::Io {
            path: self.archive.clone(),
            source,
        })
    }
}

impl Drop for StagedExport {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.private_dir);
    }
}

impl std::fmt::Debug for StagedExport {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StagedExport")
            .field("archive", &self.archive)
            .field("report", &self.report)
            .finish()
    }
}

/// The files of a project, split by whether they carry unversioned
/// changes.
struct Classified {
    source: ExportSource,
    /// The files the source of truth vouches for.
    versioned: Vec<String>,
    /// The files that differ from that truth: modified or untracked.
    unversioned: Vec<String>,
}

/// Stage the export of a project directory.
///
/// The export is decided BEFORE any sandbox exists: a failure here leaves
/// nothing to clean up but the private directory, which the value removes
/// when it is dropped.
pub fn stage_project(project_dir: &Path, policy: ExportPolicy) -> Result<StagedExport, StageError> {
    let root = project_dir
        .canonicalize()
        .map_err(|source| StageError::Io {
            path: project_dir.to_path_buf(),
            source,
        })?;
    if !root.is_dir() {
        return Err(StageError::NotADirectory {
            path: project_dir.to_path_buf(),
        });
    }
    let classified = classify(&root)?;
    let denylist: Vec<String> = EXPORT_DENYLIST.iter().map(|s| s.to_string()).collect();
    let selection = select_files_for_export(
        &classified.versioned,
        Some(&classified.unversioned),
        policy,
        &denylist,
    );
    let (files, mut denied) = keep_only_packable(&root, &selection.files)?;
    denied.extend(selection.denied);
    denied.sort_by(|a, b| a.path.cmp(&b.path));
    denied.dedup_by(|a, b| a.path == b.path);

    if files.is_empty() {
        return Err(empty_error(&classified, &denied));
    }

    let (private_dir, archive) = pack(&root, &files)?;
    Ok(StagedExport {
        archive,
        private_dir,
        report: ProjectExport {
            policy,
            source: classified.source,
            files_exported: files,
            denied,
        },
    })
}

/// The refusal of an empty export. "Nothing to export" and "everything was
/// denied" are different facts and they are reported differently.
fn empty_error(classified: &Classified, denied: &[DeniedFile]) -> StageError {
    if !denied.is_empty() {
        return StageError::AllDenied {
            denied: denied.to_vec(),
        };
    }
    let (reason, hint) = match classified.source {
        ExportSource::GitIndex => {
            let changes = classified.unversioned.len();
            (
                format!(
                    "the working tree has no versioned content ({} file(s) carry unversioned changes)",
                    changes
                ),
                "export the changes explicitly with the untracked policy".to_string(),
            )
        }
        ExportSource::TreeWalk => (
            "the directory holds no file that may leave the host".to_string(),
            "the export rules denied every file of the directory".to_string(),
        ),
    };
    StageError::Empty { reason, hint }
}

/// Classify the files of the project.
///
/// A directory that IS the root of a working tree is classified by git:
/// the index says what is versioned and the comparison with `HEAD` says
/// what carries a change. Any other directory — a subdirectory of a
/// repository, or a directory without a repository at all — is classified
/// by the filesystem: there is no index there, so nothing is a change and
/// the whole tree is the project.
fn classify(root: &Path) -> Result<Classified, StageError> {
    if git::is_worktree_root(root) {
        let tracked = git::ls_tracked(root).map_err(|source| StageError::Git {
            path: root.to_path_buf(),
            source,
        })?;
        let changed = if git::has_head(root) {
            git::ls_changed(root).map_err(|source| StageError::Git {
                path: root.to_path_buf(),
                source,
            })?
        } else {
            // Without a commit there is no versioned content at all.
            tracked.clone()
        };
        let untracked = git::ls_untracked(root).map_err(|source| StageError::Git {
            path: root.to_path_buf(),
            source,
        })?;
        let changed_paths: BTreeSet<&String> = changed.iter().collect();
        let versioned = tracked
            .iter()
            .filter(|path| !changed_paths.contains(path))
            .cloned()
            .collect();
        let mut unversioned = changed;
        unversioned.extend(untracked);
        unversioned.sort();
        unversioned.dedup();
        Ok(Classified {
            source: ExportSource::GitIndex,
            versioned,
            unversioned,
        })
    } else {
        Ok(Classified {
            source: ExportSource::TreeWalk,
            versioned: walk(root)?,
            unversioned: Vec::new(),
        })
    }
}

/// The files of a plain directory. The denied directories are never
/// entered and the symbolic links are never followed: a link is listed,
/// but the walk does not walk through it.
fn walk(root: &Path) -> Result<Vec<String>, StageError> {
    let mut files = Vec::new();
    let mut pending = vec![(root.to_path_buf(), String::new(), 0usize)];
    while let Some((dir, prefix, depth)) = pending.pop() {
        if depth > MAX_WALK_DEPTH {
            continue;
        }
        let entries = fs::read_dir(&dir).map_err(|source| StageError::Io {
            path: dir.clone(),
            source,
        })?;
        for entry in entries {
            let entry = entry.map_err(|source| StageError::Io {
                path: dir.clone(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().to_string();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = entry
                .path()
                .symlink_metadata()
                .map_err(|source| StageError::Io {
                    path: entry.path(),
                    source,
                })?;
            if metadata.is_dir() {
                if is_denied_directory(&relative) {
                    continue;
                }
                pending.push((entry.path(), relative, depth + 1));
            } else {
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}

/// Whether a directory of the tree is denied by the export rules. The
/// denied directory is never entered: the walk does not even look at what
/// it holds.
fn is_denied_directory(relative: &str) -> bool {
    EXPORT_DENYLIST
        .iter()
        .filter(|rule| rule.ends_with('/'))
        .any(|rule| matches_rule(&format!("{relative}/entry"), rule))
}

/// Keep only the paths that can be packed, and report the ones that
/// cannot.
///
/// A gitlink (a submodule) is a directory in the working tree and a
/// regular file in the index: packing it as a file would fail, and
/// packing its content would smuggle another repository into the sandbox.
fn keep_only_packable(
    root: &Path,
    files: &[String],
) -> Result<(Vec<String>, Vec<DeniedFile>), StageError> {
    let mut keep = Vec::new();
    let mut denied = Vec::new();
    for relative in files {
        let path = root.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() || metadata.is_symlink() => {
                keep.push(relative.clone())
            }
            Ok(_) => denied.push(DeniedFile {
                path: relative.clone(),
                rule: RULE_NOT_A_FILE.to_string(),
            }),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                denied.push(DeniedFile {
                    path: relative.clone(),
                    rule: RULE_MISSING.to_string(),
                });
            }
            Err(_) => denied.push(DeniedFile {
                path: relative.clone(),
                rule: RULE_UNREADABLE.to_string(),
            }),
        }
    }
    Ok((keep, denied))
}

/// Pack the files into a private archive.
///
/// The archive is written inside a directory of mode `0700` created with
/// a name that nobody can predict, and the archive itself is created with
/// mode `0600` and `O_EXCL`: another process cannot have prepared it.
fn pack(root: &Path, files: &[String]) -> Result<(PathBuf, PathBuf), StageError> {
    let private_dir = private_dir()?;
    let archive = private_dir.join(ARCHIVE_NAME);
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&archive)
        .map_err(|source| StageError::Io {
            path: archive.clone(),
            source,
        })?;
    let mut builder = Builder::new(GzEncoder::new(file, Compression::fast()));
    for relative in files {
        append_entry(&mut builder, root, relative)?;
    }
    builder.finish().map_err(|source| StageError::Io {
        path: archive.clone(),
        source,
    })?;
    builder
        .into_inner()
        .map_err(|source| StageError::Io {
            path: archive.clone(),
            source,
        })?
        .finish()
        .map_err(|source| StageError::Io {
            path: archive.clone(),
            source,
        })?;
    Ok((private_dir, archive))
}

/// Append one file to the archive.
///
/// The member name is relative to the project: the archive is extracted
/// inside the sandbox, and nothing in it may point outside the directory
/// it is extracted into.
fn append_entry<W: Write>(
    builder: &mut Builder<W>,
    root: &Path,
    relative: &str,
) -> Result<(), StageError> {
    let path = root.join(relative);
    let metadata = fs::symlink_metadata(&path).map_err(|source| StageError::Io {
        path: path.clone(),
        source,
    })?;
    let member = relative;
    let mut header = Header::new_gnu();
    header.set_uid(0);
    header.set_gid(0);
    if metadata.is_symlink() {
        // The link travels AS a link: following it would copy whatever it
        // points at — a host secret outside the project, most of the time.
        let target = fs::read_link(&path).map_err(|source| StageError::Io {
            path: path.clone(),
            source,
        })?;
        header.set_entry_type(EntryType::Symlink);
        header
            .set_link_name(target)
            .map_err(|source| StageError::Io {
                path: path.clone(),
                source,
            })?;
        header.set_mode(0o777);
        header.set_size(0);
        builder
            .append_data(&mut header, member, std::io::empty())
            .map_err(|source| StageError::Io {
                path: path.clone(),
                source,
            })
    } else {
        let mut source = File::open(&path).map_err(|error| StageError::Io {
            path: path.clone(),
            source: error,
        })?;
        header.set_entry_type(EntryType::Regular);
        header.set_mode(metadata.permissions().mode() & 0o777);
        header.set_size(metadata.len());
        builder
            .append_data(&mut header, member, &mut source)
            .map_err(|source| StageError::Io {
                path: path.clone(),
                source,
            })
    }
}

/// A private directory for the export: mode `0700`, and a name that
/// cannot be predicted by another process.
fn private_dir() -> Result<PathBuf, StageError> {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let base = std::env::temp_dir();
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_nanos())
        .unwrap_or_default();
    for _ in 0..64 {
        let attempt = COUNTER.fetch_add(1, Ordering::Relaxed);
        let candidate = base.join(format!(
            "hodei-export-{}-{stamp}-{attempt}",
            std::process::id()
        ));
        match fs::DirBuilder::new().mode(0o700).create(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(source) => {
                return Err(StageError::Io {
                    path: candidate,
                    source,
                });
            }
        }
    }
    Err(StageError::NoPrivateDir)
}
