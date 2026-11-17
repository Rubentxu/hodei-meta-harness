//! execd WebSocket pipe framing, ported from the reference daemon contract
//! (`pty_ws.go` + `model/pty_ws.go`). Pure encoding/decoding: no I/O here.
//!
//! Wire layout:
//! - Client → Server binary frame: `0x00 || stdin bytes`.
//! - Server → Client binary frames: `0x01 || stdout`, `0x02 || stderr`
//!   (pipe mode only), `0x03 || 8-byte big-endian offset || replay bytes`.
//! - Control frames are JSON objects on text messages:
//!   client `{type: stdin|signal|resize|ping}`, server
//!   `{type: connected|error|pong|exit}`.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Binary frame prefixes, verbatim from the reference daemon.
pub const BIN_STDIN: u8 = 0x00;
pub const BIN_STDOUT: u8 = 0x01;
pub const BIN_STDERR: u8 = 0x02;
pub const BIN_REPLAY: u8 = 0x03;

/// WebSocket subprotocol announced by execd PTY/pipe handshakes.
pub const PTY_SUBPROTOCOL: &str = "opensandbox-pty";

#[derive(Debug, Error)]
pub enum FrameError {
    #[error("empty WebSocket message")]
    Empty,
    #[error("unknown binary frame prefix 0x{0:02x}")]
    UnknownPrefix(u8),
    #[error("replay frame shorter than its 8-byte offset header")]
    TruncatedReplay,
    #[error("invalid control frame JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("control frame has unknown type: {0:?}")]
    UnknownType(String),
}

/// One event delivered by execd over the pipe WebSocket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    /// Handshake acknowledgement: `{type:"connected", session_id, mode, role}`.
    Connected {
        session_id: String,
        mode: Option<String>,
        role: Option<String>,
    },
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
    /// Missed output replayed on attach: offset is the byte position the
    /// replay resumes from.
    Replay {
        offset: i64,
        data: Vec<u8>,
    },
    /// Process exit; `exit_code` may be absent on older execd builds.
    Exit {
        exit_code: Option<i32>,
    },
    /// Server-side failure; `code` is a stable execd error identifier
    /// (e.g. `ALREADY_CONNECTED`, `STDIN_WRITE_FAILED`).
    Error {
        code: Option<String>,
        message: Option<String>,
    },
    Pong,
}

/// Client control frames (JSON, UTF-8 text messages).
#[derive(Debug, Serialize)]
struct ClientControl<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    signal: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cols: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rows: Option<u16>,
}

/// Encode a stdin payload as a binary frame (`0x00 || bytes`).
pub fn encode_stdin(data: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(data.len() + 1);
    frame.push(BIN_STDIN);
    frame.extend_from_slice(data);
    frame
}

/// Encode a signal request as a JSON control frame. Recognised names in the
/// reference daemon: SIGINT, SIGTERM, SIGKILL, SIGQUIT, SIGHUP (sent to the
/// whole process group).
pub fn encode_signal(signal: &str) -> Result<String, FrameError> {
    validate_signal(signal)?;
    Ok(serde_json::to_string(&ClientControl {
        kind: "signal",
        signal: Some(signal),
        cols: None,
        rows: None,
    })?)
}

/// Encode a terminal resize request (ignored in pipe mode, kept for parity).
pub fn encode_resize(cols: u16, rows: u16) -> String {
    serde_json::to_string(&ClientControl {
        kind: "resize",
        signal: None,
        cols: Some(cols),
        rows: Some(rows),
    })
    .expect("resize control frame serializes infallibly")
}

/// Encode an application-level keepalive ping.
pub fn encode_ping() -> String {
    serde_json::to_string(&ClientControl {
        kind: "ping",
        signal: None,
        cols: None,
        rows: None,
    })
    .expect("ping control frame serializes infallibly")
}

fn validate_signal(signal: &str) -> Result<(), FrameError> {
    const SUPPORTED: [&str; 5] = ["SIGINT", "SIGTERM", "SIGKILL", "SIGQUIT", "SIGHUP"];
    if SUPPORTED.contains(&signal) {
        Ok(())
    } else {
        Err(FrameError::UnknownType(signal.to_string()))
    }
}

/// Decode a server binary frame (prefix `0x01`..`0x03`).
pub fn decode_server_binary(payload: &[u8]) -> Result<ServerEvent, FrameError> {
    let (&prefix, body) = payload.split_first().ok_or(FrameError::Empty)?;
    match prefix {
        BIN_STDOUT => Ok(ServerEvent::Stdout(body.to_vec())),
        BIN_STDERR => Ok(ServerEvent::Stderr(body.to_vec())),
        BIN_REPLAY => {
            if body.len() < 8 {
                return Err(FrameError::TruncatedReplay);
            }
            let offset = i64::from_be_bytes(body[..8].try_into().expect("8-byte slice"));
            Ok(ServerEvent::Replay {
                offset,
                data: body[8..].to_vec(),
            })
        }
        other => Err(FrameError::UnknownPrefix(other)),
    }
}

#[derive(Debug, Deserialize)]
struct ServerFrame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    mode: Option<String>,
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    exit_code: Option<i32>,
    #[serde(default)]
    error: Option<String>,
    #[serde(default)]
    code: Option<String>,
}

