//! Backlog.md adapter: spawns the pinned CLI for snapshots and watch streams.
//! Query errors are always `TrackerError` — never an empty snapshot.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Stdio;

use async_trait::async_trait;
use serde_json::Deserializer;
use tokio::io::AsyncReadExt;
use tokio::process::Command;
use tokio::sync::mpsc;

use crate::core::{TrackerError, TrackerRead, WorkItem};
use crate::snapshot::parse_task_list_envelope;

#[derive(Debug, Clone)]
pub struct BacklogTracker {
    /// Explicit CLI path pins the tested installation (DOD requirement).
    pub bin: PathBuf,
    pub working_dir: PathBuf,
    pub open_aliases: Vec<String>,
}

impl BacklogTracker {
    pub fn new(bin: impl Into<PathBuf>, working_dir: impl Into<PathBuf>) -> Self {
        Self {
            bin: bin.into(),
            working_dir: working_dir.into(),
            open_aliases: Vec::new(),
        }
    }

    async fn run_json(&self, args: &[&str]) -> Result<String, TrackerError> {
        let output = Command::new(&self.bin)
            .args(args)
            .current_dir(&self.working_dir)
            .stdin(Stdio::null())
            .output()
            .await
            .map_err(|e| TrackerError::Query(format!("spawn {}: {e}", self.bin.display())))?;
        if !output.status.success() {
            return Err(TrackerError::Query(format!(
                "{} {:?} exited {}: {}",
                self.bin.display(),
                args,
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Read observed statuses configuration (`backlog config get statuses`).
    /// The installed CLI prints a plain comma-separated list; JSON arrays are
    /// accepted too when a newer build changes format.
    pub async fn observed_statuses(&self) -> Result<Vec<String>, TrackerError> {
        let raw = self.run_json(&["config", "get", "statuses"]).await?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Vec::new());
        }
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
            return Ok(value
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default());
        }
        Ok(trimmed
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect())
    }
}

#[async_trait]
impl TrackerRead for BacklogTracker {
    async fn snapshot(&self) -> Result<Vec<WorkItem>, TrackerError> {
        let raw = self.run_json(&["task", "list", "--json"]).await?;
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            // An empty response body is an empty snapshot (documented CLI
            // behavior), not an error.
            return Ok(Vec::new());
        }
        parse_task_list_envelope(trimmed, &self.open_aliases)
            .map(|parsed| parsed.items)
            .map_err(TrackerError::Query)
    }
}

/// Watch events per AC-2: complete JSON objects in sequence (empty ones
/// included), EOF/error triggers a bounded polling resync.
#[derive(Debug, Clone, PartialEq)]
pub enum WatchEvent {
    Snapshot(Vec<WorkItem>),
    /// The stream ended; polling took over (bounded by `max_polls`).
    ResyncedViaPolling,
    Failed(TrackerError),
}

/// Feed stream bytes incrementally; complete envelopes (including empty
/// objects) come back in order. A truncated final object stays buffered.
pub fn feed_stream(
    buffer: &mut String,
    chunk: &str,
    open_aliases: &[String],
) -> Result<Vec<Result<Vec<WorkItem>, TrackerError>>, TrackerError> {
    buffer.push_str(chunk);
    let mut events = Vec::new();
    let mut de = Deserializer::from_str(buffer.as_str()).into_iter::<serde_json::Value>();
    let mut consumed = 0usize;
    loop {
        match de.next() {
            Some(Ok(value)) => {
                consumed = de.byte_offset();
                let text = value.to_string();
                if value.get("kind").and_then(|k| k.as_str()) == Some("task-list") {
                    events.push(
                        parse_task_list_envelope(&text, open_aliases)
                            .map(|p| p.items)
                            .map_err(TrackerError::Query),
                    );
                } else {
                    // Empty object `{}` or heartbeat: a valid empty snapshot.
                    events.push(Ok(Vec::new()));
                }
            }
            Some(Err(err)) if err.is_eof() => break,
            Some(Err(err)) => return Err(TrackerError::Stream(err.to_string())),
            None => break,
        }
    }
    buffer.drain(..consumed);
    Ok(events)
}

/// Watch a live CLI stream; on EOF/error, resync via bounded polling of
/// `snapshot()` — the resync itself never fabricates readiness.
pub async fn watch(
    tracker: BacklogTracker,
    args: Vec<String>,
    poll_interval: std::time::Duration,
    max_polls: usize,
    events: mpsc::Sender<WatchEvent>,
) {
    let mut child = match Command::new(&tracker.bin)
        .args(&args)
        .current_dir(&tracker.working_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            let _ = events
                .send(WatchEvent::Failed(TrackerError::Stream(e.to_string())))
                .await;
            return;
        }
    };
    let mut buffer = String::new();
    if let Some(stdout) = child.stdout.take() {
        let mut reader = stdout;
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk).await {
                Ok(0) => break,
                Ok(n) => {
                    let text = String::from_utf8_lossy(&chunk[..n]).to_string();
                    match feed_stream(&mut buffer, &text, &tracker.open_aliases) {
                        Ok(items) => {
                            for item in items {
                                match item {
                                    Ok(snapshot) => {
                                        if events
                                            .send(WatchEvent::Snapshot(snapshot))
                                            .await
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }
                                    Err(err) => {
                                        let _ = events.send(WatchEvent::Failed(err)).await;
                                        return;
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            let _ = events.send(WatchEvent::Failed(err)).await;
                            return;
                        }
                    }
                }
                Err(e) => {
                    let _ = events
                        .send(WatchEvent::Failed(TrackerError::Stream(e.to_string())))
                        .await;
                    return;
                }
            }
        }
    }
    let _ = child.kill().await;
    // Bounded polling resync after stream end.
    for _ in 0..max_polls {
        tokio::time::sleep(poll_interval).await;
        match tracker.snapshot().await {
            Ok(items) => {
                if events.send(WatchEvent::Snapshot(items)).await.is_err() {
                    return;
                }
            }
            Err(err) => {
                let _ = events.send(WatchEvent::Failed(err)).await;
                return;
            }
        }
    }
    let _ = events.send(WatchEvent::ResyncedViaPolling).await;
}

/// Ids visible in a snapshot, used to detect unresolved dependencies.
pub fn known_ids(items: &[WorkItem]) -> HashSet<String> {
    items.iter().map(|i| i.id.clone()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::WorkStatus;

    fn tracker_with(bin: &str) -> BacklogTracker {
        BacklogTracker::new(bin, std::env::temp_dir())
    }

    #[tokio::test]
    async fn snapshot_query_error_is_tracker_error_not_empty() {
        // A command that cannot produce JSON must yield TrackerError::Query.
        let tracker = tracker_with("false");
        let result = tracker.snapshot().await;
        assert!(matches!(result, Err(TrackerError::Query(_))));
    }

    #[tokio::test]
    async fn snapshot_parses_pinned_cli_fixture_script() {
        // Fixture script: a stand-in CLI emitting the envelope contract.
        let dir = std::env::temp_dir().join(format!("hodei-tracker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("fake-backlog.sh");
        std::fs::write(
            &script,
            "#!/bin/sh\nif [ \"$1\" = \"task\" ]; then echo '{\"kind\":\"task-list\",\"schemaVersion\":1,\"tasks\":[{\"id\":\"T1\",\"title\":\"one\",\"status\":\"To Do\",\"isReady\":true,\"dependencies\":[]}]}'\nfi\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let tracker = BacklogTracker::new(&script, &dir);
        let items = tracker.snapshot().await.unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].status, WorkStatus::Open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stream_processes_successive_objects_including_empty() {
        let mut buffer = String::new();
        let events = feed_stream(
            &mut buffer,
            r#"{"kind":"task-list","tasks":[]} {"kind":"task-list","tasks":[{"id":"a","title":"t","status":"To Do","isReady":true}]}"#,
            &[],
        )
        .unwrap();
        assert_eq!(events.len(), 2);
        assert!(events[0].as_ref().unwrap().is_empty());
        assert_eq!(events[1].as_ref().unwrap().len(), 1);
        assert!(buffer.is_empty());
        // Truncated tail stays buffered, no error.
        let events = feed_stream(&mut buffer, r#"{"kind":"task-li"#, &[]).unwrap();
        assert!(events.is_empty());
    }

    #[test]
    fn stream_malformed_content_is_stream_error() {
        let mut buffer = String::new();
        let result = feed_stream(&mut buffer, "definitely {{{ not json", &[]);
        assert!(matches!(result, Err(TrackerError::Stream(_))));
    }
}
