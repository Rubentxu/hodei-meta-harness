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

use std::path::{Path, PathBuf};
use std::process::Command;
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

/// The git identity the harness authors the candidate commit with: the
/// repository's configured `user.name` and `user.email`, so the promoted
/// history carries the host's author and not the agent's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitIdentity {
    pub name: String,
    pub email: String,
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
    /// The identity the harness commits the candidate with. The CLI leaves
    /// it empty; the wiring resolves it from the host repository before
    /// the agent runs.
    pub author: Option<GitIdentity>,
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
    #[error(
        "the repository {path} has no user.name/user.email: the harness authors the candidate commit with the host identity, so git must be configured"
    )]
    MissingIdentity { path: String },
    #[error("the tracker did not answer at boot: {0}")]
    Tracker(String),
    #[error("git in {path} could not answer: {detail}")]
    Git { path: String, detail: String },
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
    // The managed branch must exist before any port is built: the
    // promotion moves that branch, and it cannot move what is not there.
    managed_head(&config.project_dir, &config.managed_branch)?;
    // The harness authors the candidate commit, so the host identity must
    // exist before any port is built.
    let config = LiveConfig {
        author: Some(host_identity(&config.project_dir)?),
        ..config.clone()
    };
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
        // The journal of the promotion intents is the coordinator state
        // where the attempt holds its claim: the intent is fenced to it.
        state_db: hodei_mh_coordinator::paths::project_state_db(&config.project),
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

/// Resolve the identity the harness commits the candidate with from the
/// host repository's configuration (local or global, as git resolves it).
fn host_identity(project_dir: &Path) -> Result<GitIdentity, LiveError> {
    let missing = || LiveError::MissingIdentity {
        path: project_dir.display().to_string(),
    };
    let git = |detail: String| LiveError::Git {
        path: project_dir.display().to_string(),
        detail,
    };
    let name = git_config(project_dir, "user.name")
        .map_err(git)?
        .ok_or_else(missing)?;
    let email = git_config(project_dir, "user.email")
        .map_err(git)?
        .ok_or_else(missing)?;
    Ok(GitIdentity { name, email })
}

/// The head of the managed branch.
///
/// A branch that git reports as missing is a missing branch; a git that
/// could not answer is THAT failure, never mistaken for a missing branch.
fn managed_head(project_dir: &Path, managed_branch: &str) -> Result<String, LiveError> {
    match promote::git::current_ref(project_dir, managed_branch) {
        Ok(Some(head)) => Ok(head),
        Ok(None) => Err(LiveError::MissingBranch {
            branch: managed_branch.to_string(),
            path: project_dir.display().to_string(),
        }),
        Err(error) => Err(LiveError::Git {
            path: project_dir.display().to_string(),
            detail: error.to_string(),
        }),
    }
}

/// One configured git value.
///
/// `None` means git answered that the key is unset. Anything else that
/// went wrong is an error with git's own words: an unanswered question is
/// not an unset value.
fn git_config(project_dir: &Path, key: &str) -> Result<Option<String>, String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project_dir)
        .args(["config", "--get", key])
        .output()
        .map_err(|error| format!("git config --get {key}: {error}"))?;
    if !output.status.success() {
        // `git config --get` exits 1 when the key has no value.
        if output.status.code() == Some(1) {
            return Ok(None);
        }
        return Err(format!(
            "git config --get {key} exited {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok((!value.is_empty()).then_some(value))
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
        author: None,
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
        assert_eq!(
            config.author, None,
            "the CLI states no identity: the wiring resolves it"
        );
    }

    #[test]
    fn the_host_identity_comes_from_the_repository_configuration() {
        let dir = std::env::temp_dir().join(format!("hodei-live-identity-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        hodei_mh_workspace::git::init_fixture_repo(&dir).expect("the fixture repository");
        let configure = |key: &str, value: &str| {
            let status = Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(["config", "--local", key, value])
                .status()
                .expect("git is available");
            assert!(status.success(), "git config {key}");
        };

        configure("user.name", "Harness Author");
        configure("user.email", "harness@example.com");
        let identity = host_identity(&dir).expect("the configured identity");
        assert_eq!(identity.name, "Harness Author");
        assert_eq!(identity.email, "harness@example.com");

        // An empty local value is not an identity, even when a global one
        // exists: the harness would commit under a name that says nothing.
        configure("user.name", "");
        match host_identity(&dir) {
            Err(LiveError::MissingIdentity { path }) => {
                assert_eq!(path, dir.display().to_string());
            }
            other => panic!("expected MissingIdentity, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_managed_branch_that_git_knows_is_absent_is_a_missing_branch() {
        let dir = std::env::temp_dir().join(format!("hodei-live-head-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        hodei_mh_workspace::git::init_fixture_repo(&dir).expect("the fixture repository");
        match managed_head(&dir, "hodei-mh/main") {
            Err(LiveError::MissingBranch { branch, .. }) => assert_eq!(branch, "hodei-mh/main"),
            other => panic!("expected MissingBranch, got {other:?}"),
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn git_failing_to_answer_is_surfaced_instead_of_blamed_on_the_checkout() {
        // Neither a missing branch nor a missing identity: git itself could
        // not answer, and the harness must say so instead of guessing. A
        // directory git cannot even enter is such a case.
        let dir = std::env::temp_dir().join(format!("hodei-live-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        match managed_head(&dir, "hodei-mh/main") {
            Err(LiveError::Git { detail, .. }) => assert!(
                detail.contains("rev-parse"),
                "the git command is named: {detail}"
            ),
            other => panic!("expected LiveError::Git, got {other:?}"),
        }
        match host_identity(&dir) {
            Err(LiveError::Git { detail, .. }) => {
                assert!(detail.contains("user.name"), "the key is named: {detail}")
            }
            other => panic!("expected LiveError::Git, got {other:?}"),
        }
    }
}
