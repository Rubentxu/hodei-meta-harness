//! HTTP surface: axum routes for the implemented lifecycle contract subset,
//! with an honest `501` for every documented-but-unimplemented endpoint.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::auth::require_control_token;
use crate::model::{CreateSandboxRequest, ErrorEnvelope, ListSandboxesResponse, error_codes};
use crate::service::{ServiceError, SharedService};

/// The routes that touch an environment are authorized; the liveness
/// routes are not, because they reveal nothing about any environment.
pub fn router(service: SharedService) -> Router {
    let authorized = Router::new()
        .route("/v1/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route(
            "/v1/sandboxes/{id}",
            get(get_sandbox).delete(delete_sandbox),
        )
        .route("/v1/sandboxes/{id}/endpoints/{port}", get(get_endpoint))
        .layer(axum::middleware::from_fn_with_state(
            service.clone(),
            require_control_token,
        ));
    Router::new()
        .merge(authorized)
        .route("/ping", get(ping))
        .route("/ready", get(ready))
        .fallback(unimplemented_contract)
        .with_state(service)
}

fn error_response(err: &ServiceError) -> Response {
    let status = match err.code() {
        error_codes::INVALID_PARAMETER => StatusCode::BAD_REQUEST,
        error_codes::SANDBOX_NOT_FOUND => StatusCode::NOT_FOUND,
        error_codes::BUDGET_EXHAUSTED => StatusCode::TOO_MANY_REQUESTS,
        error_codes::EXECD_START_FAILED | error_codes::EXECD_DISTRIBUTION_FAILED => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    let envelope = ErrorEnvelope::new(err.code(), err.to_string());
    (status, Json(envelope)).into_response()
}

async fn create_sandbox(
    State(service): State<SharedService>,
    Json(req): Json<CreateSandboxRequest>,
) -> Response {
    match service.create_sandbox(req).await {
        Ok(created) => (StatusCode::OK, Json(created)).into_response(),
        Err(err) => error_response(&err),
    }
}

async fn list_sandboxes(State(service): State<SharedService>) -> Response {
    match service.list_sandboxes().await {
        Ok(items) => {
            let total = i64::try_from(items.len()).unwrap_or(i64::MAX);
            let response = ListSandboxesResponse {
                items,
                total_count: total,
            };
            (StatusCode::OK, Json(response)).into_response()
        }
        Err(err) => error_response(&err),
    }
}

async fn get_sandbox(State(service): State<SharedService>, Path(id): Path<String>) -> Response {
    match service.get_sandbox(&id).await {
        Ok(sandbox) => (StatusCode::OK, Json(sandbox)).into_response(),
        Err(err) => error_response(&err),
    }
}

async fn delete_sandbox(State(service): State<SharedService>, Path(id): Path<String>) -> Response {
    match service.delete_sandbox(&id).await {
        Ok(()) => StatusCode::OK.into_response(),
        Err(err) => error_response(&err),
    }
}

async fn get_endpoint(
    State(service): State<SharedService>,
    Path((id, port)): Path<(String, i32)>,
) -> Response {
    match service.resolve_endpoint(&id, port).await {
        Ok(endpoint) => (StatusCode::OK, Json(endpoint)).into_response(),
        Err(err) => error_response(&err),
    }
}

async fn ping() -> &'static str {
    "OK"
}

async fn ready() -> &'static str {
    "ready"
}

/// Documented contract endpoints this server deliberately does not implement
/// (templates, snapshots, pause/resume, network policy, credential vault...).
async fn unimplemented_contract() -> Response {
    let body = json!({
        "detail": {
            "code": error_codes::NOT_IMPLEMENTED,
            "message": "this lifecycle endpoint is documented by the OpenSandbox contract but not implemented by hodei-mh-lifecycle-server; the implemented subset is: create/list/get/delete sandbox, endpoint resolution, ping/ready"
        }
    });
    (StatusCode::NOT_IMPLEMENTED, Json(body)).into_response()
}

