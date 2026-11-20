//! One sandbox session: the transport, the readiness and the cleanup of
//! ONE attempt.
//!
//! The session owns one [`ExecdClient`] for the whole sandbox and a fresh
//! [`Deadline`] per operation, so the connections are reused and every
//! step spends a share of one budget instead of its own. The cleanup
//! guard is armed when the sandbox exists: a session that is dropped (a
//! cancelled attempt) leaves its sandbox registered as a residue instead
//! of leaking it, and `close` releases it and confirms the cleanup.

use std::time::Duration;

use opensandbox::cleanup::{CleanupGuard, CleanupRegistry, cleanup_sandbox};
use opensandbox::control::ControlPlane;
use opensandbox::execd::api::{CreateSessionRequest, CreateSessionResponse};
use opensandbox::execd::client::ExecdClient;
use opensandbox::execd::deadline::Deadline;
use opensandbox::execd::endpoint::ExecdEndpoint;
use opensandbox::execd::oneshot::{self, OneshotOutcome};
use opensandbox::execd::pipe::{self, AttachOptions, PipeSession};
use serde_json::json;

use crate::live::LiveConfig;

/// The port the execd of a sandbox publishes.
pub const EXECD_PORT: u16 = 44772;

/// The budget of ONE operation against the sandbox: the readiness wait,
/// the staging and the measured command are shares of this deadline.
pub const OPERATION_BUDGET: Duration = Duration::from_secs(1800);

/// How long the execd of a fresh sandbox may take to accept sessions.
pub const READINESS_TIMEOUT: Duration = Duration::from_secs(60);

/// How long one sandbox lives before the lifecycle server reaps it.
pub const SANDBOX_TIMEOUT_SECONDS: u64 = 3600;

/// Why a sandbox operation could not be carried out.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("the sandbox could not be created: {reason}")]
    Create { reason: String },
    #[error("the sandbox {sandbox} published no execd endpoint: {reason}")]
    Endpoint { sandbox: String, reason: String },
    #[error("the execd did not accept sessions within {}s", READINESS_TIMEOUT.as_secs())]
    NotReady,
    #[error("the command could not be confirmed and must not be repeated: {command}")]
    Uncertain { command: String },
    #[error("the command failed: {command}: {detail}")]
    Command { command: String, detail: String },
    #[error("the file {path} could not be transferred: {reason}")]
    Transfer { path: String, reason: String },
    #[error("the sandbox {sandbox} could not be released: {reason}")]
    Release { sandbox: String, reason: String },
}

/// The credential handshake of the ACP adapter: everything the session
/// needs to start the agent WITHOUT a credential in the command.
///
/// The wrapper script and the launch command are safe to log and to
/// persist: neither carries a value. The credential line is the only
/// carrier in the whole handshake, it travels exclusively through the
/// stdin pipe, and only the wrapper (never the daemon, never the adapter
/// arguments) reads it.
pub struct AgentHandshake<'a> {
    /// Where the wrapper is staged inside the sandbox: the pre-staged
    /// adapter directory, private since its staging.
    pub wrapper_path: &'a str,
    /// The POSIX wrapper: reads exactly one stdin line, exports the
    /// credential it names, and execs the adapter.
    pub wrapper_script: &'a str,
    /// The one line the wrapper consumes, newline terminated, sent as
    /// the first raw stdin frame before the ACP stream begins.
    pub credential_line: &'a str,
}

impl std::fmt::Debug for AgentHandshake<'_> {
    /// The debug view redacts the credential line: a handshake printed
    /// in an error or a log must never print the secret it transports.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentHandshake")
            .field("wrapper_path", &self.wrapper_path)
            .field("wrapper_script", &self.wrapper_script)
            .field("credential_line", &"<redacted>")
            .finish()
    }
}

