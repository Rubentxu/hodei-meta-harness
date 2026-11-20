//! The agent of the live pipeline: one fresh sandbox, one fresh ACP
//! conversation, one candidate commit.
//!
//! The sandbox is created for the attempt alone and released on every
//! path. The candidate is observed WHERE IT LIVES — the bundle, the head
//! and the tree are read before the sandbox goes away — so the
//! verification judges the very state the agent left. The commit itself
//! is authored by the harness, with the host identity, so no agent
//! attribution reaches the promoted history.

use std::fs::OpenOptions;
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use interact::policy::InteractionPolicy;
use interact::request::{InteractionRequest, PermissionOption, ToolKind};
use interact::resolver::{GrantSource, Resolution, resolve};
use opensandbox::acp::{AcpAssembler, METHOD_NOT_FOUND, encode_error};
use opensandbox::control::ControlPlane;
use opensandbox::execd::frames::ServerEvent;
use opensandbox::execd::pipe::{PipeError, PipeSession};
use serde_json::{Value, json};
use verify::git::{GitRevision, GitTree};
use verify::runner::Candidate;
use verify::transport::TransportDigest;

use runner::ports::{AgentError, AgentPort, Objective};

use crate::live::checkpoint::{CandidateRef, LiveCheckpoints};
use crate::live::session::{AgentHandshake, SandboxSession};
use crate::live::{GitIdentity, LiveConfig, LiveError, PermissionMode};

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

/// The subdirectory of the harness state that holds the staged adapters.
const ADAPTER_STATE_DIR: &str = "adapters";

/// How long the archive of the adapter lives: it is removed as soon as it
/// is read.
struct AdapterArchive {
    path: PathBuf,
}

impl AdapterArchive {
    /// Create a uniquely named archive that nobody else can take over.
    ///
    /// The name carries the process, the time and a counter, and the file
    /// is opened with `O_EXCL` and mode `0600`: a name somebody prepared
    /// beforehand is skipped, never written through.
    fn create(directory: &Path) -> std::io::Result<Self> {
        static ATTEMPT: AtomicU64 = AtomicU64::new(0);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|elapsed| elapsed.as_nanos())
            .unwrap_or_default();
        for _ in 0..16 {
            let name = format!(
                "adapter-{}-{nanos}-{}.tgz",
                std::process::id(),
                ATTEMPT.fetch_add(1, Ordering::Relaxed)
            );
            let path = directory.join(name);
            match OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("no free adapter archive name in {}", directory.display()),
        ))
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for AdapterArchive {
    /// The archive never outlives the staging that created it, whichever
    /// way that staging ends.
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The directory the pinned adapter is installed in: private state of this
/// user, under the harness state.
///
/// It is NOT a predictable path in the world-writable temporary directory:
/// an adapter staged from a path any local user could prepare is an
/// adapter they could replace.
fn adapter_install_dir() -> std::io::Result<PathBuf> {
    let root = hodei_mh_coordinator::paths::state_root();
    let adapters = root.join(ADAPTER_STATE_DIR);
    let owned = adapters.join(supervisor::paths::current_uid().to_string());
    for directory in [root.as_path(), adapters.as_path(), owned.as_path()] {
        ensure_private_dir(directory)?;
    }
    Ok(owned)
}

/// Make `directory` a directory only this user can enter, refusing a
/// symbolic link and a directory owned by somebody else.
///
/// The mode of a directory says nothing about who can replace what is
/// inside it; the owner does. A symbolic link is refused BEFORE anything
/// is written through it.
fn ensure_private_dir(directory: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(directory) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "{} is a symbolic link: refusing to stage the adapter through it",
                    directory.display()
                ),
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    supervisor::paths::private_dir(directory)?;
    let meta = std::fs::symlink_metadata(directory)?;
    if !meta.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("{} is not a directory", directory.display()),
        ));
    }
    if meta.uid() != supervisor::paths::current_uid() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "{} belongs to uid {}, not to uid {}",
                directory.display(),
                meta.uid(),
                supervisor::paths::current_uid()
            ),
        ));
    }
    Ok(())
}

/// How long one ACP conversation may take. The turn carries the whole
/// objective — model latency, tool calls and all — so a conversation that
/// has not ended its turn by now has no result left to wait for: the
/// attempt ends and the sandbox goes away. Fixed on purpose; there is no
/// per-run override.
const AGENT_TIMEOUT: Duration = Duration::from_secs(180);

/// How many characters of agent text a timeout summary keeps. The tail is
/// the last thing the agent said before it stalled, so it is the part of
/// the record that helps the reader.
const TEXT_TAIL_LIMIT: usize = 200;

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
    checkpoints: LiveCheckpoints,
}

