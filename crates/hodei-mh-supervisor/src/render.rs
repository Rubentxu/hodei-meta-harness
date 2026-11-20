//! Pure rendering: the SAME textual view for the TUI and the CLI. One
//! contract, two clients — `sup-status` prints these lines and the TUI
//! draws them as rows, so the two surfaces can never disagree about what
//! the state means.

use crate::protocol::{ResourceObservation, Snapshot, StructuredEvent};

/// Make a client-supplied string safe to print: control characters would
/// let a ticket forge whole output lines. They are shown escaped instead.
pub fn escape_control(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

pub fn render_snapshot(snapshot: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("project: {}", escape_control(&snapshot.project)));
    lines.push(format!("phase: {}", phase_label(&snapshot.phase)));
    if snapshot.paused {
        lines.push("paused: yes (prompt loop gated)".to_string());
    }
    lines.push(format!(
        "ticket: {}",
        escape_control(snapshot.ticket.as_deref().unwrap_or("(none)"))
    ));
    lines.push(format!(
        "goal: {}",
        match &snapshot.goal {
            Some(objective) => format!("custom objective: {}", escape_control(objective)),
            None => "tracker work".to_string(),
        }
    ));
    lines.push(format!(
        "revision: {}",
        escape_control(snapshot.revision.as_deref().unwrap_or("(none)"))
    ));
    // The runtime's KIND is stated, never hidden: a rehearsal that looks
    // like a live run is the one lie this surface must not tell.
    lines.push(format!(
        "runtime: {}",
        match &snapshot.runtime {
            Some(runtime) => escape_control(&runtime.label()),
            None => "(none — no runtime is wired)".to_string(),
        }
    ));
    lines.push(format!(
        "objective: {}",
        match &snapshot.objective {
            Some(report) => escape_control(&report.outcome.describe()),
            None => "(none yet)".to_string(),
        }
    ));
    lines.push(format!(
        "resources: {}",
        resources_label(&snapshot.resources)
    ));
    lines.push(format!(
        "context available: {}",
        if snapshot.context_available {
            "yes"
        } else {
            "no"
        }
    ));
    lines.push(format!(
        "waiting: {}",
        escape_control(snapshot.wait_reason.as_deref().unwrap_or("(not waiting)"))
    ));
    lines.push(format!(
        "ttl remaining: {}",
        snapshot
            .ttl_remaining_seconds
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "(untracked)".to_string())
    ));
    lines.push(format!("epoch: {}", snapshot.epoch));
    lines.push(format!(
        "grant scope: {}",
        if snapshot.grant_effects.is_empty() {
            "(none — the supervisor authorizes no effect by itself)".to_string()
        } else {
            snapshot.grant_effects.join(", ")
        }
    ));
    if let Some(degraded) = &snapshot.degraded {
        lines.push(format!("DEGRADED: {}", escape_control(degraded)));
    }
    lines.push(format!(
        "last action: {}",
        match &snapshot.last_action {
            Some(outcome) => format!(
                "{} applied={} ({})",
                action_label(&outcome.action),
                outcome.applied,
                escape_control(&outcome.detail)
            ),
            None => "(none yet)".to_string(),
        }
    ));
    lines
}

pub fn render_event(event: &StructuredEvent) -> String {
    let provenance = match &event.provenance {
        crate::provenance::Provenance::Human => "human".to_string(),
        crate::provenance::Provenance::Automatic { rule } => format!("auto[{rule}]"),
        crate::provenance::Provenance::Telemetry { unknown } => {
            if *unknown {
                "telemetry(UNKNOWN)".to_string()
            } else {
                "telemetry".to_string()
            }
        }
    };
    format!(
        "#{} [{provenance}] {} (epoch {}) {}",
        event.seq,
        escape_control(&event.kind),
        event.epoch,
        render_payload(&event.payload)
    )
}

/// Render the body of an event for a human.
///
/// The body is a TYPED value: an object is rendered as its fields, so
/// the reader sees what the event said instead of a flat sentence that
/// has to be parsed.
fn render_payload(payload: &serde_json::Value) -> String {
    match payload {
        serde_json::Value::Object(fields) => fields
            .iter()
            .map(|(key, value)| format!("{}={}", escape_control(key), render_scalar(value)))
            .collect::<Vec<String>>()
            .join(" "),
        other => escape_control(&other.to_string()),
    }
}

/// Render one value of a body.
fn render_scalar(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => escape_control(text),
        serde_json::Value::Null => "null".to_string(),
        other => escape_control(&other.to_string()),
    }
}

