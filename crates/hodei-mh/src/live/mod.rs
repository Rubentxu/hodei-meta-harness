//! The live pipeline: the real ports behind the pure live runtime.
//!
//! The pure core decides (select, claim, verify, publish); this module is
//! the effect shell that talks to the real tracker, the real sandbox and
//! the real managed branch. Boot validation happens here, once, so a
//! controller that cannot execute refuses to start instead of accepting
//! an objective it can never run.

pub mod agent;
pub mod claims;
pub mod publisher;
pub mod requirements;
pub mod session;
pub mod verifier;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use hodei_mh_tracker::backlog::BacklogTracker;
use hodei_mh_tracker::core::TrackerRead;
use runner::live::LiveRuntime;
use verify::runner::CleanRunner;

use crate::live::agent::{AcpAgent, Credentials};
use crate::live::claims::StoreClaims;
use crate::live::publisher::VerifiedPublisher;
use crate::live::requirements::BacklogRequirements;
use crate::live::verifier::CleanVerifier;

/// How the ACP agent answers a permission request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionMode {
    /// Answer the request with the `allow_once` option.
    AllowOnce,
    /// Refuse the request: the agent cannot use the tool.
    Deny,
}

/// The configuration of the live runtime, as the CLI states it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveConfig {
    /// The project the controller serves.
    pub project: String,
    /// The lifecycle control plane the sandboxes are created on.
    pub server: String,
    /// The image the agent works in.
    pub image: String,
    /// The egress proxy the sandbox is allowed to reach.
    pub proxy_url: String,
    /// The repository the managed branch lives in.
    pub project_dir: PathBuf,
    /// The directory the Backlog.md CLI is invoked in.
    pub tracker_dir: PathBuf,
    /// The local managed branch the promotion moves.
    pub managed_branch: String,
    /// How the agent answers permission requests.
    pub permission: PermissionMode,
}

impl LiveConfig {
    /// Where the candidate bundle of this project is kept between the
    /// agent and the promotion. It lives under the project's private
    /// state, never in the user's tree.
    pub fn bundle_path(&self) -> PathBuf {
        hodei_mh_coordinator::paths::project_state_dir(&self.project).join("candidate.bundle")
    }
}

/// Why the live runtime could not be wired.
#[derive(Debug, thiserror::Error)]
pub enum LiveError {
    #[error(
        "ANTHROPIC_AUTH_TOKEN is empty: the live runtime takes its credentials from the environment only"
    )]
    MissingCredentials,
    #[error("--{flag} is required with --live")]
    MissingFlag { flag: String },
    #[error("the flag {flag} has the value '{value}': {expected}")]
    InvalidFlag {
        flag: String,
        value: String,
        expected: String,
    },
    #[error("{path} is not a git repository: the managed branch lives in a repository")]
    NotARepository { path: String },
    #[error("the managed branch '{branch}' does not exist in {path}")]
    MissingBranch { branch: String, path: String },
    #[error("the tracker did not answer at boot: {0}")]
    Tracker(String),
    #[error("the claims of the project could not be opened: {0}")]
    Claims(String),
}

/// Build the live runtime, validating the boot preconditions first.
///
/// The order is deliberate: the credentials, the repository, the managed
/// branch and the tracker answer are checked BEFORE any port is built, so
/// a refusal names the fact that is wrong.
pub async fn real_ports(config: &LiveConfig) -> Result<LiveRuntime, LiveError> {
    let credentials = Credentials::from_env()?;
    if !hodei_mh_workspace::git::is_git_repo(&config.project_dir) {
        return Err(LiveError::NotARepository {
            path: config.project_dir.display().to_string(),
        });
    }
    let branch = promote::git::current_ref(&config.project_dir, &config.managed_branch).map_err(
        |error| LiveError::MissingBranch {
            branch: config.managed_branch.clone(),
            path: format!("{}: {error}", config.project_dir.display()),
        },
    )?;
    if branch.is_none() {
        return Err(LiveError::MissingBranch {
            branch: config.managed_branch.clone(),
            path: config.project_dir.display().to_string(),
        });
    }
    let tracker = Arc::new(BacklogTracker::new("backlog", &config.tracker_dir));
    tracker
        .snapshot()
        .await
        .map_err(|error| LiveError::Tracker(error.to_string()))?;
    let claims = Arc::new(
        StoreClaims::open(
            &config.project,
            &hodei_mh_coordinator::paths::project_state_db(&config.project),
            claims::LIVE_HOLDER,
        )
        .map_err(|error| LiveError::Claims(error.to_string()))?,
    );
    let requirements = Arc::new(BacklogRequirements::new("backlog", &config.tracker_dir));
    let verifier = Arc::new(CleanVerifier {
        runner: CleanRunner {
            lifecycle_url: config.server.clone(),
            image: config.image.clone(),
            journal: opensandbox::cleanup::CleanupRegistry::default_journal_path(),
            timeout: Duration::from_secs(120),
        },
    });
    let publisher = Arc::new(VerifiedPublisher {
        project: config.project.clone(),
        project_dir: config.project_dir.clone(),
        managed_branch: config.managed_branch.clone(),
        bundle_path: config.bundle_path(),
        writer: promote::writer::BacklogWorkWriter {
            bin: "backlog".into(),
            working_dir: config.tracker_dir.clone(),
        },
    });
    let agent = Arc::new(AcpAgent::new(config.clone(), credentials));
    Ok(LiveRuntime::new(
        config.project.clone(),
        tracker,
        requirements,
        agent,
        verifier,
        publisher,
        claims,
    ))
}