impl AcpAgent {
    pub fn new(config: LiveConfig, credentials: Credentials) -> Self {
        let checkpoints = config.checkpoints();
        Self {
            config,
            credentials,
            checkpoints,
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
        let author = self.config.author.as_ref().ok_or_else(|| {
            Self::failure(
                work_ref,
                "the host git identity is not wired: the harness authors the candidate commit, so it must be resolved before the agent runs",
            )
        })?;
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
        let wrapper_path = adapter_wrapper_path();
        let credential_line = self.credential_line().ok_or_else(|| {
            Self::failure(
                work_ref,
                "the credential cannot travel as one stdin line: it carries a tab or a line break",
            )
        })?;
        let handshake = AgentHandshake {
            wrapper_path: &wrapper_path,
            wrapper_script: AcpAgent::wrapper_script(),
            credential_line: &credential_line,
        };
        let (mut conversation, agent_session) = session
            .attach_agent(&command, WORKTREE, &handshake)
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        // The record and its handoff were written BEFORE the claim, by
        // the durable attempt port: a crash from here on leaves a
        // pending checkpoint the boot classifies as uncertain.
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
            let interactions = turn.interactions.join("; ");
            let interactions_part = if interactions.is_empty() {
                String::new()
            } else {
                format!("; interactions: {interactions}")
            };
            let detail = match &turn.refusal {
                // The refusal travels with the failure: whoever reads the
                // record sees the structured event, not only its prose.
                Some(event) => format!("{}: {event}{interactions_part}", turn.summary),
                None if turn.summary.starts_with("timeout") => format!(
                    "{}{interactions_part}: the agent did not finish its turn within AGENT_TIMEOUT ({}s); retry or shorten the task",
                    turn.summary,
                    AGENT_TIMEOUT.as_secs()
                ),
                None => format!("{}{interactions_part}", turn.summary),
            };
            return Err(Self::failure(
                work_ref,
                format!("the agent did not finish its turn: {detail}"),
            ));
        }
        // The harness authors the candidate commit: whatever the agent
        // committed on its own is squashed into one commit carrying the
        // host identity and a policy-neutral message.
        let trees = session
            .run_checked(&staged_trees_command(WORKTREE, &base))
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
        let (index_tree, base_tree) = staged_trees(&trees)
            .ok_or_else(|| Self::failure(work_ref, "the staged worktree named no tree"))?;
        if let Some(refusal) = unchanged_stage(work_ref, index_tree, base_tree) {
            return Err(refusal);
        }
        session
            .run_checked(&commit_command(WORKTREE, &base, author, work_ref))
            .await
            .map_err(|error| Self::failure(work_ref, error))?;
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
        let tree = GitTree::parse(tree.trim()).map_err(|error| Self::failure(work_ref, error))?;
        // The candidate is recorded while the phase stays Pending: only the
        // publication settles it.
        self.checkpoints
            .observe(CandidateRef {
                base: base.clone(),
                head: head.trim().to_string(),
                bundle_digest: TransportDigest::of(&candidate_bundle).as_str().to_string(),
            })
            .map_err(|error| Self::failure(work_ref, error))?;
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
        let install_dir = adapter_install_dir().map_err(|error| {
            Self::failure(work_ref, format!("the private adapter directory: {error}"))
        })?;
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
        let archive = AdapterArchive::create(&install_dir)
            .map_err(|error| Self::failure(work_ref, format!("the adapter archive: {error}")))?;
        let status = tokio::process::Command::new("tar")
            .args([
                "czf",
                archive.path().to_string_lossy().as_ref(),
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
        let bytes = tokio::fs::read(archive.path())
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
    /// It carries NO credential: the token reaches the adapter through
    /// the stdin handshake that [`AcpAgent::wrapper_script`] consumes,
    /// so the create_session body, the sandbox process table and every
    /// log or error built from this command stay secret-free.
    pub fn agent_command(&self) -> String {
        format!(
            "PATH={ADAPTER_ROOT}/node_modules/.bin:$PATH HTTPS_PROXY={proxy} sh {wrapper} claude-code-acp",
            proxy = crate::probe_util::shell_quote(&self.config.proxy_url),
            wrapper = crate::probe_util::shell_quote(&adapter_wrapper_path()),
        )
    }

    /// The single stdin line the wrapper consumes: the base URL (empty
    /// when unset) and the token, separated by one tab and terminated
    /// by exactly one newline.
    ///
    /// The line IS the whole credential transport. A token or base URL
    /// carrying a tab or a line break would split the framing and hand
    /// half a secret to the adapter, so it is refused instead of
    /// truncated.
    pub fn credential_line(&self) -> Option<String> {
        let base = self.credentials.base_url.as_deref().unwrap_or_default();
        let token = self.credentials.token.as_str();
        (!base.contains(['\n', '\r', '\t']) && !token.contains(['\n', '\r', '\t']))
            .then(|| format!("{base}\t{token}\n"))
    }

    /// The POSIX wrapper staged beside the adapter inside the sandbox.
    ///
    /// It reads exactly ONE stdin line (the credential handshake),
    /// exports it, and execs the adapter, so the token lives only in the
    /// environment of the agent's process and never in an argv. A POSIX
    /// `read` consumes its line without buffering the rest of the pipe,
    /// so the ACP stream that follows reaches the exec'd adapter whole.
    pub fn wrapper_script() -> &'static str {
        r#"#!/bin/sh
IFS="$(printf '\t')" read -r base token
if [ -z "$token" ]; then
  exit 1
fi
ANTHROPIC_AUTH_TOKEN="$token"
export ANTHROPIC_AUTH_TOKEN
if [ -n "$base" ]; then
  ANTHROPIC_BASE_URL="$base"
  export ANTHROPIC_BASE_URL
fi
exec "$@"
"#
    }
}

/// Where the credential wrapper travels inside the sandbox: the staged
/// adapter directory, private by construction since its staging.
fn adapter_wrapper_path() -> String {
    format!("{ADAPTER_ROOT}/run-agent.sh")
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

/// The command that stages the worktree and prints the index tree and the
/// base tree, so the two can be compared without leaving the sandbox.
fn staged_trees_command(worktree: &str, base: &str) -> String {
    format!(
        "git -C {worktree} add -A && git -C {worktree} write-tree && git -C {worktree} rev-parse {base_tree}",
        worktree = crate::probe_util::shell_quote(worktree),
        base_tree = crate::probe_util::shell_quote(&format!("{base}^{{tree}}")),
    )
}

/// The two trees printed by [`staged_trees_command`].
fn staged_trees(output: &str) -> Option<(&str, &str)> {
    let mut trees = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    Some((trees.next()?, trees.next()?))
}

/// The refusal when the staged worktree is still the base state, so there
/// is nothing to promote.
fn unchanged_stage(work_ref: &str, index_tree: &str, base_tree: &str) -> Option<AgentError> {
    (index_tree.trim() == base_tree.trim()).then(|| AgentError {
        work_ref: work_ref.to_string(),
        reason: "the agent produced no changes to the managed state: the staged worktree is still the base tree".to_string(),
    })
}

/// The command that records the final state of the worktree as ONE commit
/// on top of the base, authored by the harness with the host identity.
///
/// The soft reset drops any commit the agent made on its own without
/// touching the worktree, so its attribution and trailers never reach the
/// promoted history.
fn commit_command(worktree: &str, base: &str, author: &GitIdentity, work_ref: &str) -> String {
    let message = format!("chore(work): verified work for {work_ref}");
    format!(
        "git -C {worktree} add -A && git -C {worktree} reset -q --soft {base} && git -C {worktree} -c user.name={name} -c user.email={email} commit -q -m {message}",
        worktree = crate::probe_util::shell_quote(worktree),
        base = crate::probe_util::shell_quote(base),
        name = crate::probe_util::shell_quote(&author.name),
        email = crate::probe_util::shell_quote(&author.email),
        message = crate::probe_util::shell_quote(&message),
    )
}

/// The instruction the agent receives: the objective, and the mechanics
/// of the sandbox it works in.
fn instruction_for(instruction: &str) -> String {
    format!(
        "{}\n\nWork inside {WORKTREE}: edit the files there. Do not commit: the harness records the final state of the worktree as the candidate commit. Do not add Co-authored-by or any other attribution trailers, and do not push.",
        instruction.trim()
    )
}

/// What one ACP conversation produced.
pub struct AcpTurn {
    pub completed: bool,
    pub summary: String,
    /// How the client answered the interactions the agent raised. Every
    /// decision is recorded with its reason, so a refusal is never silent.
    pub interactions: Vec<String>,
    /// The structured event of an interaction the POLICY refused, when
    /// the turn ended on that refusal. A denial is a verdict, not a
    /// stall: the record says which tool, which rule, and what stopped.
    pub refusal: Option<Value>,
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
            interactions: Vec::new(),
            refusal: None,
        }
    }
}

/// The pipe one ACP conversation travels on: server events in, stdin out.
/// Production is the execd pipe; the offline tests script it, so the
/// refusals and the timeouts can be observed without a container.
#[async_trait::async_trait]
pub trait AcpPipe: Send {
    /// The next server event, or `None` once the pipe is closed.
    async fn next_event(&mut self) -> Option<ServerEvent>;

