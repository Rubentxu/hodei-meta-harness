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
    /// before reasoning). A name is read as a set of WORDS: a keyword
    /// matches a whole word of the name, never a fragment inside one,
    /// so a tool called `Pulse` is not a directory listing.
    pub fn from_tool_name(name: &str) -> Self {
        let words = name_words(name);
        let names = |kinds: &[&str]| words.iter().any(|word| kinds.contains(&word.as_str()));
        if names(&["read", "grep", "glob", "ls"]) {
            Self::Read
        } else if names(&["write", "edit"]) {
            Self::Write
        } else if names(&["bash", "execute", "command"]) {
            Self::Execute
        } else if names(&["terminal"]) {
            Self::Terminal
        } else if names(&["web", "fetch", "net"]) {
            Self::Network
        } else {
            Self::Unknown
        }
    }
}

/// The lower-case words of a tool name: split on separators and on
/// camelCase boundaries.
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words = Vec::new();
    let mut current: Vec<char> = Vec::new();
    for (index, c) in chars.iter().enumerate() {
        let starts_word = index > 0
            && (if !c.is_alphanumeric() || !chars[index - 1].is_alphanumeric() {
                true
            } else if c.is_uppercase() {
                let previous_lower =
                    chars[index - 1].is_lowercase() || chars[index - 1].is_numeric();
                let run_ends_before_lower = chars[index - 1].is_uppercase()
                    && chars.get(index + 1).is_some_and(|next| next.is_lowercase());
                previous_lower || run_ends_before_lower
            } else {
                false
            });
        if starts_word && !current.is_empty() {
            words.push(current.iter().collect::<String>().to_lowercase());
            current.clear();
        }
        if c.is_alphanumeric() {
            current.push(*c);
        }
    }
    if !current.is_empty() {
        words.push(current.iter().collect::<String>().to_lowercase());
    }
    words
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

    #[test]
    fn a_keyword_inside_another_word_never_decides_the_kind() {
        // `ls` is a word of `ls`, not a word of `Pulse`: the classifier
        // reads whole words, so an unrelated tool is never mislabelled
        // by a fragment.
        assert_eq!(ToolKind::from_tool_name("Pulse"), ToolKind::Unknown);
        assert_eq!(ToolKind::from_tool_name("pulse_monitor"), ToolKind::Unknown);
        assert_eq!(ToolKind::from_tool_name("LSMTree"), ToolKind::Unknown);
        assert_eq!(ToolKind::from_tool_name("netrunner"), ToolKind::Unknown);
        assert_eq!(ToolKind::from_tool_name("ls"), ToolKind::Read);
        assert_eq!(ToolKind::from_tool_name("mcp__fs__ls"), ToolKind::Read);
        // The whole-word rule cuts both ways: `network` is not the word
        // `net`, so an unlisted name stays Unknown and is surfaced.
        assert_eq!(ToolKind::from_tool_name("network_probe"), ToolKind::Unknown);
        assert_eq!(ToolKind::from_tool_name("net"), ToolKind::Network);
    }
}
