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
    /// The product default: reads are ordinary, writes/executions/terminals
    /// happen inside the sandbox, networks need review, unknown tools are
    /// never auto-anything.
    pub fn sandbox_first() -> Self {
        Self {
            auto_allow: vec![ToolKind::Read],
            sandbox_routed: vec![ToolKind::Write, ToolKind::Execute, ToolKind::Terminal],
            requires_human: vec![ToolKind::Network, ToolKind::Unknown],
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
        assert!(policy.requires_human.contains(&ToolKind::Unknown));
    }

    #[test]
    fn host_capabilities_never_advertise_fs_or_terminal() {
        let caps = host_client_capabilities();
        assert!(caps["clientCapabilities"].get("fs").is_none());
        assert!(caps["clientCapabilities"].get("terminal").is_none());
        assert!(caps["clientCapabilities"].get("execute").is_none());
    }
}
