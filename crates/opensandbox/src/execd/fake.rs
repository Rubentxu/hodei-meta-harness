//! A fake execd: the HTTP surface and the pipe WebSocket of the real
//! daemon, with counters. It exists to measure what the transport asks
//! for, offline and without a daemon.
//!
//! It is a test double, not a daemon: it answers what the transport
//! needs to observe (a session id, an output offset, an exit frame),
//! counts every request it received and records the bodies (uploads) and
//! the client pipe frames (unmasked stdin payloads) so a test can assert
//! on the exact bytes a transport put on the wire.

use std::io;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;

/// How the fake answers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FakeBehavior {
    /// Every request is answered as the real daemon would.
    Cooperative,
    /// A create is received and never answered: the caller waits.
    SilentOnCreate,
    /// A create is answered with a server error.
    RefusesCreate,
    /// The pipe of a session closes without an exit frame: the session
    /// is lost.
    LosesTheSession,
    /// Only the FIRST pipe loses its session: the daemon recovers, and
    /// the readiness has to be observed again.
    LosesTheFirstSession,
}

/// What the fake was asked to do.
#[derive(Debug, Default, Clone)]
pub struct FakeCounts {
    /// Every HTTP request the fake answered.
    pub http_requests: usize,
    /// The TCP connections the fake accepted.
    pub connections: usize,
    /// The sessions that were created.
    pub creates: usize,
    /// The sessions that were deleted.
    pub deletes: usize,
    /// The session states that were asked for.
    pub statuses: usize,
    /// The pipes that were attached.
    pub pipes: usize,
    /// The commands the fake was asked to run, in order.
    pub commands: Vec<String>,
    /// The raw bodies of the `/files/upload` requests, in order.
    pub uploads: Vec<Vec<u8>>,
    /// The client pipe WebSocket frames, in order: the opcode and the
    /// unmasked payload of every frame the pipe received.
    pub pipe_frames: Vec<(u8, Vec<u8>)>,
}

impl FakeCounts {
    /// How many times the fake was asked to run this command.
    pub fn times(&self, command: &str) -> usize {
        self.commands
            .iter()
            .filter(|seen| seen.as_str() == command)
            .count()
    }
}

/// A fake execd listening on a local port.
pub struct FakeExecd {
    /// The address the transport talks to.
    pub base_url: String,
    counts: Arc<Mutex<FakeCounts>>,
    /// One token per pipe connection the fake saw close.
    closures: tokio::sync::Mutex<mpsc::UnboundedReceiver<()>>,
}

impl FakeExecd {
    /// Start the fake and wait until it accepts connections.
    pub async fn start(behavior: FakeBehavior) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("the fake binds");
        let addr = listener.local_addr().expect("the fake has an address");
        let counts = Arc::new(Mutex::new(FakeCounts::default()));
        let served = counts.clone();
        let (closures_tx, closures_rx) = mpsc::unbounded_channel();
        tokio::spawn(async move {
            serve(listener, served, behavior, closures_tx).await;
        });
        Self {
            base_url: format!("http://{addr}"),
            counts,
            closures: tokio::sync::Mutex::new(closures_rx),
        }
    }

    /// What the fake was asked to do.
    pub fn counts(&self) -> FakeCounts {
        self.counts.lock().expect("the counts are readable").clone()
    }

    /// Wait until `count` pipe connections were closed by the client.
    /// The wait is on the close event itself, never on a timeout: the
    /// frames recorded by the fake are complete once this returns.
    pub async fn await_pipes_closed(&self, count: usize) {
        let mut closures = self.closures.lock().await;
        for _ in 0..count {
            closures
                .recv()
                .await
                .expect("the fake records every pipe closure");
        }
    }
}

async fn serve(
    listener: TcpListener,
    counts: Arc<Mutex<FakeCounts>>,
    behavior: FakeBehavior,
    closures: mpsc::UnboundedSender<()>,
) {
    loop {
        let Ok((socket, _)) = listener.accept().await else {
            return;
        };
        count_connection(&counts);
        let counts = counts.clone();
        let closures = closures.clone();
        tokio::spawn(async move {
            let _ = serve_connection(socket, counts, behavior, closures).await;
        });
    }
}

/// What the fake answers to a request.
enum Route {
    Create,
    Upload,
    Status(String),
    Delete(String),
    Pipe(String),
    Unknown,
}

