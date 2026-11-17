//! Pure rendering: the SAME textual view for the TUI and the CLI. One
//! contract, two clients — `sup-status` prints these lines and the TUI
//! draws them as rows, so the two surfaces can never disagree about what
//! the state means.

use crate::protocol::{ResourceObservation, Snapshot, StructuredEvent};

pub fn render_snapshot(snapshot: &Snapshot) -> Vec<String> {
    let mut lines = Vec::new();
    lines.push(format!("project: {}", snapshot.project));
    lines.push(format!("phase: {}", phase_label(&snapshot.phase)));
    if snapshot.paused {
        lines.push("paused: yes (prompt loop gated)".to_string());
    }
    lines.push(format!(
        "ticket: {}",
        snapshot.ticket.as_deref().unwrap_or("(none)")
    ));
    lines.push(format!(
        "revision: {}",
        snapshot.revision.as_deref().unwrap_or("(none)")
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
        snapshot.wait_reason.as_deref().unwrap_or("(not waiting)")
    ));
    lines.push(format!(
        "ttl remaining: {}",
        snapshot
            .ttl_remaining_seconds
            .map(|s| format!("{s}s"))
            .unwrap_or_else(|| "(untracked)".to_string())
    ));
    lines.push(format!(
        "last action: {}",
        match &snapshot.last_action {
            Some(outcome) => format!(
                "{} applied={} ({})",
                action_label(&outcome.action),
                outcome.applied,
                outcome.detail
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
        "#{} [{provenance}] {} {}",
        event.seq, event.kind, event.payload
    )
}

fn phase_label(phase: &run::run::Phase) -> String {
    match phase {
        run::run::Phase::Idle => "idle".to_string(),
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
            project: "gc".into(),
            phase: run::run::Phase::Running,
            paused: false,
            ticket: None,
            revision: None,
            resources: ResourceObservation::Unknown {
                reason: "no cgroup path".into(),
            },
            context_available: false,
            wait_reason: None,
            last_action: None,
            ttl_remaining_seconds: None,
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
            kind: "telemetry/resources".into(),
            payload: serde_json::Value::String("missing".into()),
            provenance: crate::provenance::Provenance::Telemetry { unknown: true },
        };
        let line = render_event(&event);
        assert!(line.contains("telemetry(UNKNOWN)"));
        assert!(line.contains("#7"));
    }
}
