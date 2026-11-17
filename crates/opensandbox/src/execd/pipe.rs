//! execd pipe WebSocket transport: attach to a `/pty/{id}/ws?pty=0` session,
//! multiplex binary stdout/stderr/replay frames and JSON control frames into
//! a typed event stream, and drive stdin/signals from the client side.
//!
//! This is an adapter: the only mutation is the WebSocket connection state
//! owned by the spawned reader task.

use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use thiserror::Error;
use tokio::sync::mpsc;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;

use super::frames::{self, FrameError, PTY_SUBPROTOCOL, ServerEvent};

/// Attachment options for a pipe session.
#[derive(Debug, Clone, Default)]
pub struct AttachOptions {
    /// execd access token; when present it is appended as the
    /// `base64url.bearer.execd.opensandbox.<token>` credential subprotocol
    /// (browser-compatible auth path used by the reference daemon).
    pub access_token: Option<String>,
    /// Forwarded as `X-EXECD-ACCESS-TOKEN` header when present.
    pub header_token: Option<String>,
    /// Handshake timeout; defaults to 10s.
    pub connect_timeout: Option<Duration>,
}

#[derive(Debug, Error)]
pub enum PipeError {
    #[error("websocket handshake failed: {0}")]
    Handshake(#[from] tokio_tungstenite::tungstenite::Error),
    #[error("handshake timed out after {0:?}")]
    Timeout(Duration),
    #[error("subprotocol negotiation failed: server accepted {0:?}, expected {1:?}")]
    SubprotocolMismatch(Option<String>, String),
    #[error("pipe framing error: {0}")]
    Frame(#[from] FrameError),
    #[error("invalid header value")]
    InvalidHeader,
    #[error("stdin write failed")]
    WriteClosed,
}

/// Handle to an attached pipe session: bounded event stream plus writers.
pub struct PipeSession {
    events: mpsc::Receiver<ServerEvent>,
    writer: mpsc::Sender<Message>,
}

impl PipeSession {
    /// Next server event, or `None` once the socket closes cleanly.
    pub async fn next_event(&mut self) -> Option<ServerEvent> {
        self.events.recv().await
    }

    pub async fn send_stdin(&self, data: &[u8]) -> Result<(), PipeError> {
        let frame = frames::encode_stdin(data);
        self.writer
            .send(Message::Binary(frame.into()))
            .await
            .map_err(|_| PipeError::WriteClosed)
    }

    pub async fn send_signal(&self, signal: &str) -> Result<(), PipeError> {
        let text = frames::encode_signal(signal)?;
        self.writer
            .send(Message::text(text))
            .await
            .map_err(|_| PipeError::WriteClosed)
    }

    pub async fn send_ping(&self) -> Result<(), PipeError> {
        let text = frames::encode_ping();
        self.writer
            .send(Message::text(text))
            .await
            .map_err(|_| PipeError::WriteClosed)
    }

    /// Close the WebSocket (execd detaches the holder and keeps the process).
    pub async fn close(&self) -> Result<(), PipeError> {
        self.writer
            .send(Message::Close(None))
            .await
            .map_err(|_| PipeError::WriteClosed)
    }
}

/// Attach to an execd pipe WebSocket (`ws://host:port/pty/{id}/ws?pty=0`).
pub async fn attach(ws_url: &str, opts: AttachOptions) -> Result<PipeSession, PipeError> {
    let mut request = ws_url.into_client_request()?;
    // Header-authenticated clients never offer the subprotocol (tungstenite
    // rejects a missing echo); the browser-compatible credential subprotocol
    // is offered only in the token path, exactly as execd documents.
    if let Some(token) = &opts.access_token {
        let protocol = format!("{PTY_SUBPROTOCOL} {}", credential_subprotocol(token));
        let header = HeaderValue::from_str(&protocol).map_err(|_| PipeError::InvalidHeader)?;
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", header);
    }
    if let Some(token) = &opts.header_token {
        let header = HeaderValue::from_str(token).map_err(|_| PipeError::InvalidHeader)?;
        request.headers_mut().insert("X-EXECD-ACCESS-TOKEN", header);
    }

    let timeout = opts.connect_timeout.unwrap_or(Duration::from_secs(10));
    let (ws, _response) = tokio::time::timeout(timeout, connect_async(request))
        .await
        .map_err(|_| PipeError::Timeout(timeout))?
        .map_err(PipeError::Handshake)?;

    // The reference daemon does not always echo the negotiated subprotocol;
    // when it echoes anything else the handshake is not what we asked for.
    if let Some(negotiated) = _response
        .headers()
        .get("Sec-WebSocket-Protocol")
        .and_then(|v| v.to_str().ok())
        .filter(|negotiated| negotiated.split_whitespace().all(|p| p != PTY_SUBPROTOCOL))
    {
        return Err(PipeError::SubprotocolMismatch(
            Some(negotiated.to_string()),
            PTY_SUBPROTOCOL.to_string(),
        ));
    }

    let (mut sink, mut stream) = ws.split();
    let (event_tx, event_rx) = mpsc::channel(64);
    let (write_tx, mut write_rx) = mpsc::channel::<Message>(32);

    // Reader: translate raw WS messages into typed events.
    tokio::spawn(async move {
        while let Some(item) = stream.next().await {
            let forward = match item {
                Ok(Message::Binary(bytes)) => frames::decode_server_binary(&bytes).map(Some),
                Ok(Message::Text(text)) => frames::decode_server_json(&text).map(Some),
                Ok(Message::Ping(_) | Message::Pong(_) | Message::Frame(_)) => Ok(None),
                Ok(Message::Close(_)) => break,
                Err(err) => {
                    let _ = event_tx
                        .send(ServerEvent::Error {
                            code: Some("WS_TRANSPORT".into()),
                            message: Some(err.to_string()),
                        })
                        .await;
                    break;
                }
            };
            match forward {
                Ok(Some(event)) => {
                    if event_tx.send(event).await.is_err() {
                        break;
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    let _ = event_tx
                        .send(ServerEvent::Error {
                            code: Some("WS_FRAME".into()),
                            message: Some(err.to_string()),
                        })
                        .await;
                    break;
                }
            }
        }
    });

    // Writer: serialize outgoing messages from the handle.
    tokio::spawn(async move {
        while let Some(message) = write_rx.recv().await {
            let is_close = matches!(message, Message::Close(_));
            match sink.send(message).await {
                Ok(()) if is_close => break,
                Ok(()) => {}
                Err(_) => break,
            }
        }
    });

    Ok(PipeSession {
        events: event_rx,
        writer: write_tx,
    })
}

/// Build the credential subprotocol execd accepts for browser-compatible auth:
/// `base64url.bearer.execd.opensandbox.<unpadded base64url(token bytes)>`.
pub fn credential_subprotocol(token: &str) -> String {
    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(token.as_bytes());
    format!("base64url.bearer.execd.opensandbox.{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::tungstenite::protocol::Message as WsMessage;

    /// Spawn a mock execd pipe server. When `server_closes` is true it plays
    /// the script (which must end in `Close`) and shuts down; otherwise it
    /// records every client frame until the client sends `Close`.
    async fn spawn_mock(
        script: Vec<Message>,
        server_closes: bool,
    ) -> (String, tokio::sync::oneshot::Receiver<Vec<Message>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (tx, rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            #[allow(clippy::result_large_err)] // test scaffolding; library type dictates the signature
            fn echo_subprotocol(
                req: &tokio_tungstenite::tungstenite::http::Request<()>,
                mut resp: tokio_tungstenite::tungstenite::http::Response<()>,
            ) -> Result<
                tokio_tungstenite::tungstenite::http::Response<()>,
                tokio_tungstenite::tungstenite::http::Response<Option<String>>,
            > {
                if let Some(protocol) = req.headers().get("Sec-WebSocket-Protocol") {
                    resp.headers_mut()
                        .insert("Sec-WebSocket-Protocol", protocol.clone());
                }
                Ok(resp)
            }
            let callback = echo_subprotocol;
            let mut ws = accept_hdr_async(socket, callback).await.unwrap();
            for frame in script {
                ws.send(frame).await.unwrap();
            }
            if server_closes {
                let _ = tx.send(Vec::new());
                return;
            }
            let mut received = Vec::new();
            while let Some(item) = ws.next().await {
                match item {
                    Ok(WsMessage::Close(_)) => break,
                    Ok(other) => received.push(other),
                    Err(_) => break,
                }
            }
            let _ = tx.send(received);
        });
        (format!("ws://{addr}/pty/test/ws?pty=0"), rx)
    }

    fn connected_then_output_script() -> Vec<Message> {
        vec![
            Message::text(
                r#"{"type":"connected","session_id":"test","mode":"pipe","role":"holder"}"#,
            ),
            Message::Binary(vec![0x01, b'o', b'1'].into()),
            Message::Binary(vec![0x02, b'e', b'1'].into()),
            Message::text(r#"{"type":"exit","exit_code":0}"#),
            // Server-initiated close terminates the client event loop.
            Message::Close(None),
        ]
    }

    #[tokio::test]
    async fn attach_receives_connected_output_streams_and_exit_in_order() {
        let (url, rx) = spawn_mock(connected_then_output_script(), true).await;
        let mut session = attach(&url, AttachOptions::default()).await.unwrap();

        let mut events = Vec::new();
        while let Some(event) = session.next_event().await {
            events.push(event);
        }
        assert_eq!(
            events[0],
            ServerEvent::Connected {
                session_id: "test".into(),
                mode: Some("pipe".into()),
                role: Some("holder".into())
            }
        );
        assert_eq!(events[1], ServerEvent::Stdout(b"o1".to_vec()));
        assert_eq!(events[2], ServerEvent::Stderr(b"e1".to_vec()));
        assert_eq!(events[3], ServerEvent::Exit { exit_code: Some(0) });
        drop(rx);
    }

    #[tokio::test]
    async fn stdin_and_signal_frames_reach_the_server() {
        let (url, rx) = spawn_mock(
            connected_then_output_script().into_iter().take(1).collect(),
            false,
        )
        .await;
        let session = attach(&url, AttachOptions::default()).await.unwrap();
        session.send_stdin(b"agent-prompt\n").await.unwrap();
        session.send_signal("SIGKILL").await.unwrap();
        session.close().await.unwrap();

        let received = rx.await.unwrap();
        assert_eq!(
            received[0],
            Message::Binary(frames::encode_stdin(b"agent-prompt\n").into())
        );
        let text = received[1].to_text().unwrap();
        assert!(text.contains(r#""type":"signal""#) && text.contains("SIGKILL"));
    }

    #[tokio::test]
    async fn replay_frames_arrive_as_typed_events() {
        let mut frame = vec![0x03];
        frame.extend_from_slice(&42i64.to_be_bytes());
        frame.extend_from_slice(b"snapshot");
        let script = vec![
            Message::text(r#"{"type":"connected","session_id":"r","mode":"pipe","role":"holder"}"#),
            Message::Binary(frame.into()),
        ];
        let (url, _rx) = spawn_mock(script, false).await;
        let mut session = attach(&url, AttachOptions::default()).await.unwrap();
        let mut saw_replay = None;
        while let Some(event) = session.next_event().await {
            if let ServerEvent::Replay { offset, data } = event {
                saw_replay = Some((offset, data));
                break;
            }
        }
        assert_eq!(saw_replay, Some((42, b"snapshot".to_vec())));
    }

    #[tokio::test]
    async fn credential_subprotocol_is_unpadded_base64url() {
        let sub = credential_subprotocol("tok");
        assert!(sub.starts_with("base64url.bearer.execd.opensandbox."));
        assert!(!sub.contains('='));
        assert!(!sub.contains('+') && !sub.contains('/'));
    }
}
