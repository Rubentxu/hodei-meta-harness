//! The agent of the live pipeline: one fresh sandbox, one fresh ACP
//! conversation, one candidate commit.
//!
//! The sandbox is created for the attempt alone and released on every
//! path. The candidate is observed WHERE IT LIVES — the bundle, the head
//! and the tree are read before the sandbox goes away — so the
//! verification judges the very commit the agent produced.

use std::path::Path;
use std::time::{Duration, Instant};

use opensandbox::acp::AcpAssembler;
use opensandbox::control::ControlPlane;
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::PipeSession;
use serde_json::{Value, json};
use verify::git::{GitRevision, GitTree};
use verify::runner::Candidate;
use verify::transport::TransportDigest;

use runner::ports::{AgentError, AgentPort, Objective};

use crate::live::session::SandboxSession;
use crate::live::{LiveConfig, LiveError, PermissionMode};

/// The worktree the agent works in, inside the sandbox.
pub const WORKTREE: &str = "/workspace/work";

/// Where the managed state travels into the sandbox.
const INCOMING_BUNDLE: &str = "/tmp/candidate.bundle";

/// Where the candidate leaves the sandbox.
const OUTGOING_BUNDLE: &str = "/tmp/candidate-out.bundle";

/// The npm package of the ACP adapter, pinned.
const ADAPTER_PACKAGE: &str = "@zed-industries/claude-code-acp@0.16.2";

/// Where the adapter is staged inside the sandbox.
const ADAPTER_ROOT: &str = "/workspace/ccacp";

/// The host directory the adapter is packed from.
const ADAPTER_PACKAGE_DIR: &str = "hodei-ccacp";

/// The archive that carries the adapter into the sandbox.
const ADAPTER_ARCHIVE: &str = "hodei-ccacp.tgz";

/// How long one ACP conversation may take.
const AGENT_TIMEOUT: Duration = Duration::from_secs(180);

/// How long the cleanup of the agent session may take.
const SESSION_CLEANUP_TIMEOUT: Duration = Duration::from_secs(30);

/// The stop reason of a turn the agent ended on its own.
///
/// Any other reason — a refusal, a cancellation — means the agent did not
/// finish the work it was asked for, whatever it produced along the way.
const END_TURN: &str = "end_turn";

/// The credentials of the agent. They come from the environment of this
/// process only, and they are never printed: the debug representation
/// says only that they exist.
#[derive(Clone)]
pub struct Credentials {
    token: String,
    base_url: Option<String>,
}

impl std::fmt::Debug for Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credentials")
            .field("token", &"<redacted>")
            .field("base_url", &self.base_url.as_ref().map(|_| "<set>"))
            .finish()
    }
}

impl Credentials {
    /// The credentials the environment carries.
    pub fn from_env() -> Result<Self, LiveError> {
        let token = std::env::var("ANTHROPIC_AUTH_TOKEN").unwrap_or_default();
        if token.trim().is_empty() {
            return Err(LiveError::MissingCredentials);
        }
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .filter(|value| !value.trim().is_empty());
        Ok(Self::new(token, base_url))
    }

    /// Explicit credentials, for the construction and its tests.
    pub fn new(token: impl Into<String>, base_url: Option<String>) -> Self {
        Self {
            token: token.into(),
            base_url,
        }
    }
}

/// The agent that executes one objective in a fresh sandbox.
pub struct AcpAgent {
    config: LiveConfig,
    credentials: Credentials,
}

impl AcpAgent {
    pub fn new(config: LiveConfig, credentials: Credentials) -> Self {
        Self {
            config,
            credentials,
        }
    }

    fn failure(work_ref: &str, reason: impl ToString) -> AgentError {
        AgentError {
            work_ref: work_ref.to_string(),
            reason: reason.to_string(),
        }
    }

