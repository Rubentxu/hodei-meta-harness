//! The agent runtime ADT: which ACP agent CLI a live session drives.
//!
//! One closed enum, one variant per agent, and every fact the runtime
//! shell needs (artifact, staging layout, launch command, credential
//! variable) is data inside the variant. The core never branches on a
//! magic string and never hardcodes an egress domain in logic: hosts are
//! inert variant data for the operator's proxy configuration.
//!
//! `ClaudeCodeAcp` carries the published behavior of the default: the
//! launch command, the wrapper and the staging layout are exactly what
//! they were before this enum existed.

/// How the agent's binaries reach the sandbox. Both are staged from the
/// host: the sandbox has no direct egress for package registries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentArtifact {
    /// A pinned npm adapter installed into private host state, packed
    /// and uploaded as a tarball of `node_modules`.
    HostNpmAdapter {
        /// The pinned npm package spec.
        package: &'static str,
        /// The executable relative to the sandbox staging root.
        entrypoint: &'static str,
    },
    /// A pinned vendor release tarball: the host downloads it, verifies
    /// its sha256 against the pinned digest, and uploads the verified
    /// bytes unchanged; the sandbox re-verifies before extracting.
    VendorTarball {
        /// The pinned release version.
        version: &'static str,
        /// The pinned artifact URL.
        url: &'static str,
        /// The vendor channel manifest that publishes the artifact digests.
        manifest_url: &'static str,
        /// The pinned sha256 of the tarball (it matches `manifest_url`).
        sha256: &'static str,
        /// The archive file name, used for the private host cache.
        name: &'static str,
        /// The executable relative to the sandbox staging root after
        /// extraction.
        entrypoint: &'static str,
    },
}

/// The ACP agent runtime of a live session. The fields have the same
/// shape in both variants: what differs is the pinned data, never logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentRuntime {
    /// Claude Code through the pinned `claude-code-acp` adapter. The
    /// default, and the published behavior.
    ClaudeCodeAcp {
        artifact: AgentArtifact,
        sandbox_root: &'static str,
        executable: &'static str,
        acp_flag: Option<&'static str>,
        credential_env: &'static str,
        base_url_env: Option<&'static str>,
        egress_hosts: &'static [&'static str],
    },
    /// Qoder CLI in ACP server mode (`qodercli --acp`). The flag is not
    /// in `--help`: the spike proved the server answers initialize and
    /// session/new on protocol version 1.
    QoderAcp {
        artifact: AgentArtifact,
        sandbox_root: &'static str,
        executable: &'static str,
        acp_flag: Option<&'static str>,
        credential_env: &'static str,
        base_url_env: Option<&'static str>,
        egress_hosts: &'static [&'static str],
    },
}

/// One exhaustive read of a variant's data. The match lives here, once;
/// every command and script below is written against this view.
struct Spec<'a> {
    artifact: &'a AgentArtifact,
    sandbox_root: &'a str,
    executable: &'a str,
    acp_flag: Option<&'static str>,
    credential_env: &'static str,
    base_url_env: Option<&'static str>,
}

