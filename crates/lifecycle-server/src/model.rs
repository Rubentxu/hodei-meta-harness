//! Contract types for the OpenSandbox lifecycle API (subset used by hodei-meta-harness).
//!
//! Field names follow the public OpenAPI contract (camelCase). Unknown request
//! fields are ignored; response types omit `None` fields to match
//! `response_model_exclude_none` behavior of the reference server.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `POST /v1/sandboxes` request (subset).
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSandboxRequest {
    pub image: Option<ImageSpec>,
    pub entrypoint: Option<Vec<String>>,
    pub env: Option<BTreeMap<String, String>>,
    pub metadata: Option<BTreeMap<String, String>>,
    /// Seconds until automatic expiration; omit or `null` disables expiration.
    pub timeout: Option<i64>,
    pub resource_limits: Option<ResourceLimits>,
    pub platform: Option<PlatformSpec>,
    /// The stable key of the creation intent.
    ///
    /// Two requests with the same key and the same body are ONE creation:
    /// the second one is answered with the environment the first one
    /// produced. The same key with another body is a conflict.
    pub creation_key: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageSpec {
    pub uri: String,
    pub auth: Option<ImageAuth>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImageAuth {
    pub username: String,
    pub password: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceLimits {
    /// e.g. "512Mi" or bytes; resolved by Docker's memory parser semantics.
    pub memory: Option<String>,
    /// e.g. "1" or "0.5" (CPUs).
    pub cpu: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct PlatformSpec {
    pub os: String,
    pub arch: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateSandboxResponse {
    pub id: String,
    /// The id the runtime resolved for the image the sandbox runs, so a
    /// consumer can tell which environment it actually got.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image_id: Option<String>,
    pub status: SandboxStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandboxStatus {
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// `GET /v1/sandboxes/{id}` response.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Sandbox {
    pub id: String,
    pub status: SandboxStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<BTreeMap<String, String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<chrono::DateTime<chrono::Utc>>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub entrypoint: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ListSandboxesResponse {
    pub items: Vec<Sandbox>,
    pub total_count: i64,
}

/// `GET /v1/sandboxes/{id}/endpoints/{port}` response.
#[derive(Debug, Serialize)]
pub struct Endpoint {
    pub endpoint: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub headers: Option<BTreeMap<String, String>>,
}

/// Error body shape: `{"detail": {"code": ..., "message": ...}}` as produced
/// by the reference server's `HTTPException(detail={...})`.
#[derive(Debug, Serialize)]
pub struct ErrorEnvelope<'a> {
    pub detail: ErrorDetail<'a>,
}

#[derive(Debug, Serialize)]
pub struct ErrorDetail<'a> {
    pub code: &'a str,
    pub message: String,
}

impl<'a> ErrorEnvelope<'a> {
    pub fn new(code: &'a str, message: impl Into<String>) -> Self {
        Self {
            detail: ErrorDetail {
                code,
                message: message.into(),
            },
        }
    }
}

/// Well-known error codes shared with the reference server.
pub mod error_codes {
    pub const INVALID_PARAMETER: &str = "SANDBOX::INVALID_PARAMETER";
    pub const SANDBOX_NOT_FOUND: &str = "SANDBOX::NOT_FOUND";
    pub const CONTAINER_START_FAILED: &str = "SANDBOX::CONTAINER_START_FAILED";
    pub const EXECD_START_FAILED: &str = "SANDBOX::EXECD_START_FAILED";
    pub const EXECD_DISTRIBUTION_FAILED: &str = "SANDBOX::EXECD_DISTRIBUTION_FAILED";
    pub const NOT_IMPLEMENTED: &str = "SANDBOX::NOT_IMPLEMENTED";
    pub const INTERNAL: &str = "SANDBOX::INTERNAL";
    /// The global environment budget is exhausted: nothing was created.
    pub const BUDGET_EXHAUSTED: &str = "SANDBOX::BUDGET_EXHAUSTED";
    /// The creation key was already used with another body: nothing was
    /// created, and the environment of the first request was not touched.
    pub const CREATION_CONFLICT: &str = "SANDBOX::CREATION_CONFLICT";
    /// The creation with this key is still in flight: nothing was
    /// created, and the caller must wait for it.
    pub const CREATION_IN_PROGRESS: &str = "SANDBOX::CREATION_IN_PROGRESS";
    /// The request carries no valid credential: nothing was created,
    /// enumerated, executed nor destroyed.
    pub const UNAUTHORIZED: &str = "SANDBOX::UNAUTHORIZED";
    /// The network policy could not be enforced: nothing was created.
    pub const NETWORK_POLICY_VIOLATION: &str = "SANDBOX::NETWORK_POLICY_VIOLATION";
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn create_request_parses_camel_case_subset_and_ignores_unknown_fields() {
        let raw = json!({
            "image": {"uri": "opensandbox/code-interpreter:v1.1.0"},
            "entrypoint": ["sleep", "infinity"],
            "timeout": 1800,
            "env": {"FOO": "bar"},
            "metadata": {"team": "gc"},
            "resourceLimits": {"cpu": "1", "memory": "2Gi"},
            "networkPolicy": {"defaultAction": "deny"},
            "credentialProxy": {"enabled": true},
            "snapshotId": "snap-1",
            "templateId": "tpl-1"
        });
        let req: CreateSandboxRequest = serde_json::from_value(raw).expect("parse");
        assert_eq!(
            req.image.as_ref().unwrap().uri,
            "opensandbox/code-interpreter:v1.1.0"
        );
        assert_eq!(
            req.entrypoint.as_deref(),
            Some(vec!["sleep".to_string(), "infinity".to_string()].as_slice())
        );
        assert_eq!(req.timeout, Some(1800));
        assert_eq!(
            req.resource_limits.as_ref().unwrap().memory.as_deref(),
            Some("2Gi")
        );
        assert!(req.platform.is_none());
    }

    #[test]
    fn sandbox_serializes_camel_case_and_skips_none() {
        let sb = Sandbox {
            id: "abc".into(),
            status: SandboxStatus {
                state: "Running".into(),
                reason: None,
                message: None,
            },
            metadata: None,
            expires_at: None,
            created_at: chrono::Utc::now(),
            entrypoint: Some(vec!["sleep".into()]),
        };
        let v = serde_json::to_value(&sb).unwrap();
        assert!(v.get("createdAt").is_some());
        assert!(v.get("expiresAt").is_none());
        assert!(v.get("entrypoint").is_some());
    }

    #[test]
    fn endpoint_uses_field_name_endpoint() {
        let e = Endpoint {
            endpoint: "localhost:45678".into(),
            headers: None,
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["endpoint"], "localhost:45678");
        assert!(v.get("headers").is_none());
    }

    #[test]
    fn error_envelope_matches_reference_shape() {
        let env = ErrorEnvelope::new(error_codes::SANDBOX_NOT_FOUND, "nope");
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["detail"]["code"], "SANDBOX::NOT_FOUND");
    }
}
