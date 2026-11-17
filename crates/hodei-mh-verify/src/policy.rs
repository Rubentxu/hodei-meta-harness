//! Effective verification policy (GC-09): configured commands first; when
//! none exist, a PROPOSAL is inferred from project instructions and the
//! inference is documented — the evidence stays proportional and is never
//! dressed up as a configured gate.

use serde::{Deserialize, Serialize};

/// What to run, where, and how much to trust the choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationPolicy {
    pub commands: Vec<String>,
    /// Run in a clean environment (fresh checkout, no dev residue). This is
    /// the default: contamination-sensitivity is the rule, not the exception.
    pub clean_environment_required: bool,
    /// True when the commands were inferred from instructions rather than
    /// configured: consumers must treat the evidence as proportional, not
    /// authoritative.
    pub inferred: bool,
}

/// A policy plus the documented reason it was chosen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyProposal {
    pub policy: VerificationPolicy,
    pub rationale: String,
}

/// A configured policy: authoritative, not inferred.
pub fn explicit(commands: Vec<String>) -> VerificationPolicy {
    VerificationPolicy {
        commands,
        clean_environment_required: true,
        inferred: false,
    }
}

/// Known verification commands, longest first so `cargo test --workspace`
/// matches before `cargo test`.
const KNOWN_COMMANDS: [&str; 10] = [
    "cargo test",
    "cargo clippy",
    "cargo fmt",
    "npm test",
    "npm run",
    "pytest",
    "go test",
    "make check",
    "make test",
    "make",
];

/// Bare tool mentions that imply a concrete command when the instruction
/// names the tool without its invocation.
const TOOL_HINTS: [(&str, &str); 2] = [("clippy", "cargo clippy"), ("rustfmt", "cargo fmt")];

/// Infer a POLICY PROPOSAL (never a claim) from project instructions. The
/// rationale documents what was found so the evidence can be judged
/// proportionally.
pub fn infer_from_instructions(instructions: &str) -> PolicyProposal {
    let lower = instructions.to_lowercase();
    let mut commands: Vec<String> = Vec::new();
    for known in KNOWN_COMMANDS {
        if lower.contains(known) && !commands.iter().any(|c| c.contains(known)) {
            commands.push(known.to_string());
        }
    }
    for (mention, implied) in TOOL_HINTS {
        if lower.contains(mention) && !commands.iter().any(|c| c.contains(implied)) {
            commands.push(implied.to_string());
        }
    }
    if commands.is_empty() {
        return PolicyProposal {
            policy: VerificationPolicy {
                commands: Vec::new(),
                clean_environment_required: true,
                inferred: true,
            },
            rationale: "no known verification command found in project instructions; \
                        evidence must be proposed and documented proportionally, not assumed"
                .into(),
        };
    }
    PolicyProposal {
        policy: VerificationPolicy {
            commands,
            clean_environment_required: true,
            inferred: true,
        },
        rationale: "commands inferred from project instructions; run in a clean \
                    environment and record results as inferred-policy evidence"
            .into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configured_policy_is_explicit_and_clean_by_default() {
        let policy = explicit(vec!["cargo test --workspace".into()]);
        assert!(!policy.inferred);
        assert!(policy.clean_environment_required);
    }

    #[test]
    fn inference_detects_known_commands() {
        let proposal = infer_from_instructions(
            "Run `cargo test --workspace` before every merge. Keep clippy clean.",
        );
        assert!(proposal.policy.inferred);
        assert!(
            proposal
                .policy
                .commands
                .iter()
                .any(|c| c.starts_with("cargo test"))
        );
        assert!(
            proposal
                .policy
                .commands
                .iter()
                .any(|c| c.contains("clippy"))
        );
        assert!(!proposal.rationale.is_empty());
    }

    #[test]
    fn inference_without_commands_documents_the_gap() {
        let proposal = infer_from_instructions("Just review the code carefully.");
        assert!(proposal.policy.commands.is_empty());
        assert!(proposal.policy.inferred);
        assert!(proposal.rationale.contains("proportionally"));
    }

    #[test]
    fn inference_is_deduplicated_and_deterministic() {
        let a = infer_from_instructions("cargo test and cargo test again");
        let b = infer_from_instructions("cargo test");
        assert_eq!(a.policy.commands, b.policy.commands);
    }
}