impl AgentRuntime {
    /// The values `--agent` accepts, in refusal order.
    pub const CLI_VALUES: &'static [&'static str] = &["claude-code", "qoder"];

    /// Parse the `--agent` value at the CLI edge. `None` is not a value:
    /// the caller applies the default (`claude_code_acp`).
    pub fn from_cli_value(value: &str) -> Option<Self> {
        match value {
            "claude-code" => Some(Self::claude_code_acp()),
            "qoder" => Some(Self::qoder_acp()),
            _ => None,
        }
    }

    /// The refusal text for a value that is not a runtime: it names every
    /// valid value, so the operator never has to read the source.
    pub fn expected_values() -> String {
        format!("expected {}", Self::CLI_VALUES.join(" or "))
    }

    /// The pinned Claude Code adapter runtime (the published default).
    pub fn claude_code_acp() -> Self {
        Self::ClaudeCodeAcp {
            artifact: AgentArtifact::HostNpmAdapter {
                package: "@zed-industries/claude-code-acp@0.16.2",
                entrypoint: "node_modules/.bin/claude-code-acp",
            },
            sandbox_root: "/workspace/ccacp",
            executable: "claude-code-acp",
            // The adapter IS the ACP entry point: it needs no flag.
            acp_flag: None,
            credential_env: "ANTHROPIC_AUTH_TOKEN",
            base_url_env: Some("ANTHROPIC_BASE_URL"),
            egress_hosts: &["api.anthropic.com"],
        }
    }

    /// The pinned Qoder CLI runtime. The credential is the headless
    /// personal access token, delivered through the environment only,
    /// exactly like every other credential in this harness.
    pub fn qoder_acp() -> Self {
        Self::QoderAcp {
            artifact: AgentArtifact::VendorTarball {
                version: "1.1.94",
                url: "https://qoder-ide.oss-accelerate.aliyuncs.com/qodercli/releases/1.1.94/qodercli-linux-x64.tar.gz",
                manifest_url: "https://download.qoder.com/qodercli/channels/1.1.94/manifest.json",
                sha256: "cba1ecddddaa434628c88eef9d95f4dbbc545169208aab35bac8c0eb1cdc40c3",
                name: "qodercli-linux-x64.tar.gz",
                entrypoint: "qodercli",
            },
            sandbox_root: "/workspace/qoder",
            executable: "qodercli",
            // ACP server mode is an undocumented flag of the CLI itself.
            acp_flag: Some("--acp"),
            credential_env: "QODER_PERSONAL_ACCESS_TOKEN",
            // Qoder has no gateway base URL override: the endpoint is
            // fixed by the vendor.
            base_url_env: None,
            egress_hosts: &[
                "download.qoder.com",
                "qoder-ide.oss-accelerate.aliyuncs.com",
            ],
        }
    }

    /// The `--agent` name of this runtime.
    pub fn name(&self) -> &'static str {
        match self {
            Self::ClaudeCodeAcp { .. } => "claude-code",
            Self::QoderAcp { .. } => "qoder",
        }
    }

    fn spec(&self) -> Spec<'_> {
        match self {
            Self::ClaudeCodeAcp {
                artifact,
                sandbox_root,
                executable,
                acp_flag,
                credential_env,
                base_url_env,
                ..
            }
            | Self::QoderAcp {
                artifact,
                sandbox_root,
                executable,
                acp_flag,
                credential_env,
                base_url_env,
                ..
            } => Spec {
                artifact,
                sandbox_root,
                executable,
                acp_flag: *acp_flag,
                credential_env,
                base_url_env: *base_url_env,
            },
        }
    }

    /// The artifact pin of this runtime (drives host-side staging).
    pub fn artifact(&self) -> &AgentArtifact {
        self.spec().artifact
    }

    /// The environment variable that carries the credential, and the
    /// optional gateway base URL variable. The edge reads these names
    /// from the process environment; nothing else names them.
    pub fn credential_env(&self) -> &'static str {
        self.spec().credential_env
    }

    pub fn base_url_env(&self) -> Option<&'static str> {
        self.spec().base_url_env
    }

    /// Hosts this agent reaches from inside the sandbox. Inert data for
    /// the operator's proxy configuration: no core logic reads it.
    pub fn egress_hosts(&self) -> &'static [&'static str] {
        match self {
            Self::ClaudeCodeAcp { egress_hosts, .. } | Self::QoderAcp { egress_hosts, .. } => {
                egress_hosts
            }
        }
    }

    /// Where the credential wrapper is staged inside the sandbox.
    pub fn wrapper_path(&self) -> String {
        format!("{}/run-agent.sh", self.spec().sandbox_root)
    }

    /// The staged executable inside the sandbox, verified after
    /// extraction.
    pub fn staged_executable(&self) -> String {
        let spec = self.spec();
        let relative = match spec.artifact {
            AgentArtifact::HostNpmAdapter { entrypoint, .. }
            | AgentArtifact::VendorTarball { entrypoint, .. } => entrypoint,
        };
        format!("{}/{}", spec.sandbox_root, relative)
    }

    /// The directory the executable is found in through PATH.
    pub fn bin_dir(&self) -> String {
        match self.staged_executable().rsplit_once('/') {
            Some((dir, _)) => dir.to_string(),
            None => self.spec().sandbox_root.to_string(),
        }
    }

    /// The archive path the artifact is uploaded to inside the sandbox.
    pub fn archive_path(&self) -> String {
        let root = self.spec().sandbox_root;
        let name = match root.rsplit_once('/') {
            Some((_, name)) => name,
            None => root,
        };
        format!("/tmp/{name}.tgz")
    }

    /// The process the wrapper execs: the agent CLI plus its ACP flag
    /// when the agent needs one to answer on the ACP stream.
    pub fn agent_exec(&self) -> String {
        let spec = self.spec();
        match spec.acp_flag {
            Some(flag) => format!("{} {flag}", spec.executable),
            None => spec.executable.to_string(),
        }
    }

    /// The command that starts the agent through the credential wrapper.
    /// It carries NO credential: the token reaches the agent only
    /// through the stdin handshake the wrapper consumes.
    pub fn launch_command(&self, proxy_url: &str) -> String {
        format!(
            "PATH={}:$PATH HTTPS_PROXY={} sh {} {}",
            self.bin_dir(),
            crate::probe_util::shell_quote(proxy_url),
            crate::probe_util::shell_quote(&self.wrapper_path()),
            self.agent_exec(),
        )
    }

    /// The POSIX wrapper staged beside the agent inside the sandbox: it
    /// reads exactly ONE stdin line (base URL and token, tab separated),
    /// exports the credential under the name this variant declares, and
    /// execs the agent. Values never enter argv, in any variant.
    pub fn wrapper_script(&self) -> String {
        let spec = self.spec();
        let mut script = format!(
            r#"#!/bin/sh
IFS="$(printf '\t')" read -r base token
if [ -z "$token" ]; then
  exit 1
fi
{cred}="$token"
export {cred}
"#,
            cred = spec.credential_env,
        );
        if let Some(base_env) = spec.base_url_env {
            script.push_str(&format!(
                r#"if [ -n "$base" ]; then
  {base_env}="$base"
  export {base_env}
fi
"#,
                base_env = base_env,
            ));
        }
        script.push_str("exec \"$@\"\n");
        script
    }

    /// The sandbox-side staging command: re-verify the uploaded archive
    /// (vendor tarballs carry a pinned digest), extract it under the
    /// staging root and require the executable bit on the agent.
    pub fn sandbox_stage_command(&self) -> String {
        let spec = self.spec();
        let archive = self.archive_path();
        let staged = self.staged_executable();
        let verify = match spec.artifact {
            AgentArtifact::HostNpmAdapter { .. } => String::new(),
            AgentArtifact::VendorTarball { sha256, .. } => {
                format!("printf '{}  {}\\n' | sha256sum -c - && ", sha256, archive)
            }
        };
        format!(
            "{verify}mkdir -p {root} && tar xzf {archive} -C {root} && test -x {staged}",
            root = spec.sandbox_root,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The published launch command of the default runtime, kept as the
    /// exact string it had before the enum existed: zero regression.
    #[test]
    fn claude_code_keeps_the_published_launch_command() {
        let runtime = AgentRuntime::claude_code_acp();
        assert_eq!(
            runtime.launch_command("http://127.0.0.1:8888"),
            "PATH=/workspace/ccacp/node_modules/.bin:$PATH \
HTTPS_PROXY='http://127.0.0.1:8888' sh '/workspace/ccacp/run-agent.sh' claude-code-acp",
        );
        assert_eq!(
            runtime.wrapper_script(),
            // The exact script the runtime shipped with, anchored here.
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
        );
        assert_eq!(runtime.archive_path(), "/tmp/ccacp.tgz");
        assert_eq!(
            runtime.credential_env(),
            "ANTHROPIC_AUTH_TOKEN",
            "the published credential name"
        );
        assert_eq!(runtime.base_url_env(), Some("ANTHROPIC_BASE_URL"));
    }

    #[test]
    fn qoder_launches_the_cli_in_acp_server_mode_under_its_own_credential() {
        let runtime = AgentRuntime::qoder_acp();
        let command = runtime.launch_command("http://127.0.0.1:8888");
        assert_eq!(
            command,
            "PATH=/workspace/qoder:$PATH HTTPS_PROXY='http://127.0.0.1:8888' \
sh '/workspace/qoder/run-agent.sh' qodercli --acp",
        );
        assert_eq!(runtime.credential_env(), "QODER_PERSONAL_ACCESS_TOKEN");
        assert_eq!(runtime.base_url_env(), None);
        assert_eq!(runtime.wrapper_path(), "/workspace/qoder/run-agent.sh");
        assert_eq!(runtime.archive_path(), "/tmp/qoder.tgz");
    }

    #[test]
    fn each_wrapper_exports_only_the_credential_variable_its_variant_declares() {
        let claude = AgentRuntime::claude_code_acp().wrapper_script();
        assert!(claude.contains("ANTHROPIC_AUTH_TOKEN=\"$token\""));
        assert!(claude.contains("ANTHROPIC_BASE_URL=\"$base\""));
        assert!(
            !claude.contains("QODER"),
            "the default wrapper does not mention the other agent"
        );
        let qoder = AgentRuntime::qoder_acp().wrapper_script();
        assert!(qoder.contains("QODER_PERSONAL_ACCESS_TOKEN=\"$token\""));
        assert!(
            !qoder.contains("ANTHROPIC"),
            "the qoder wrapper exports no claude variable: {qoder}"
        );
        assert!(qoder.contains("exec \"$@\""), "it execs, it does not fork");
    }

    #[test]
    fn the_sandbox_staging_reverifies_a_vendor_tarball_before_extracting_it() {
        let claude = AgentRuntime::claude_code_acp().sandbox_stage_command();
        assert_eq!(
            claude,
            "mkdir -p /workspace/ccacp && tar xzf /tmp/ccacp.tgz -C /workspace/ccacp \
&& test -x /workspace/ccacp/node_modules/.bin/claude-code-acp",
        );
        let qoder = AgentRuntime::qoder_acp().sandbox_stage_command();
        assert_eq!(
            qoder,
            "printf 'cba1ecddddaa434628c88eef9d95f4dbbc545169208aab35bac8c0eb1cdc40c3  \
/tmp/qoder.tgz\\n' | sha256sum -c - && mkdir -p /workspace/qoder \
&& tar xzf /tmp/qoder.tgz -C /workspace/qoder && test -x /workspace/qoder/qodercli",
        );
    }

    #[test]
    fn the_agent_flag_parses_the_variants_and_nothing_else() {
        assert_eq!(
            AgentRuntime::from_cli_value("claude-code"),
            Some(AgentRuntime::claude_code_acp())
        );
        assert_eq!(
            AgentRuntime::from_cli_value("qoder"),
            Some(AgentRuntime::qoder_acp())
        );
        for invented in ["codex", "Qoder", "", "claude_code"] {
            assert_eq!(
                AgentRuntime::from_cli_value(invented),
                None,
                "'{invented}' is not a runtime"
            );
        }
        assert_eq!(
            AgentRuntime::expected_values(),
            "expected claude-code or qoder",
            "the refusal names every valid value"
        );
        // The name round-trips: the parse of a printed name is that runtime.
        for runtime in [AgentRuntime::claude_code_acp(), AgentRuntime::qoder_acp()] {
            assert_eq!(
                AgentRuntime::from_cli_value(runtime.name()),
                Some(runtime.clone()),
                "{runtime:?}"
            );
        }
    }

    #[test]
    fn the_egress_hosts_are_inert_pinned_data_per_variant() {
        // No core logic reads these: they exist so the proxy is
        // configured from the variant, not from a hardcoded string.
        assert_eq!(
            AgentRuntime::claude_code_acp().egress_hosts(),
            &["api.anthropic.com"]
        );
        assert_eq!(
            AgentRuntime::qoder_acp().egress_hosts(),
            &[
                "download.qoder.com",
                "qoder-ide.oss-accelerate.aliyuncs.com"
            ]
        );
    }
}