/// A sandbox created for one attempt, with its transport and its cleanup.
pub struct SandboxSession {
    sandbox_id: String,
    control: ControlPlane,
    execd: ExecdClient,
    guard: Option<CleanupGuard>,
}

impl SandboxSession {
    /// Create the sandbox, arm its cleanup, resolve its execd and wait
    /// (bounded) until the execd accepts sessions.
    pub async fn create(control: &ControlPlane, config: &LiveConfig) -> Result<Self, SessionError> {
        let registry = CleanupRegistry::durable(CleanupRegistry::default_journal_path());
        for residue in registry.reconcile_sandboxes(control).await {
            eprintln!("sandbox residue not released: {}", residue.describe());
        }
        let created = control
            .post(ControlPlane::sandboxes_path())
            .json(&json!({
                "image": {"uri": config.image},
                "entrypoint": ["sleep", "infinity"],
                "timeout": SANDBOX_TIMEOUT_SECONDS,
                "metadata": {"purpose": "hodei-mh-live"},
            }))
            .send()
            .await
            .map_err(|error| SessionError::Create {
                reason: error.to_string(),
            })?;
        let sandbox_id = created
            .json::<serde_json::Value>()
            .await
            .ok()
            .and_then(|value| value["id"].as_str().map(str::to_string))
            .filter(|id| !id.is_empty())
            .ok_or_else(|| SessionError::Create {
                reason: "the lifecycle server answered no sandbox id".to_string(),
            })?;
        // The cleanup is armed NOW: from this point on, a dropped session
        // registers the sandbox as a residue instead of leaking it.
        let guard = CleanupGuard::sandbox(sandbox_id.clone(), registry);
        let endpoint = match control
            .get(&ControlPlane::endpoint_path(&sandbox_id, EXECD_PORT))
            .send()
            .await
        {
            Ok(response) => response
                .json::<serde_json::Value>()
                .await
                .ok()
                .as_ref()
                .and_then(ExecdEndpoint::from_contract),
            Err(error) => {
                return Err(SessionError::Endpoint {
                    sandbox: sandbox_id,
                    reason: error.to_string(),
                });
            }
        };
        let endpoint = endpoint.ok_or_else(|| SessionError::Endpoint {
            sandbox: sandbox_id.clone(),
            reason: "the answer carries no endpoint".to_string(),
        })?;
        let execd = ExecdClient::new(endpoint);
        let ready = oneshot::ensure_ready(&execd, Deadline::in_budget(READINESS_TIMEOUT)).await;
        if !ready {
            return Err(SessionError::NotReady);
        }
        Ok(Self {
            sandbox_id,
            control: control.clone(),
            execd,
            guard: Some(guard),
        })
    }

    /// The sandbox this session owns.
    pub fn sandbox_id(&self) -> &str {
        &self.sandbox_id
    }

    /// The transport of this sandbox. The connections are reused across
    /// the operations of the session.
    pub fn transport(&self) -> &ExecdClient {
        &self.execd
    }

    /// The deadline of ONE operation.
    pub fn deadline(&self) -> Deadline {
        Deadline::in_budget(OPERATION_BUDGET)
    }

    /// Upload a file into the sandbox.
    pub async fn upload(&self, bytes: Vec<u8>, remote: &str) -> Result<(), SessionError> {
        let deadline = self.deadline();
        self.execd
            .upload_file(bytes, remote, deadline.remaining())
            .await
            .map_err(|error| SessionError::Transfer {
                path: remote.to_string(),
                reason: error.to_string(),
            })
    }

    /// Download a file from the sandbox.
    pub async fn download(&self, remote: &str) -> Result<Vec<u8>, SessionError> {
        let deadline = self.deadline();
        self.execd
            .download_file(remote, deadline.remaining())
            .await
            .map_err(|error| SessionError::Transfer {
                path: remote.to_string(),
                reason: error.to_string(),
            })
    }

