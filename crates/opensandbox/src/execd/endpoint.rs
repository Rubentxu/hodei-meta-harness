//! The address of an execd, with the credential that authorizes it.
//!
//! The execd is the execution plane of a sandbox: whoever reaches it
//! runs commands inside the environment. The endpoint therefore carries
//! the credential, and the credential is part of the type: a caller
//! cannot drive an execd it did not receive the credential for.
//!
//! The credential travels in the environment of the process that needs
//! it (the control plane hands it over in the endpoint contract) and
//! never in a URL, a command or a record.

use super::pipe::AttachOptions;

/// The header the execd expects the credential in.
pub const EXECD_ACCESS_TOKEN_HEADER: &str = "X-EXECD-ACCESS-TOKEN";

/// The environment variable that carries the credential of an execd for
/// the tools that talk to it directly. The credential travels in the
/// environment of the process that needs it, never in a command line.
pub const EXECD_ACCESS_TOKEN_ENV: &str = "HODEI_MH_EXECD_ACCESS_TOKEN";

/// An execd, with the credential that authorizes the calls to it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecdEndpoint {
    base_url: String,
    access_token: Option<String>,
}

impl std::fmt::Display for ExecdEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The credential NEVER appears in a rendered endpoint.
        f.write_str(&self.base_url)
    }
}

impl ExecdEndpoint {
    /// An endpoint without a credential. It can only address an execd
    /// that does not ask for one.
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: None,
        }
    }

    /// An endpoint with the credential that authorizes it.
    pub fn with_token(base_url: impl Into<String>, access_token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            access_token: access_token.filter(|token| !token.is_empty()),
        }
    }

    /// An endpoint whose credential comes from the environment of THIS
    /// process. It is the way a tool that talks to an execd directly
    /// (a debug command, a script) carries the credential.
    pub fn from_env(base_url: impl Into<String>) -> Self {
        let token = std::env::var(EXECD_ACCESS_TOKEN_ENV)
            .ok()
            .filter(|token| !token.trim().is_empty());
        Self::with_token(base_url, token)
    }

    /// Build the endpoint from the contract the control plane answers:
    /// the address of the execd and the header that authorizes it.
    pub fn from_contract(value: &serde_json::Value) -> Option<Self> {
        let endpoint = value.get("endpoint")?.as_str()?;
        let base_url = if endpoint.starts_with("http://") || endpoint.starts_with("https://") {
            endpoint.to_string()
        } else {
            format!("http://{endpoint}")
        };
        let token = value
            .get("headers")
            .and_then(|headers| headers.get(EXECD_ACCESS_TOKEN_HEADER))
            .and_then(|token| token.as_str())
            .map(str::to_string);
        Some(Self::with_token(base_url, token))
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Whether the endpoint carries an address at all.
    pub fn is_empty(&self) -> bool {
        self.base_url.is_empty()
    }

    pub fn access_token(&self) -> Option<&str> {
        self.access_token.as_deref()
    }

    /// The WebSocket address of a session.
    pub fn ws_url(&self, session_id: &str) -> String {
        let ws_base = self
            .base_url
            .replacen("http://", "ws://", 1)
            .replacen("https://", "wss://", 1);
        format!("{ws_base}/pty/{session_id}/ws?pty=0")
    }

    /// The options that attach to a session of this execd, with the
    /// credential in the header the daemon expects.
    pub fn attach_options(&self) -> AttachOptions {
        AttachOptions {
            access_token: None,
            header_token: self.access_token.clone(),
            connect_timeout: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_endpoint_of_the_contract_carries_its_credential() {
        let endpoint = ExecdEndpoint::from_contract(&json!({
            "endpoint": "127.0.0.1:45678",
            "headers": {"X-EXECD-ACCESS-TOKEN": "token-1"}
        }))
        .expect("the endpoint");
        assert_eq!(endpoint.base_url(), "http://127.0.0.1:45678");
        assert_eq!(endpoint.access_token(), Some("token-1"));
        assert_eq!(
            endpoint.attach_options().header_token.as_deref(),
            Some("token-1")
        );
    }

    #[test]
    fn an_endpoint_without_a_credential_has_none() {
        let endpoint = ExecdEndpoint::from_contract(&json!({"endpoint": "127.0.0.1:45678"}))
            .expect("the endpoint");
        assert_eq!(endpoint.base_url(), "http://127.0.0.1:45678");
        assert_eq!(endpoint.access_token(), None);
        assert!(endpoint.attach_options().header_token.is_none());
    }

    #[test]
    fn the_credential_never_travels_in_the_url() {
        let endpoint = ExecdEndpoint::with_token("http://127.0.0.1:45678", Some("token-1".into()));
        let url = endpoint.ws_url("session-1");
        assert_eq!(url, "ws://127.0.0.1:45678/pty/session-1/ws?pty=0");
        assert!(!url.contains("token-1"), "{url}");
    }
}
