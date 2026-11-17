//! Interaction requests (GC-08): what an agent asks its client, typed
//! before any reasoning happens.

use serde::{Deserialize, Serialize};

/// Class of tool a permission request refers to. Unknown is its own variant:
/// an untyped tool never falls into an optimistic default.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ToolKind {
    Read,
    Write,
    Execute,
    Terminal,
    Network,
    Unknown,
}

impl ToolKind {
    /// Classify from the tool name the agent reports (pure, typed rules
    /// before reasoning).
    pub fn from_tool_name(name: &str) -> Self {
        let lower = name.to_lowercase();
        if lower.contains("read")
            || lower.contains("grep")
            || lower.contains("glob")
            || lower.contains("ls")
        {
            Self::Read
        } else if lower.contains("write") || lower.contains("edit") {
            Self::Write
        } else if lower.contains("bash") || lower.contains("execute") || lower.contains("command") {
            Self::Execute
        } else if lower.contains("terminal") {
            Self::Terminal
        } else if lower.contains("web") || lower.contains("fetch") || lower.contains("net") {
            Self::Network
        } else {
            Self::Unknown
        }
    }
}

/// One option the agent offers for a permission decision (ACP shape).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionOption {
    pub option_id: String,
    pub name: String,
    /// ACP kind: "allow_once" | "allow_always" | "reject_once" |
    /// "reject_always" | free-form agent values are preserved.
    pub kind: String,
}

/// A structured question with selectable options (form-like).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FormField {
    pub id: String,
    pub label: String,
    pub options: Vec<String>,
    /// How many answers this field expects: single-select is 1.
    pub cardinality: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum InteractionRequest {
    /// A tool permission decision (ACP session/request_permission).
    Permission {
        tool_name: String,
        kind: ToolKind,
        options: Vec<PermissionOption>,
    },
    /// A structured form with fields and per-field cardinality.
    Form { fields: Vec<FormField> },
    /// Free-text question: never auto-answered, always surfaced.
    Question { text: String },
}

impl InteractionRequest {
    /// The tool kind this request refers to, when it is a permission.
    pub fn tool_kind(&self) -> Option<&ToolKind> {
        match self {
            InteractionRequest::Permission { kind, .. } => Some(kind),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_classification_is_rule_based_not_guessed() {
        assert_eq!(ToolKind::from_tool_name("Read"), ToolKind::Read);
        assert_eq!(
            ToolKind::from_tool_name("mcp__fs__write_file"),
            ToolKind::Write
        );
        assert_eq!(ToolKind::from_tool_name("Bash"), ToolKind::Execute);
        assert_eq!(ToolKind::from_tool_name("WebFetch"), ToolKind::Network);
        assert_eq!(ToolKind::from_tool_name("something_new"), ToolKind::Unknown);
    }
}
