//! Provenance (pure, AC-4): every log line and event states WHERE it came
//! from — a human action, an automatic decision (with its rule) or
//! telemetry. Unknown telemetry is never presented as success, and
//! credentials NEVER reach the wire: lines are redacted before emission.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "prov", rename_all = "snake_case")]
pub enum Provenance {
    /// A client (CLI or TUI) asked for it.
    Human,
    /// The supervisor decided on its own — the rule is part of the record.
    Automatic { rule: String },
    /// Observed telemetry. `unknown: true` marks MISSING/unreadable
    /// telemetry; consumers must render it as unknown, not as success.
    Telemetry { unknown: bool },
}

/// Names whose VALUE is a credential. Compared case-insensitively: an
/// environment variable is the same secret whatever its case.
pub const SECRET_KEYS: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "AUTH_TOKEN",
    "API_KEY",
    "GATEWAY_AUTH_TOKEN",
    "REGISTRY_PASSWORD",
    "PASSWORD",
    "SECRET",
    "TOKEN",
];

/// The visible part of a redacted value.
const PLACEHOLDER: &str = "[redacted]";

/// Redact credentials from a line.
///
/// Covered shapes (all of them are real ways a secret travels):
/// - `KEY=value`, `KEY: value`
/// - `KEY="value"`, `KEY='value'` (the quotes stay, the value goes)
/// - `"KEY": "value"` (JSON)
/// - `Authorization: Bearer <token>`
///
/// Every occurrence is redacted, and the key name is matched regardless of
/// case. What is NOT covered: a bare secret with no key name next to it
/// (there is no way to know it is a secret) — callers that have the value
/// should never print it.
pub fn redact(text: &str) -> String {
    let mut redacted = redact_bearer_tokens(text);
    for key in SECRET_KEYS {
        redacted = redact_key_values(&redacted, key);
    }
    redacted
}

/// Replace every `Bearer <token>` with `Bearer [redacted]`.
fn redact_bearer_tokens(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find("bearer ") {
        let start = cursor + offset;
        let value_start = start + "bearer ".len();
        let rest = &text[value_start..];
        let value_len = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"' && *c != ';')
            .map(char::len_utf8)
            .sum::<usize>();
        out.push_str(&text[cursor..value_start]);
        if value_len > 0 {
            out.push_str(PLACEHOLDER);
            cursor = value_start + value_len;
        } else {
            cursor = value_start;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replace every value assigned to `key` (case-insensitive), in the shapes
/// `KEY=value`, `KEY: value`, `KEY="value"` and `"KEY": "value"`.
fn redact_key_values(text: &str, key: &str) -> String {
    let lower_key = key.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    let lower = text.to_ascii_lowercase();
    while let Some(offset) = lower[cursor..].find(&lower_key) {
        let start = cursor + offset;
        // The key must not be part of a longer word (TOKEN vs TOKENS).
        let before_ok = start == 0
            || !text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_');
        // JSON shape: the key is quoted, so a closing quote sits between the
        // key name and the separator (`"KEY": "value"`).
        let mut after = start + key.len();
        let quoted_key = text[after..].starts_with('"');
        if quoted_key {
            after += 1;
        }
        let separator = lower[after..].chars().next();
        let is_separator = matches!(separator, Some('=') | Some(':'));
        if !before_ok || !is_separator {
            out.push_str(&text[cursor..start + key.len()]);
            cursor = start + key.len();
            continue;
        }
        // Skip the separator and any spaces.
        let value_start = after + 1;
        let rest = &text[value_start..];
        let spaces = rest.len() - rest.trim_start().len();
        let value_start = value_start + spaces;
        // Quoted value: the quotes stay, the content goes. An unterminated
        // quote redacts to the end of the line (fail closed).
        let value = &text[value_start..];
        let (value_len, quoted) = match value.chars().next() {
            Some(quote @ ('"' | '\'')) => {
                let closing = value[1..].find(quote).map(|i| i + 1);
                (closing.unwrap_or(value.len()), true)
            }
            _ => {
                let len = value
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"' && *c != ';')
                    .map(char::len_utf8)
                    .sum::<usize>();
                (len, false)
            }
        };
        out.push_str(&text[cursor..value_start]);
        if value_len > 0 {
            out.push_str(PLACEHOLDER);
            if quoted {
                out.push(value.chars().next().expect("quoted value has a quote"));
            }
            cursor = value_start + value_len;
        } else {
            cursor = value_start;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credentials_never_reach_the_record() {
        let line = "started agent with ANTHROPIC_AUTH_TOKEN=sk-ant-987xyz and api key API_KEY: abc123 done";
        let safe = redact(line);
        assert!(!safe.contains("sk-ant-987xyz"));
        assert!(!safe.contains("abc123"));
        assert!(safe.contains("ANTHROPIC_AUTH_TOKEN=[redacted]"));
        assert!(safe.contains("API_KEY: [redacted]"));
        assert!(safe.contains("done"));
    }

    #[test]
    fn plain_lines_pass_through_untouched() {
        let line = "run phase Running, ticket TASK-9";
        assert_eq!(redact(line), line);
    }

    #[test]
    fn quoted_values_are_redacted_too() {
        let safe = redact(r#"ANTHROPIC_AUTH_TOKEN="sk-quoted-secret""#);
        assert!(!safe.contains("sk-quoted-secret"), "{safe}");
        assert!(safe.contains("[redacted]"));
    }

    #[test]
    fn json_shaped_secrets_are_redacted() {
        let safe = redact(r#"{"ANTHROPIC_AUTH_TOKEN": "sk-json-secret"}"#);
        assert!(!safe.contains("sk-json-secret"), "{safe}");
    }

    #[test]
    fn every_occurrence_is_redacted_not_only_the_first() {
        let safe = redact("A=1 ANTHROPIC_AUTH_TOKEN=sk-first ANTHROPIC_AUTH_TOKEN=sk-second");
        assert!(!safe.contains("sk-first"), "{safe}");
        assert!(!safe.contains("sk-second"), "{safe}");
    }

    #[test]
    fn bearer_tokens_are_redacted_without_a_key_name() {
        let safe = redact("Authorization: Bearer sk-ant-bare");
        assert!(!safe.contains("sk-ant-bare"), "{safe}");
        assert!(safe.contains("Bearer [redacted]"));
    }

    #[test]
    fn key_names_are_matched_case_insensitively() {
        let safe = redact("anthropic_auth_token=sk-lower");
        assert!(!safe.contains("sk-lower"), "{safe}");
    }

    #[test]
    fn a_longer_word_containing_a_key_is_not_touched() {
        let line = "TOKENS=3 tokens_total=9";
        assert_eq!(redact(line), line);
    }

    #[test]
    fn unknown_telemetry_is_explicit_never_success() {
        let prov = Provenance::Telemetry { unknown: true };
        let encoded = serde_json::to_string(&prov).expect("encode");
        assert!(encoded.contains("\"unknown\":true"));
    }
}
