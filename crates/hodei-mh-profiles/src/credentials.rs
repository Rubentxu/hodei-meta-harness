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

/// What a redacted secret looks like. The placeholder is NOT a secret:
/// the detector must not mistake it for one, or a redacted record would
/// keep failing the leak check that it exists to pass.
pub const REDACTED: &str = "[redacted]";

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
                REDACTED.to_string()
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
        // Detection and redaction must see the same shapes: a URL
        // credential whose token has no known shape
        // (`https://<random>@host`) is still a credential.
        || url_credentials_present(command)
}

/// Redact the secrets of a free-form output (a build log). The shapes
/// covered are the same as the command detector's, plus a bearer token,
/// URL userinfo (colon or not), scheme-less `user:pass@host` references
/// and AWS access key ids.
///
/// CONTRACT: any text reaching a log, a record or a truncation must pass
/// through this FIRST. Truncating before redacting keeps the bytes of a
/// secret that sat in the retained window — the tail of a credential-
/// printing build was persisted whole while the head was dropped.
pub fn redact_output(text: &str) -> String {
    let mut out = redact_spans(&url_userinfo_spans(text), text);
    out = redact_spans(&schemeless_credential_spans(&out), &out);
    out = redact_bearer(&out);
    out = redact_pem_blocks(&out);
    out = redact_aws_access_key_id(&out);
    for pattern in SECRET_VALUE_PATTERNS {
        out = redact_value_shape(&out, pattern);
    }
    for key in SECRET_KEYS {
        out = redact_key_value(&out, key);
    }
    out
}

