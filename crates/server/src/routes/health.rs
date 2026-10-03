//! `GET /health`: liveness plus a database check (plan §3.6). The blackbox
//! probe, the compose healthcheck and `just check` read the status code.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use shelfy_core::db::DbError;
use utoipa::ToSchema;

use crate::extract::Json;
use crate::state::{AppState, blocking};

/// How long the database check may take before it counts as failed.
const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(2);

/// Result of one check.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    /// The check passed.
    Ok,
    /// The check failed.
    Fail,
}

/// Health of the process.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    /// `ok` when every check passed.
    pub status: CheckStatus,
    /// Version of the server build.
    pub version: String,
    /// The individual checks.
    pub checks: HealthChecks,
}

/// The checks behind the overall `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct HealthChecks {
    /// The control database answers a query within 2 s.
    pub control_db: CheckStatus,
}

/// Liveness and readiness of the server.
#[utoipa::path(
    get,
    path = "/health",
    tag = "platform",
    operation_id = "getHealth",
    security(()),
    responses(
        (status = OK, description = "The server is up and its database answers.", body = Health),
        (status = SERVICE_UNAVAILABLE, description = "A check failed.", body = Health),
    )
)]
pub async fn health(State(state): State<AppState>) -> Response {
    let control_db = check_control_db(&state).await;
    let health = Health {
        status: control_db,
        version: crate::VERSION.to_owned(),
        checks: HealthChecks { control_db },
    };
    let status = match health.status {
        CheckStatus::Ok => StatusCode::OK,
        CheckStatus::Fail => StatusCode::SERVICE_UNAVAILABLE,
    };
    let mut response = (status, Json(health)).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn check_control_db(state: &AppState) -> CheckStatus {
    let control = Arc::clone(state.control());
    let query = blocking(move || {
        control.read(|conn| {
            conn.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))
                .map_err(DbError::from)
        })
    });
    match tokio::time::timeout(DB_CHECK_TIMEOUT, query).await {
        Ok(Ok(1)) => CheckStatus::Ok,
        Ok(Ok(other)) => {
            tracing::warn!(value = other, "health: unexpected database answer");
            CheckStatus::Fail
        }
        Ok(Err(err)) => {
            tracing::warn!(error = %err, "health: control database check failed");
            CheckStatus::Fail
        }
        Err(_) => {
            tracing::warn!("health: control database check timed out");
            CheckStatus::Fail
        }
    }
}

/// Public capture readiness, with no service or configuration details.
#[utoipa::path(get,path="/health/capture",tag="platform",operation_id="getCaptureHealth",security(()),responses((status=OK,description="Capture service responds."),(status=SERVICE_UNAVAILABLE,description="Capture service unavailable.")))]
pub async fn capture_health(State(state): State<AppState>) -> Response {
    let status = if crate::capture::configured(&state)
        && crate::capture::client::health(&state).await.is_ok()
    {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    let mut response = status.into_response();
    // A typed empty body prevents the generic bare-error normalizer from
    // attaching a problem document to this status-only probe.
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