    /// Run one command and require that it completed successfully.
    ///
    /// An UNCERTAIN result is an error and is never retried: the command
    /// may have run, and running it again would duplicate its effect.
    pub async fn run_checked(&self, command: &str) -> Result<String, SessionError> {
        let outcome = oneshot::run_when_ready(&self.execd, command, self.deadline()).await;
        if outcome.uncertain() {
            return Err(SessionError::Uncertain {
                command: command.to_string(),
            });
        }
        if !outcome.succeeded() {
            return Err(SessionError::Command {
                command: command.to_string(),
                detail: outcome_summary(&outcome),
            });
        }
        Ok(outcome.output)
    }

    /// Start the ACP adapter through the credential handshake and attach
    /// to its session.
    ///
    /// The command carries NO credential: the wrapper is staged first,
    /// the session is started, and the credential is then handed over as
    /// the FIRST stdin frame — before any ACP byte — which only the
    /// wrapper consumes. The adapter receives it as environment of its
    /// own process, never in argv, never in the request body.
    pub async fn attach_agent(
        &self,
        command: &str,
        cwd: &str,
        handshake: &AgentHandshake<'_>,
    ) -> Result<(PipeSession, CreateSessionResponse), SessionError> {
        let deadline = self.deadline();
        self.execd
            .upload_file(
                handshake.wrapper_script.as_bytes().to_vec(),
                handshake.wrapper_path,
                deadline.remaining(),
            )
            .await
            .map_err(|error| SessionError::Transfer {
                path: handshake.wrapper_path.to_string(),
                reason: error.to_string(),
            })?;
        let session = self
            .execd
            .create_session(
                CreateSessionRequest {
                    cwd: Some(cwd),
                    command: Some(command),
                },
                deadline.remaining(),
            )
            .await
            .map_err(|error| SessionError::Command {
                command: "the ACP adapter session".to_string(),
                detail: error.to_string(),
            })?;
        let attached = pipe::attach(
            &self.execd.ws_url(&session.session_id),
            AttachOptions {
                connect_timeout: Some(deadline.remaining()),
                ..self.execd.attach_options()
            },
        )
        .await
        .map_err(|error| SessionError::Command {
            command: "the ACP adapter pipe".to_string(),
            detail: error.to_string(),
        })?;
        attached
            .send_stdin(handshake.credential_line.as_bytes())
            .await
            .map_err(|error| SessionError::Command {
                command: "the credential handshake of the ACP adapter".to_string(),
                detail: error.to_string(),
            })?;
        Ok((attached, session))
    }

    /// Release the sandbox and confirm its cleanup.
    pub async fn close(mut self) -> Result<(), SessionError> {
        let mut guard = self.guard.take().ok_or_else(|| SessionError::Release {
            sandbox: self.sandbox_id.clone(),
            reason: "the cleanup guard was already released".to_string(),
        })?;
        cleanup_sandbox(&self.control, &mut guard)
            .await
            .map_err(|error| SessionError::Release {
                sandbox: self.sandbox_id.clone(),
                reason: error.to_string(),
            })
    }
}