    /// Execute the objective: prepare the worktree, run the conversation
    /// and observe the candidate it produced.
    async fn work(
        &self,
        session: &SandboxSession,
        objective: &Objective,
    ) -> Result<Candidate, AgentError> {
        let work_ref = objective.work_ref.as_str();
        let base = self.build_bundle(work_ref).await?;
        let bundle = tokio::fs::read(self.config.bundle_path())
            .await
            .map_err(|error| Self::failure(work_ref, format!("the candidate bundle: {error}")))?;
        session
            .upload(bundle, INCOMING_BUNDLE)
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        session
            .run_checked(&prepare_command(&self.config.managed_branch))
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        self.stage_adapter(session, work_ref).await?;
        let command = self.agent_command();
        let (mut conversation, agent_session) = session
            .attach_agent(&command, WORKTREE)
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let instruction = instruction_for(&objective.instruction);
        let turn = drive_agent(
            &mut conversation,
            &instruction,
            WORKTREE,
            AGENT_TIMEOUT,
            self.config.permission,
        )
        .await;
        let _ = session
            .transport()
            .delete_session(&agent_session.session_id, SESSION_CLEANUP_TIMEOUT)
            .await;
        if !turn.completed {
            return Err(Self::failure(
                work_ref,
                format!("the agent did not finish its turn: {}", turn.summary),
            ));
        }
        session
            .run_checked(&export_command())
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let candidate_bundle = session
            .download(OUTGOING_BUNDLE)
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let head = session
            .run_checked("git -C /workspace/work rev-parse HEAD")
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let tree = session
            .run_checked("git -C /workspace/work rev-parse 'HEAD^{tree}'")
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let revision =
            GitRevision::parse(head.trim()).map_err(|error| Self::failure(work_ref, error))?;
        if revision.as_str() == base {
            return Err(Self::failure(
                work_ref,
                "the agent produced no new commit: HEAD is still the managed state",
            ));
        }
        let tree = GitTree::parse(tree.trim()).map_err(|error| Self::failure(work_ref, error))?;
        Ok(Candidate {
            revision,
            tree,
            transport_digest: TransportDigest::of(&candidate_bundle),
            bundle: candidate_bundle,
        })
    }

    /// Build the bundle of the current managed branch: the state the new
    /// conversation starts from.
    async fn build_bundle(&self, work_ref: &str) -> Result<String, AgentError> {
        let path = self.config.bundle_path();
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await.map_err(|error| {
                Self::failure(work_ref, format!("the bundle directory: {error}"))
            })?;
        }
        managed_bundle(
            &self.config.project_dir,
            &path,
            &self.config.managed_branch,
            work_ref,
        )
    }

    /// Pack the pinned ACP adapter on the host and stage it in the
    /// sandbox: the sandbox has no egress for npm.
    async fn stage_adapter(
        &self,
        session: &SandboxSession,
        work_ref: &str,
    ) -> Result<(), AgentError> {
        let install_dir = std::env::temp_dir().join(ADAPTER_PACKAGE_DIR);
        let adapter = install_dir.join("node_modules/.bin/claude-code-acp");
        if !adapter.exists() {
            let status = tokio::process::Command::new("npm")
                .args([
                    "install",
                    "--prefix",
                    install_dir.to_string_lossy().as_ref(),
                    "--no-audit",
                    "--no-fund",
                    ADAPTER_PACKAGE,
                ])
                .status()
                .await
                .map_err(|error| Self::failure(work_ref, format!("npm: {error}")))?;
            if !status.success() {
                return Err(Self::failure(
                    work_ref,
                    "the ACP adapter could not be installed",
                ));
            }
        }
        let archive = std::env::temp_dir().join(ADAPTER_ARCHIVE);
        let status = tokio::process::Command::new("tar")
            .args([
                "czf",
                archive.to_string_lossy().as_ref(),
                "-C",
                install_dir.to_string_lossy().as_ref(),
                "node_modules",
            ])
            .status()
            .await
            .map_err(|error| Self::failure(work_ref, format!("tar: {error}")))?;
        if !status.success() {
            return Err(Self::failure(
                work_ref,
                "the adapter archive could not be built",
            ));
        }
        let bytes = tokio::fs::read(&archive)
            .await
            .map_err(|error| Self::failure(work_ref, format!("the adapter archive: {error}")))?;
        session
            .upload(bytes, "/tmp/ccacp.tgz")
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        session
            .run_checked(&format!(
                "mkdir -p {ADAPTER_ROOT} && tar xzf /tmp/ccacp.tgz -C {ADAPTER_ROOT} && test -x {ADAPTER_ROOT}/node_modules/.bin/claude-code-acp"
            ))
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        Ok(())
    }

    /// The command that starts the ACP adapter.
    ///
    /// It carries the credentials, so it is NEVER logged: the detector
    /// proves the command is secret-bearing (see the tests), which is why
    /// it travels into the session and nowhere else.
    pub fn agent_command(&self) -> String {
        let base_prefix = self
            .credentials
            .base_url
            .as_ref()
            .map(|base| {
                format!(
                    "ANTHROPIC_BASE_URL={} ",
                    crate::probe_util::shell_quote(base)
                )
            })
            .unwrap_or_default();
        format!(
            "PATH={ADAPTER_ROOT}/node_modules/.bin:$PATH HTTPS_PROXY={proxy} {base_prefix}ANTHROPIC_AUTH_TOKEN={token} claude-code-acp",
            proxy = crate::probe_util::shell_quote(&self.config.proxy_url),
            token = crate::probe_util::shell_quote(&self.credentials.token),
        )
    }
}

