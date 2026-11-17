//! Explicit authorization grants (AC-1): a grant names the project and the
//! exact effects it authorizes. Nothing implicit — installing or editing
//! workflow/management files is never inside a grant's scope.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Effects a grant can authorize, exhaustively.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum EffectKind {
    /// Write files inside the run's workspace only.
    WorkspaceFileWrite,
    /// Create git commits inside the run's workspace only.
    WorkspaceGitCommit,
    /// One ACP conversation with the agent.
    AgentPromptSession,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grant {
    pub project_id: String,
    pub run_id: String,
    pub epoch: u64,
    pub effects: Vec<EffectKind>,
}

#[derive(Debug, Error, PartialEq)]
#[error("effect {requested:?} denied by grant: {reason}")]
pub struct Denial {
    pub requested: EffectKind,
    pub reason: String,
}

impl Grant {
    /// A grant is explicit: project-bound, epoch-tagged, finite effect list.
    pub fn new(
        project_id: impl Into<String>,
        run_id: impl Into<String>,
        epoch: u64,
        effects: Vec<EffectKind>,
    ) -> Self {
        Self {
            project_id: project_id.into(),
            run_id: run_id.into(),
            epoch,
            effects,
        }
    }

    pub fn authorize(&self, requested: &EffectKind) -> Result<(), Denial> {
        if self.effects.contains(requested) {
            Ok(())
        } else {
            Err(Denial {
                requested: requested.clone(),
                reason: "effect not present in the explicit grant scope".into(),
            })
        }
    }
}

/// Paths that can never become authorized by content changes: management
/// and workflow state lives outside every grant (AC-1 second sentence).
pub const MANAGEMENT_PATHS: &[&str] = &["backlog/", ".claude/", "AGENTS.md", "CLAUDE.md"];

pub fn is_management_path(path: &str) -> bool {
    let normalized = path.trim_start_matches("./");
    MANAGEMENT_PATHS.iter().any(|rule| {
        if let Some(dir) = rule.strip_suffix('/') {
            normalized.starts_with(dir) || normalized.contains(&format!("/{dir}/"))
        } else {
            normalized == *rule || normalized.ends_with(&format!("/{rule}"))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scoped() -> Grant {
        Grant::new(
            "hodei-meta-harness",
            "run-1",
            1,
            vec![
                EffectKind::WorkspaceFileWrite,
                EffectKind::AgentPromptSession,
            ],
        )
    }

    #[test]
    fn grant_authorizes_only_listed_effects() {
        let grant = scoped();
        assert!(grant.authorize(&EffectKind::WorkspaceFileWrite).is_ok());
        assert!(grant.authorize(&EffectKind::AgentPromptSession).is_ok());
        assert!(grant.authorize(&EffectKind::WorkspaceGitCommit).is_err());
    }

    #[test]
    fn grant_is_project_and_epoch_bound() {
        let grant = scoped();
        assert_eq!(grant.project_id, "hodei-meta-harness");
        assert_eq!(grant.epoch, 1);
        assert_ne!(
            Grant::new("other", "run-1", 1, grant.effects.clone()),
            grant
        );
    }

    #[test]
    fn management_paths_are_outside_every_grant() {
        assert!(is_management_path("backlog/tasks/task-1.md"));
        assert!(is_management_path("AGENTS.md"));
        assert!(is_management_path("src/CLAUDE.md"));
        assert!(!is_management_path("src/main.rs"));
        assert!(!is_management_path("my-agents.md"));
        // Editing workflow files never extends a grant: the grant check is
        // structural (effect list), not content-derived.
        let grant = scoped();
        assert!(grant.authorize(&EffectKind::WorkspaceGitCommit).is_err());
    }
}
