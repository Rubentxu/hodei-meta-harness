//! Bounded, redacted HANDOFF.md rendering (AC-4): built ONLY from the
//! confirmed checkpoint — never from the conversation transcript, so a
//! canary present only in the old session cannot leak into the new request
//! (AC-2). Secrets are redacted; size is capped.

use crate::checkpoint::Checkpoint;

/// Max rendered size in bytes before truncation markers.
pub const MAX_HANDOFF_BYTES: usize = 8 * 1024;

#[derive(Debug, thiserror::Error, PartialEq)]
#[error("handoff rejected: {0}")]
pub struct HandoffRejected(pub String);

/// Content lines that must never enter a handoff. By construction the
/// handoff takes no transcript parameter at all — this guard exists for
/// callers that compose text from arbitrary sources.
pub fn contains_canary_risk(text: &str) -> bool {
    const TRANSCRIPT_MARKERS: [&str; 3] = ["<transcript", "TRANSCRIPT>", "full conversation"];
    TRANSCRIPT_MARKERS.iter().any(|m| text.contains(m))
}

/// Secret-looking substrings are redacted in place.
pub fn redact_secrets(text: &str) -> String {
    let mut redacted = String::with_capacity(text.len());
    for line in text.lines() {
        redacted.push_str(&redact_line(line));
        redacted.push('\n');
    }
    redacted
}

fn redact_line(line: &str) -> String {
    const MARKERS: [&str; 10] = [
        "AKIA",
        "ghp_",
        "gho_",
        "sk-",
        "xoxb-",
        "BEGIN RSA PRIVATE KEY",
        "BEGIN PRIVATE KEY",
        "password",
        "token",
        "secret",
    ];
    let lower = line.to_ascii_lowercase();
    if MARKERS
        .iter()
        .any(|m| lower.contains(&m.to_ascii_lowercase()))
    {
        return "[REDACTED]".to_string();
    }
    line.to_string()
}

/// Render HANDOFF.md from the checkpoint: next action, decisions,
/// references, evidences, failed strategies. Bounded size.
pub fn render_handoff(checkpoint: &Checkpoint) -> Result<String, HandoffRejected> {
    let mut out = String::new();
    out.push_str(&format!(
        "# HANDOFF — {} (epoch {}, session {})\n\n",
        checkpoint.goal_id, checkpoint.epoch, checkpoint.session_id
    ));
    out.push_str(&format!("WorkRef: {}\n\n", checkpoint.work_ref));
    out.push_str(&format!("## Next action\n{}\n\n", checkpoint.next_action));
    section(&mut out, "Decisions", &checkpoint.decisions);
    section(&mut out, "References", &checkpoint.references);
    section(&mut out, "Evidences", &checkpoint.evidences);
    section(&mut out, "Failed strategies", &checkpoint.failed_strategies);

    if contains_canary_risk(&out) {
        return Err(HandoffRejected("transcript-like content detected".into()));
    }
    let redacted = redact_secrets(&out);
    if redacted.len() > MAX_HANDOFF_BYTES {
        let mut bounded = redacted[..MAX_HANDOFF_BYTES].to_string();
        bounded.push_str("\n[TRUNCATED]\n");
        return Ok(bounded);
    }
    Ok(redacted)
}

fn section(out: &mut String, title: &str, items: &[String]) {
    if items.is_empty() {
        return;
    }
    out.push_str(&format!("## {title}\n"));
    for item in items {
        out.push_str(&format!("- {item}\n"));
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checkpoint::Checkpoint;

    fn checkpoint() -> Checkpoint {
        Checkpoint {
            goal_id: "goal-9".into(),
            work_ref: "TASK-7".into(),
            epoch: 5,
            session_id: "s5".into(),
            next_action: "run rollover-probe".into(),
            decisions: vec!["pipe transport".into()],
            references: vec!["TASK-7".into()],
            evidences: vec!["3 green runs".into()],
            failed_strategies: vec!["session/load resume".into()],
        }
    }

    #[test]
    fn handoff_contains_next_action_and_sections_not_transcript() {
        let rendered = render_handoff(&checkpoint()).unwrap();
        assert!(rendered.contains("run rollover-probe"));
        assert!(rendered.contains("## Decisions"));
        assert!(rendered.contains("- pipe transport"));
        assert!(rendered.contains("## Failed strategies"));
        assert!(rendered.contains("goal-9"));
        assert!(!rendered.contains("transcript"));
    }

    #[test]
    fn conversation_canary_cannot_leak_handoff_is_checkpoint_only() {
        // The renderer signature has no transcript parameter: a canary that
        // existed only in the old conversation has no path into the output.
        // Guard rails reject transcript-like content if ever composed in.
        assert!(contains_canary_risk("<transcript>canary-xyz</transcript>"));
        let mut bad = checkpoint();
        bad.next_action = "see full conversation of prior session".into();
        assert!(render_handoff(&bad).is_err());
    }

    #[test]
    fn secrets_are_redacted_in_every_section() {
        let mut leaking = checkpoint();
        leaking.references = vec!["https://x/token=abc123".into()];
        leaking.evidences = vec!["key sk-abcdef0123".into()];
        let rendered = render_handoff(&leaking).unwrap();
        assert!(!rendered.contains("abc123"));
        assert!(!rendered.contains("sk-abcdef0123"));
        assert!(rendered.contains("[REDACTED]"));
    }

    #[test]
    fn oversized_handoffs_are_bounded() {
        let mut big = checkpoint();
        big.evidences = (0..2000)
            .map(|i| format!("evidence line {i} with padding padding padding"))
            .collect();
        let rendered = render_handoff(&big).unwrap();
        const MARKER: &str = "\n[TRUNCATED]\n";
        assert!(rendered.len() <= MAX_HANDOFF_BYTES + MARKER.len());
        assert!(rendered.contains("[TRUNCATED]"));
    }
}