/// Decode a server JSON control frame.
pub fn decode_server_json(text: &str) -> Result<ServerEvent, FrameError> {
    let frame: ServerFrame = serde_json::from_str(text)?;
    match frame.kind.as_str() {
        "connected" => Ok(ServerEvent::Connected {
            session_id: frame.session_id.unwrap_or_default(),
            mode: frame.mode,
            role: frame.role,
        }),
        "exit" => Ok(ServerEvent::Exit {
            exit_code: frame.exit_code,
        }),
        "error" => Ok(ServerEvent::Error {
            code: frame.code,
            message: frame.error,
        }),
        "pong" => Ok(ServerEvent::Pong),
        other => Err(FrameError::UnknownType(other.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdin_frame_prefixes_payload_and_survives_empty() {
        assert_eq!(encode_stdin(b"hi"), vec![0x00, b'h', b'i']);
        assert_eq!(encode_stdin(b""), vec![0x00]);
    }

    #[test]
    fn stdout_stderr_decode_preserves_bytes_exactly() {
        assert_eq!(
            decode_server_binary(&[0x01, 0xd0, 0xbc]).unwrap(),
            ServerEvent::Stdout(vec![0xd0, 0xbc])
        );
        assert_eq!(
            decode_server_binary(&[0x02]).unwrap(),
            ServerEvent::Stderr(vec![])
        );
    }

    #[test]
    fn fragmented_output_reassembles_by_concatenation() {
        // Reference contract: concatenate event text per stream to rebuild output.
        let a = decode_server_binary(&[0x01, b'h', b'e']).unwrap();
        let b = decode_server_binary(&[0x01, b'y']).unwrap();
        let (ServerEvent::Stdout(a), ServerEvent::Stdout(b)) = (a, b) else {
            panic!("expected stdout events");
        };
        let mut full = a;
        full.extend(b);
        assert_eq!(full, b"hey");
    }

    #[test]
    fn replay_frame_carries_big_endian_offset() {
        let mut frame = vec![0x03];
        frame.extend_from_slice(&1024i64.to_be_bytes());
        frame.extend_from_slice(b"missed output");
        match decode_server_binary(&frame).unwrap() {
            ServerEvent::Replay { offset, data } => {
                assert_eq!(offset, 1024);
                assert_eq!(data, b"missed output");
            }
            other => panic!("expected replay, got {other:?}"),
        }
    }

    #[test]
    fn truncated_replay_is_a_specific_error() {
        let frame = [0x03, 0x00, 0x01];
        assert!(matches!(
            decode_server_binary(&frame),
            Err(FrameError::TruncatedReplay)
        ));
    }

    #[test]
    fn unknown_and_empty_prefixes_are_rejected() {
        assert!(matches!(
            decode_server_binary(&[0x7f, b'x']),
            Err(FrameError::UnknownPrefix(0x7f))
        ));
        assert!(matches!(decode_server_binary(&[]), Err(FrameError::Empty)));
    }

    #[test]
    fn connected_frame_decodes_all_fields() {
        let text = r#"{"type":"connected","session_id":"s1","mode":"pipe","role":"holder"}"#;
        match decode_server_json(text).unwrap() {
            ServerEvent::Connected {
                session_id,
                mode,
                role,
            } => {
                assert_eq!(session_id, "s1");
                assert_eq!(mode.as_deref(), Some("pipe"));
                assert_eq!(role.as_deref(), Some("holder"));
            }
            other => panic!("expected connected, got {other:?}"),
        }
    }

    #[test]
    fn error_frame_codes_survive_and_exit_may_omit_code() {
        let err =
            decode_server_json(r#"{"type":"error","code":"ALREADY_CONNECTED","error":"busy"}"#)
                .unwrap();
        assert_eq!(
            err,
            ServerEvent::Error {
                code: Some("ALREADY_CONNECTED".into()),
                message: Some("busy".into())
            }
        );
        let exit = decode_server_json(r#"{"type":"exit"}"#).unwrap();
        assert_eq!(exit, ServerEvent::Exit { exit_code: None });
        let exit_ok = decode_server_json(r#"{"type":"exit","exit_code":0}"#).unwrap();
        assert_eq!(exit_ok, ServerEvent::Exit { exit_code: Some(0) });
    }

    #[test]
    fn unknown_json_type_is_rejected() {
        let err = decode_server_json(r#"{"type":"mystery"}"#).unwrap_err();
        assert!(matches!(err, FrameError::UnknownType(t) if t == "mystery"));
    }

    #[test]
    fn signal_encoding_validates_against_daemon_support() {
        let frame = encode_signal("SIGKILL").unwrap();
        assert!(frame.contains(r#""type":"signal""#));
        assert!(frame.contains(r#""signal":"SIGKILL""#));
        assert!(matches!(
            encode_signal("SIGUSR1"),
            Err(FrameError::UnknownType(_))
        ));
    }

    #[test]
    fn ping_and_resize_frames_shape() {
        assert_eq!(encode_ping(), r#"{"type":"ping"}"#);
        assert!(encode_resize(80, 24).contains(r#""cols":80"#));
    }
}