#[async_trait::async_trait]
impl AgentPort for AcpAgent {
    async fn execute(&self, objective: &Objective) -> Result<Candidate, AgentError> {
        let control = ControlPlane::new(&self.config.server);
        let session = SandboxSession::create(&control, &self.config)
            .await
            .map_err(|error| Self::failure(&objective.work_ref, error))?;
        let sandbox = session.sandbox_id().to_string();
        let result = self.work(&session, objective).await;
        if let Err(error) = session.close().await {
            // The sandbox stays journaled as a residue; the candidate (if
            // any) was already observed inside it and still stands.
            eprintln!("sandbox {sandbox} was not released: {error}");
        }
        result
    }
}

/// Build the bundle of the managed branch and return the base commit it
/// carries.
///
/// The base is read from the MANAGED branch's ref inside the bundle: that
/// is the state the attempt starts from. The `main` branch a sandbox
/// export happens to name is a different fact, and it is not in this
/// bundle at all.
fn managed_bundle(
    project_dir: &Path,
    bundle_path: &Path,
    managed_branch: &str,
    work_ref: &str,
) -> Result<String, AgentError> {
    let failure = |reason: String| AgentError {
        work_ref: work_ref.to_string(),
        reason,
    };
    let reference = format!("refs/heads/{managed_branch}");
    hodei_mh_workspace::git::create_bundle(project_dir, bundle_path, &reference)
        .map_err(|error| failure(error.to_string()))?;
    promote::git::candidate_head(bundle_path, managed_branch)
        .map_err(|error| failure(error.to_string()))
}

/// The command that recreates the managed state inside the sandbox.
fn prepare_command(managed_branch: &str) -> String {
    format!(
        "git init -q -b main {WORKTREE} && git -C {WORKTREE} fetch -q {INCOMING_BUNDLE} '+refs/heads/*:refs/remotes/bundle/*' && git -C {WORKTREE} checkout -q -f -B main refs/remotes/bundle/{managed_branch}"
    )
}

/// The command that exports the candidate as a bundle.
fn export_command() -> String {
    format!("git -C {WORKTREE} bundle create {OUTGOING_BUNDLE} --all")
}

/// The instruction the agent receives: the objective, and the mechanics
/// of the sandbox it works in.
fn instruction_for(instruction: &str) -> String {
    format!(
        "{}\n\nWork inside {WORKTREE}. Commit your changes with git (user hodei-mh <hodei-mh@local>). Do not push.",
        instruction.trim()
    )
}

/// What one ACP conversation produced.
pub struct AcpTurn {
    pub completed: bool,
    pub summary: String,
}

