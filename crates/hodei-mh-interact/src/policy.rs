//! Typed interaction policy (GC-08): ordinary permissions are resolved by
//! rules, not by a consent window per decision (AC-1); filesystem and
//! terminal work is ALWAYS routed to the sandbox (AC-3); genuinely human
//! reviews and unsupported tools yield visibly (AC-4).

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::request::ToolKind;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InteractionPolicy {
    /// Tool kinds granted automatically — ordinary, low-risk reads.
    pub auto_allow: Vec<ToolKind>,
    /// Tool kinds that must never reach the client host: the sandbox
    /// executes them (the client grants by protocol, nothing runs locally).
    pub sandbox_routed: Vec<ToolKind>,
    /// Tool kinds requiring an explicit human review: the run YIELDS.
    pub requires_human: Vec<ToolKind>,
}

impl InteractionPolicy {
    /// The product default: reads are ordinary, writes, executions,
    /// terminals and unknown tools run inside the sandbox, and networks
    /// need review.
    ///
    /// Unknown tools are routed to the sandbox because the host never
    /// advertises fs/terminal callbacks, so no host-side path could run
    /// them anyway; they are never auto-allowed, because we do not know
    /// what they do. Human review for an unknown tool happens only when
    /// the operator opts into the deny-all policy.
    pub fn sandbox_first() -> Self {
        Self {
            auto_allow: vec![ToolKind::Read],
            sandbox_routed: vec![
                ToolKind::Write,
                ToolKind::Execute,
                ToolKind::Terminal,
                ToolKind::Unknown,
            ],
            requires_human: vec![ToolKind::Network],
        }
    }

    /// The policy of a refused automation: every tool kind needs a human
    /// review, so no interaction is ever granted or routed by rule.
    pub fn deny_all() -> Self {
        Self {
            auto_allow: Vec::new(),
            sandbox_routed: Vec::new(),
            requires_human: vec![
                ToolKind::Read,
                ToolKind::Write,
                ToolKind::Execute,
                ToolKind::Terminal,
                ToolKind::Network,
                ToolKind::Unknown,
            ],
        }
    }
}

/// Host client capabilities (AC-3): the hodei-mh client NEVER advertises
/// fs/terminal callbacks, so agent filesystem/terminal tools can never be
/// delegated to the host — they can only run inside the sandbox.
pub fn host_client_capabilities() -> Value {
    json!({"clientCapabilities": {}})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_never_auto_allows_writes_or_unknown() {
        let policy = InteractionPolicy::sandbox_first();
        assert!(policy.auto_allow.contains(&ToolKind::Read));
        assert!(!policy.auto_allow.contains(&ToolKind::Write));
        assert!(!policy.auto_allow.contains(&ToolKind::Unknown));
        assert!(policy.sandbox_routed.contains(&ToolKind::Write));
        assert!(policy.sandbox_routed.contains(&ToolKind::Execute));
        assert!(policy.sandbox_routed.contains(&ToolKind::Unknown));
        assert!(!policy.requires_human.contains(&ToolKind::Unknown));
    }

    /// Unknown tools run in the sandbox: with empty host client capabilities
    /// there is no host callback that could execute them, so a human review
    /// would stop the run without reducing any risk.
    #[test]
    fn unknown_tools_go_to_the_sandbox_and_never_the_host() {
        let policy = InteractionPolicy::sandbox_first();
        assert!(
            host_client_capabilities()["clientCapabilities"]
                .as_object()
                .is_some_and(|caps| caps.is_empty()),
            "the host advertises no callbacks, so no tool can run on it"
        );
        for kind in [
            ToolKind::Read,
            ToolKind::Write,
            ToolKind::Execute,
            ToolKind::Terminal,
            ToolKind::Unknown,
        ] {
            assert!(
                !policy.requires_human.contains(&kind),
                "{kind:?} must not need a human review"
            );
        }
        assert_eq!(policy.requires_human, vec![ToolKind::Network]);
        for kind in [
            ToolKind::Write,
            ToolKind::Execute,
            ToolKind::Terminal,
            ToolKind::Unknown,
        ] {
            assert!(
                !policy.auto_allow.contains(&kind),
                "{kind:?} must never be auto-allowed"
            );
            assert!(
                policy.sandbox_routed.contains(&kind),
                "{kind:?} must be routed to the sandbox"
            );
        }
    }

    #[test]
    fn the_deny_policy_requires_review_for_every_kind() {
        let policy = InteractionPolicy::deny_all();
        for kind in [
            ToolKind::Read,
            ToolKind::Write,
            ToolKind::Execute,
            ToolKind::Terminal,
            ToolKind::Network,
            ToolKind::Unknown,
        ] {
            assert!(!policy.auto_allow.contains(&kind), "{kind:?}");
            assert!(!policy.sandbox_routed.contains(&kind), "{kind:?}");
            assert!(policy.requires_human.contains(&kind), "{kind:?}");
        }
    }

    #[test]
    fn host_capabilities_never_advertise_fs_or_terminal() {
        let caps = host_client_capabilities();
        assert!(caps["clientCapabilities"].get("fs").is_none());
        assert!(caps["clientCapabilities"].get("terminal").is_none());
        assert!(caps["clientCapabilities"].get("execute").is_none());
    }
}
