//! Pure workspace planning: explicit base revision, private-path denylist,
//! candidate validation, export selection. No I/O — the git adapter and the
//! probe feed data in.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Paths the agent must never promote: hodei management state and secrets.
pub const DEFAULT_DENYLIST: &[&str] = &[
    "backlog/",
    ".claude/",
    ".env",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa",
    "id_ed25519",
    "credentials",
    "secrets",
];

/// The export rule set: the promotion denylist PLUS what must never leave
/// the host towards a sandbox even when it IS versioned or not a config
/// file — the git metadata, build caches, and the whole `.env` family
/// (`.env`, `.env.local`, `.env.production`...), which the exact-basename
/// promotion rule does not cover. Widening `DEFAULT_DENYLIST` would change
/// promotion semantics; these are egress semantics.
pub const EXPORT_DENYLIST: &[&str] = &[
    ".git/",
    ".env*",
    "*.pem",
    "*.key",
    "*.p12",
    "id_rsa",
    "id_ed25519",
    "credentials",
    "secrets",
    "backlog/",
    ".claude/",
    "target/",
    "node_modules/",
    "__pycache__/",
    ".venv/",
    ".gradle/",
];

/// Whether untracked working-tree files may join an export. The default is
/// the conservative one: untracked changes leave the host ONLY under an
/// explicit policy the operator chose, never as a side effect of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExportPolicy {
    TrackedOnly,
    WithUntracked,
}

/// One file kept out of an export, with the rule that kept it out. The
/// reason set belongs in the run record: an export claiming "no secrets"
/// must be able to show WHICH files it declined and WHY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeniedFile {
    pub path: String,
    pub rule: String,
}

/// The pure export selection: the exact file list plus the honest denial
/// report. Built by `select_files_for_export`; consumed by the staging
/// adapter, which must tar exactly these paths and record exactly these
/// denials.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportSelection {
    pub files: Vec<String>,
    pub denied: Vec<DeniedFile>,
}

/// Decide which host files may enter a sandbox export.
///
/// `tracked` and `untracked` are repository-root-relative (or walk-relative)
/// path lists produced by the git adapter or the tree walker — this function
/// never runs anything. A denylisted path is denied EVEN WHEN TRACKED: the
/// gitignore is not a secret detector, so a committed `.env` is still a
/// committed secret. `untracked` files are considered only when `policy` is
/// `WithUntracked`; `None` means the source could not classify them at all
/// (a non-git directory), which is the caller's fact to record, not a
/// licence to guess.
pub fn select_files_for_export(
    tracked: &[String],
    untracked: Option<&[String]>,
    policy: ExportPolicy,
    denylist: &[String],
) -> ExportSelection {
    let mut files = Vec::new();
    let mut denied = Vec::new();
    let mut consider = |path: &str| {
        if path.trim().is_empty() {
            return;
        }
        match denylist.iter().find(|rule| matches_rule(path, rule)) {
            Some(rule) => denied.push(DeniedFile {
                path: path.to_string(),
                rule: rule.clone(),
            }),
            None => files.push(path.to_string()),
        }
    };
    for path in tracked {
        consider(path);
    }
    if policy == ExportPolicy::WithUntracked {
        if let Some(paths) = untracked {
            for path in paths {
                consider(path);
            }
        }
    }
    files.sort();
    files.dedup();
    denied.sort_by(|a, b| a.path.cmp(&b.path));
    ExportSelection { files, denied }
}

#[derive(Debug, Error, PartialEq)]
pub enum PlanError {
    #[error("base revision is required; a dirty checkout is never copied implicitly")]
    MissingBaseRevision,
    #[error("workspace path cannot be empty")]
    EmptyWorkdir,
}

/// The chosen workspace plan. `base_revision` must be explicit by design:
/// the sandbox receives exactly that revision, never a live host checkout.
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspacePlan {
    pub base_revision: String,
    pub workdir: String,
    pub denylist: Vec<String>,
}

impl WorkspacePlan {
    pub fn new(
        base_revision: impl Into<String>,
        workdir: impl Into<String>,
    ) -> Result<Self, PlanError> {
        let base_revision = base_revision.into();
        let workdir = workdir.into();
        if base_revision.trim().is_empty() {
            return Err(PlanError::MissingBaseRevision);
        }
        if workdir.trim().is_empty() {
            return Err(PlanError::EmptyWorkdir);
        }
        Ok(Self {
            base_revision,
            workdir,
            denylist: DEFAULT_DENYLIST.iter().map(|s| s.to_string()).collect(),
        })
    }
}

