//! The client of the lifecycle control plane.
//!
//! The control plane creates, enumerates, executes and destroys
//! environments: it is authorized, and the credential that proves it
//! travels in the environment of the process that needs it — never in a
//! URL, a command or a record.
//!
//! Every call goes through this client, so the rule of the
//! authorization lives in exactly one place.

use reqwest::{Client, RequestBuilder};

/// The environment variable that carries the control credential.
pub const CONTROL_TOKEN_ENV: &str = "HODEI_MH_CONTROL_TOKEN";

/// The header the control plane expects the credential in.
pub const AUTHORIZATION_HEADER: &str = "Authorization";

/// A client of the lifecycle control plane.
#[derive(Clone)]
pub struct ControlPlane {
    base_url: String,
    token: Option<String>,
    http: Client,
}

impl std::fmt::Debug for ControlPlane {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlPlane")
            .field("base_url", &"<redacted>")
            .field("token", &self.token.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl ControlPlane {
    /// A client for a base URL. The credential comes from the
    /// environment of THIS process.
    pub fn new(base_url: impl Into<String>) -> Self {
        let token = std::env::var(CONTROL_TOKEN_ENV)
            .ok()
            .filter(|token| !token.trim().is_empty());
        Self::with_token(base_url, token)
    }

    /// A client with an explicit credential. `None` means an
    /// UNAUTHORIZED client: the control plane will refuse it, and that
    /// refusal is part of the contract.
    pub fn with_token(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into().trim_end_matches('/').to_string(),
            token,
            http: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .expect("static HTTP client configuration is valid"),
        }
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// The credential this client presents, if any.
    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The HTTP client, for the calls this type does not cover.
    pub fn http(&self) -> &Client {
        &self.http
    }

    /// The reason this client cannot talk to the control plane, when
    /// the credential is missing.
    pub fn missing_credential(&self) -> Option<String> {
        if self.token.is_some() {
            return None;
        }
        Some(format!(
            "the control credential is absent: set {CONTROL_TOKEN_ENV} in the environment of the \
             process that talks to the control plane"
        ))
    }

    fn request(&self, method: reqwest::Method, path: &str) -> RequestBuilder {
        let url = format!("{}{path}", self.base_url);
        let mut builder = self.http.request(method, url);
        if let Some(token) = &self.token {
            builder = builder.header(AUTHORIZATION_HEADER, format!("Bearer {token}"));
        }
        builder
    }

    pub fn get(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::GET, path)
    }

    pub fn post(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::POST, path)
    }

    pub fn delete(&self, path: &str) -> RequestBuilder {
        self.request(reqwest::Method::DELETE, path)
    }

    /// The liveness route of the control plane. It carries no
    /// credential: it says nothing about any environment.
    pub fn ready(&self) -> RequestBuilder {
        self.get("/ready")
    }

    /// The path of the collection of environments.
    pub fn sandboxes_path() -> &'static str {
        "/v1/sandboxes"
    }

    /// The path of one environment.
    pub fn sandbox_path(id: &str) -> String {
        format!("/v1/sandboxes/{id}")
    }

    /// The path of one published port of an environment.
    pub fn endpoint_path(id: &str, port: u16) -> String {
        format!("/v1/sandboxes/{id}/endpoints/{port}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_base_url_is_normalized_and_the_paths_are_built_from_it() {
        let control = ControlPlane::with_token("http://127.0.0.1:8081/", None);
        assert_eq!(control.base_url(), "http://127.0.0.1:8081");
        assert_eq!(ControlPlane::sandboxes_path(), "/v1/sandboxes");
        assert_eq!(ControlPlane::sandbox_path("abc"), "/v1/sandboxes/abc");
        assert_eq!(
            ControlPlane::endpoint_path("abc", 44772),
            "/v1/sandboxes/abc/endpoints/44772"
        );
    }

    #[test]
    fn debug_output_does_not_expose_control_credentials() {
        let control =
            ControlPlane::with_token("http://127.0.0.1:8081", Some("private-token".into()));
        assert!(!format!("{control:?}").contains("private-token"));
    }

    #[test]
    fn a_client_without_a_credential_says_why() {
        let control = ControlPlane::with_token("http://127.0.0.1:8081", None);
        let reason = control
            .missing_credential()
            .expect("the credential is missing");
        assert!(reason.contains(CONTROL_TOKEN_ENV), "{reason}");
    }

    #[tokio::test]
    async fn control_requests_do_not_follow_redirects() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let target = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_addr = source.local_addr().unwrap();
        let target_addr = target.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = source.accept().await.unwrap();
            let mut bytes = [0; 4096];
            let _ = socket.read(&mut bytes).await.unwrap();
            socket.write_all(format!("HTTP/1.1 302 Found\r\nLocation: http://{target_addr}/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
        });
        let control =
            ControlPlane::with_token(format!("http://{source_addr}"), Some("secret".into()));
        let response = control.get("/v1/sandboxes").send().await.unwrap();
        server.await.unwrap();
        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(100), target.accept())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn the_credential_travels_in_the_authorization_header() {
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
            let authorization = request
                .lines()
                .find_map(|line| line.strip_prefix("authorization: "))
                .map(|value| value.trim().to_string());
            *recorded.lock().unwrap() = authorization;
            use tokio::io::AsyncWriteExt;
            let _ = writer
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                .await;
        });
        let control = ControlPlane::with_token(
            format!("http://{addr}"),
            Some("token-of-the-client".to_string()),
        );
        let _ = control.get("/v1/sandboxes").send().await;
        assert_eq!(
            seen.lock().unwrap().as_deref(),
            Some("Bearer token-of-the-client")
        );
    }
}