    /// Write bytes to the agent's stdin.
    async fn send_stdin(&self, data: &[u8]) -> Result<(), PipeError>;
}

#[async_trait::async_trait]
impl AcpPipe for PipeSession {
    async fn next_event(&mut self) -> Option<ServerEvent> {
        PipeSession::next_event(self).await
    }

    async fn send_stdin(&self, data: &[u8]) -> Result<(), PipeError> {
        PipeSession::send_stdin(self, data).await
    }
}

/// The last `limit` characters of `text`, in reading order.
///
/// The tail of a reversed iterator must be reversed back before it is
/// printed: an error record that reads "L .noitaerc" tells the reader
/// nothing about what the agent was doing.
fn readable_tail(text: &str, limit: usize) -> String {
    let characters: Vec<char> = text.chars().collect();
    let start = characters.len().saturating_sub(limit);
    characters[start..].iter().collect()
}

/// Drive one ACP conversation: initialize, open a session, send the
/// prompt, answer the permission requests by protocol and wait for the
/// end of the turn.
///
/// A permission the POLICY refuses ends the conversation AT ONCE with a
/// refusal: the agent cannot do the work the policy just denied, and
/// waiting out the timeout would hide that verdict behind a "timeout".
pub async fn drive_agent(
    pipe: &mut dyn AcpPipe,
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
    let mut interactions: Vec<String> = Vec::new();
    let mut turn = AcpTurn {
        completed: false,
        summary: String::new(),
        interactions: Vec::new(),
        refusal: None,
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
                    if let Some((response, detail, refusal)) =
                        interaction_answer(&message, permission)
                    {
                        interactions.push(detail);
                        let _ = pipe.send_stdin(response.as_bytes()).await;
                        if let Some(event) = refusal {
                            // The denial is a fact worth a log line of its
                            // own: emit it structured, then end the turn.
                            eprintln!("{event}");
                            let tool = event["tool"].as_str().unwrap_or("unknown tool").to_string();
                            turn.summary = format!(
                                "the permission policy refused {tool:?}: the attempt ends as a refusal"
                            );
                            turn.interactions = interactions;
                            turn.refusal = Some(event);
                            return turn;
                        }
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
                        turn = AcpTurn::of_prompt_result(&message["result"], agent_text.len());
                        turn.interactions = interactions;
                        return turn;
                    }
                }
            }
            ServerEvent::Exit { .. } => break,
            _ => {}
        }
    }
    turn.summary = format!(
        "timeout after {}s, text_tail={:?}",
        timeout.as_secs(),
        readable_tail(&agent_text, TEXT_TAIL_LIMIT)
    );
    turn.interactions = interactions;
    turn
}

/// The interaction policy the configured permission mode applies.
pub fn policy_for(permission: PermissionMode) -> InteractionPolicy {
    match permission {
        PermissionMode::AllowOnce => InteractionPolicy::sandbox_first(),
        PermissionMode::Deny => InteractionPolicy::deny_all(),
    }
}

/// The tool kind an ACP permission request refers to, typed before any
/// decision: the protocol kind first, the tool name only as a fallback.
fn acp_tool_kind(message: &Value) -> ToolKind {
    let tool_call = &message["params"]["toolCall"];
    match tool_call["kind"].as_str().unwrap_or_default() {
        "read" | "search" => ToolKind::Read,
        "edit" | "delete" | "move" => ToolKind::Write,
        "execute" => ToolKind::Execute,
        "fetch" => ToolKind::Network,
        "" => ToolKind::from_tool_name(tool_call["title"].as_str().unwrap_or_default()),
        _ => ToolKind::Unknown,
    }
}