/// What a command produced, for a refusal that does not hide it.
fn outcome_summary(outcome: &OneshotOutcome) -> String {
    let code = outcome
        .exit_code
        .map(|code| code.to_string())
        .unwrap_or_else(|| "no terminal status".to_string());
    format!("exit {code}: {}", outcome.output.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::runtime::AgentRuntime;
    use opensandbox::execd::fake::{CLIENT_FRAME_BINARY, FakeBehavior, FakeExecd};

    /// The credential of the fixture: a value shaped like a real token,
    /// never a real one.
    const FAKE_TOKEN: &str = "sk-ant-fixture-token";

    /// The launch command the session is asked to run: secret-free by
    /// construction (its own shape is asserted in the agent tests), so
    /// this test can measure exactly what the daemon received.
    const LAUNCH_COMMAND: &str = "PATH=/workspace/ccacp/node_modules/.bin:$PATH \
HTTPS_PROXY=http://127.0.0.1:8888 sh /workspace/ccacp/run-agent.sh claude-code-acp";

    fn credential_line_for(token: &str) -> String {
        format!("http://model.local\t{token}\n")
    }

    async fn session_over(fake: &FakeExecd) -> SandboxSession {
        // The session is the transport plus the cleanup guard the fake
        // never needs: only the adapter handshake is under test.
        let control = ControlPlane::new(fake.base_url.clone());
        SandboxSession {
            sandbox_id: "sandbox-under-test".to_string(),
            control,
            execd: ExecdClient::new(ExecdEndpoint::new(fake.base_url.clone())),
            guard: None,
        }
    }

    /// The credential is the FIRST stdin frame and the ACP stream follows:
    /// the bytes the daemon saw are the contract, observed offline.
    #[tokio::test]
    async fn the_credential_line_is_the_first_stdin_frame_and_the_acp_stream_follows() {
        let fake = FakeExecd::start(FakeBehavior::Cooperative).await;
        let session = session_over(&fake).await;
        let credential_line = credential_line_for(FAKE_TOKEN);
        let wrapper_script = AgentRuntime::claude_code_acp().wrapper_script();
        let handshake = AgentHandshake {
            wrapper_path: "/workspace/ccacp/run-agent.sh",
            wrapper_script: &wrapper_script,
            credential_line: &credential_line,
        };

        let (pipe, _created) = session
            .attach_agent(LAUNCH_COMMAND, "/workspace/work", &handshake)
            .await
            .expect("the handshake completes over the fake");
        let initialize = r#"{"jsonrpc":"2.0","method":"initialize"}"#;
        pipe.send_stdin(initialize.as_bytes())
            .await
            .expect("the pipe is open");
        drop(pipe);
        fake.await_pipes_closed(1).await;

        let counts = fake.counts();
        let stdin: Vec<&[u8]> = counts
            .pipe_frames
            .iter()
            .filter(|(opcode, _)| *opcode == CLIENT_FRAME_BINARY)
            .map(|(_, payload)| payload.as_slice())
            .collect();
        assert_eq!(stdin.len(), 2, "the credential line and the ACP request");
        assert_eq!(
            stdin[0],
            opensandbox::execd::frames::encode_stdin(credential_line.as_bytes()),
            "the credential travels as the first raw stdin frame, before any ACP byte"
        );
        assert_eq!(
            stdin[1],
            opensandbox::execd::frames::encode_stdin(initialize.as_bytes()),
            "then the ACP stream"
        );

        // The request bodies the daemon keeps: the command and the staged
        // wrapper carry no credential.
        assert!(
            counts
                .commands
                .iter()
                .all(|seen| !seen.contains(FAKE_TOKEN)),
            "the create_session command carries no credential"
        );
        assert_eq!(
            profiles::credentials::secret_leak_positions(&counts.commands),
            Vec::<usize>::new(),
            "the command is not even recognised as secret-bearing"
        );
        assert!(
            counts
                .uploads
                .iter()
                .all(|body| !String::from_utf8_lossy(body).contains(FAKE_TOKEN)),
            "the staged wrapper carries no credential either"
        );
    }

    /// The credential line is redacted in the debug view: the handshake
    /// can travel with an error without printing its secret.
    #[test]
    fn the_handshake_never_prints_the_credential_it_carries() {
        let credential_line = credential_line_for(FAKE_TOKEN);
        let wrapper_script = AgentRuntime::claude_code_acp().wrapper_script();
        let handshake = AgentHandshake {
            wrapper_path: "/workspace/ccacp/run-agent.sh",
            wrapper_script: &wrapper_script,
            credential_line: &credential_line,
        };
        let shown = format!("{handshake:?}");
        assert!(!shown.contains(FAKE_TOKEN), "{shown}");
        assert!(shown.contains("<redacted>"), "{shown}");
    }
}
