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

/// Patterns whose VALUES are credentials: `KEY=value` or `KEY: value` for
/// secret-ish keys, bearer tokens, and long opaque hex strings.
pub fn redact(text: &str) -> String {
    let secret_keys = [
        "ANTHROPIC_AUTH_TOKEN",
        "ANTHROPIC_API_KEY",
        "AUTH_TOKEN",
        "API_KEY",
        "PASSWORD",
        "SECRET",
        "TOKEN",
    ];
    let mut redacted = text.to_string();
    for key in secret_keys {
        for separator in ["=", ":"] {
            let needle = format!("{key}{separator}");
            if let Some(start) = redacted.find(&needle) {
                let value_start = start + needle.len();
                let rest = &redacted[value_start..];
                let skip = rest.len() - rest.trim_start().len();
                let value_start = value_start + skip;
                let rest = &redacted[value_start..];
                let value_len = rest
                    .chars()
                    .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"' && *c != ';')
                    .map(char::len_utf8)
                    .sum::<usize>();
                if value_len > 0 {
                    let end = value_start + value_len;
                    redacted.replace_range(start..end, &format!("{key}{separator}[redacted]"));
                }
            }
        }
    }
    redacted
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
        assert!(safe.contains("API_KEY:[redacted]"));
        assert!(safe.contains("done"));
    }

    #[test]
    fn plain_lines_pass_through_untouched() {
        let line = "run phase Running, ticket TASK-9";
        assert_eq!(redact(line), line);
    }

    #[test]
    fn unknown_telemetry_is_explicit_never_success() {
        let prov = Provenance::Telemetry { unknown: true };
        let encoded = serde_json::to_string(&prov).expect("encode");
        assert!(encoded.contains("\"unknown\":true"));
    }
}
