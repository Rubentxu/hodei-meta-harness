//! The resolver (GC-08): pure decision over typed rules. Every outcome is
//! attributable (AC-1: automation grants are labelled; AC-4: unsupported and
//! human cases yield with a visible reason).

use serde::{Deserialize, Serialize};

use crate::policy::InteractionPolicy;
use crate::request::InteractionRequest;

/// Who decided a grant. Attribution is part of the decision (AC-1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GrantSource {
    /// Rule-based automatic grant: no consent window involved.
    PolicyRule,
    /// The sandbox executes the tool; the host client grants by protocol
    /// only (AC-3: nothing runs on the host).
    SandboxRoute,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Resolution {
    /// Granted without a per-decision consent window.
    AutoGranted { source: GrantSource },
    /// The tool runs inside the sandbox; the client host is out of the path.
    RoutedToSandbox,
    /// Mandatory human review: the run yields with the reason (AC-4).
    HumanRequired { reason: String },
    /// Unsupported interaction: visible reason, the run yields (AC-4).
    Unsupported { reason: String },
}

/// Validate the session identity/scope of a request before resolving:
/// interactions from another run/session are not ours to answer.
pub fn resolve(policy: &InteractionPolicy, request: &InteractionRequest) -> Resolution {
    let Some(kind) = request.tool_kind() else {
        // Forms and free questions are surfaced, never auto-answered.
        return match request {
            InteractionRequest::Form { .. } => Resolution::HumanRequired {
                reason: "structured form requires user input".into(),
            },
            InteractionRequest::Question { .. } => Resolution::HumanRequired {
                reason: "free-text question requires user input".into(),
            },
            InteractionRequest::Permission { .. } => unreachable!("permission has a kind"),
        };
    };

    if policy.sandbox_routed.contains(kind) {
        return Resolution::RoutedToSandbox;
    }
    if policy.auto_allow.contains(kind) {
        return Resolution::AutoGranted {
            source: GrantSource::PolicyRule,
        };
    }
    if policy.requires_human.contains(kind) {
        return Resolution::HumanRequired {
            reason: format!("tool kind {kind:?} requires explicit review"),
        };
    }
    Resolution::Unsupported {
        reason: format!("tool kind {kind:?} is not covered by the interaction policy"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::{PermissionOption, ToolKind};

    fn permission(name: &str, kind: ToolKind) -> InteractionRequest {
        InteractionRequest::Permission {
            tool_name: name.into(),
            kind,
            options: vec![PermissionOption {
                option_id: "opt-1".into(),
                name: "Allow once".into(),
                kind: "allow_once".into(),
            }],
        }
    }

    #[test]
    fn ordinary_reads_are_auto_granted_with_attribution() {
        let decision = resolve(
            &InteractionPolicy::sandbox_first(),
            &permission("Read", ToolKind::Read),
        );
        assert_eq!(
            decision,
            Resolution::AutoGranted {
                source: GrantSource::PolicyRule
            }
        );
    }

    #[test]
    fn writes_and_executions_route_to_sandbox_nothing_runs_on_host() {
        let policy = InteractionPolicy::sandbox_first();
        for kind in [ToolKind::Write, ToolKind::Execute, ToolKind::Terminal] {
            let decision = resolve(&policy, &permission("tool", kind.clone()));
            assert_eq!(
                decision,
                Resolution::RoutedToSandbox,
                "{kind:?} must route to sandbox"
            );
        }
    }

    #[test]
    fn unknown_and_network_require_visible_review() {
        let policy = InteractionPolicy::sandbox_first();
        let decision = resolve(&policy, &permission("mystery", ToolKind::Unknown));
        assert!(matches!(decision, Resolution::HumanRequired { .. }));

        let decision = resolve(&policy, &permission("WebFetch", ToolKind::Network));
        assert!(matches!(decision, Resolution::HumanRequired { .. }));
    }

    #[test]
    fn forms_and_questions_yield_visibly() {
        let policy = InteractionPolicy::sandbox_first();
        let decision = resolve(
            &policy,
            &InteractionRequest::Question {
                text: "which database?".into(),
            },
        );
        assert!(matches!(decision, Resolution::HumanRequired { .. }));
    }
}