/// Query type kept explicit so the route signature documents what we accept.
pub type EmptyQuery = Query<()>;

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt as _;

    use std::sync::Arc;

    use crate::auth::ControlToken;
    use crate::docker::DockerAdapter;
    use crate::service::{LifecycleService, ServiceConfig};

    /// The credential the authorized tests present.
    const TOKEN: &str = "control-token-of-the-tests";

    /// A service that CANNOT talk to Docker: bollard accepts a socket path
    /// that exists but is not a socket, so the connection is refused at once
    /// and no daemon is ever reached. The failure paths of the HTTP surface
    /// are therefore observable offline: no daemon, no image pull, no
    /// container, no sleep — and a route that reaches the adapter by mistake
    /// fails fast instead of touching the host.
    fn service() -> SharedService {
        let path = std::env::temp_dir().join("hodei-mh-offline-docker.sock");
        std::fs::write(&path, b"this file is not a socket").expect("the placeholder socket");
        let docker = bollard::Docker::connect_with_socket(
            path.to_str().expect("socket path"),
            1,
            bollard::API_DEFAULT_VERSION,
        )
        .expect("the client is built without dialing");
        let mut config = ServiceConfig::default();
        config.trust.control_token = ControlToken::new(TOKEN.to_string()).expect("a token");
        Arc::new(LifecycleService::new(
            DockerAdapter::connect(docker, "execd:local"),
            config,
        ))
    }

    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
    ) -> (StatusCode, String) {
        call_authorized(app, method, uri, body, false).await
    }

    /// The same call, with the control credential when `authorized`.
    async fn call_authorized(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<&str>,
        authorized: bool,
    ) -> (StatusCode, String) {
        let mut request = Request::builder()
            .method(method)
            .uri(uri)
            .header(axum::http::header::CONTENT_TYPE, "application/json");
        if authorized {
            request = request.header(axum::http::header::AUTHORIZATION, format!("Bearer {TOKEN}"));
        }
        let response = app
            .clone()
            .oneshot(
                request
                    .body(match body {
                        Some(body) => Body::from(body.to_string()),
                        None => Body::empty(),
                    })
                    .expect("request"),
            )
            .await
            .expect("the router answers");
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("body");
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn ping_and_ready_answer_without_touching_docker() {
        let app = router(service());
        let (status, body) = call(&app, "GET", "/ping", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "OK");

        let (status, body) = call(&app, "GET", "/ready", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, "ready");
    }

    #[tokio::test]
    async fn a_documented_endpoint_this_server_does_not_implement_answers_501() {
        let app = router(service());
        let (status, body) = call(&app, "GET", "/v1/sandboxes/abc/templates", None).await;
        assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
        assert!(
            body.contains(error_codes::NOT_IMPLEMENTED),
            "the refusal names the code: {body}"
        );
    }

    #[tokio::test]
    async fn a_create_without_an_image_is_refused_with_400() {
        let app = router(service());
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(r#"{"entrypoint":["/bin/sh"]}"#),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(error_codes::INVALID_PARAMETER), "{body}");
    }

    #[tokio::test]
    async fn a_create_without_an_entrypoint_is_refused_with_400() {
        let app = router(service());
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(r#"{"image":{"uri":"rust:1-bookworm"}}"#),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(error_codes::INVALID_PARAMETER), "{body}");
    }

    #[tokio::test]
    async fn a_negative_timeout_is_refused_with_400() {
        let app = router(service());
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(r#"{"image":{"uri":"rust:1-bookworm"},"entrypoint":["/bin/sh"],"timeout":-1}"#),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(error_codes::INVALID_PARAMETER), "{body}");
    }

    #[tokio::test]
    async fn a_metadata_key_that_collides_with_the_reserved_prefix_is_refused_with_400() {
        let app = router(service());
        let intruder = format!("{}intruder", crate::compose::RESERVED_LABEL_PREFIX);
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(&format!(
                r#"{{"image":{{"uri":"rust:1-bookworm"}},"entrypoint":["/bin/sh"],
                    "metadata":{{"{intruder}":"x"}}}}"#
            )),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(error_codes::INVALID_PARAMETER), "{body}");
    }

    #[tokio::test]
    async fn a_create_that_passes_validation_fails_against_the_absent_daemon() {
        // The other tests prove the refusals happen BEFORE the adapter is
        // touched. This one proves the opposite side of the same edge: a
        // request that passes validation reaches the adapter and fails
        // there, against a client that cannot reach any daemon.
        let app = router(service());
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(r#"{"image":{"uri":"rust:1-bookworm"},"entrypoint":["/bin/sh"]}"#),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(
            !body.contains(error_codes::INVALID_PARAMETER),
            "the refusal comes from the daemon, not from the validation: {body}"
        );
    }

    #[tokio::test]
    async fn an_unauthorized_request_never_reaches_an_environment() {
        // The refusal happens BEFORE the handler: no environment is
        // created, enumerated, executed nor destroyed. The daemon is
        // unreachable in this test, so a request that got through would
        // fail with 500 instead of 401 — the status says which happened.
        let app = router(service());
        for (method, uri) in [
            ("POST", "/v1/sandboxes"),
            ("GET", "/v1/sandboxes"),
            ("GET", "/v1/sandboxes/some-id"),
            ("DELETE", "/v1/sandboxes/some-id"),
            ("GET", "/v1/sandboxes/some-id/endpoints/44772"),
        ] {
            let (status, body) = call(
                &app,
                method,
                uri,
                Some(r#"{"image":{"uri":"rust:1-bookworm"},"entrypoint":["/bin/sh"]}"#),
            )
            .await;
            assert_eq!(
                status,
                StatusCode::UNAUTHORIZED,
                "{method} {uri} must be refused"
            );
            assert!(body.contains(error_codes::UNAUTHORIZED), "{body}");
        }
    }

    #[tokio::test]
    async fn the_authorized_request_gets_through_the_door() {
        // The same requests, with the credential: they reach the
        // handler and its own refusals, which are not the refusal of
        // the authorization.
        let app = router(service());
        let (status, body) = call_authorized(
            &app,
            "POST",
            "/v1/sandboxes",
            Some(r#"{"entrypoint":["/bin/sh"]}"#),
            true,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains(error_codes::INVALID_PARAMETER), "{body}");
    }

    #[test]
    fn error_response_maps_codes_to_statuses() {
        let not_found = ServiceError::Contract {
            code: error_codes::SANDBOX_NOT_FOUND,
            message: "gone".into(),
        };
        let resp = error_response(&not_found);
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);

        let invalid = ServiceError::Contract {
            code: error_codes::INVALID_PARAMETER,
            message: "bad".into(),
        };
        assert_eq!(error_response(&invalid).status(), StatusCode::BAD_REQUEST);

        let execd = ServiceError::Contract {
            code: error_codes::EXECD_START_FAILED,
            message: "no execd".into(),
        };
        assert_eq!(
            error_response(&execd).status(),
            StatusCode::SERVICE_UNAVAILABLE
        );

        let internal = ServiceError::Contract {
            code: error_codes::INTERNAL,
            message: "boom".into(),
        };
        assert_eq!(
            error_response(&internal).status(),
            StatusCode::INTERNAL_SERVER_ERROR
        );
    }
}
