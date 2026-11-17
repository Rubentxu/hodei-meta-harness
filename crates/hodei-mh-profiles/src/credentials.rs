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
pub fn secret_leak_positions(commands: &[String]) -> Vec<usize> {
    commands
        .iter()
        .enumerate()
        .filter(|(_, command)| {
            SECRET_KEYS.iter().any(|key| {
                command.contains(&format!("{key}=")) || command.contains(&format!("{key}:"))
            })
        })
        .map(|(index, _)| index)
        .collect()
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
}
