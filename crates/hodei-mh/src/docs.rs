//! The product's documentation: where it comes from, and where the
//! consumer finds it.
//!
//! The bundle ships the documentation of the PRODUCT. A file named
//! `docs/profiles.md` that happens to sit in the working directory is
//! NOT the product's documentation: shipping it would put someone
//! else's text inside a release, silently, with the manifest claiming
//! it was ours. Only two sources are trusted:
//!
//! - the copy INSTALLED next to the running binary
//!   (`<prefix>/share/hodei-mh/docs/profiles.md`), and
//! - the copy of the WORKSPACE that owns the product (the tree that
//!   holds `crates/hodei-mh/Cargo.toml`).
//!
//! When neither exists the bundle is not built: a release without its
//! documentation is incomplete, and the refusal says why.

use std::path::{Path, PathBuf};

/// The documentation of the product, inside the bundle.
pub const DOC_RELATIVE: &str = "docs/profiles.md";

/// The marker of a tree that owns the product.
const WORKSPACE_MARKER: &str = "crates/hodei-mh/Cargo.toml";

/// Where a trusted copy of the documentation comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocOrigin {
    /// The copy installed next to the running binary.
    Installed,
    /// The copy of the workspace that owns the product.
    Workspace,
}

impl DocOrigin {
    /// The name the manifest states.
    pub fn as_str(self) -> &'static str {
        match self {
            DocOrigin::Installed => "installed",
            DocOrigin::Workspace => "workspace",
        }
    }
}

/// The trusted places the documentation may come from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocCandidates {
    /// `<prefix>/share/hodei-mh/docs/profiles.md`.
    pub installed: Option<PathBuf>,
    /// `<workspace>/docs/profiles.md`.
    pub workspace: Option<PathBuf>,
}

/// A documentation source that was found and may be trusted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DocSource {
    /// Where it comes from.
    pub origin: DocOrigin,
    /// The file itself.
    pub path: PathBuf,
}

/// The documentation to ship, or `None` when no trusted copy exists.
///
/// The installed copy wins: it is the one that belongs to the binary
/// that is packaging, and it is the one the consumer of that binary
/// already has.
pub fn choose(candidates: &DocCandidates) -> Option<DocSource> {
    if let Some(path) = &candidates.installed {
        return Some(DocSource {
            origin: DocOrigin::Installed,
            path: path.clone(),
        });
    }
    candidates.workspace.as_ref().map(|path| DocSource {
        origin: DocOrigin::Workspace,
        path: path.clone(),
    })
}

/// The trusted candidates for a running binary.
///
/// The installation is looked up from the running binary, never from
/// the working directory: the working directory is the one thing the
/// caller controls, and the documentation must not be.
pub fn candidates(exe: Option<&Path>, cwd: Option<&Path>) -> DocCandidates {
    let installed = exe.and_then(installed_doc);
    let workspace = exe
        .and_then(workspace_doc)
        .or_else(|| cwd.and_then(workspace_doc));
    DocCandidates {
        installed,
        workspace,
    }
}

/// The documentation installed next to the running binary.
fn installed_doc(exe: &Path) -> Option<PathBuf> {
    let prefix = exe.parent()?.parent()?;
    let doc = prefix.join("share").join("hodei-mh").join(DOC_RELATIVE);
    doc.is_file().then_some(doc)
}

/// The documentation of the workspace that owns the product, found by
/// walking up from `start`.
fn workspace_doc(start: &Path) -> Option<PathBuf> {
    let root = workspace_root(start)?;
    let doc = root.join(DOC_RELATIVE);
    doc.is_file().then_some(doc)
}

/// The root of the workspace that owns the product, found by walking
/// up from `start`.
///
/// The marker (`crates/hodei-mh/Cargo.toml`) is what makes a tree the
/// product's workspace: a directory that merely happens to contain a
/// `docs/profiles.md` is not one.
pub fn workspace_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(WORKSPACE_MARKER).is_file())
        .map(Path::to_path_buf)
}

/// The line that tells the consumer where the documentation is.
pub fn pointer_line(source: Option<&DocSource>) -> String {
    match source {
        Some(source) => format!("documentation: {}\n", source.path.display()),
        None => format!(
            "documentation: not found next to this binary \
             (expected <prefix>/share/hodei-mh/{DOC_RELATIVE})\n"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hodei-docs-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn write(path: &Path) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("parent");
        }
        std::fs::write(path, "# docs\n").expect("write");
    }

    #[test]
    fn the_installed_copy_wins_over_the_workspace() {
        let candidates = DocCandidates {
            installed: Some(PathBuf::from("/prefix/share/hodei-mh/docs/profiles.md")),
            workspace: Some(PathBuf::from("/repo/docs/profiles.md")),
        };
        let chosen = choose(&candidates).expect("a source");
        assert_eq!(chosen.origin, DocOrigin::Installed);
        assert_eq!(
            chosen.path,
            PathBuf::from("/prefix/share/hodei-mh/docs/profiles.md")
        );
    }

    #[test]
    fn the_workspace_is_used_when_there_is_no_installation() {
        let candidates = DocCandidates {
            installed: None,
            workspace: Some(PathBuf::from("/repo/docs/profiles.md")),
        };
        let chosen = choose(&candidates).expect("a source");
        assert_eq!(chosen.origin, DocOrigin::Workspace);
    }

    #[test]
    fn without_a_trusted_source_there_is_nothing_to_ship() {
        let candidates = DocCandidates::default();
        assert_eq!(choose(&candidates), None);
    }

    #[test]
    fn the_installation_is_found_next_to_the_running_binary() {
        let root = scratch("installed");
        let prefix = root.join("prefix");
        let exe = prefix.join("bin").join("hodei-mh");
        write(&exe);
        write(&prefix.join("share/hodei-mh/docs/profiles.md"));

        let found = installed_doc(&exe).expect("the installed doc");
        assert_eq!(found, prefix.join("share/hodei-mh/docs/profiles.md"));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_binary_without_an_installation_has_no_installed_doc() {
        let root = scratch("no-install");
        let exe = root.join("bin").join("hodei-mh");
        write(&exe);
        assert_eq!(installed_doc(&exe), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_workspace_is_found_by_its_marker() {
        let root = scratch("workspace");
        let repo = root.join("repo");
        write(&repo.join(WORKSPACE_MARKER));
        write(&repo.join(DOC_RELATIVE));
        let exe = repo.join("target/x86_64-unknown-linux-gnu/release/hodei-mh");
        write(&exe);

        let found = workspace_doc(&exe).expect("the workspace doc");
        assert_eq!(found, repo.join(DOC_RELATIVE));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_tree_without_the_marker_is_not_the_workspace() {
        let root = scratch("not-workspace");
        let foreign = root.join("foreign");
        write(&foreign.join(DOC_RELATIVE));
        assert_eq!(workspace_root(&foreign), None);
        assert_eq!(workspace_doc(&foreign), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_pointer_names_the_documentation_the_consumer_has() {
        let source = DocSource {
            origin: DocOrigin::Installed,
            path: PathBuf::from("/prefix/share/hodei-mh/docs/profiles.md"),
        };
        assert_eq!(
            pointer_line(Some(&source)),
            "documentation: /prefix/share/hodei-mh/docs/profiles.md\n"
        );
        assert!(pointer_line(None).contains("not found"));
    }
}