impl AcpTurn {
    /// Read the answer of the prompt turn.
    ///
    /// The turn is COMPLETED only when the agent says it ended it. A
    /// refusal or a cancellation answers the prompt too, but the work was
    /// not done: promoting it would publish what the agent declined to
    /// produce. The stop reason is always kept in the summary, so the
    /// record says which of the two happened.
    fn of_prompt_result(result: &Value, text_len: usize) -> Self {
        let stop_reason = result
            .get("stopReason")
            .and_then(Value::as_str)
            .unwrap_or("no stop reason");
        Self {
            completed: stop_reason == END_TURN,
            summary: format!("{stop_reason}, text_len={text_len}"),
        }
    }
}

/// Drive one ACP conversation: initialize, open a session, send the
/// prompt, answer the permission requests by protocol and wait for the
/// end of the turn.
pub async fn drive_agent(
    pipe: &mut PipeSession,
    instruction: &str,
    cwd: &str,
    timeout: Duration,
    permission: PermissionMode,
) -> AcpTurn {
    let mut assembler = AcpAssembler::new();
    let mut next_id: u64 = 10;
    let mut agent_text = String::new();
    let _ = pipe
        .send_stdin(
            opensandbox::acp::encode_request(
                next_id,
                "initialize",
                json!({"protocolVersion": 1, "clientCapabilities": {}}),
            )
            .as_bytes(),
        )
        .await;
    next_id += 1;
    let deadline = Instant::now() + timeout;
    let mut phase = 0u8;
    let mut session_id = json!(null);
    let mut turn = AcpTurn {
        completed: false,
        summary: String::new(),
    };
    while Instant::now() < deadline {
        let Some(event) = tokio::time::timeout(Duration::from_secs(10), pipe.next_event())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        match event {
            ServerEvent::Stdout(bytes) => {
                for message in assembler.push(&bytes).unwrap_or_default() {
                    if message.get("method").and_then(|m| m.as_str())
                        == Some("session/request_permission")
                    {
                        let answer = permission_answer(&message, permission);
                        let _ = pipe
                            .send_stdin(
                                opensandbox::acp::encode_response(
                                    message.get("id").cloned().unwrap_or(json!(0)),
                                    answer,
                                )
                                .as_bytes(),
                            )
                            .await;
                    }
                    if let Some(chunk) = message["params"]["update"]["content"]["text"].as_str()
                        && agent_text.len() < 3000
                    {
                        agent_text.push_str(chunk);
                    }
                    let is_result = message.get("result").is_some();
                    if is_result && phase == 0 {
                        let _ = pipe
                            .send_stdin(
                                opensandbox::acp::encode_request(
                                    next_id,
                                    "session/new",
                                    json!({"cwd": cwd, "mcpServers": []}),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 1;
                    } else if is_result
                        && message["result"].get("sessionId").is_some()
                        && phase == 1
                    {
                        session_id = message["result"]["sessionId"].clone();
                        let _ = pipe
                            .send_stdin(
                                opensandbox::acp::encode_request(
                                    next_id,
                                    "session/prompt",
                                    json!({
                                        "sessionId": session_id.clone(),
                                        "prompt": [{"type": "text", "text": instruction}],
                                    }),
                                )
                                .as_bytes(),
                            )
                            .await;
                        next_id += 1;
                        phase = 2;
                    } else if is_result
                        && phase == 2
                        && message["result"].get("stopReason").is_some()
                    {
                        return AcpTurn::of_prompt_result(&message["result"], agent_text.len());
                    }
                }
            }
            ServerEvent::Exit { .. } => break,
            _ => {}
        }
    }
    turn.summary = format!(
        "timeout, text_tail={:?}",
        agent_text.chars().rev().take(200).collect::<String>()
    );
    turn
}

/// The answer to a permission request, by the configured mode.
fn permission_answer(message: &Value, permission: PermissionMode) -> Value {
    match permission {
        PermissionMode::AllowOnce => {
            let option = message["params"]["options"]
                .as_array()
                .and_then(|options| {
                    options
                        .iter()
                        .find(|option| option["kind"].as_str() == Some("allow_once"))
                })
                .and_then(|option| option["optionId"].as_str())
                .unwrap_or_default();
            json!({"outcome": {"outcome": "selected", "optionId": option}})
        }
        PermissionMode::Deny => json!({"outcome": {"outcome": "cancelled"}}),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::process::Command;

    fn config() -> LiveConfig {
        LiveConfig {
            project: "project".into(),
            server: "http://127.0.0.1:8081".into(),
            image: "example/image:1".into(),
            proxy_url: "http://127.0.0.1:8888".into(),
            project_dir: PathBuf::from("/tmp/project"),
            tracker_dir: PathBuf::from("/tmp/project"),
            managed_branch: "hodei-mh/main".into(),
            permission: PermissionMode::AllowOnce,
        }
    }

    #[test]
    fn the_agent_command_is_recognised_as_secret_bearing_and_is_never_logged() {
        let agent = AcpAgent::new(
            config(),
            Credentials::new("sk-ant-test-token", Some("http://model.local".into())),
        );
        let command = agent.agent_command();
        assert_eq!(
            profiles::credentials::secret_leak_positions(&[command]),
            vec![0],
            "the command carries credentials: it must never reach a log"
        );
        assert!(format!("{:?}", agent.credentials).contains("<redacted>"));
    }

    #[test]
    fn the_prepared_worktree_is_checked_out_from_the_managed_branch() {
        let command = prepare_command("hodei-mh/main");
        assert!(command.contains("refs/remotes/bundle/hodei-mh/main"));
        assert!(command.contains("checkout -q -f -B main"));
    }

    #[test]
    fn the_instruction_keeps_the_objective_and_states_the_worktree() {
        let instruction = instruction_for("do the work");
        assert!(instruction.starts_with("do the work"));
        assert!(instruction.contains(WORKTREE));
    }

    #[test]
    fn a_denied_permission_is_answered_with_a_cancellation() {
        let message = json!({"params": {"options": [{"kind": "allow_once", "optionId": "a"}]}});
        assert_eq!(
            permission_answer(&message, PermissionMode::Deny),
            json!({"outcome": {"outcome": "cancelled"}})
        );
        assert_eq!(
            permission_answer(&message, PermissionMode::AllowOnce)["outcome"]["optionId"],
            json!("a")
        );
    }

    #[test]
    fn only_an_end_turn_completes_the_turn() {
        let ended = AcpTurn::of_prompt_result(&json!({"stopReason": "end_turn"}), 42);
        assert!(ended.completed);
        assert_eq!(ended.summary, "end_turn, text_len=42");

        // A refusal (what `--permission deny` produces) or a cancellation
        // answers the prompt, but the work was not done.
        for reason in ["refusal", "cancelled"] {
            let turn = AcpTurn::of_prompt_result(&json!({"stopReason": reason}), 42);
            assert!(!turn.completed, "{reason} is not a completed turn");
            assert!(
                turn.summary.contains(reason),
                "the actual stop reason is kept: {}",
                turn.summary
            );
        }
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git is available");
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn fixture_repo(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("hodei-live-agent-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        hodei_mh_workspace::git::init_fixture_repo(&dir).expect("the fixture repository");
        dir
    }

    fn commit_file(dir: &Path, name: &str, content: &str, message: &str) -> String {
        std::fs::write(dir.join(name), content).expect("the fixture file");
        git(dir, &["add", "-A"]);
        hodei_mh_workspace::git::commit_index(dir, message).expect("the fixture commits")
    }

    #[test]
    fn the_base_is_read_from_the_managed_branch_inside_the_bundle() {
        let repo = fixture_repo("bundle-base");
        let managed = commit_file(&repo, "managed.txt", "managed\n", "managed state");
        let main = commit_file(&repo, "main.txt", "main\n", "main moved on");
        assert_ne!(managed, main);
        // The managed branch stays on the earlier commit: the two heads
        // name different states, and the bundle only carries the managed
        // one.
        git(&repo, &["update-ref", "refs/heads/hodei-mh/main", &managed]);

        let bundle = repo.join("candidate.bundle");
        let base = managed_bundle(&repo, &bundle, "hodei-mh/main", "TASK-1")
            .expect("the base of the managed branch");
        assert_eq!(base, managed, "the base is the managed state");
        assert_ne!(base, main, "the base is not the main branch");

        let _ = std::fs::remove_dir_all(&repo);
    }
}