/// The typed request an ACP permission message describes.
fn permission_request(message: &Value) -> InteractionRequest {
    let options = message["params"]["options"]
        .as_array()
        .map(|options| {
            options
                .iter()
                .map(|option| PermissionOption {
                    option_id: option["optionId"].as_str().unwrap_or_default().to_string(),
                    name: option["name"].as_str().unwrap_or_default().to_string(),
                    kind: option["kind"].as_str().unwrap_or_default().to_string(),
                })
                .collect()
        })
        .unwrap_or_default();
    InteractionRequest::Permission {
        tool_name: message["params"]["toolCall"]["title"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        kind: acp_tool_kind(message),
        options,
    }
}

/// How a grant reads in the interaction record.
fn grant_label(source: Option<&GrantSource>) -> &'static str {
    match source {
        Some(GrantSource::PolicyRule) => "granted by the interaction policy",
        Some(GrantSource::SandboxRoute) => "routed to the sandbox",
        None => "granted",
    }
}

/// The encoded answer an interaction message needs, with its record and
/// the structured refusal event when the interaction was denied.
///
/// Only REQUESTS are answered: a response or a notification is not an
/// interaction. A request this client does not implement is answered with
/// an explicit error, so the agent never waits for a reply that would
/// never come.
pub fn interaction_answer(
    message: &Value,
    permission: PermissionMode,
) -> Option<(String, String, Option<Value>)> {
    let method = message.get("method").and_then(Value::as_str)?;
    let id = message.get("id").cloned()?;
    if method == "session/request_permission" {
        let (answer, detail, refusal) = permission_answer(message, permission);
        return Some((
            opensandbox::acp::encode_response(id, answer),
            detail,
            refusal,
        ));
    }
    let reason = format!(
        "{method} is not a supported interaction: this client answers permission requests only"
    );
    Some((encode_error(id, METHOD_NOT_FOUND, &reason), reason, None))
}

/// The option the agent offers for an `allow_once` answer.
fn allow_once_option(message: &Value) -> Option<&str> {
    message["params"]["options"]
        .as_array()?
        .iter()
        .find(|option| option["kind"].as_str() == Some("allow_once"))
        .and_then(|option| option["optionId"].as_str())
}

/// The ACP answer to a permission request, how it was decided, and the
/// structured refusal event when the POLICY denied it (`None` otherwise).
///
/// The decision comes from the typed interaction policy, never from the
/// handoff of another session; a request the policy does not cover is
/// refused with its reason instead of being answered on trust.
pub fn permission_answer(
    message: &Value,
    permission: PermissionMode,
) -> (Value, String, Option<Value>) {
    let request = permission_request(message);
    let resolution = resolve(&policy_for(permission), &request);
    let granted = match &resolution {
        Resolution::AutoGranted { source } => Ok(grant_label(Some(source))),
        Resolution::RoutedToSandbox => Ok(grant_label(Some(&GrantSource::SandboxRoute))),
        Resolution::HumanRequired { reason } => Err(reason.clone()),
        Resolution::Unsupported { reason } => Err(reason.clone()),
    };
    match granted {
        Ok(detail) => match allow_once_option(message) {
            Some(option) => (
                json!({"outcome": {"outcome": "selected", "optionId": option}}),
                format!("permission {detail}"),
                None,
            ),
            None => (
                json!({"outcome": {"outcome": "cancelled"}}),
                format!("permission refused: the request offers no allow_once option ({detail})"),
                Some(permission_refusal_event(message, permission)),
            ),
        },
        Err(reason) => (
            json!({"outcome": {"outcome": "cancelled"}}),
            format!("permission refused: {reason}"),
            Some(permission_refusal_event(message, permission)),
        ),
    }
}

