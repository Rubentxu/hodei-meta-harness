//! Wire protocol (pure): NDJSON request/response over the supervisor
//! socket. BOTH clients — the TUI and the CLI — speak exactly this
//! contract; neither can boot a second controller because starting the
//! supervisor is NOT part of the protocol.

use serde::{Deserialize, Serialize};

use run::run::Phase;
use run::runtime::{ObjectiveReport, RuntimeIdentity};

/// What a client may ask the supervisor to do. These are the SAME actions
/// the CLI documents; the TUI only renders keys for them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum SupervisorAction {
    Start {
        project: String,
        ticket: Option<String>,
        revision: Option<String>,
    },
    Pause,
    Resume,
    Stop,
    /// Extend the environment TTL of the managed sandbox.
    Renew {
        added_seconds: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "req", rename_all = "snake_case")]
pub enum Request {
    Snapshot,
    /// Events with sequence number >= `since` (poll model: reconnecting
    /// clients replay from their last seen sequence).
    Events {
        since: u64,
    },
    Action {
        action: SupervisorAction,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionOutcome {
    pub action: SupervisorAction,
    /// TRUE only when the engine actually applied the transition. A refused
    /// action is a real answer, never a silent success.
    pub applied: bool,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "resp", rename_all = "snake_case")]
pub enum Response {
    Snapshot(Box<Snapshot>),
    Events {
        events: Vec<StructuredEvent>,
        next_seq: u64,
    },
    Action(ActionOutcome),
    Error {
        message: String,
    },
}

/// Structured ACP-derived event with provenance and a monotonic sequence.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StructuredEvent {
    pub seq: u64,
    pub kind: String,
    pub payload: serde_json::Value,
    pub provenance: crate::provenance::Provenance,
}

pub use crate::provenance::Provenance;

/// One observation of resource telemetry. Unknown telemetry is a FIRST
/// CLASS value: it is shown as unknown, never dressed up as a measurement.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "obs", rename_all = "snake_case")]
pub enum ResourceObservation {
    Measured { cpus: f64, memory_gib: f64 },
    Unknown { reason: String },
}

/// What the supervisor knows right now: project/phase/ticket/revision/
/// resources/context, the reason it is waiting (if any) and the last real
/// action confirmation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub project: String,
    pub phase: Phase,
    /// Whether the supervisor loop is paused (Start/Stop are run-level;
    /// Pause/Resume gate the prompt loop without touching the session).
    pub paused: bool,
    pub ticket: Option<String>,
    pub revision: Option<String>,
    /// The runtime that executes objectives. Its KIND is never hidden: a
    /// rehearsal says so. Absent means no runtime is wired.
    #[serde(default)]
    pub runtime: Option<RuntimeIdentity>,
    /// What the last objective did. Absent means none has run yet.
    #[serde(default)]
    pub objective: Option<ObjectiveReport>,
    pub resources: ResourceObservation,
    pub context_available: bool,
    pub wait_reason: Option<String>,
    pub last_action: Option<ActionOutcome>,
    /// Environment TTL remaining (seconds), if one is being tracked.
    pub ttl_remaining_seconds: Option<u64>,
    /// Epoch of the current run. It advances only when a run starts — it
    /// is not the event count.
    pub epoch: u64,
    /// The effect scope the current grant authorizes, stated explicitly.
    /// Empty means the supervisor authorizes NO effect by itself.
    pub grant_effects: Vec<String>,
    /// Set when the core recovered from a poisoned lock. Visible, never
    /// silently swallowed.
    pub degraded: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn actions_roundtrip_through_json() {
        let raw = r#"{"req":"action","action":{"action":"start","project":"gc","ticket":"TASK-1","revision":"abc"}}"#;
        let request: Request = serde_json::from_str(raw).expect("parse");
        match request {
            Request::Action {
                action:
                    SupervisorAction::Start {
                        ref project,
                        ref ticket,
                        ref revision,
                    },
            } => {
                assert_eq!(project, "gc");
                assert_eq!(ticket.as_deref(), Some("TASK-1"));
                assert_eq!(revision.as_deref(), Some("abc"));
            }
            other => panic!("wrong request: {other:?}"),
        }
        let encoded = serde_json::to_string(&request).expect("encode");
        let back: Request = serde_json::from_str(&encoded).expect("re-parse");
        assert_eq!(request, back);
    }

    #[test]
    fn unknown_telemetry_survives_the_wire_as_unknown() {
        let snapshot = Snapshot {
            project: "gc".into(),
            phase: Phase::Running,
            paused: false,
            ticket: None,
            revision: None,
            runtime: None,
            objective: None,
            resources: ResourceObservation::Unknown {
                reason: "no cgroup path".into(),
            },
            context_available: true,
            wait_reason: None,
            last_action: None,
            ttl_remaining_seconds: None,
            epoch: 0,
            grant_effects: Vec::new(),
            degraded: None,
        };
        let encoded = serde_json::to_string(&snapshot).expect("encode");
        let back: Snapshot = serde_json::from_str(&encoded).expect("re-parse");
        assert_eq!(
            back.resources,
            ResourceObservation::Unknown {
                reason: "no cgroup path".into()
            }
        );
    }
}
