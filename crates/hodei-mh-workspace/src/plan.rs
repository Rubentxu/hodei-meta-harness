//! Pure workspace planning: explicit base revision, private-path denylist,
//! candidate validation. No I/O — the git adapter and the probe feed data in.

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
}