/// A PEM block travels whole or not at all: the body of a private key IS
/// the credential, and redacting only its header would leave the key in
/// the log.
fn redact_pem_blocks(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find("-----BEGIN ") {
        let start = cursor + rel;
        let Some(end_rel) = text[start..].find("-----END ") else {
            break;
        };
        let end_start = start + end_rel;
        let Some(end) = text[end_start..]
            .find("-----")
            .map(|offset| end_start + offset + "-----".len())
        else {
            break;
        };
        out.push_str(&text[cursor..start]);
        out.push_str(REDACTED);
        cursor = end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Replace every `[start..end)` span with the placeholder.
fn redact_spans(spans: &[(usize, usize)], text: &str) -> String {
    if spans.is_empty() {
        return text.to_string();
    }
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    for (start, end) in spans {
        if *start < cursor {
            continue; // overlapping span from an earlier pass
        }
        out.push_str(&text[cursor..*start]);
        out.push_str(REDACTED);
        cursor = *end;
    }
    out.push_str(&text[cursor..]);
    out
}

/// Where a URL authority ends: path/query/fragment start, a quote or
/// whitespace — never the remainder of the whole text. An unbounded scan
/// redacts across sentences; a `break` on the first clean URL hides every
/// later credential in the same line.
fn authority_end(text: &str, from: usize) -> usize {
    from + text[from..]
        .char_indices()
        .find(|(_, c)| matches!(c, '/' | '?' | '#' | '"' | '\'' | '`') || c.is_whitespace())
        .map(|(i, _)| i)
        .unwrap_or(text.len() - from)
}

/// The userinfo spans of every `scheme://user[:pass]@host` in the text.
/// ANY non-empty userinfo counts: a token used AS the username
/// (`https://<hex>@registry.internal`, routine in package.json git
/// dependencies printed by npm) is exactly as secret as `user:pass@`.
/// The old colon-only check passed it through untouched.
fn url_userinfo_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut scan = 0usize;
    while let Some(rel) = text[scan..].find("://") {
        let authority_start = scan + rel + 3;
        let end = authority_end(text, authority_start);
        if let Some(at) = text[authority_start..end].find('@')
            && at > 0
        {
            let userinfo = &text[authority_start..authority_start + at];
            // The placeholder of an already redacted text is not a
            // credential.
            if userinfo != REDACTED {
                spans.push((authority_start, authority_start + at));
            }
        }
        scan = end.max(authority_start);
    }
    spans
}

/// Credentials in scheme-less `user:pass@host` references (git's scp-like
/// syntax, JDBC URLs): there is no `://` to anchor on. The userinfo window
/// runs back to whitespace, a quote, a slash or a previous `@`; it must
/// carry a `:` (a bare `git@github.com` is an identity, not a credential)
/// and the host after the `@` must look like a host.
fn schemeless_credential_spans(text: &str) -> Vec<(usize, usize)> {
    let mut spans = Vec::new();
    let mut cursor = 0usize;
    while let Some(rel) = text[cursor..].find('@') {
        let at = cursor + rel;
        let start = text[..at]
            .char_indices()
            .rev()
            .find(|(_, c)| matches!(c, '/' | '"' | '\'' | '`' | '@') || c.is_whitespace())
            .map(|(i, _)| i + 1)
            .unwrap_or(0);
        let userinfo = &text[start..at];
        let host = &text[at + 1..authority_end(text, at + 1)];
        if userinfo.contains(':') && (host.contains('.') || host.contains(':')) {
            spans.push((start, at));
        }
        cursor = at + 1;
    }
    spans
}

/// The prefixes of an AWS access key id. The detection patterns are
/// case-sensitive on purpose: `AKIA`/`ASIA` are fixed literals.
const AWS_KEY_PREFIXES: &[&str] = &["AKIA", "ASIA"];

/// The offset of the first occurrence of any of the literals, or `None`.
fn find_any(text: &str, needles: &[&str]) -> Option<usize> {
    needles
        .iter()
        .filter_map(|needle| text.find(needle).map(|at| (at, needle.len())))
        .min_by_key(|(at, _)| *at)
        .map(|(at, _)| at)
}

/// AWS access key ids are caught by the case-sensitive detection patterns
/// but were NEVER redacted: a build that echoes its provider config
/// persisted `AKIA...` verbatim into the log tail and the durable export.
fn redact_aws_access_key_id(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut cursor = 0usize;
    while let Some(rel) = find_any(&text[cursor..], AWS_KEY_PREFIXES) {
        let start = cursor + rel;
        let body = &text[start + 4..];
        let body_len = body
            .bytes()
            .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
            .count();
        if body_len == 16 {
            out.push_str(&text[cursor..start]);
            out.push_str(REDACTED);
            cursor = start + 4 + body_len;
        } else {
            out.push_str(&text[cursor..start + 4]);
            cursor = start + 4;
        }
    }
    out.push_str(&text[cursor..]);
    out
}

/// Whether the text carries URL credentials of any userinfo shape.
fn url_credentials_present(text: &str) -> bool {
    !url_userinfo_spans(text).is_empty() || !schemeless_credential_spans(text).is_empty()
}

/// Whether a reference (an image, a registry) carries credentials in its
/// authority.
///
/// The `name@sha256:<digest>` form is NOT a credential: the `@` there
/// separates the name from its digest, and a registry reference never
/// needs a user. Everything else that carries an `@` in the authority
/// does.
pub fn reference_carries_credentials(reference: &str) -> bool {
    let authority = reference.split('/').next().unwrap_or_default();
    let Some((_, host)) = authority.rsplit_once('@') else {
        return false;
    };
    !host.starts_with("sha256:") && !host.starts_with("sha512:")
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
            out.push_str(REDACTED);
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
            out.push_str(REDACTED);
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
            out.push_str(REDACTED);
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
        assert_eq!(scrubbed[0].1, REDACTED);
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
        assert!(safe.contains(REDACTED));
    }

    #[test]
    fn a_bearer_token_in_a_build_output_is_redacted() {
        let safe = redact_output("Authorization: Bearer sk-ant-bare\n");
        assert!(!safe.contains("sk-ant-bare"), "{safe}");
        assert!(safe.contains(REDACTED));
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

    #[test]
    fn a_token_used_as_the_username_of_a_url_is_redacted() {
        // `https://<token>@host` is routine in package.json git
        // dependencies: the token is the username and there is no colon.
        let safe = redact_output("cloning https://ghp_abcdef123456@github.com/team/repo\n");
        assert!(!safe.contains("ghp_abcdef123456"), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
        // The host survives: only the credential is replaced.
        assert!(safe.contains("github.com/team/repo"), "{safe}");
    }

    #[test]
    fn a_scp_style_credential_is_redacted() {
        // git's scp-like syntax has no scheme to anchor on.
        let safe = redact_output("git clone git://user:pass@internal.host/team/repo.git\n");
        assert!(!safe.contains("user:pass"), "{safe}");
    }

    #[test]
    fn an_identity_without_a_password_is_not_a_credential() {
        // `git@github.com` is an identity, not a secret.
        let log = "cloning git@github.com:team/repo.git\n";
        assert_eq!(redact_output(log), log);
    }

    #[test]
    fn an_aws_access_key_id_is_redacted() {
        let safe = redact_output("aws_access_key_id = AKIAIOSFODNN7EXAMPLE\n");
        assert!(!safe.contains("AKIAIOSFODNN7EXAMPLE"), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
    }

    #[test]
    fn a_pem_block_is_redacted_whole() {
        let log = "-----BEGIN PRIVATE KEY-----\nMIIEvQIBADANBgkqhkiG9w0BAQEFAASC\nCgQ\n-----END PRIVATE KEY-----\n";
        let safe = redact_output(log);
        assert!(!safe.contains("MIIEvQIBADANBgkqhkiG9w0BAQEFAASC"), "{safe}");
        assert!(safe.contains(REDACTED), "{safe}");
        // The text around the block survives.
        assert!(safe.ends_with('\n'), "{safe}");
    }

    #[test]
    fn a_provider_token_is_redacted_by_its_prefix() {
        let safe = redact_output("using ghp_abcdef123456 to clone\n");
        assert!(!safe.contains("abcdef123456"), "{safe}");
    }

    #[test]
    fn the_detector_and_the_redaction_see_the_same_shapes() {
        // Every shape the detector flags must lose its VALUE: a
        // detector that sees what the redaction missed is the difference
        // between a clean log and a leaked one.
        for (secret, value) in [
            ("export ANTHROPIC_AUTH_TOKEN=sk-ant-abc123", "sk-ant-abc123"),
            (
                "curl -H 'Authorization: Bearer sk-ant-abc123'",
                "sk-ant-abc123",
            ),
            ("git clone https://user:pass@example.com/repo", "user:pass"),
            (
                "git clone https://ghp_abcdef@example.com/repo",
                "ghp_abcdef",
            ),
            ("export GITHUB_TOKEN=ghp_abcdef", "abcdef"),
            (
                "aws_access_key_id = AKIAIOSFODNN7EXAMPLE",
                "AKIAIOSFODNN7EXAMPLE",
            ),
        ] {
            assert!(command_has_secret(secret), "not detected: {secret}");
            let safe = redact_output(secret);
            assert!(!safe.contains(value), "the value survived: {safe}");
        }
    }

    #[test]
    fn a_redacted_credential_is_not_a_credential_anymore() {
        // The detector that works by VALUE SHAPE must fall silent once
        // the value is gone. The one that works by KEY NAME does not: a
        // command that names a secret key is suspicious even with an
        // empty value, and the leak check reads the raw strings anyway.
        for secret in [
            "curl -H 'Authorization: Bearer sk-ant-abc123'",
            "git clone https://user:pass@example.com/repo",
            "git clone https://ghp_abcdef@example.com/repo",
        ] {
            let safe = redact_output(secret);
            assert!(
                !command_has_secret(&safe),
                "still a credential after the redaction: {safe}"
            );
        }
    }

    #[test]
    fn a_reference_carries_credentials_only_when_it_has_a_user() {
        assert!(reference_carries_credentials(
            "user:pass@registry.local/team/image:tag"
        ));
        assert!(reference_carries_credentials("token@registry.local/img"));
        assert!(!reference_carries_credentials(
            "registry.local/team/image:tag"
        ));
        assert!(!reference_carries_credentials(
            "rust@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea"
        ));
    }
}
