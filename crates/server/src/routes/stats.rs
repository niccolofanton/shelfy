//! `GET /api/v1/stats`: the library counters (plan §2.9; desktop `getStats`,
//! with every platform counted, `manual` included, and the trash apart).

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use shelfy_core::repo::stats;

use super::model::Stats;
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::Json;
use crate::state::{AppState, blocking};

/// The library counters.
#[utoipa::path(
    get,
    path = "/api/v1/stats",
    tag = "library",
    operation_id = "getStats",
    params(ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "The counters.",
            body = Stats,
            headers(
                ("ETag" = String, description = "Weak ETag of this library state."),
                ("Cache-Control" = String, description = "`private, no-cache`."),
            )
        ),
        (
            status = NOT_MODIFIED,
            description = "Unchanged since the ETag in `If-None-Match`; no body.",
            headers(("ETag" = String, description = "The same ETag."))
        ),
    )
)]
pub async fn get_stats(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view("stats", user.id(), db.generation(), &());
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let counters = blocking(move || db.read(stats::get)).await?;
    Ok(etag.respond(Json(Stats::from(counters))))
}