fn phase_label(phase: &run::run::Phase) -> String {
    match phase {
        run::run::Phase::Idle => "idle".to_string(),
        run::run::Phase::Starting => "starting (awaiting the runtime's confirmation)".to_string(),
        run::run::Phase::Running => "running".to_string(),
        run::run::Phase::Stopping => "stopping".to_string(),
        run::run::Phase::Stopped => "stopped".to_string(),
    }
}

fn resources_label(observation: &ResourceObservation) -> String {
    match observation {
        ResourceObservation::Measured { cpus, memory_gib } => {
            format!("measured cpus={cpus} memory_gib={memory_gib}")
        }
        ResourceObservation::Unknown { reason } => {
            format!("UNKNOWN ({reason})")
        }
    }
}

fn action_label(action: &crate::protocol::SupervisorAction) -> String {
    match action {
        crate::protocol::SupervisorAction::Start { project, .. } => format!("start({project})"),
        crate::protocol::SupervisorAction::StartObjective { project, .. } => {
            format!("start_objective({project})")
        }
        crate::protocol::SupervisorAction::Pause => "pause".to_string(),
        crate::protocol::SupervisorAction::Resume => "resume".to_string(),
        crate::protocol::SupervisorAction::Stop => "stop".to_string(),
        crate::protocol::SupervisorAction::Renew { added_seconds } => {
            format!("renew(+{added_seconds}s)")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_telemetry_is_shown_as_unknown_not_as_a_measurement() {
        let snapshot = Snapshot {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            project: "gc".into(),
            phase: run::run::Phase::Running,
            paused: false,
            ticket: None,
            goal: None,
            revision: None,
            runtime: None,
            objective: None,
            resources: ResourceObservation::Unknown {
                reason: "no cgroup path".into(),
            },
            context_available: false,
            wait_reason: None,
            last_action: None,
            ttl_remaining_seconds: None,
            epoch: 0,
            grant_effects: Vec::new(),
            degraded: None,
        };
        let lines = render_snapshot(&snapshot);
        let resources = lines
            .iter()
            .find(|l| l.starts_with("resources:"))
            .expect("resources line");
        assert!(resources.contains("UNKNOWN"));
        assert!(!resources.contains("measured"));
    }

    #[test]
    fn events_render_provenance_and_unknown_telemetry_flag() {
        let event = StructuredEvent {
            seq: 7,
            epoch: 2,
            kind: "telemetry/resources".into(),
            payload: serde_json::json!({"unknown": true, "reason": "missing"}),
            provenance: crate::provenance::Provenance::Telemetry { unknown: true },
        };
        let line = render_event(&event);
        assert!(line.contains("telemetry(UNKNOWN)"));
        assert!(line.contains("#7"));
        // The body keeps its structure: the reader sees the fields.
        assert!(line.contains("reason=missing"), "{line}");
        assert!(line.contains("epoch 2"), "{line}");
    }

    #[test]
    fn control_characters_in_client_data_cannot_forge_output_lines() {
        let snapshot = Snapshot {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            project: "gc".into(),
            phase: run::run::Phase::Idle,
            paused: false,
            ticket: Some("TASK-1\nphase: running".into()),
            goal: None,
            revision: None,
            runtime: None,
            objective: None,
            resources: ResourceObservation::Unknown {
                reason: "none".into(),
            },
            context_available: false,
            wait_reason: None,
            last_action: None,
            ttl_remaining_seconds: None,
            epoch: 0,
            grant_effects: Vec::new(),
            degraded: None,
        };
        let lines = render_snapshot(&snapshot);
        // The forged line must NOT appear as a line of its own.
        assert!(
            !lines.iter().any(|l| l.trim() == "phase: running"),
            "a ticket must not forge a phase line: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains("TASK-1\\nphase: running")),
            "the newline must be visible as an escape: {lines:?}"
        );
    }

    #[test]
    fn the_declared_grant_scope_and_a_degraded_core_are_visible() {
        let snapshot = Snapshot {
            protocol_version: crate::protocol::PROTOCOL_VERSION,
            project: "gc".into(),
            phase: run::run::Phase::Running,
            paused: false,
            ticket: None,
            goal: None,
            revision: None,
            runtime: None,
            objective: None,
            resources: ResourceObservation::Unknown {
                reason: "none".into(),
            },
            context_available: false,
            wait_reason: None,
            last_action: None,
            ttl_remaining_seconds: None,
            epoch: 3,
            grant_effects: Vec::new(),
            degraded: Some("poisoned lock recovered".into()),
        };
        let lines = render_snapshot(&snapshot);
        assert!(lines.iter().any(|l| l.contains("epoch: 3")));
        assert!(lines.iter().any(|l| l.contains("grant scope: (none")));
        assert!(lines.iter().any(|l| l.contains("DEGRADED:")));
    }
}