/// The head of an HTTP request.
struct RequestHead {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
}

impl RequestHead {
    fn parse(head: &str) -> Self {
        let mut lines = head.lines();
        let mut request_line = lines.next().unwrap_or_default().split_whitespace();
        let method = request_line.next().unwrap_or_default().to_string();
        let path = request_line.next().unwrap_or_default().to_string();
        let headers = lines
            .filter_map(|line| line.split_once(':'))
            .map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_string()))
            .collect();
        Self {
            method,
            path,
            headers,
        }
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn content_length(&self) -> usize {
        self.header("content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0)
    }

    fn route(&self) -> Route {
        let path = self.path.split('?').next().unwrap_or_default();
        let session = path.strip_prefix("/pty/").unwrap_or_default();
        match (self.method.as_str(), path) {
            ("POST", "/pty") => Route::Create,
            ("POST", "/files/upload") => Route::Upload,
            ("GET", _) if session.ends_with("/ws") => {
                Route::Pipe(session.trim_end_matches("/ws").to_string())
            }
            ("GET", _) if !session.is_empty() => Route::Status(session.to_string()),
            ("DELETE", _) if !session.is_empty() => Route::Delete(session.to_string()),
            _ => Route::Unknown,
        }
    }
}

/// The bytes the fake writes as the output of every session.
const SESSION_OUTPUT: &[u8] = b"ok\n";

async fn serve_connection(
    mut socket: TcpStream,
    counts: Arc<Mutex<FakeCounts>>,
    behavior: FakeBehavior,
    closures: mpsc::UnboundedSender<()>,
) -> io::Result<()> {
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let head_end = loop {
            if let Some(position) = find_head_end(&buffer) {
                break position;
            }
            let mut chunk = [0u8; 4096];
            let read = socket.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            buffer.extend_from_slice(&chunk[..read]);
        };
        let head = String::from_utf8_lossy(&buffer[..head_end]).to_string();
        buffer.drain(..head_end + 4);
        let request = RequestHead::parse(&head);
        let mut body = vec![0u8; request.content_length()];
        // The body may have arrived in the SAME read as the head: it is
        // taken from the buffer first, and only the missing bytes are
        // awaited from the socket.
        let taken = body.len().min(buffer.len());
        body[..taken].copy_from_slice(&buffer[..taken]);
        buffer.drain(..taken);
        if taken < body.len() {
            socket.read_exact(&mut body[taken..]).await?;
        }
        count_request(&counts);
        match request.route() {
            Route::Create => {
                match create_session(&counts, &body, behavior) {
                    CreateAnswer::Answer(session) => {
                        let body = format!(r#"{{"session_id":"{session}"}}"#);
                        respond(&mut socket, "200 OK", &body).await?;
                    }
                    CreateAnswer::Refuse => {
                        respond(&mut socket, "500 Internal Server Error", "{}").await?;
                    }
                    CreateAnswer::Hang => {
                        // The request was received and never answered: the
                        // caller waits until its own deadline.
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                        return Ok(());
                    }
                }
            }
            Route::Upload => {
                count_upload(&counts, body);
                respond(&mut socket, "200 OK", "{}").await?;
            }
            Route::Status(session) => {
                let produced = count_status(&counts);
                let body = format!(
                    r#"{{"session_id":"{session}","running":false,"output_offset":{produced}}}"#
                );
                respond(&mut socket, "200 OK", &body).await?;
            }
            Route::Delete(session) => {
                count_delete(&counts);
                let body = format!(r#"{{"session_id":"{session}"}}"#);
                respond(&mut socket, "200 OK", &body).await?;
            }
            Route::Pipe(session) => {
                let first_pipe = count_pipe(&counts) == 1;
                serve_pipe(
                    &mut socket,
                    &request,
                    &session,
                    behavior,
                    first_pipe,
                    &counts,
                )
                .await?;
                let _ = closures.send(());
                return Ok(());
            }
            Route::Unknown => {
                respond(&mut socket, "404 Not Found", "{}").await?;
            }
        }
    }
}

/// What the fake answers to a create.
enum CreateAnswer {
    /// The session was created.
    Answer(String),
    /// The daemon refused: the session does not exist.
    Refuse,
    /// The request was received and never answered.
    Hang,
}

/// Answer a create.
///
/// The counters are updated in a SYNCHRONOUS helper: a guard held across
/// an await would make the fake's futures un-sendable.
fn create_session(
    counts: &Arc<Mutex<FakeCounts>>,
    body: &[u8],
    behavior: FakeBehavior,
) -> CreateAnswer {
    let command = serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("command")
                .and_then(|command| command.as_str())
                .map(str::to_string)
        })
        .unwrap_or_default();
    let session = {
        let mut counts = counts.lock().expect("the counts are writable");
        counts.creates += 1;
        counts.commands.push(command);
        format!("pty-{}", counts.creates)
    };
    match behavior {
        FakeBehavior::SilentOnCreate => CreateAnswer::Hang,
        FakeBehavior::RefusesCreate => CreateAnswer::Refuse,
        FakeBehavior::Cooperative
        | FakeBehavior::LosesTheSession
        | FakeBehavior::LosesTheFirstSession => CreateAnswer::Answer(session),
    }
}

