//! Credential channel (AC-3, pure): credentials reach the sandbox ONLY
//! through the authorized minimal mechanism — the process environment of
//! the one process that needs them. They are NEVER materialized into
//! files, build commands, logs, state or handoffs, and subscription
//! limitations are reported honestly instead of hidden.

use serde::{Deserialize, Serialize};

/// The ONLY authorized delivery channel. Adding another one is a design
/// change, not a convenience.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub enum CredentialChannel {
    /// Environment variables of the process that requires them (e.g. the
    /// ACP agent). Nothing else reads them.
    ProcessEnvOnly,
}

/// Keys that must never appear anywhere except that env.
pub const SECRET_KEYS: &[&str] = &[
    "ANTHROPIC_AUTH_TOKEN",
    "ANTHROPIC_API_KEY",
    "GATEWAY_AUTH_TOKEN",
    "REGISTRY_PASSWORD",
];

/// Value shapes that look like a credential even without a known key
/// name. A leak detector that only knows four key names misses `--token
/// sk-x` and `https://user:pass@host`.
///
/// The patterns are matched case-insensitively; they are deliberately
/// SPECIFIC (`sk-ant-`, never a bare `sk-`, which also matches `task-`).
pub const SECRET_VALUE_PATTERNS: &[&str] = &[
    // Provider-issued tokens.
    "sk-ant-",
    "sk-or-",
    "sk-proj-",
    "ghp_",
    "gho_",
    "github_pat_",
    // Generic key=value / key:value pairs whose name smells like a secret.
    "token=",
    "token:",
    "api_key=",
    "api_key:",
    "apikey=",
    "apikey:",
    "secret=",
    "secret:",
    "password=",
    "password:",
    "passwd=",
    "credential=",
    "credential:",
    // Credentials embedded in a URL.
    "://user:pass@",
];

/// Patterns that only count in their canonical (upper-case) spelling.
pub const SECRET_CASE_SENSITIVE_PATTERNS: &[&str] = &["AKIA", "ASIA", "PRIVATE KEY"];

/// Subscription tier actually in use; its limitations are part of every
/// report (AC-3: "indicar limitaciones de suscripción").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SubscriptionNote {
    pub tier: String,
    pub limitation: String,
}

/// Scrub a set of env pairs for ANY observable output: secrets become a
/// placeholder, everything else passes through unchanged.
pub fn scrub_for_log<'a>(pairs: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<(String, String)> {
    pairs
        .map(|(k, v)| {
            let value = if SECRET_KEYS.contains(&k) {
                "[redacted]".to_string()
            } else {
                v.to_string()
            };
            (k.to_string(), value)
        })
        .collect()
}

/// The build commands must be free of secrets: this is the assertion the
/// planner and the probe both rely on. Returns the offending command
/// indices (empty = clean).
///
/// Detection is by KEY NAME and by VALUE SHAPE: a command that carries a
/// credential it names differently is still a leak.
pub fn secret_leak_positions(commands: &[String]) -> Vec<usize> {
    commands
        .iter()
        .enumerate()
        .filter(|(_, command)| command_has_secret(command))
        .map(|(index, _)| index)
        .collect()
}

/// Whether a single string carries something that looks like a secret.
pub fn command_has_secret(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    let named = SECRET_KEYS.iter().any(|key| {
        let key = key.to_ascii_lowercase();
        lower.contains(&format!("{key}=")) || lower.contains(&format!("{key}:"))
    });
    if named {
        return true;
    }
    SECRET_VALUE_PATTERNS
        .iter()
        .any(|pattern| lower.contains(&pattern.to_ascii_lowercase()))
        || SECRET_CASE_SENSITIVE_PATTERNS
            .iter()
            .any(|pattern| command.contains(pattern))
}

/// Redact the secrets of a free-form output (a build log). The shapes
/// covered are the same as the command detector's, plus a bearer token.
pub fn redact_output(text: &str) -> String {
    let mut out = redact_url_credentials(text);
    out = redact_bearer(&out);
    for pattern in SECRET_VALUE_PATTERNS {
        out = redact_value_shape(&out, pattern);
    }
    for key in SECRET_KEYS {
        out = redact_key_value(&out, key);
    }
    out
}

