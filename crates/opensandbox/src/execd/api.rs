//! Minimal execd HTTP API client (surface needed by the ACP probe):
//! create/delete PTY sessions over the execd REST endpoint.
//!
//! Every call carries the credential of the endpoint: the execd is the
//! execution plane of a sandbox, and whoever reaches it runs commands
//! inside the environment.

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::endpoint::{EXECD_ACCESS_TOKEN_HEADER, ExecdEndpoint};

#[derive(Debug, Error)]
pub enum ExecdApiError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("execd returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("invalid JSON from execd: {0}")]
    InvalidJson(#[from] serde_json::Error),
    /// The transport of the client itself could not be built: nothing was
    /// sent and no session exists.
    #[error("the HTTP client of the execd is unavailable: {0}")]
    ClientUnavailable(String),
}

#[derive(Debug, Serialize)]
pub struct CreateSessionRequest<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<&'a str>,
}

#[derive(Debug, Deserialize)]
pub struct CreateSessionResponse {
    pub session_id: String,
}

/// Deadline of the regular execd calls.
const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// `POST /pty` — create a PTY/pipe session running `command` (shell syntax).
pub async fn create_session(
    endpoint: &ExecdEndpoint,
    req: CreateSessionRequest<'_>,
) -> Result<CreateSessionResponse, ExecdApiError> {
    create_session_with_timeout(endpoint, req, DEFAULT_TIMEOUT).await
}

/// Same, with an explicit deadline. The readiness probe needs a SHORT
/// one: it must fail fast while the sandbox is still booting, so the
/// bounded wait can poll without spending the whole budget on one call.
pub async fn create_session_with_timeout(
    endpoint: &ExecdEndpoint,
    req: CreateSessionRequest<'_>,
    timeout: std::time::Duration,
) -> Result<CreateSessionResponse, ExecdApiError> {
    let http = http_client()?;
    create_session_with_client(&http, endpoint, req, timeout).await
}

