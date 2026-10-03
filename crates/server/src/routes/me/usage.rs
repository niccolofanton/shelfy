//! `GET /api/v1/me/usage` (plan §2.13 Quota): the storage the account uses,
//! media plus database, against its quota.
//!
//! The media bytes move as objects are stored and deleted ([`crate::quota`]:
//! a store's commit shows here at once); the `usage.recompute` job
//! ([`crate::jobs::usage`]) counts everything again, the database file
//! included, nightly and after an install or a purge. `updatedAt` is the
//! time of that last count: until the first one it is `null`, and this
//! request starts one; the client reads again when its `job.updated`
//! reports it done.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::control::users;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::jobs::usage;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_usage))
}

/// The account's storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    /// Media plus database, in bytes.
    pub used_bytes: i64,
    /// The stored media (masters and kept videos), in bytes.
    pub media_bytes: i64,
    /// The library database, in bytes.
    pub db_bytes: i64,
    /// The quota in bytes; 0 means unlimited (the owner).
    pub quota_bytes: i64,
    /// When the library was last counted, unix ms; `null` until the first
    /// count. Stores since then are in the media bytes already.
    #[schema(required = true)]
    pub updated_at: Option<i64>,
}

impl From<users::Usage> for Usage {
    fn from(usage: users::Usage) -> Self {
        Self {
            used_bytes: usage.used_bytes,
            media_bytes: usage.media_bytes,
            db_bytes: usage.db_bytes,
            quota_bytes: usage.quota_bytes,
            updated_at: usage.updated_at,
        }
    }
}

/// The storage the account uses and its quota: the last count, plus what
/// was stored or deleted since. When it was never counted, the count starts
/// now (`usage.recompute`).
#[utoipa::path(
    get,
    path = "/api/v1/me/usage",
    tag = "account",
    operation_id = "getUsage",
    responses(
        (status = OK, description = "The storage used and the quota.", body = Usage),
    )
)]
pub async fn get_usage(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let control = Arc::clone(state.control());
    let id = user.id().to_owned();
    let found = blocking(move || control.read(|conn| users::usage(conn, &id)))
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;
    if found.updated_at.is_none()
        && let Err(err) = usage::enqueue(state.jobs(), user.id()).await
    {
        tracing::warn!(error = %err, "cannot enqueue the first usage count");
    }
    Ok(no_store(Json(Usage::from(found)).into_response()))
}
