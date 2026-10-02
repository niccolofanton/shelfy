//! `GET /api/v1/notifications` and `POST /api/v1/notifications/read`: the
//! user's notifications, newest first, and marking them read (plan §2.9
//! Platform). New ones also arrive live as `notification` events
//! ([`crate::events`]); the server creates them with
//! [`crate::events::notify`].
//!
//! The list is conditional ([`crate::conditional`]). Its cursor is opaque, like
//! the post cursors, and tied to this route.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::{self, ReadSelector};
use utoipa::{IntoParams, ToSchema};

use super::listing::page_size;
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::Notification;
use crate::extract::{Json, Query};
use crate::ids::now_ms;
use crate::state::{AppState, blocking};

/// Most ids one `POST /notifications/read` takes.
pub const MAX_READ_IDS: usize = 200;

/// Prefix of the decoded cursor text, so a cursor of another route never
/// parses here.
const CURSOR_TAG: &str = "notifications.";
/// Longest cursor accepted, in characters.
const MAX_CURSOR_CHARS: usize = 64;

/// Paging of `GET /api/v1/notifications`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct NotificationsQuery {
    /// Page size, 1–200 (larger values are clamped). Default 60.
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<String>,
}

/// One page of notifications, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NotificationPage {
    /// The notifications of this page.
    pub items: Vec<Notification>,
    /// Pass it as `cursor` to get the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Unread notifications, over all pages.
    pub unread_count: u64,
}

/// Which notifications to mark read: exactly one of `ids` and `upTo`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct MarkReadRequest {
    /// These notifications, at most 200. Ids that do not exist are skipped.
    #[schema(nullable = false)]
    pub ids: Option<Vec<i64>>,
    /// Every notification up to this id, included: "mark all read" with the
    /// newest id shown, which leaves alone any that arrived since.
    #[schema(nullable = false)]
    pub up_to: Option<i64>,
}

/// The outcome of `POST /notifications/read`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MarkReadResult {
    /// Notifications that went from unread to read.
    pub updated: u64,
    /// Unread notifications left.
    pub unread_count: u64,
}

/// The user's notifications, newest first, with the unread count.
///
/// The response is conditional: send the `ETag` back in `If-None-Match` and
/// an unchanged page answers 304.
#[utoipa::path(
    get,
    path = "/api/v1/notifications",
    tag = "platform",
    operation_id = "listNotifications",
    params(NotificationsQuery, ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "One page of notifications.",
            body = NotificationPage,
            headers(
                ("ETag" = String, description = "Weak ETag of this page of this library state."),
                ("Cache-Control" = String, description = "`private, no-cache`."),
            )
        ),
        (
            status = NOT_MODIFIED,
            description = "The page is unchanged since the ETag in `If-None-Match`; no body.",
            headers(("ETag" = String, description = "The same ETag."))
        ),
    )
)]
pub async fn list_notifications(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<NotificationsQuery>,
) -> Result<Response, ApiError> {
    let limit = page_size(query.limit);
    let before = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view(
        "notifications.list",
        user.id(),
        db.generation(),
        &(limit, &query.cursor),
    );
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let (page, unread) = blocking(move || {
        db.read(|conn| {
            let page = notifications::list(conn, before, limit)?;
            let unread = notifications::unread_count(conn)?;
            Ok::<_, RepoError>((page, unread))
        })
    })
    .await?;
    let body = NotificationPage {
        items: page.items.into_iter().map(Notification::from).collect(),
        next_cursor: page.next_before.map(encode_cursor),
        unread_count: unread,
    };
    Ok(etag.respond(Json(body)))
}

/// Marks notifications read.
///
/// Give `ids`, or `upTo` to mark everything up to the newest notification
/// shown. Repeating a call changes nothing.
#[utoipa::path(
    post,
    path = "/api/v1/notifications/read",
    tag = "platform",
    operation_id = "markNotificationsRead",
    request_body = MarkReadRequest,
    responses(
        (status = OK, description = "How many changed, and what is left unread.", body = MarkReadResult),
    )
)]
pub async fn mark_notifications_read(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<MarkReadRequest>,
) -> Result<Json<MarkReadResult>, ApiError> {
    let selector = match (request.ids, request.up_to) {
        (Some(ids), None) if ids.len() > MAX_READ_IDS => {
            return Err(ApiError::invalid_field("ids", "more than 200 ids"));
        }
        (Some(ids), None) => ReadSelector::Ids(ids),
        (None, Some(up_to)) => ReadSelector::UpTo(up_to),
        _ => return Err(ApiError::invalid_field("ids", "give either ids or upTo")),
    };
    let db = state.user_db(user.id()).await?;
    let result = blocking(move || {
        db.write(|tx| {
            let updated = notifications::mark_read(tx, &selector, now_ms())?;
            let unread_count = notifications::unread_count(tx)?;
            Ok::<_, RepoError>(MarkReadResult {
                updated,
                unread_count,
            })
        })
    })
    .await?;
    Ok(Json(result))
}

fn encode_cursor(before: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{CURSOR_TAG}{before}"))
}

fn decode_cursor(text: &str) -> Result<i64, ApiError> {
    let invalid = || ApiError::new(ErrorCode::InvalidCursor);
    if text.len() > MAX_CURSOR_CHARS {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    text.strip_prefix(CURSOR_TAG)
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_anything_else() {
        let text = encode_cursor(42);
        assert_eq!(decode_cursor(&text).unwrap(), 42);
        for bad in [
            String::new(),
            "not base64!".to_owned(),
            URL_SAFE_NO_PAD.encode("posts.42"),
            URL_SAFE_NO_PAD.encode("notifications.x"),
            URL_SAFE_NO_PAD.encode("notifications.-1"),
            URL_SAFE_NO_PAD.encode([0xff, 0xfe]),
            "A".repeat(MAX_CURSOR_CHARS + 1),
        ] {
            let err = decode_cursor(&bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidCursor, "{bad}");
        }
    }
}
