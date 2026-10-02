//! `GET /api/v1/collections`: the user's collections with their live counts
//! (plan §2.9 Collections). The writes come with P1-03.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use shelfy_core::repo::collections;

use super::model::{Collection, CollectionList};
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::Json;
use crate::state::{AppState, blocking};

/// Every collection, in manual order, then creation order.
#[utoipa::path(
    get,
    path = "/api/v1/collections",
    tag = "library",
    operation_id = "listCollections",
    params(ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "The collections.",
            body = CollectionList,
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
pub async fn list_collections(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view("collections.list", user.id(), db.generation(), &());
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let rows = blocking(move || db.read(collections::list)).await?;
    let body = CollectionList {
        items: rows.into_iter().map(Collection::from).collect(),
    };
    Ok(etag.respond(Json(body)))
}