/// The structured event of a denied interaction: the tool that was asked
/// for, the rule that refused it, and the effect that stops. A consumer
/// must be able to tell "the policy refused" from "the model was slow".
fn permission_refusal_event(message: &Value, permission: PermissionMode) -> Value {
    let tool = message["params"]["toolCall"]["title"]
        .as_str()
        .or_else(|| message["params"]["toolCall"]["kind"].as_str())
        .unwrap_or("unknown tool");
    json!({
        "event": "agent.permission_refused",
        "tool": tool,
        "policy": match permission {
            PermissionMode::Deny => "deny",
            PermissionMode::AllowOnce => "allow_once",
        },
        "effect": "the attempt ends as a refusal before the agent can continue",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;
    use std::process::Command;
    use std::sync::{Arc, Mutex};

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
            author: Some(GitIdentity {
                name: "Harness Author".into(),
                email: "harness@example.com".into(),
            }),
        }
    }

    fn harness_identity() -> GitIdentity {
        GitIdentity {
            name: "Harness Author".into(),
            email: "harness@example.com".into(),
        }
    }

    #[test]
    fn the_launch_command_carries_no_credential_in_its_body_or_its_views() {
        let agent = AcpAgent::new(
            config(),
            Credentials::new("sk-ant-test-token", Some("http://model.local".into())),
        );
        let command = agent.agent_command();
        assert_eq!(
            profiles::credentials::secret_leak_positions(std::slice::from_ref(&command)),
            Vec::<usize>::new(),
            "the command is no longer recognised as secret-bearing: {command}"
        );
        assert!(
            !command.contains("ANTHROPIC_AUTH_TOKEN=") && !command.contains("sk-ant-test-token"),
            "no credential name and no credential value in the command: {command}"
        );
        assert!(
            command.contains("run-agent.sh"),
            "the command starts the credential wrapper: {command}"
        );
        assert!(
            command.contains("claude-code-acp"),
            "and the adapter is still the process that answers the ACP stream"
        );
        // The persisted request body: the exact bytes the daemon keeps
        // for the session, not only the shell string.
        let body = serde_json::to_string(&opensandbox::execd::api::CreateSessionRequest {
            cwd: Some(WORKTREE),
            command: Some(&command),
        })
        .expect("the create_session request serializes");
        assert!(
            profiles::credentials::secret_leak_positions(std::slice::from_ref(&body)).is_empty(),
            "the create_session body carries no credential: {body}"
        );
        // The views an error or a log may print.
        assert!(format!("{:?}", agent.credentials).contains("<redacted>"));
        let error = crate::live::session::SessionError::Uncertain { command };
        assert!(
            !error.to_string().contains("sk-ant-test-token")
                && !format!("{error:?}").contains("sk-ant-test-token"),
            "the failure surface of a command repeats no credential"
        );
    }

    #[test]
    fn the_credential_line_frames_exactly_one_stdin_handshake() {
        let agent = AcpAgent::new(
            config(),
            Credentials::new("sk-ant-test-token", Some("http://model.local".into())),
        );
        assert_eq!(
            agent.credential_line().as_deref(),
            Some("http://model.local\tsk-ant-test-token\n"),
            "the base URL comes first: the token keeps the whole tail of the line"
        );
        let no_base = AcpAgent::new(config(), Credentials::new("tok-2", None));
        assert_eq!(no_base.credential_line().as_deref(), Some("\ttok-2\n"));
        // A secret that breaks the one-line framing is refused, never truncated.
        for broken in ["bad\ntoken", "bad\ttoken", "bad\r\ntoken"] {
            let agent = AcpAgent::new(config(), Credentials::new(broken, None));
            assert_eq!(
                agent.credential_line(),
                None,
                "a credential with a line break or a tab cannot travel this way"
            );
        }
    }

    /// Simulate the handshake over a real POSIX shell and a fake stdin
    /// channel: the wrapper, then exec, then the ACP bytes that follow.
    #[test]
    fn the_wrapper_hands_the_credential_to_the_child_environment_only() {
        let dir = staging_dir("wrapper");
        std::fs::create_dir_all(&dir).expect("the fixture directory");
        std::fs::write(dir.join("run-agent.sh"), AcpAgent::wrapper_script()).expect("the wrapper");
        // The stand-in adapter: it prints its argv and the two credential
        // variables of its environment, then EVERY remaining stdin byte.
        std::fs::write(
            dir.join("adapter.sh"),
            "#!/bin/sh\nprintf 'argv=%s\\n' \"$*\"\nprintf 'token=%s\\n' \"$ANTHROPIC_AUTH_TOKEN\"\nprintf 'base=%s\\n' \"${ANTHROPIC_BASE_URL:-unset}\"\ncat\n",
        )
        .expect("the adapter stand-in");
        let command = format!(
            "env -i PATH=/usr/bin:/bin HTTPS_PROXY=http://127.0.0.1:8888 sh {} sh {}",
            dir.join("run-agent.sh").display(),
            dir.join("adapter.sh").display(),
        );
        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&command)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("sh is available");
        {
            let mut stdin = child.stdin.take().expect("the fake stdin channel");
            stdin
                .write_all("http://model.local\tsk-ant-test-token\n".as_bytes())
                .expect("the credential line");
            stdin
                .write_all(b"{\"jsonrpc\":\"2.0\",\"method\":\"initialize\"}\n")
                .expect("the ACP stream");
        }
        let output = child.wait_with_output().expect("the adapter exits");
        assert!(output.status.success());
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(
            text.contains("token=sk-ant-test-token"),
            "the child environment carries the credential: {text}"
        );
        assert!(
            text.contains("base=http://model.local"),
            "and the gateway base URL too: {text}"
        );
        assert!(
            text.contains(r#"{"jsonrpc":"2.0","method":"initialize"}"#),
            "the ACP stream reaches the exec'd adapter whole: {text}"
        );
        let argv = text
            .lines()
            .find(|line| line.starts_with("argv="))
            .expect("the adapter printed its argv");
        assert!(
            !argv.contains("sk-ant-test-token"),
            "the credential never entered an argv: {argv}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_wrapper_script_itself_carries_no_credential_value() {
        let script = AcpAgent::wrapper_script();
        assert!(
            !script.contains("sk-"),
            "the staged script is static: only the stdin line carries values"
        );
        assert!(script.contains("exec"), "and it execs, it does not fork");
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
        // The harness authors the commit: the agent is told to leave the
        // history alone, and no agent identity is announced anywhere.
        assert!(instruction.contains("Do not commit"));
        assert!(instruction.contains("Co-authored-by"));
        assert!(!instruction.contains("hodei-mh@local"));
    }

    fn permission(kind: &str, title: &str, options: Value) -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "session/request_permission",
            "params": {"toolCall": {"kind": kind, "title": title}, "options": options},
        })
    }

    fn allow_once() -> Value {
        json!([
            {"optionId": "a", "name": "Allow once", "kind": "allow_once"},
            {"optionId": "r", "name": "Reject", "kind": "reject_once"},
        ])
    }

    /// The decision the client's interaction entry point makes: the
    /// outcome as it goes on the wire, the interaction record, and the
    /// structured refusal event when the policy denied the request.
    fn answered(message: &Value, mode: PermissionMode) -> (Value, String, Option<Value>) {
        let (response, detail, refusal) =
            interaction_answer(message, mode).expect("a request is answered");
        let envelope: Value = serde_json::from_str(&response).expect("an encoded response");
        (envelope["result"].clone(), detail, refusal)
    }

    #[test]
    fn an_ordinary_read_is_granted_by_the_policy_under_allow_once() {
        let (answer, detail, refusal) = answered(
            &permission("read", "Read", allow_once()),
            PermissionMode::AllowOnce,
        );
        assert_eq!(answer["outcome"]["optionId"], json!("a"));
        assert!(detail.contains("policy"), "{detail}");
        assert!(refusal.is_none(), "a grant refuses nothing");
    }

    #[test]
    fn a_write_is_routed_to_the_sandbox_and_still_answers_allow_once() {
        let (answer, detail, refusal) = answered(
            &permission("edit", "Edit", allow_once()),
            PermissionMode::AllowOnce,
        );
        assert_eq!(answer["outcome"]["optionId"], json!("a"));
        assert!(detail.contains("sandbox"), "{detail}");
        assert!(refusal.is_none(), "a routed grant refuses nothing");
    }

    #[test]
    fn a_reviewed_or_unknown_tool_is_refused_with_its_reason() {
        for (kind, title) in [("fetch", "WebFetch"), ("other", "mystery")] {
            let (answer, detail, refusal) = answered(
                &permission(kind, title, allow_once()),
                PermissionMode::AllowOnce,
            );
            assert_eq!(answer["outcome"]["outcome"], json!("cancelled"), "{kind}");
            assert!(detail.starts_with("permission refused:"), "{detail}");
            let event = refusal.expect("a policy denial is a refusal event");
            assert_eq!(event["event"], json!("agent.permission_refused"));
        }
    }

    #[test]
    fn the_deny_mode_refuses_every_kind_including_reads() {
        for kind in ["read", "edit", "execute", "fetch"] {
            let (answer, detail, refusal) = answered(
                &permission(kind, "tool", allow_once()),
                PermissionMode::Deny,
            );
            assert_eq!(answer["outcome"]["outcome"], json!("cancelled"), "{kind}");
            assert!(detail.contains("refused"), "{detail}");
            let event = refusal.expect("the deny mode is a refusal event");
            assert_eq!(event["event"], json!("agent.permission_refused"));
        }
    }

    #[test]
    fn a_grant_without_an_allow_once_option_is_an_explicit_refusal() {
        let options = json!([{"optionId": "x", "name": "Other", "kind": "allow_always"}]);
        let (answer, detail, refusal) = answered(
            &permission("read", "Read", options),
            PermissionMode::AllowOnce,
        );
        assert_eq!(answer["outcome"]["outcome"], json!("cancelled"));
        assert!(detail.contains("no allow_once option"), "{detail}");
        let event = refusal.expect("no grant to select is a refusal event");
        assert_eq!(event["event"], json!("agent.permission_refused"));
    }

    #[test]
    fn an_unsupported_interaction_is_answered_instead_of_stalling() {
        let request = json!({
            "jsonrpc": "2.0",
            "id": 8,
            "method": "session/request_form",
            "params": {},
        });
        let (response, detail, refusal) =
            interaction_answer(&request, PermissionMode::AllowOnce).expect("an answer");
        let value: Value = serde_json::from_str(&response).expect("encoded");
        assert_eq!(value["id"], json!(8));
        assert_eq!(value["error"]["code"], json!(METHOD_NOT_FOUND));
        assert!(detail.contains("not a supported interaction"), "{detail}");
        assert!(
            refusal.is_none(),
            "an unsupported interaction stalls nothing: it is answered with an error"
        );
        // Notifications and responses are not interactions.
        assert!(
            interaction_answer(
                &json!({"method": "session/update", "params": {}}),
                PermissionMode::AllowOnce
            )
            .is_none()
        );
        assert!(
            interaction_answer(
                &json!({"id": 3, "result": {"ok": true}}),
                PermissionMode::AllowOnce
            )
            .is_none()
        );
    }

    #[test]
    fn a_permission_decision_is_the_policy_and_never_a_handoff() {
        // The decision takes the request and the CLI mode only, so a grant
        // taken in one session cannot be inherited by the next one.
        let message = permission("read", "Read", allow_once());
        let (allowed, granted, _) = permission_answer(&message, PermissionMode::AllowOnce);
        assert_eq!(allowed["outcome"]["optionId"], json!("a"));
        let (refused, denial, _) = permission_answer(&message, PermissionMode::Deny);
        assert_eq!(refused["outcome"]["outcome"], json!("cancelled"));
        assert_ne!(
            granted, denial,
            "the deny mode re-decides, it does not inherit"
        );
        // And the same request decides the same way with no history.
        let (_, again, _) = permission_answer(&message, PermissionMode::AllowOnce);
        assert_eq!(granted, again);
    }

    #[test]
    fn a_denied_permission_is_answered_with_a_cancellation_and_a_refusal_event() {
        let message = json!({
            "jsonrpc": "2.0",
            "id": 21,
            "method": "session/request_permission",
            "params": {
                "toolCall": {"title": "write file", "kind": "edit"},
                "options": [{"kind": "allow_once", "optionId": "a"}]
            }
        });
        let (answer, _detail, refusal) = answered(&message, PermissionMode::Deny);
        assert_eq!(answer, json!({"outcome": {"outcome": "cancelled"}}));
        let event = refusal.expect("the denial is recorded as a refusal event");
        assert_eq!(event["event"], json!("agent.permission_refused"));
        assert_eq!(event["tool"], json!("write file"));
        assert_eq!(event["policy"], json!("deny"));

        let (answer, _detail, refusal) = answered(&message, PermissionMode::AllowOnce);
        assert_eq!(answer["outcome"]["optionId"], json!("a"));
        assert!(refusal.is_none(), "an allowed permission refuses nothing");
    }

    #[test]
    fn the_readable_tail_keeps_the_last_characters_in_order() {
        assert_eq!(
            readable_tail("hello", 200),
            "hello",
            "a short text is whole"
        );
        let text: String = (0..250).map(|i| (b'a' + (i % 26) as u8) as char).collect();
        let tail = readable_tail(&text, 200);
        assert_eq!(tail.len(), 200, "the tail is capped, not the whole text");
        assert_eq!(
            tail,
            text[text.len() - 200..],
            "the LAST characters, in order"
        );
        assert_ne!(
            tail,
            tail.chars().rev().collect::<String>(),
            "it reads forward: a mirrored tail tells the reader nothing"
        );
    }

    fn stdout(message: Value) -> ServerEvent {
        ServerEvent::Stdout(format!("{message}\n").into_bytes())
    }

    /// A scripted ACP pipe: the offline fake of the conversation surface.
    /// Every event is already there, so nothing waits on anything.
    struct FakePipe {
        script: std::collections::VecDeque<ServerEvent>,
        sent: Arc<Mutex<Vec<String>>>,
    }

    fn scripted(script: Vec<ServerEvent>) -> FakePipe {
        FakePipe {
            script: script.into(),
            sent: Arc::new(Mutex::new(Vec::new())),
        }
    }

    #[async_trait::async_trait]
    impl AcpPipe for FakePipe {
        async fn next_event(&mut self) -> Option<ServerEvent> {
            self.script.pop_front()
        }

        async fn send_stdin(&self, data: &[u8]) -> Result<(), PipeError> {
            self.sent
                .lock()
                .expect("the fake records stdin")
                .push(String::from_utf8_lossy(data).to_string());
            Ok(())
        }
    }

    fn hello_initialize() -> Vec<ServerEvent> {
        vec![
            stdout(json!({"jsonrpc": "2.0", "id": 1, "result": {"protocolVersion": 1}})),
            stdout(json!({"jsonrpc": "2.0", "id": 10, "result": {"sessionId": "s-1"}})),
        ]
    }

    #[tokio::test]
    async fn the_timeout_summary_carries_the_agent_text_tail_readable() {
        // 300 digits: the last 200 are unique enough that a mirrored or
        // middle-truncated tail cannot match them by accident.
        let text: String = (0..300)
            .map(|i| char::from_digit(i % 10, 10).unwrap())
            .collect();
        let tail: String = text
            .chars()
            .skip(text.chars().count() - TEXT_TAIL_LIMIT)
            .collect();
        assert_ne!(
            tail,
            tail.chars().rev().collect::<String>(),
            "the fixture tail is not a palindrome: a mirror cannot pass"
        );
        let mut pipe = scripted(
            hello_initialize()
                .into_iter()
                .chain([
                    stdout(json!({"params": {"update": {"content": {"text": text}}}})),
                    ServerEvent::Exit { exit_code: None },
                ])
                .collect(),
        );
        let turn = drive_agent(
            &mut pipe,
            "do it",
            WORKTREE,
            Duration::from_secs(60),
            PermissionMode::AllowOnce,
        )
        .await;
        assert!(!turn.completed);
        assert!(
            turn.summary.contains(&tail),
            "the last {TEXT_TAIL_LIMIT} characters appear in order: {:?}",
            &turn.summary[..turn.summary.len().min(120)]
        );
        let mirrored: String = tail.chars().rev().collect();
        assert!(
            !turn.summary.contains(&mirrored),
            "the tail is not printed reversed"
        );
        assert!(
            turn.summary.contains("timeout after 60s"),
            "the summary names the window it ran out of: {}",
            turn.summary
        );
    }

    #[tokio::test]
    async fn a_denied_permission_ends_the_turn_as_a_refusal_without_waiting() {
        let mut pipe = scripted(
            hello_initialize()
                .into_iter()
                .chain([
                    stdout(
                        json!({"jsonrpc": "2.0", "id": 5, "method": "session/request_permission",
                        "params": {
                            "toolCall": {"title": "run: git push", "kind": "execute"},
                            "options": [{"kind": "allow_once", "optionId": "a"}]}}),
                    ),
                    stdout(json!({"params": {"update": {"content": {"text": "still going"}}}})),
                    ServerEvent::Exit { exit_code: None },
                ])
                .collect(),
        );
        let turn = drive_agent(
            &mut pipe,
            "do it",
            WORKTREE,
            Duration::from_secs(60),
            PermissionMode::Deny,
        )
        .await;
        assert!(!turn.completed, "a refusal is not a completed turn");
        let event = turn.refusal.expect("the refusal is a structured event");
        assert_eq!(event["event"], json!("agent.permission_refused"));
        assert_eq!(event["tool"], json!("run: git push"));
        assert_eq!(event["policy"], json!("deny"));
        assert!(
            turn.summary.contains("permission policy refused")
                && turn.summary.contains("run: git push"),
            "the outcome text says the policy refused: {}",
            turn.summary
        );
        assert!(
            !turn.summary.contains("timeout"),
            "a refusal is never reported as a timeout: {}",
            turn.summary
        );
        let sent = pipe.sent.lock().expect("the fake records stdin");
        assert!(
            sent.iter()
                .any(|written| written.contains("\"outcome\":\"cancelled\"")),
            "the wire answer stays the protocol cancellation: {sent:?}"
        );
        assert_eq!(
            pipe.script.len(),
            2,
            "the turn returned AT the refusal: the rest of the script was never waited for"
        );
        // The record is complete on both exits: the refusal carries the
        // interaction it ended on, not only the structured event.
        assert_eq!(
            turn.interactions.len(),
            1,
            "the denied interaction is recorded: {:?}",
            turn.interactions
        );
        assert!(
            turn.interactions[0].starts_with("permission refused:"),
            "the record says the interaction was refused: {}",
            turn.interactions[0]
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

    /// Run a command string exactly as the sandbox shell would, in the
    /// fixture repository.
    ///
    /// The fixture runs a PLAIN git, like the sandbox does: the host may
    /// install a policy front-end as a shell script earlier on PATH, and
    /// that front-end does not judge the sandbox. The global and system
    /// git configuration are emptied for the same reason.
    fn run_shell(dir: &Path, command: &str) -> String {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let plain: Vec<PathBuf> = std::env::split_paths(&path)
            .filter(|directory| {
                let candidate = directory.join("git");
                !std::fs::read(&candidate)
                    .map(|bytes| bytes.starts_with(b"#!"))
                    .unwrap_or(false)
            })
            .collect();
        let plain = std::env::join_paths(&plain).unwrap_or(path);
        let output = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(dir)
            .env("PATH", plain)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .output()
            .expect("sh is available");
        assert!(
            output.status.success(),
            "sh -c {command}: {}",
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

    #[test]
    fn the_candidate_commit_quotes_identity_base_and_message() {
        let author = GitIdentity {
            name: "Rubén's Agent".into(),
            email: "ruben@example.com".into(),
        };
        let work_ref = "TASK-1's work";
        let command = commit_command(WORKTREE, "abc123", &author, work_ref);
        let message = "chore(work): verified work for TASK-1's work";
        assert!(
            command.contains(&crate::probe_util::shell_quote(message)),
            "the message is one quoted word: {command}"
        );
        assert!(
            command.contains(&crate::probe_util::shell_quote(&author.name)),
            "the identity name is quoted: {command}"
        );
        assert!(command.contains(&crate::probe_util::shell_quote(&author.email)));
        assert!(command.contains(&crate::probe_util::shell_quote("abc123")));
        assert!(command.contains(&crate::probe_util::shell_quote(WORKTREE)));
        // The single quote of the work reference never leaves its quotes.
        assert!(
            command.contains(r#"'chore(work): verified work for TASK-1'\''s work'"#),
            "{command}"
        );
    }

    #[test]
    fn the_harness_squashes_the_agent_commit_into_one_candidate_commit() {
        let repo = fixture_repo("squash");
        let base = commit_file(&repo, "managed.txt", "managed\n", "managed state");
        // The agent commits on its own, under its own identity and with
        // an attribution trailer.
        std::fs::write(repo.join("agent.txt"), "agent work\n").expect("the agent file");
        git(&repo, &["add", "-A"]);
        let agent = hodei_mh_workspace::git::commit_index(
            &repo,
            "chore: do it\n\nCo-Authored-By: Claude Sonnet 4.5 <noreply@anthropic.com>",
        )
        .expect("the agent commit");
        assert_ne!(agent, base);
        // And leaves one file uncommitted.
        std::fs::write(repo.join("uncommitted.txt"), "harness records this\n")
            .expect("the uncommitted file");

        let worktree = repo.to_str().expect("the fixture path is UTF-8");
        let command = commit_command(worktree, &base, &harness_identity(), "TASK-9");
        run_shell(&repo, &command);

        assert_eq!(
            git(&repo, &["rev-parse", "HEAD^"]),
            base,
            "one child of base"
        );
        assert_eq!(
            git(&repo, &["rev-list", "--count", &format!("{base}..HEAD")]),
            "1",
            "the agent commit was squashed away"
        );
        assert_eq!(git(&repo, &["log", "-1", "--format=%an"]), "Harness Author");
        assert_eq!(
            git(&repo, &["log", "-1", "--format=%ae"]),
            "harness@example.com"
        );
        assert_eq!(git(&repo, &["log", "-1", "--format=%cn"]), "Harness Author");
        assert_eq!(
            git(&repo, &["log", "-1", "--format=%ce"]),
            "harness@example.com"
        );
        let message = git(&repo, &["log", "-1", "--format=%B"]);
        assert_eq!(message, "chore(work): verified work for TASK-9");
        assert!(!message.contains("Co-Authored-By"));
        assert!(
            !git(&repo, &["log", "-1", "--format=%ae|%ce"]).contains("hodei-mh@local"),
            "no agent identity survives in the candidate commit"
        );
        assert!(
            git(&repo, &["ls-tree", "-r", "--name-only", "HEAD"])
                .lines()
                .any(|line| line == "uncommitted.txt"),
            "the uncommitted file is part of the candidate tree"
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn a_worktree_that_still_matches_the_base_is_refused() {
        let repo = fixture_repo("no-changes");
        let base = commit_file(&repo, "managed.txt", "managed\n", "managed state");
        let worktree = repo.to_str().expect("the fixture path is UTF-8");
        let output = run_shell(&repo, &staged_trees_command(worktree, &base));
        let (index_tree, base_tree) = staged_trees(&output).expect("the two trees");
        assert_eq!(index_tree, base_tree, "nothing changed on the worktree");

        let refusal = unchanged_stage("TASK-9", index_tree, base_tree).expect("the refusal");
        assert_eq!(refusal.work_ref, "TASK-9");
        assert!(
            refusal.reason.contains("no changes"),
            "the refusal says what is missing: {}",
            refusal.reason
        );

        let _ = std::fs::remove_dir_all(&repo);
    }

    fn staging_dir(tag: &str) -> PathBuf {
        // The fixture lives under the same base the product uses: the
        // state root of this user.
        let dir = hodei_mh_coordinator::paths::state_root()
            .join(format!("agent-staging-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn the_adapter_directory_is_private_to_this_user() {
        let dir = staging_dir("private");
        ensure_private_dir(&dir).expect("the directory is created");
        let mode = std::fs::symlink_metadata(&dir).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o700, "only its owner enters the directory");
        // Asking again is idempotent, and an existing directory with a
        // wider mode is tightened instead of trusted.
        std::fs::set_permissions(&dir, PermissionsExt::from_mode(0o755)).expect("widen");
        ensure_private_dir(&dir).expect("the directory is tightened");
        let mode = std::fs::symlink_metadata(&dir).expect("metadata").mode() & 0o777;
        assert_eq!(mode, 0o700);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_symlinked_adapter_directory_is_refused() {
        let base = staging_dir("symlink");
        std::fs::create_dir_all(&base).expect("the fixture base");
        let elsewhere = base.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).expect("the target directory");
        let link = base.join(ADAPTER_STATE_DIR);
        std::os::unix::fs::symlink(&elsewhere, &link).expect("the symbolic link");

        let refusal = ensure_private_dir(&link).expect_err("a symbolic link is not a private dir");
        assert_eq!(refusal.kind(), std::io::ErrorKind::PermissionDenied);
        assert!(refusal.to_string().contains("symbolic link"), "{refusal}");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_adapter_archive_is_unique_private_and_removed_on_every_path() {
        let dir = staging_dir("archive");
        ensure_private_dir(&dir).expect("the private directory");

        let first = AdapterArchive::create(&dir).expect("the archive is created");
        let second = AdapterArchive::create(&dir).expect("a second archive is created");
        assert_ne!(first.path(), second.path(), "every staging owns its name");
        let meta = std::fs::symlink_metadata(first.path()).expect("metadata");
        assert_eq!(meta.mode() & 0o777, 0o600, "only its owner reads it");
        assert!(meta.uid() == supervisor::paths::current_uid());

        let path = first.path().to_path_buf();
        drop(first);
        assert!(
            !path.exists(),
            "the archive does not outlive the staging that created it"
        );
        drop(second);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