#[derive(Debug, Error, PartialEq)]
#[error("candidate touches private path {path:?} (rule {rule:?})")]
pub struct RejectedPath {
    pub path: String,
    pub rule: String,
}

/// Match a repo-relative path against one denylist rule.
/// - `dir/` rules match at any path-segment boundary.
/// - `*.ext` rules match the file suffix.
/// - `name*` rules match a base-name prefix (secret families like
///   `.env.local` that exact-basename rules cannot reach).
/// - other rules match the exact base name.
pub fn matches_rule(path: &str, rule: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    if let Some(dir) = rule.strip_suffix('/') {
        return segment_dir_match(normalized, dir);
    }
    if let Some(ext) = rule.strip_prefix('*') {
        return normalized
            .rsplit('/')
            .next()
            .is_some_and(|name| name.ends_with(ext));
    }
    if let Some(prefix) = rule.strip_suffix('*') {
        return normalized
            .rsplit('/')
            .next()
            .is_some_and(|name| name.starts_with(prefix));
    }
    normalized.rsplit('/').next() == Some(rule)
}

fn segment_dir_match(path: &str, dir: &str) -> bool {
    let dir_prefix = format!("{dir}/");
    path.starts_with(&dir_prefix) || path.contains(&format!("/{dir_prefix}"))
}