/// Same, over a client that is SHARED with the other calls: the
/// connections are reused and the deadline of the call is the one of the
/// operation that asked for it.
pub async fn create_session_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    req: CreateSessionRequest<'_>,
    timeout: std::time::Duration,
) -> Result<CreateSessionResponse, ExecdApiError> {
    let url = format!("{}/pty", endpoint.base_url());
    let mut builder = http.post(url).timeout(timeout).json(&req);
    if let Some(token) = endpoint.access_token() {
        builder = builder.header(EXECD_ACCESS_TOKEN_HEADER, token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let body = response.text().await?;
    if !(200..300).contains(&status) {
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(serde_json::from_str(&body)?)
}

/// Is the execd accepting sessions RIGHT NOW? A freshly created sandbox
/// can have its execd still booting: the port is published before the
/// server inside is able to answer. The probe runs a harmless command
/// and deletes the session it opened.
pub async fn session_accepted(endpoint: &ExecdEndpoint, timeout: std::time::Duration) -> bool {
    let req = CreateSessionRequest {
        cwd: None,
        command: Some("true"),
    };
    match create_session_with_timeout(endpoint, req, timeout).await {
        Ok(session) => {
            let _ = delete_session(endpoint, &session.session_id).await;
            true
        }
        Err(_) => false,
    }
}

/// The state of a session: whether it still runs and how much output it
/// produced.
///
/// The fields are OPTIONAL: a daemon that answers a different shape
/// does not make the caller crash, it makes the fact unknown.
#[derive(Debug, Deserialize)]
pub struct SessionStatusResponse {
    pub session_id: String,
    /// Whether the session still runs. `None` when the daemon does not
    /// say: an unknown state is not a finished session.
    #[serde(default)]
    pub running: Option<bool>,
    /// How many bytes the session produced: the yardstick for the
    /// completeness of the output. `None` when the daemon does not say.
    #[serde(default)]
    pub output_offset: Option<i64>,
}

/// `GET /pty/{id}` — the session state and its output offset.
///
/// The reference daemon answers the session state at `/pty/{id}`; some
/// builds answer the same shape at `/pty/{id}/status`. Both are tried,
/// in that order, and the search continues on ANY failure — a 404, a
/// transport error or a body with another shape — because the output
/// offset is what tells a truncated output from a complete one, and
/// losing it would silently disable that distinction.
pub async fn session_status(
    endpoint: &ExecdEndpoint,
    session_id: &str,
) -> Result<SessionStatusResponse, ExecdApiError> {
    let http = http_client()?;
    session_status_with_client(&http, endpoint, session_id, DEFAULT_TIMEOUT).await
}

/// Same, over a client that is SHARED with the other calls.
pub async fn session_status_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    session_id: &str,
    timeout: std::time::Duration,
) -> Result<SessionStatusResponse, ExecdApiError> {
    let mut last: Option<ExecdApiError> = None;
    for path in [
        format!("/pty/{session_id}"),
        format!("/pty/{session_id}/status"),
    ] {
        let url = format!("{}{path}", endpoint.base_url());
        let mut builder = http.get(url).timeout(timeout);
        if let Some(token) = endpoint.access_token() {
            builder = builder.header(EXECD_ACCESS_TOKEN_HEADER, token);
        }
        let response = match builder.send().await {
            Ok(response) => response,
            // A transport failure is not the end of the search.
            Err(err) => {
                last = Some(ExecdApiError::Http(err));
                continue;
            }
        };
        let status = response.status().as_u16();
        let body = response.text().await.unwrap_or_default();
        if !(200..300).contains(&status) {
            last = Some(ExecdApiError::Status { status, body });
            continue;
        }
        match serde_json::from_str::<SessionStatusResponse>(&body) {
            Ok(parsed) => {
                // A body without the facts we need is not an answer:
                // the other endpoint may know them.
                if parsed.running.is_some() || parsed.output_offset.is_some() {
                    return Ok(parsed);
                }
                last = Some(ExecdApiError::Status {
                    status,
                    body: format!("session state without the expected fields: {body}"),
                });
            }
            Err(err) => last = Some(ExecdApiError::InvalidJson(err)),
        }
    }
    Err(last.unwrap_or(ExecdApiError::Status {
        status: 404,
        body: format!("no session state for {session_id}"),
    }))
}

/// How many output bytes the execd says the session produced.
pub async fn session_output_offset(endpoint: &ExecdEndpoint, session_id: &str) -> Option<i64> {
    let http = http_client().ok()?;
    session_output_offset_with_client(&http, endpoint, session_id, DEFAULT_TIMEOUT).await
}

/// Same, over a client that is SHARED with the other calls.
pub async fn session_output_offset_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    session_id: &str,
    timeout: std::time::Duration,
) -> Option<i64> {
    session_status_with_client(http, endpoint, session_id, timeout)
        .await
        .ok()
        .and_then(|status| status.output_offset)
}

/// `DELETE /pty/{id}` — terminate the session and its process group.
pub async fn delete_session(
    endpoint: &ExecdEndpoint,
    session_id: &str,
) -> Result<(), ExecdApiError> {
    let http = http_client()?;
    delete_session_with_client(&http, endpoint, session_id, DEFAULT_TIMEOUT).await
}

/// Same, over a client that is SHARED with the other calls.
pub async fn delete_session_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    session_id: &str,
    timeout: std::time::Duration,
) -> Result<(), ExecdApiError> {
    let url = format!("{}/pty/{session_id}", endpoint.base_url());
    let mut builder = http.delete(url).timeout(timeout);
    if let Some(token) = endpoint.access_token() {
        builder = builder.header(EXECD_ACCESS_TOKEN_HEADER, token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(())
}

/// `POST /files/upload` — multipart upload of a file into the sandbox.
///
/// Contract: one metadata part (JSON `{"path":...,"mode":...}`) followed
/// by the binary `file` part, in that order.
pub async fn upload_file(
    endpoint: &ExecdEndpoint,
    bytes: Vec<u8>,
    remote_path: &str,
) -> Result<(), ExecdApiError> {
    let http = http_client()?;
    upload_file_with_client(&http, endpoint, bytes, remote_path, DEFAULT_TIMEOUT).await
}

/// Same, over a client that is SHARED with the other calls.
pub async fn upload_file_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    bytes: Vec<u8>,
    remote_path: &str,
    timeout: std::time::Duration,
) -> Result<(), ExecdApiError> {
    let url = format!("{}/files/upload", endpoint.base_url());
    let metadata = serde_json::json!({
        "path": remote_path,
    })
    .to_string();
    let metadata_part = reqwest::multipart::Part::text(metadata)
        .file_name("metadata.json")
        .mime_str("application/json")?;
    let file_part = reqwest::multipart::Part::bytes(bytes)
        .file_name("payload")
        .mime_str("application/json")?;
    let form = reqwest::multipart::Form::new()
        .part("metadata", metadata_part)
        .part("file", file_part);
    let mut builder = http.post(url).timeout(timeout).multipart(form);
    if let Some(token) = endpoint.access_token() {
        builder = builder.header(EXECD_ACCESS_TOKEN_HEADER, token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(())
}

/// `GET /files/download?path=...` — stream a sandbox file back to the host.
pub async fn download_file(endpoint: &ExecdEndpoint, path: &str) -> Result<Vec<u8>, ExecdApiError> {
    let http = http_client()?;
    download_file_with_client(&http, endpoint, path, DEFAULT_TIMEOUT).await
}

/// Same, over a client that is SHARED with the other calls.
pub async fn download_file_with_client(
    http: &reqwest::Client,
    endpoint: &ExecdEndpoint,
    path: &str,
    timeout: std::time::Duration,
) -> Result<Vec<u8>, ExecdApiError> {
    let url = format!("{}/files/download", endpoint.base_url());
    let mut builder = http.get(url).timeout(timeout).query(&[("path", path)]);
    if let Some(token) = endpoint.access_token() {
        builder = builder.header(EXECD_ACCESS_TOKEN_HEADER, token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(response.bytes().await?.to_vec())
}

/// The HTTP client of the execd calls. The deadline of a call is the
/// deadline of the REQUEST, not of the client: the same client serves
/// calls with different budgets.
fn http_client() -> Result<reqwest::Client, ExecdApiError> {
    Ok(reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_session_request_omits_optional_fields() {
        let none = serde_json::to_value(CreateSessionRequest {
            cwd: None,
            command: None,
        })
        .unwrap();
        assert_eq!(none, serde_json::json!({}));
        let full = serde_json::to_value(CreateSessionRequest {
            cwd: Some("/workspace"),
            command: Some("claude-code-acp"),
        })
        .unwrap();
        assert_eq!(
            full,
            serde_json::json!({"cwd": "/workspace", "command": "claude-code-acp"})
        );
    }

    #[tokio::test]
    async fn create_session_parses_session_id_from_real_shape() {
        // Spin a tiny local HTTP responder instead of mocking the client.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(socket);
            let mut buf = vec![0u8; 4096];
            let _ = tokio::io::AsyncReadExt::read(&mut reader, &mut buf).await;
            // Raw HTTP response with the documented session shape.
            let body = br#"{"session_id":"pty-123"}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{}",
                body.len(),
                String::from_utf8_lossy(body)
            );
            use tokio::io::AsyncWriteExt;
            let _ = writer.write_all(response.as_bytes()).await;
        });
        let response = create_session(
            &ExecdEndpoint::new(format!("http://{addr}")),
            CreateSessionRequest {
                cwd: None,
                command: Some("sh"),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.session_id, "pty-123");
    }

    #[tokio::test]
    async fn execd_requests_do_not_follow_redirects_with_credentials() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_addr = source.local_addr().unwrap();
        let target_addr = target.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket.write_all(format!("HTTP/1.1 307 Temporary Redirect\r\nLocation: http://{target_addr}/pty\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let endpoint =
            ExecdEndpoint::with_token(format!("http://{source_addr}"), Some("secret".into()));
        let result = create_session_with_timeout(
            &endpoint,
            CreateSessionRequest {
                cwd: None,
                command: None,
            },
            std::time::Duration::from_millis(500),
        )
        .await;
        server.await.unwrap();
        assert!(matches!(
            result,
            Err(ExecdApiError::Status { status: 307, .. })
        ));
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), target.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_credential_travels_in_the_header_the_daemon_expects() {
        // The daemon answers 401 without the credential: the refusal is
        // observable without any infrastructure.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(None));
        let recorded = seen.clone();
        tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let (mut reader, mut writer) = tokio::io::split(socket);
            let mut buf = vec![0u8; 4096];
            let read = tokio::io::AsyncReadExt::read(&mut reader, &mut buf)
                .await
                .unwrap();
            let request = String::from_utf8_lossy(&buf[..read]).to_string();
            let token = request
                .lines()
                .find_map(|line| line.strip_prefix("x-execd-access-token: "))
                .map(|value| value.trim().to_string());
            *recorded.lock().unwrap() = token;
            let body = b"unauthorized";
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            use tokio::io::AsyncWriteExt;
            let _ = writer.write_all(response.as_bytes()).await;
            let _ = writer.write_all(body).await;
        });
        let endpoint = ExecdEndpoint::with_token(
            format!("http://{addr}"),
            Some("token-of-the-client".to_string()),
        );
        let error = create_session(
            &endpoint,
            CreateSessionRequest {
                cwd: None,
                command: Some("sh"),
            },
        )
        .await
        .expect_err("the responder refuses");
        assert!(matches!(error, ExecdApiError::Status { status: 401, .. }));
        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("token-of-the-client"),
            "the credential travels in the header"
        );
    }
}