/// Count one accepted connection.
fn count_connection(counts: &Arc<Mutex<FakeCounts>>) {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.connections += 1;
}

/// Count one answered HTTP request.
fn count_request(counts: &Arc<Mutex<FakeCounts>>) {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.http_requests += 1;
}

/// Count one session state request and answer how many bytes the
/// session produced.
fn count_status(counts: &Arc<Mutex<FakeCounts>>) -> usize {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.statuses += 1;
    SESSION_OUTPUT.len()
}

/// Count one deleted session.
fn count_delete(counts: &Arc<Mutex<FakeCounts>>) {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.deletes += 1;
}

/// Count one attached pipe and answer how many pipes the fake served.
fn count_pipe(counts: &Arc<Mutex<FakeCounts>>) -> usize {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.pipes += 1;
    counts.pipes
}

/// Count one uploaded file and keep its raw body: the bytes the
/// transport put on the wire are observable, not only their count.
fn count_upload(counts: &Arc<Mutex<FakeCounts>>, body: Vec<u8>) {
    let mut counts = counts.lock().expect("the counts are writable");
    counts.uploads.push(body);
}

async fn serve_pipe(
    socket: &mut TcpStream,
    request: &RequestHead,
    session: &str,
    behavior: FakeBehavior,
    first_pipe: bool,
    counts: &Arc<Mutex<FakeCounts>>,
) -> io::Result<()> {
    let Some(key) = request.header("sec-websocket-key") else {
        respond(socket, "400 Bad Request", "{}").await?;
        return Ok(());
    };
    let accept = derive_accept_key(key.as_bytes());
    let handshake = format!(
        "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {accept}\r\n\r\n"
    );
    socket.write_all(handshake.as_bytes()).await?;

    // The output travels as a BINARY frame whose payload carries the
    // execd prefix (stdout), exactly as the daemon sends it.
    let mut payload = vec![super::frames::BIN_STDOUT];
    payload.extend_from_slice(SESSION_OUTPUT);
    let mut frames = websocket_frame(0x02, &payload);
    let loses_the_session = match behavior {
        FakeBehavior::LosesTheSession => true,
        FakeBehavior::LosesTheFirstSession => first_pipe,
        _ => false,
    };
    if !loses_the_session {
        frames.extend(websocket_frame(0x01, br#"{"type":"exit","exit_code":0}"#));
    }
    socket.write_all(&frames).await?;
    if loses_the_session {
        // The session is LOST: the pipe closes without an exit frame,
        // so the client sees the end of the stream at once.
        return Ok(());
    }

    // The client closes the pipe: its frames are read until the socket
    // ends, so the close is orderly. Every complete client frame is
    // unmasked and recorded: the order of the payloads is what the
    // transport promised to put on the wire.
    let mut chunk = [0u8; 1024];
    let mut pending = Vec::new();
    loop {
        let read = socket.read(&mut chunk).await?;
        if read == 0 {
            break;
        }
        pending.extend_from_slice(&chunk[..read]);
        let frames = take_client_frames(&mut pending);
        if !frames.is_empty() {
            let mut counts = counts.lock().expect("the counts are writable");
            counts.pipe_frames.extend(frames);
        }
    }
    let _ = session;
    Ok(())
}

/// The WebSocket opcode of a binary payload frame, as tungstenite sends it.
pub const CLIENT_FRAME_BINARY: u8 = 0x2;

/// Pop every complete client WebSocket frame from the buffer: the
/// transports mask their payloads, so the fake unmasks them here.
fn take_client_frames(buffer: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let mut frames = Vec::new();
    loop {
        if buffer.len() < 2 {
            return frames;
        }
        let opcode = buffer[0] & 0x0f;
        let masked = buffer[1] & 0x80 != 0;
        let mut length = usize::from(buffer[1] & 0x7f);
        let mut offset = 2;
        if length == 126 {
            if buffer.len() < offset + 2 {
                return frames;
            }
            length = usize::from(u16::from_be_bytes([buffer[offset], buffer[offset + 1]]));
            offset += 2;
        } else if length == 127 {
            if buffer.len() < offset + 8 {
                return frames;
            }
            let wide = [
                buffer[offset],
                buffer[offset + 1],
                buffer[offset + 2],
                buffer[offset + 3],
                buffer[offset + 4],
                buffer[offset + 5],
                buffer[offset + 6],
                buffer[offset + 7],
            ];
            let Ok(wide) = usize::try_from(u64::from_be_bytes(wide)) else {
                return frames;
            };
            length = wide;
            offset += 8;
        }
        // A client that does not mask is not this transport: record nothing.
        if !masked {
            return frames;
        }
        if buffer.len() < offset + 4 + length {
            return frames;
        }
        let key = [
            buffer[offset],
            buffer[offset + 1],
            buffer[offset + 2],
            buffer[offset + 3],
        ];
        let payload: Vec<u8> = buffer[offset + 4..offset + 4 + length]
            .iter()
            .enumerate()
            .map(|(position, byte)| byte ^ key[position % 4])
            .collect();
        buffer.drain(..offset + 4 + length);
        frames.push((opcode, payload));
    }
}

/// A server frame: the final bit is set and the payload is not masked.
fn websocket_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
    let mut frame = vec![0x80 | opcode];
    let length = payload.len();
    if length < 126 {
        frame.push(u8::try_from(length).unwrap_or(0));
    } else if length <= usize::from(u16::MAX) {
        frame.push(126);
        frame.extend_from_slice(&u16::try_from(length).unwrap_or(0).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&u64::try_from(length).unwrap_or(0).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    frame
}

fn find_head_end(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

async fn respond(socket: &mut TcpStream, status: &str, body: &str) -> io::Result<()> {
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    socket.write_all(response.as_bytes()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A client frame, masked the way the transports mask them.
    fn masked_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let key = [0x11, 0x22, 0x33, 0x44];
        let mut frame = vec![0x80 | opcode];
        match payload.len() {
            0..=125 => frame.push(0x80 | u8::try_from(payload.len()).expect("short payload")),
            other => {
                frame.push(0x80 | 126);
                let wide = u16::try_from(other).expect("payload fits the widened length");
                frame.extend_from_slice(&wide.to_be_bytes());
            }
        }
        frame.extend_from_slice(&key);
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(position, byte)| byte ^ key[position % 4]),
        );
        frame
    }

    #[test]
    fn client_frames_are_unmasked_in_order() {
        let mut wire = masked_client_frame(CLIENT_FRAME_BINARY, b"one\n");
        wire.extend(masked_client_frame(CLIENT_FRAME_BINARY, b"two\n"));
        let mut buffer = wire;
        let frames = take_client_frames(&mut buffer);
        assert_eq!(
            frames,
            vec![
                (CLIENT_FRAME_BINARY, b"one\n".to_vec()),
                (CLIENT_FRAME_BINARY, b"two\n".to_vec()),
            ]
        );
        assert!(buffer.is_empty(), "the whole wire was consumed");
    }

    #[test]
    fn an_incomplete_client_frame_waits_for_the_rest_of_its_bytes() {
        let mut wire = masked_client_frame(CLIENT_FRAME_BINARY, b"payload");
        let cut = wire.len() - 3;
        let missing = wire.split_off(cut);
        assert!(
            take_client_frames(&mut wire).is_empty(),
            "a half frame is not a frame"
        );
        wire.extend(missing);
        assert_eq!(
            take_client_frames(&mut wire),
            vec![(CLIENT_FRAME_BINARY, b"payload".to_vec())]
        );
    }

    #[test]
    fn a_long_client_frame_uses_the_widened_length() {
        let payload = vec![b'x'; 300];
        let mut buffer = masked_client_frame(CLIENT_FRAME_BINARY, &payload);
        let frames = take_client_frames(&mut buffer);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].1, payload);
        assert!(buffer.is_empty());
    }
}
