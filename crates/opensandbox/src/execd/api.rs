//! Minimal execd HTTP API client (surface needed by the ACP probe):
//! create/delete PTY sessions over the execd REST endpoint.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExecdApiError {
    #[error("http request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("execd returned {status}: {body}")]
    Status { status: u16, body: String },
    #[error("invalid JSON from execd: {0}")]
    InvalidJson(#[from] serde_json::Error),
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
    base_url: &str,
    access_token: Option<&str>,
    req: CreateSessionRequest<'_>,
) -> Result<CreateSessionResponse, ExecdApiError> {
    create_session_with_timeout(base_url, access_token, req, DEFAULT_TIMEOUT).await
}

/// Same, with an explicit deadline. The readiness probe needs a SHORT
/// one: it must fail fast while the sandbox is still booting, so the
/// bounded wait can poll without spending the whole budget on one call.
pub async fn create_session_with_timeout(
    base_url: &str,
    access_token: Option<&str>,
    req: CreateSessionRequest<'_>,
    timeout: std::time::Duration,
) -> Result<CreateSessionResponse, ExecdApiError> {
    let url = format!("{base_url}/pty");
    let mut builder = http_client(timeout)?.post(url).json(&req);
    if let Some(token) = access_token {
        builder = builder.header("X-EXECD-ACCESS-TOKEN", token);
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
pub async fn session_accepted(
    base_url: &str,
    access_token: Option<&str>,
    timeout: std::time::Duration,
) -> bool {
    let req = CreateSessionRequest {
        cwd: None,
        command: Some("true"),
    };
    match create_session_with_timeout(base_url, access_token, req, timeout).await {
        Ok(session) => {
            let _ = delete_session(base_url, access_token, &session.session_id).await;
            true
        }
        Err(_) => false,
    }
}

/// The state of a session: whether it still runs and how much output it
/// produced.
#[derive(Debug, Deserialize)]
pub struct SessionStatusResponse {
    pub session_id: String,
    pub running: bool,
    pub output_offset: i64,
}

/// `GET /pty/{id}` — the session state and its output offset.
///
/// The reference daemon answers the session state at `/pty/{id}`; some
/// builds answer the same shape at `/pty/{id}/status`. Both are tried,
/// in that order: a 404 on one of them says nothing about the session,
/// and the output offset is what tells a truncated output from a
/// complete one.
pub async fn session_status(
    base_url: &str,
    access_token: Option<&str>,
    session_id: &str,
) -> Result<SessionStatusResponse, ExecdApiError> {
    let mut last: Option<ExecdApiError> = None;
    for path in [
        format!("/pty/{session_id}"),
        format!("/pty/{session_id}/status"),
    ] {
        let url = format!("{base_url}{path}");
        let mut builder = http_client(DEFAULT_TIMEOUT)?.get(url);
        if let Some(token) = access_token {
            builder = builder.header("X-EXECD-ACCESS-TOKEN", token);
        }
        let response = builder.send().await?;
        let status = response.status().as_u16();
        let body = response.text().await?;
        if (200..300).contains(&status) {
            return Ok(serde_json::from_str(&body)?);
        }
        let error = ExecdApiError::Status { status, body };
        if status != 404 {
            return Err(error);
        }
        last = Some(error);
    }
    Err(last.unwrap_or(ExecdApiError::Status {
        status: 404,
        body: format!("no session state for {session_id}"),
    }))
}

/// How many output bytes the execd says the session produced.
///
/// `None` when the execd does not answer: the caller then knows nothing
/// about the completeness of the output, and must not claim it.
pub async fn session_output_offset(
    base_url: &str,
    access_token: Option<&str>,
    session_id: &str,
) -> Option<i64> {
    session_status(base_url, access_token, session_id)
        .await
        .ok()
        .map(|status| status.output_offset)
}

/// `DELETE /pty/{id}` — terminate the session and its process group.
pub async fn delete_session(
    base_url: &str,
    access_token: Option<&str>,
    session_id: &str,
) -> Result<(), ExecdApiError> {
    let url = format!("{base_url}/pty/{session_id}");
    let mut builder = http_client(DEFAULT_TIMEOUT)?.delete(url);
    if let Some(token) = access_token {
        builder = builder.header("X-EXECD-ACCESS-TOKEN", token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(())
}

/// `POST /files/upload` — multipart upload of a file into the sandbox.
/// Contract: one metadata part (JSON `{"path":..., "mode":...}`) followed by
/// the binary `file` part, in that order.
pub async fn upload_file(
    base_url: &str,
    access_token: Option<&str>,
    content: Vec<u8>,
    remote_path: &str,
) -> Result<(), ExecdApiError> {
    let url = format!("{base_url}/files/upload");
    let metadata = serde_json::json!({
        "path": remote_path,
    })
    .to_string();
    let metadata_part = reqwest::multipart::Part::text(metadata)
        .file_name("metadata.json")
        .mime_str("application/json")?;
    let file_part = reqwest::multipart::Part::bytes(content).file_name("payload");
    let form = reqwest::multipart::Form::new()
        .part("metadata", metadata_part)
        .part("file", file_part);
    let mut builder = http_client(DEFAULT_TIMEOUT)?.post(url).multipart(form);
    if let Some(token) = access_token {
        builder = builder.header("X-EXECD-ACCESS-TOKEN", token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(())
}

/// `GET /files/download?path=...` — stream a sandbox file back to the host.
pub async fn download_file(
    base_url: &str,
    access_token: Option<&str>,
    remote_path: &str,
) -> Result<Vec<u8>, ExecdApiError> {
    let url = format!("{base_url}/files/download");
    let mut builder = http_client(DEFAULT_TIMEOUT)?
        .get(url)
        .query(&[("path", remote_path)]);
    if let Some(token) = access_token {
        builder = builder.header("X-EXECD-ACCESS-TOKEN", token);
    }
    let response = builder.send().await?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        return Err(ExecdApiError::Status { status, body });
    }
    Ok(response.bytes().await?.to_vec())
}

fn http_client(timeout: std::time::Duration) -> Result<reqwest::Client, ExecdApiError> {
    Ok(reqwest::Client::builder().timeout(timeout).build()?)
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
            &format!("http://{addr}"),
            None,
            CreateSessionRequest {
                cwd: None,
                command: Some("sh"),
            },
        )
        .await
        .unwrap();
        assert_eq!(response.session_id, "pty-123");
    }
}
