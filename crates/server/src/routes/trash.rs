//! `/api/v1/trash`: the posts taken out of the library, kept 30 days (plan
//! §2.9 Library, §2.13; P1-11). The model is in [`shelfy_core::trash`].
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /trash?limit&cursor` | the trashed posts, most recently trashed first, with `total` and `retentionDays` (conditional) |
//! | `POST /trash/restore` `{selector}` or `{deletedAt}` (`Idempotency-Key`) | the posts back, with their folders and search text: 200 inline, or 202 and a `bulk` job past 500 posts |
//! | `POST /trash/empty` (`Idempotency-Key`) | 202 and the `purge` job of exactly the posts in the trash at the request |
//!
//! Posts go to the trash with `POST /posts/bulk` (`delete`) and with
//! `DELETE /collections/{id}?mode=withPosts`; both answer the `deletedAt`
//! every post of that delete got, which `POST /trash/restore {deletedAt}`
//! takes to undo it. The nightly purge deletes what has been in the trash for
//! 30 days ([`crate::jobs::purge`]).
//!
//! Every route needs a session; another user's posts are unknown keys.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use shelfy_core::bulk::Action;
use shelfy_core::repo::RepoError;
use shelfy_core::selector::Selector;
use shelfy_core::trash::{self, Position, RETENTION_DAYS};
use utoipa::{IntoParams, ToSchema};

use super::bulk::{self, BulkAction, BulkResult, Plan};
use super::jobs::Job;
use super::listing::page_size;
use super::model::Post;
use super::selector::PostSelector;
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Query};
use crate::jobs::bulk::Selection;
use crate::jobs::idempotency::IdempotencyHeader;
use crate::jobs::purge;
use crate::state::{AppState, blocking};

/// Prefix of a decoded cursor, so a cursor of another route never parses
/// here.
const CURSOR_TAG: &str = "trash.";
/// Longest cursor accepted, in characters.
const MAX_CURSOR_CHARS: usize = 96;

/// Paging of `GET /api/v1/trash`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TrashQuery {
    /// Page size, 1–200 (larger values are clamped). Default 60.
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<String>,
}

/// One page of the trash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrashPage {
    /// The trashed posts, most recently trashed first; each has its
    /// `deletedAt`.
    pub items: Vec<Post>,
    /// Pass it as `cursor` to get the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Posts in the trash.
    pub total: u64,
    /// Days a post stays in the trash: the nightly purge deletes it after
    /// `deletedAt` plus this many days.
    pub retention_days: u32,
}

/// The trashed posts, most recently trashed first. Paging is by keyset: a
/// post trashed or restored between two pages shifts no other. The response
/// is conditional: send the `ETag` back in `If-None-Match` and an unchanged
/// page answers 304.
#[utoipa::path(
    get,
    path = "/api/v1/trash",
    tag = "library",
    operation_id = "listTrash",
    params(TrashQuery, ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "One page of the trash.",
            body = TrashPage,
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
pub async fn list_trash(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<TrashQuery>,
) -> Result<Response, ApiError> {
    let limit = page_size(query.limit);
    let after = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let db = state.user_db(user.id()).await?;
    // Read before the snapshot (`crate::conditional`).
    let etag = ETag::for_view(
        "trash.list",
        user.id(),
        db.generation(),
        &(limit, &query.cursor),
    );
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let (page, total) = blocking(move || {
        db.read(|conn| {
            let page = trash::page(conn, limit, after)?;
            let total = trash::count(conn)?;
            Ok::<_, RepoError>((page, total))
        })
    })
    .await?;
    let body = TrashPage {
        items: page.items.into_iter().map(Post::from).collect(),
        next_cursor: page.next.map(encode_cursor),
        total,
        retention_days: RETENTION_DAYS,
    };
    Ok(etag.respond(Json(body)))
}

/// The posts to bring back from the trash: exactly one of `selector` and
/// `deletedAt`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct RestoreRequest {
    /// Which posts: keys (at most 500), or a filter over the trash, which
    /// must say `trash: true` (the filter of `GET /posts?trash=1`), minus
    /// some keys. Posts outside the trash are left alone.
    #[schema(nullable = false)]
    pub selector: Option<PostSelector>,
    /// The posts one delete moved to the trash: the `deletedAt` its answer
    /// gave (`POST /posts/bulk` `delete`, `DELETE /collections/{id}` with
    /// posts). This is the undo of that delete.
    #[schema(nullable = false)]
    pub deleted_at: Option<i64>,
}

/// Brings posts back from the trash, with their folders and search text as
/// they were: some by key, all those a filter selects in the trash, or all
/// those of one delete (its `deletedAt`, the undo). Up to 500 posts it runs
/// in the request and answers 200; a larger selection answers 202 with the
/// `bulk` job that runs it, reported by `job.updated`. Send an
/// `Idempotency-Key` so that a repeat acts once.
#[utoipa::path(
    post,
    path = "/api/v1/trash/restore",
    tag = "library",
    operation_id = "restoreTrash",
    params(IdempotencyHeader),
    request_body = RestoreRequest,
    responses(
        (
            status = OK,
            description = "The posts are back; `changed` counts them. For a repeated `Idempotency-Key`, the first answer.",
            body = BulkResult,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
        (
            status = ACCEPTED,
            description = "Over 500 posts: the `bulk` job that brings them back.",
            body = BulkResult,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
    )
)]
pub async fn restore_trash(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<RestoreRequest>,
) -> Result<Response, ApiError> {
    let (selection, selector) = match (request.selector, request.deleted_at) {
        (Some(wire), None) => {
            let selector = wire.clone().resolve("selector")?;
            if let Selector::Filter { filter, .. } = &selector
                && !filter.trash
            {
                return Err(ApiError::invalid_field(
                    "selector.filter.trash",
                    "must be true: a restore selects in the trash",
                ));
            }
            (Selection::selector(wire), selector)
        }
        (None, Some(at)) => (Selection::DeletedAt(at), Selector::TrashedAt(at)),
        _ => {
            return Err(ApiError::invalid_field(
                "selector",
                "takes `selector` or `deletedAt`, exactly one",
            ));
        }
    };
    let plan = Plan {
        action: BulkAction::Restore,
        params: None,
        core: Action::Restore,
        selection,
        selector,
    };
    bulk::start(&state, user.id(), plan).await
}

/// The purge that empties the trash.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TrashEmptying {
    /// Posts in the trash when the request came: what the job deletes (less
    /// those restored before it gets to them).
    pub selected: u64,
    /// The `purge` job: its progress arrives as `job.updated`.
    pub job: Job,
}

/// Empties the trash: starts the `purge` job that deletes for good every
/// post in the trash at this request, with their tags, folder memberships
/// and search rows. Posts that enter the trash later stay, even those of a
/// bulk delete asked before; posts restored before the job gets to them
/// stay too. The job runs after the user's earlier bulk jobs. Their media
/// are released for the storage cleanup. Answers 202 at once; the job's
/// progress arrives as `job.updated`. Send an `Idempotency-Key` so that a
/// repeat starts one job.
#[utoipa::path(
    post,
    path = "/api/v1/trash/empty",
    tag = "library",
    operation_id = "emptyTrash",
    params(IdempotencyHeader),
    responses(
        (
            status = ACCEPTED,
            description = "The purge is queued. For a repeated `Idempotency-Key`, the first answer.",
            body = TrashEmptying,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
    )
)]
pub async fn empty_trash(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let now = state.jobs().clock().now_ms();
    let db = state.user_db(user.id()).await?;
    // In a write transaction, so that no move to the trash is halfway: the
    // cut covers every post in the trash, and none that comes later
    // (`trash::emptying`, P1-11 review H1).
    let emptying = blocking(move || db.write(|tx| trash::emptying(tx, now))).await?;
    let enqueued = purge::enqueue(state.jobs(), user.id(), emptying.through).await?;
    let body = TrashEmptying {
        selected: emptying.posts,
        job: enqueued.job.into(),
    };
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

fn encode_cursor(position: Position) -> String {
    URL_SAFE_NO_PAD.encode(format!(
        "{CURSOR_TAG}{}.{}",
        position.deleted_at, position.id
    ))
}

fn decode_cursor(text: &str) -> Result<Position, ApiError> {
    let invalid = || ApiError::new(ErrorCode::InvalidCursor);
    if text.len() > MAX_CURSOR_CHARS {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (deleted_at, id) = text
        .strip_prefix(CURSOR_TAG)
        .and_then(|rest| rest.split_once('.'))
        .ok_or_else(invalid)?;
    Ok(Position {
        deleted_at: deleted_at.parse().map_err(|_| invalid())?,
        id: id.parse().map_err(|_| invalid())?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_anything_else() {
        let position = Position {
            deleted_at: 1_790_899_200_000,
            id: 42,
        };
        assert_eq!(decode_cursor(&encode_cursor(position)).unwrap(), position);
        for bad in [
            String::new(),
            "not base64!".to_owned(),
            URL_SAFE_NO_PAD.encode("jobs.42"),
            URL_SAFE_NO_PAD.encode("trash.42"),
            URL_SAFE_NO_PAD.encode("trash.x.1"),
            URL_SAFE_NO_PAD.encode("trash.1.y"),
            "A".repeat(MAX_CURSOR_CHARS + 1),
        ] {
            let err = decode_cursor(&bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidCursor, "{bad}");
        }
    }
}
