//! Authentication of the control plane.
//!
//! The lifecycle API creates, enumerates, executes and destroys
//! environments: an unauthorized caller must not reach any of it. The
//! server therefore requires a token, and the client proves it carries
//! the same one.
//!
//! The token travels ONLY in the environment of the process that needs
//! it: the server reads it from its own environment, the client reads it
//! from its own. It is never written to disk, never logged and never
//! echoed back in a response.

use axum::extract::State;
use axum::http::{Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::model::error_codes;
use crate::service::SharedService;

/// The environment variable that carries the control-plane token.
pub const CONTROL_TOKEN_ENV: &str = "HODEI_MH_CONTROL_TOKEN";

/// The token that authorizes a client of the control plane.
#[derive(Clone, PartialEq, Eq)]
pub struct ControlToken(String);

impl ControlToken {
    /// Build a token from a raw value. An empty token is refused: it
    /// would authorize everybody who asks for nothing.
    pub fn new(raw: impl Into<String>) -> Result<Self, TokenError> {
        let raw = raw.into();
        if raw.trim().is_empty() {
            return Err(TokenError::Empty);
        }
        Ok(Self(raw))
    }

    /// Read the token from the environment of this process.
    pub fn from_env() -> Result<Self, TokenError> {
        match std::env::var(CONTROL_TOKEN_ENV) {
            Ok(raw) => Self::new(raw),
            Err(_) => Err(TokenError::Absent),
        }
    }

    /// A token nobody else knows: used when the process must serve
    /// without a configured credential (the library default). The
    /// operator's server never uses it: the binary requires the token
    /// from the environment.
    pub fn random() -> Self {
        Self(format!(
            "{}{}",
            uuid::Uuid::new_v4().simple(),
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Whether the presented credential is the authorized one.
    ///
    /// Comparison is constant-time for credentials of equal length;
    /// unequal lengths are rejected without comparing their contents.
    pub fn authorizes(&self, presented: Option<&str>) -> bool {
        use subtle::ConstantTimeEq;
        let Some(presented) = presented else {
            return false;
        };
        bool::from(self.0.as_bytes().ct_eq(presented.as_bytes()))
    }
}

/// The value the credential travels in: an `Authorization: Bearer` header.
pub const BEARER_PREFIX: &str = "Bearer ";

/// Extract the presented credential from an `Authorization` header.
pub fn presented_credential(value: &str) -> Option<&str> {
    let credential = value.strip_prefix(BEARER_PREFIX)?;
    if credential.is_empty() {
        None
    } else {
        Some(credential)
    }
}

impl std::fmt::Debug for ControlToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The token NEVER appears in a log line: only its presence.
        f.write_str("ControlToken(<redacted>)")
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum TokenError {
    #[error(
        "the control token is absent: set {CONTROL_TOKEN_ENV} in the environment of the process"
    )]
    Absent,
    #[error("the control token is empty: {CONTROL_TOKEN_ENV} must carry a non-empty value")]
    Empty,
}

/// The refusal for a request without a valid credential.
pub fn unauthorized() -> Response {
    let body = crate::model::ErrorEnvelope::new(
        error_codes::UNAUTHORIZED,
        "this request carries no valid control credential; the lifecycle API is authorized",
    );
    (StatusCode::UNAUTHORIZED, axum::Json(body)).into_response()
}

/// Reject every request that does not carry the control token.
///
/// The check happens BEFORE the handler runs, so an unauthorized request
/// never creates, enumerates, executes nor destroys an environment.
pub async fn require_control_token(
    State(service): State<SharedService>,
    request: Request<axum::body::Body>,
    next: Next,
) -> Response {
    let credential = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(presented_credential);
    if service.control_token().authorizes(credential) {
        return next.run(request).await;
    }
    unauthorized()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(raw: &str) -> ControlToken {
        ControlToken::new(raw.to_string()).expect("a non-empty token")
    }

    #[test]
    fn an_empty_token_is_refused() {
        assert_eq!(ControlToken::new(""), Err(TokenError::Empty));
        assert_eq!(ControlToken::new("   "), Err(TokenError::Empty));
    }

    #[test]
    fn the_right_credential_authorizes_and_anything_else_does_not() {
        let token = token("s3cr3t");
        assert!(token.authorizes(Some("s3cr3t")));
        assert!(!token.authorizes(Some("s3cr3")));
        assert!(!token.authorizes(Some("s3cr3t ")));
        assert!(!token.authorizes(Some("S3CR3T")));
        assert!(!token.authorizes(None));
        assert!(!token.authorizes(Some("")));
        assert!(!token.authorizes(Some(&format!("s3cr3t{}", "\0".repeat(256)))));
    }

    #[test]
    fn the_token_never_appears_in_its_own_debug_output() {
        let rendered = format!("{:?}", token("s3cr3t"));
        assert!(!rendered.contains("s3cr3t"), "{rendered}");
        assert!(rendered.contains("redacted"), "{rendered}");
    }

    #[test]
    fn the_credential_is_read_from_the_bearer_header() {
        assert_eq!(presented_credential("Bearer abc"), Some("abc"));
        assert_eq!(presented_credential("abc"), None);
        assert_eq!(presented_credential("Bearer "), None);
        assert_eq!(presented_credential(""), None);
    }
}