/// Build the live configuration from the CLI arguments.
pub fn config_from_args(args: &[String], project: &str) -> Result<LiveConfig, LiveError> {
    fn value(args: &[String], name: &str) -> Option<String> {
        args.iter()
            .position(|argument| argument == name)
            .and_then(|index| args.get(index + 1))
            .filter(|value| !value.trim().is_empty())
            .cloned()
    }
    fn required(args: &[String], name: &str) -> Result<String, LiveError> {
        value(args, name).ok_or_else(|| LiveError::MissingFlag {
            flag: name.trim_start_matches("--").to_string(),
        })
    }
    let permission = match value(args, "--permission").unwrap_or_else(|| "allow-once".into()) {
        mode if mode == "allow-once" => PermissionMode::AllowOnce,
        mode if mode == "deny" => PermissionMode::Deny,
        other => {
            return Err(LiveError::InvalidFlag {
                flag: "--permission".into(),
                value: other,
                expected: "expected allow-once or deny".into(),
            });
        }
    };
    Ok(LiveConfig {
        project: project.to_string(),
        server: required(args, "--server")?,
        image: required(args, "--image")?,
        proxy_url: value(args, "--proxy-url")
            .unwrap_or_else(|| crate::profile_run::DEFAULT_PROXY_URL.to_string()),
        project_dir: PathBuf::from(required(args, "--project-dir")?),
        tracker_dir: PathBuf::from(required(args, "--tracker-dir")?),
        managed_branch: required(args, "--managed-branch")?,
        permission,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_args() -> Vec<String> {
        [
            "--live",
            "--server",
            "http://127.0.0.1:8081",
            "--image",
            "example/image:1",
            "--project-dir",
            "/tmp/project",
            "--tracker-dir",
            "/tmp/project",
            "--managed-branch",
            "hodei-mh/main",
        ]
        .iter()
        .map(|value| value.to_string())
        .collect()
    }

    #[test]
    fn every_required_flag_has_its_own_refusal() {
        for flag in [
            "--server",
            "--image",
            "--project-dir",
            "--tracker-dir",
            "--managed-branch",
        ] {
            let mut args = live_args();
            let position = args
                .iter()
                .position(|argument| argument == flag)
                .expect("the flag is in the fixture");
            args.drain(position..=position + 1);
            let error = config_from_args(&args, "project").expect_err(flag);
            assert!(
                matches!(&error, LiveError::MissingFlag { flag: named } if named == flag.trim_start_matches("--")),
                "{error}"
            );
        }
    }

    #[test]
    fn a_permission_mode_that_is_not_a_mode_is_refused() {
        let mut args = live_args();
        args.push("--permission".into());
        args.push("always".into());
        let error = config_from_args(&args, "project").expect_err("unknown mode");
        assert!(
            matches!(error, LiveError::InvalidFlag { ref flag, .. } if flag == "--permission"),
            "{error}"
        );
    }

    #[test]
    fn the_configuration_defaults_are_explicit() {
        let config = config_from_args(&live_args(), "project").expect("a live configuration");
        assert_eq!(config.permission, PermissionMode::AllowOnce);
        assert_eq!(config.proxy_url, crate::profile_run::DEFAULT_PROXY_URL);
        assert_eq!(config.project, "project");
    }
}