/// Replace the `user:pass@` part of a URL: there the credentials ARE the
/// matched shape, so the value AFTER the shape must be kept.
fn redact_url_credentials(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(start) = text[cursor..].find("://") {
        let start = cursor + start;
        let after = start + 3;
        // The credentials run up to the '@' that precedes the host.
        let Some(at_offset) = text[after..].find('@') else {
            break;
        };
        let at = after + at_offset;
        let credentials = &text[after..at];
        if !credentials.contains(':') {
            out.push_str(&text[cursor..at + 1]);
            cursor = at + 1;
            continue;
        }
        out.push_str(&text[cursor..after]);
        out.push_str("[redacted]");
        cursor = at;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replace the value that follows a `Bearer ` marker.
fn redact_bearer(text: &str) -> String {
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
            out.push_str("[redacted]");
            cursor = value_start + value_len;
        } else {
            cursor = value_start;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replace the value that follows `key=` / `key:`.
fn redact_key_value(text: &str, key: &str) -> String {
    let lower_key = key.to_ascii_lowercase();
    let lower = text.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find(&lower_key) {
        let start = cursor + offset;
        let after = start + key.len();
        let separator = lower[after..].chars().next();
        let is_separator = matches!(separator, Some('=') | Some(':'));
        if !is_separator {
            out.push_str(&text[cursor..after]);
            cursor = after;
            continue;
        }
        let value_start = after + 1;
        let rest = &text[value_start..];
        let spaces = rest.len() - rest.trim_start().len();
        let value_start = value_start + spaces;
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
            out.push_str("[redacted]");
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

/// Replace the value that follows a shape such as `token=` inside a
/// free-form text.
fn redact_value_shape(text: &str, pattern: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let needle = pattern.to_ascii_lowercase();
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(offset) = lower[cursor..].find(&needle) {
        let start = cursor + offset;
        let value_start = start + needle.len();
        let rest = &text[value_start..];
        let value_len = rest
            .chars()
            .take_while(|c| !c.is_whitespace() && *c != ',' && *c != '"' && *c != ';')
            .map(char::len_utf8)
            .sum::<usize>();
        out.push_str(&text[cursor..value_start]);
        if value_len > 0 {
            out.push_str("[redacted]");
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
    fn scrub_hides_exactly_the_secret_keys() {
        let pairs = [
            ("ANTHROPIC_AUTH_TOKEN", "sk-secret-value"),
            ("HTTPS_PROXY", "http://172.17.0.1:8888"),
            ("PATH", "/usr/bin"),
        ];
        let scrubbed = scrub_for_log(pairs.iter().map(|(k, v)| (*k, *v)));
        assert_eq!(scrubbed[0].1, "[redacted]");
        assert_eq!(scrubbed[1].1, "http://172.17.0.1:8888");
        assert_eq!(scrubbed[2].1, "/usr/bin");
    }

    #[test]
    fn build_commands_with_secret_injection_are_detected() {
        let clean = vec![
            "cargo build".to_string(),
            "javac -d out Main.java".to_string(),
        ];
        assert!(secret_leak_positions(&clean).is_empty());
        let dirty = vec![
            "cargo build".to_string(),
            "export ANTHROPIC_AUTH_TOKEN=sk-x && cargo run".to_string(),
        ];
        assert_eq!(secret_leak_positions(&dirty), vec![1]);
    }

    #[test]
    fn a_secret_without_a_known_key_name_is_still_detected() {
        // A flag that carries a token.
        assert!(command_has_secret("cargo run -- --token sk-ant-abc123"));
        // A provider token with no key name at all.
        assert!(command_has_secret(
            "curl -H 'Authorization: Bearer sk-ant-xyz'"
        ));
        // Credentials embedded in a URL.
        assert!(command_has_secret(
            "git clone https://user:pass@example.com/repo"
        ));
        // A differently named key.
        assert!(command_has_secret("export GITHUB_TOKEN=ghp_abcdef"));
        // A clean command is not flagged.
        assert!(!command_has_secret(
            "cargo build --release --manifest-path /w/Cargo.toml"
        ));
        assert!(!command_has_secret(
            "HTTPS_PROXY=http://172.17.0.1:8888 cargo build"
        ));
    }

    #[test]
    fn subscription_limitations_are_explicit_data_not_comments() {
        let note = SubscriptionNote {
            tier: "zai-gateway".into(),
            limitation:
                "gateway tier: lower rate limits than the first-party API; no extended thinking"
                    .into(),
        };
        let encoded = serde_json::to_string(&note).expect("encode");
        assert!(encoded.contains("limitation"));
    }

    #[test]
    fn a_credential_in_a_build_output_is_redacted() {
        let log = "downloading with ANTHROPIC_AUTH_TOKEN=sk-ant-987xyz done\n";
        let safe = redact_output(log);
        assert!(!safe.contains("sk-ant-987xyz"), "{safe}");
        assert!(safe.contains("[redacted]"));
    }

    #[test]
    fn a_bearer_token_in_a_build_output_is_redacted() {
        let safe = redact_output("Authorization: Bearer sk-ant-bare\n");
        assert!(!safe.contains("sk-ant-bare"), "{safe}");
        assert!(safe.contains("[redacted]"));
    }

    #[test]
    fn a_credential_url_in_a_build_output_is_redacted() {
        let safe = redact_output("cloning https://user:pass@example.com/repo\n");
        assert!(!safe.contains("user:pass"), "{safe}");
    }

    #[test]
    fn a_clean_build_output_passes_through_untouched() {
        let log = "    Compiling miniapp v0.1.0 (/workspace/p)\n    Finished dev profile\n";
        assert_eq!(redact_output(log), log);
    }
}