/// Validate the changed-file list of a candidate against the denylist.
/// Returns every violation, so the host announces the full reason set.
pub fn validate_candidate<'a>(
    changed_files: impl IntoIterator<Item = &'a str>,
    denylist: &[String],
) -> Result<(), Vec<RejectedPath>> {
    let mut violations = Vec::new();
    for path in changed_files {
        let path = path.trim();
        if path.is_empty() {
            continue;
        }
        if let Some(rule) = denylist.iter().find(|rule| matches_rule(path, rule)) {
            violations.push(RejectedPath {
                path: path.to_string(),
                rule: rule.clone(),
            });
        }
    }
    if violations.is_empty() {
        Ok(())
    } else {
        Err(violations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan(base: &str) -> WorkspacePlan {
        WorkspacePlan::new(base, "/workspace/repo").unwrap()
    }

    #[test]
    fn plan_requires_explicit_base_revision() {
        assert_eq!(
            WorkspacePlan::new("", "/w").unwrap_err(),
            PlanError::MissingBaseRevision
        );
        assert_eq!(
            WorkspacePlan::new("   ", "/w").unwrap_err(),
            PlanError::MissingBaseRevision
        );
        assert_eq!(
            WorkspacePlan::new("abc123", "").unwrap_err(),
            PlanError::EmptyWorkdir
        );
        assert_eq!(plan("abc123").base_revision, "abc123");
    }

    #[test]
    fn denylist_default_includes_management_and_secrets() {
        let p = plan("x");
        assert!(p.denylist.iter().any(|r| r == "backlog/"));
        assert!(p.denylist.iter().any(|r| r == "*.key"));
    }

    #[test]
    fn directory_rules_match_only_at_segment_boundaries() {
        assert!(matches_rule("backlog/decisions.md", "backlog/"));
        assert!(!matches_rule("backlog", "backlog/")); // dir rule needs the slash path
        assert!(matches_rule("src/backlog/leak.md", "backlog/"));
        assert!(!matches_rule("mybacklog/ok.md", "backlog/"));
        assert!(!matches_rule("backloggrande/ok.md", "backlog/"));
        assert!(!matches_rule("src/ok.md", "backlog/"));
    }

    #[test]
    fn suffix_and_basename_rules() {
        assert!(matches_rule("host.pem", "*.pem"));
        assert!(matches_rule("deep/dir/host.key", "*.key"));
        assert!(!matches_rule("pema/ok.txt", "*.pem"));
        assert!(matches_rule(".claude/settings.json", ".claude/"));
        assert!(matches_rule("secrets", "secrets"));
        assert!(!matches_rule("secrets.md", "secrets"));
        assert!(matches_rule("./backlog/x", "backlog/"));
    }

    #[test]
    fn candidate_validation_reports_all_violations() {
        let p = plan("x");
        let files = ["src/main.rs", "backlog/leak.md", "host.pem", "ok.txt"];
        let err = validate_candidate(files, &p.denylist).unwrap_err();
        assert_eq!(err.len(), 2);
        assert_eq!(err[0].path, "backlog/leak.md");
        assert_eq!(err[1].path, "host.pem");
        assert!(validate_candidate(["src/main.rs", "readme.md"], &p.denylist).is_ok());
    }

    #[test]
    fn prefix_rules_reach_the_whole_secret_family() {
        assert!(matches_rule(".env", ".env*"));
        assert!(matches_rule(".env.local", ".env*"));
        assert!(matches_rule("deploy/.env.production", ".env*"));
        // The family is base-name anchored: nothing innocent is caught.
        assert!(!matches_rule("environment.txt", ".env*"));
        assert!(!matches_rule("my.env", ".env*"));
    }

    fn export_denylist() -> Vec<String> {
        EXPORT_DENYLIST.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_committed_secret_is_denied_even_though_it_is_tracked() {
        // The gitignore is not the detector: the denylist runs over the
        // TRACKED list too, because a committed `.env` is a committed
        // secret that must still never leave the host.
        let tracked = ["src/main.rs", ".env", "deploy/.env.production", "id_rsa"];
        let tracked: Vec<String> = tracked.iter().map(|s| s.to_string()).collect();
        let sel = select_files_for_export(
            &tracked,
            None,
            ExportPolicy::TrackedOnly,
            &export_denylist(),
        );
        assert_eq!(sel.files, vec!["src/main.rs".to_string()]);
        let denied: Vec<&str> = sel.denied.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(denied, vec![".env", "deploy/.env.production", "id_rsa"]);
    }

    #[test]
    fn git_metadata_and_caches_never_join_an_export() {
        let tracked = [
            "app.py",
            ".git/config",
            ".git/objects/ab/cdef",
            "target/debug/build.log",
            "node_modules/pkg/index.js",
            "__pycache__/app.pyc",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect::<Vec<_>>();
        let sel = select_files_for_export(&tracked, None, ExportPolicy::WithUntracked, &export_denylist());
        assert_eq!(sel.files, vec!["app.py".to_string()]);
    }

    #[test]
    fn untracked_files_join_only_under_the_explicit_policy() {
        let tracked = vec!["src/main.rs".to_string()];
        let untracked = vec!["new_feature.rs".to_string(), ".env.local".to_string()];
        let off = select_files_for_export(
            &tracked,
            Some(&untracked),
            ExportPolicy::TrackedOnly,
            &export_denylist(),
        );
        assert_eq!(off.files, vec!["src/main.rs".to_string()]);
        assert!(off.denied.is_empty(), "untracked files are not even inspected");
        let on = select_files_for_export(
            &tracked,
            Some(&untracked),
            ExportPolicy::WithUntracked,
            &export_denylist(),
        );
        assert_eq!(
            on.files,
            vec!["new_feature.rs".to_string(), "src/main.rs".to_string()]
        );
        // The policy opening the door does not remove the denylist:
        // an untracked secret is denied just like a tracked one.
        assert_eq!(on.denied.len(), 1);
        assert_eq!(on.denied[0].path, ".env.local");
    }

    #[test]
    fn an_unknown_untracked_set_is_never_a_licence_to_guess() {
        // Non-git source: the caller says "cannot classify". TrackedOnly
        // semantics must not smuggle anything through the None arm.
        let sel = select_files_for_export(
            &["keep.txt".to_string()],
            None,
            ExportPolicy::WithUntracked,
            &export_denylist(),
        );
        assert_eq!(sel.files, vec!["keep.txt".to_string()]);
    }

    #[test]
    fn every_denial_carries_the_rule_that_denied_it() {
        let sel = select_files_for_export(
            &["secrets".to_string(), "host.pem".to_string(), ".git/config".to_string()],
            None,
            ExportPolicy::TrackedOnly,
            &export_denylist(),
        );
        assert!(sel.files.is_empty());
        let pairs: Vec<(&str, &str)> = sel
            .denied
            .iter()
            .map(|d| (d.path.as_str(), d.rule.as_str()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                (".git/config", ".git/"),
                ("host.pem", "*.pem"),
                ("secrets", "secrets"),
            ]
        );
    }

    #[test]
    fn the_export_policy_serializes_as_a_stable_record_value() {
        assert_eq!(
            serde_json::to_string(&ExportPolicy::TrackedOnly).unwrap(),
            "\"tracked_only\""
        );
        assert_eq!(
            serde_json::to_string(&ExportPolicy::WithUntracked).unwrap(),
            "\"with_untracked\""
        );
    }
}
