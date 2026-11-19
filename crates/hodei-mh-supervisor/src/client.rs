//! Socket client adapter shared by the CLI and the TUI. One client type,
//! one contract. Reconnection is explicit: a missing socket is reported as
//! "supervisor not reachable" and retried — the client NEVER spawns a
//! second supervisor itself (AC-1).

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::protocol::{
    ActionOutcome, EventPage, PROTOCOL_VERSION, Request, Response, Snapshot, SupervisorAction,
};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error(
        "supervisor not reachable at {path}: {source} (no socket: no second controller is started)"
    )]
    NotReachable {
        path: String,
        source: std::io::Error,
    },
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub struct SupervisorClient {
    reader: BufReader<tokio::net::unix::OwnedReadHalf>,
    writer: tokio::net::unix::OwnedWriteHalf,
    pub path: String,
}

pub async fn connect(path: &str) -> Result<SupervisorClient, ClientError> {
    let stream = UnixStream::connect(path)
        .await
        .map_err(|source| ClientError::NotReachable {
            path: path.to_string(),
            source,
        })?;
    let (reader, writer) = stream.into_split();
    Ok(SupervisorClient {
        reader: BufReader::new(reader),
        writer,
        path: path.to_string(),
    })
}

impl SupervisorClient {
    async fn roundtrip(&mut self, request: Request) -> Result<Response, ClientError> {
        let mut payload =
            serde_json::to_string(&request).map_err(|e| ClientError::Protocol(e.to_string()))?;
        payload.push('\n');
        self.writer.write_all(payload.as_bytes()).await?;
        let mut line = String::new();
        let read = self.reader.read_line(&mut line).await?;
        if read == 0 {
            return Err(ClientError::Protocol(
                "supervisor closed the connection".into(),
            ));
        }
        let response = serde_json::from_str::<Response>(line.trim())
            .map_err(|e| ClientError::Protocol(format!("bad response: {e}")))?;
        // The contract is VERSIONED: an answer of another version is not
        // read as if it were this one. A client that guessed would read a
        // hole in the history as if it were the whole history.
        if response.protocol_version() != PROTOCOL_VERSION {
            return Err(ClientError::Protocol(format!(
                "the supervisor speaks the protocol version {}; this client speaks {PROTOCOL_VERSION}",
                response.protocol_version()
            )));
        }
        Ok(response)
    }

    pub async fn snapshot(&mut self) -> Result<Snapshot, ClientError> {
        match self.roundtrip(Request::Snapshot).await? {
            Response::Snapshot(snapshot) => Ok(*snapshot),
            Response::Error { message, .. } => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected snapshot response".into())),
        }
    }

    pub async fn action(&mut self, action: SupervisorAction) -> Result<ActionOutcome, ClientError> {
        match self.roundtrip(Request::Action { action }).await? {
            Response::Action { outcome, .. } => Ok(outcome),
            Response::Error { message, .. } => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected action response".into())),
        }
    }

    /// One page of the event history, from `since`.
    ///
    /// The page carries the cursor to continue with and says whether the
    /// history the client asked for had a hole.
    pub async fn events(
        &mut self,
        since: u64,
        limit: Option<u32>,
    ) -> Result<EventPage, ClientError> {
        match self.roundtrip(Request::Events { since, limit }).await? {
            Response::Events {
                events,
                next_seq,
                oldest_seq,
                history_lost,
                ..
            } => Ok(EventPage {
                events,
                next_seq,
                oldest_seq,
                history_lost,
            }),
            Response::Error { message, .. } => Err(ClientError::Protocol(message)),
            _ => Err(ClientError::Protocol("expected events response".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_reachable_names_the_path_and_never_offers_to_spawn_a_controller() {
        let error = ClientError::NotReachable {
            path: "/tmp/nope.sock".into(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "missing"),
        };
        let message = error.to_string();
        assert!(message.contains("/tmp/nope.sock"));
        assert!(message.contains("no second controller"));
    }
}
