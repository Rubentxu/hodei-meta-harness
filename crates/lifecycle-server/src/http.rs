//! HTTP surface: axum routes for the implemented lifecycle contract subset,
//! with an honest `501` for every documented-but-unimplemented endpoint.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;

use crate::model::{CreateSandboxRequest, ErrorEnvelope, ListSandboxesResponse, error_codes};
use crate::service::{LifecycleService, ServiceError};

pub type SharedService = Arc<LifecycleService>;

pub fn router(service: SharedService) -> Router {
    Router::new()
        .route("/v1/sandboxes", post(create_sandbox).get(list_sandboxes))
        .route(
            "/v1/sandboxes/{id}",
            get(get_sandbox).delete(delete_sandbox),
        )
        .route("/v1/sandboxes/{id}/endpoints/{port}", get(get_endpoint))
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
    headers: HeaderMap,
) -> Response {
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok());
    match service.resolve_endpoint(&id, port, host).await {
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
