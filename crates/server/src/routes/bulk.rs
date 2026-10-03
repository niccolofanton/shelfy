//! `POST /api/v1/posts/bulk`: one action on many posts (plan §2.9 "Bulk
//! selector"; P1-11). It replaces the desktop's "fetch every id, send it
//! back" round trip (DATA-17, UI-49).
//!
//! The body is `{selector, action, params}`. The selector is a
//! [`PostSelector`]: `{keys}` (at most 500, in the trash or not) or
//! `{filter, exceptKeys}` ("select all matching" minus the posts unticked;
//! the trash only with `trash: true`).
//!
//! | `action` | `params` | Effect ([`shelfy_core::bulk`]) | Event reason |
//! |---|---|---|---|
//! | `delete` | — | to the trash, every post stamped with the delete's own `deletedAt` | `delete` |
//! | `restore` | — | back from the trash, with their folders and search text | `delete` |
//! | `addToCollections` | `collectionIds` (1–50) | posts outside the trash join each collection | `edit` |
//! | `removeFromCollection` | `collectionId` | members leave the collection | `edit` |
//! | `clearAiDescription` | — | AI description and status cleared | `ai` |
//! | `clearAiTags` | — | AI tags and status cleared; manual tags stay | `ai` |
//! | `analyze`, `fetchMedia`, `removeStoredMedia` | — | 422 `not_available` until P2–P4 | — |
//!
//! **Inline or job.** `{keys}`, and a filter that selects at most 500 posts,
//! run in the request: 200 with what changed, announced as `posts.changed`
//! and `stats.changed` like every write ([`crate::library`]). A larger
//! selection becomes a `bulk` job ([`crate::jobs::bulk`]): 202 with the job,
//! whose progress arrives as `job.updated` (`stage` is the action) and
//! whose chunks are announced as they commit. Send an `Idempotency-Key`: a
//! repeat gets the first answer back instead of acting twice.
//!
//! **Undo.** A delete answers `deletedAt`, the stamp of every post it moves
//! (`null` when an inline delete moved nothing). The stamp is the delete's
//! time, made unique in the library ([`shelfy_core::trash::new_stamp`]; a
//! job's is reserved at the request), so `POST /trash/restore {deletedAt}`
//! ([`super::trash`]) brings back exactly those posts and no other delete's.
//!
//! Another user's keys are unknown keys (skipped), and another user's
//! collection is a 404, like a missing one.

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use shelfy_core::bulk::{self, Action, MAX_INLINE};
use shelfy_core::repo::{RepoError, posts};
use shelfy_core::selector::Selector;
use shelfy_core::trash;
use utoipa::ToSchema;

use super::jobs::Job;
use super::selector::PostSelector;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::events::MAX_EVENT_KEYS;
use crate::events::model::ChangeReason;
use crate::extract::Json;
use crate::jobs::bulk::{self as job, Payload, Selection};
use crate::jobs::idempotency::IdempotencyHeader;
use crate::library::{self, Change};
use crate::state::{AppState, blocking};

/// What a bulk action does to the selected posts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum BulkAction {
    /// Move them to the trash, all stamped with one `deletedAt`.
    Delete,
    /// Bring them back from the trash.
    Restore,
    /// Add them to collections (`params.collectionIds`); trashed posts are
    /// skipped.
    AddToCollections,
    /// Take them out of a collection (`params.collectionId`).
    RemoveFromCollection,
    /// Clear their AI description: they count as not analyzed again.
    ClearAiDescription,
    /// Clear their AI tags (manual tags stay): they count as not analyzed
    /// again.
    ClearAiTags,
    /// Analyze them with AI. Not available yet (P3).
    Analyze,
    /// Store their media on the server. Not available yet (P2, P4).
    FetchMedia,
    /// Delete their stored media. Not available yet (P4).
    RemoveStoredMedia,
}

impl BulkAction {
    /// The wire form, also the `stage` of the action's job.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Restore => "restore",
            Self::AddToCollections => "addToCollections",
            Self::RemoveFromCollection => "removeFromCollection",
            Self::ClearAiDescription => "clearAiDescription",
            Self::ClearAiTags => "clearAiTags",
            Self::Analyze => "analyze",
            Self::FetchMedia => "fetchMedia",
            Self::RemoveStoredMedia => "removeStoredMedia",
        }
    }

    /// What `posts.changed` gives as the reason of the action's changes.
    #[must_use]
    pub const fn reason(self) -> ChangeReason {
        match self {
            Self::Delete | Self::Restore => ChangeReason::Delete,
            Self::ClearAiDescription | Self::ClearAiTags | Self::Analyze => ChangeReason::Ai,
            Self::FetchMedia | Self::RemoveStoredMedia => ChangeReason::Archive,
            Self::AddToCollections | Self::RemoveFromCollection => ChangeReason::Edit,
        }
    }

    /// The core action of `self` with `params`.
    ///
    /// # Errors
    ///
    /// 422 `not_available` for an action of a later phase; 422
    /// `validation_failed` naming `params.<member>` for missing, extra or
    /// invalid parameters.
    pub fn resolve(self, params: Option<&BulkParams>) -> Result<Action, ApiError> {
        let none = BulkParams::default();
        let params = params.unwrap_or(&none);
        let unexpected = |member: &str| {
            ApiError::invalid_field(
                format!("params.{member}"),
                format!("is not a parameter of `{}`", self.as_str()),
            )
        };
        let action = match self {
            Self::Analyze | Self::FetchMedia | Self::RemoveStoredMedia => {
                return Err(ApiError::new(ErrorCode::NotAvailable).with_detail(format!(
                    "`{}` is not available on this server yet",
                    self.as_str()
                )));
            }
            Self::AddToCollections => {
                if params.collection_id.is_some() {
                    return Err(unexpected("collectionId"));
                }
                let ids = params.collection_ids.clone().ok_or_else(|| {
                    ApiError::invalid_field("params.collectionIds", "is required")
                })?;
                Action::AddToCollections(ids)
            }
            Self::RemoveFromCollection => {
                if params.collection_ids.is_some() {
                    return Err(unexpected("collectionIds"));
                }
                let id = params
                    .collection_id
                    .ok_or_else(|| ApiError::invalid_field("params.collectionId", "is required"))?;
                Action::RemoveFromCollection(id)
            }
            Self::Delete | Self::Restore | Self::ClearAiDescription | Self::ClearAiTags => {
                if params.collection_ids.is_some() {
                    return Err(unexpected("collectionIds"));
                }
                if params.collection_id.is_some() {
                    return Err(unexpected("collectionId"));
                }
                match self {
                    Self::Delete => Action::Delete,
                    Self::Restore => Action::Restore,
                    Self::ClearAiDescription => Action::ClearAiDescription,
                    _ => Action::ClearAiTags,
                }
            }
        };
        action.validate().map_err(|err| match err {
            RepoError::Invalid { field, reason } => {
                ApiError::invalid_field(format!("params.{field}"), reason)
            }
            other => other.into(),
        })?;
        Ok(action)
    }
}

/// The arguments of an action; members another action takes are refused.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BulkParams {
    /// `addToCollections`: the collections, by id, 1 to 50.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub collection_ids: Option<Vec<i64>>,
    /// `removeFromCollection`: the collection, by id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub collection_id: Option<i64>,
}

/// One action on many posts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct BulkRequest {
    /// Which posts.
    pub selector: PostSelector,
    /// What to do to them.
    pub action: BulkAction,
    /// The action's arguments, for the actions that take some.
    #[serde(default)]
    #[schema(nullable = false)]
    pub params: Option<BulkParams>,
}

/// The outcome of a bulk action: what it changed (200), or the job that
/// runs it (202).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BulkResult {
    /// The action.
    pub action: BulkAction,
    /// Posts the selector selected when the action started; unknown keys
    /// are not counted.
    pub selected: u64,
    /// Posts the action changed (posts it left as they were, such as a post
    /// already in the trash for `delete`, are not counted); `null` when a
    /// job runs it.
    #[schema(required = true)]
    pub changed: Option<u64>,
    /// `delete` only: the `deletedAt` of every post it moves to the trash,
    /// unique to this delete. `POST /trash/restore {deletedAt}` brings them
    /// back (undo). `null` when the delete ran in the request and moved
    /// nothing.
    #[schema(required = true)]
    pub deleted_at: Option<i64>,
    /// The `bulk` job that runs the action (202): its progress arrives as
    /// `job.updated`. `null` when the action ran in the request (200).
    #[schema(required = true)]
    pub job: Option<Job>,
}

/// Runs one action on many posts: a list of keys, or every post a filter
/// lists (minus some). Up to 500 posts it runs in the request and answers
/// 200 with what changed; a larger selection answers 202 with the `bulk`
/// job that runs it in chunks, reported by `job.updated`. Send an
/// `Idempotency-Key` so that a repeat acts once.
///
/// `analyze`, `fetchMedia` and `removeStoredMedia` answer 422
/// `not_available` until the server can do them. A collection that is not
/// the user's is a 404.
#[utoipa::path(
    post,
    path = "/api/v1/posts/bulk",
    tag = "library",
    operation_id = "bulkPosts",
    security(("session" = []), ("bearer" = ["library:write"])),
    params(IdempotencyHeader),
    request_body = BulkRequest,
    responses(
        (
            status = OK,
            description = "The action ran; what it changed. For a repeated `Idempotency-Key`, the first answer.",
            body = BulkResult,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
        (
            status = ACCEPTED,
            description = "Over 500 posts: the `bulk` job that runs the action.",
            body = BulkResult,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
    )
)]
pub async fn bulk_posts(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<BulkRequest>,
) -> Result<Response, ApiError> {
    let core = request.action.resolve(request.params.as_ref())?;
    let selector = request.selector.clone().resolve("selector")?;
    let plan = Plan {
        action: request.action,
        params: request.params,
        core,
        selection: Selection::selector(request.selector),
        selector,
    };
    start(&state, user.id(), plan).await
}

/// A bulk action ready to run: the request's form, which a job stores, and
/// its resolved core form.
pub(crate) struct Plan {
    /// The action as asked.
    pub action: BulkAction,
    /// Its parameters as given.
    pub params: Option<BulkParams>,
    /// The resolved action.
    pub core: Action,
    /// The posts as asked.
    pub selection: Selection,
    /// The resolved selector.
    pub selector: Selector,
}

/// Runs `plan` for `user_id` in the request when it selects at most
/// [`MAX_INLINE`] posts (`{keys}` always does), and otherwise enqueues the
/// `bulk` job that runs it (module docs).
pub(crate) async fn start(
    state: &AppState,
    user_id: &str,
    plan: Plan,
) -> Result<Response, ApiError> {
    let Plan {
        action,
        params,
        core,
        selection,
        selector,
    } = plan;
    // The job system's clock, which the jobs' chunks read too.
    let now = state.jobs().clock().now_ms();
    let delete = action == BulkAction::Delete;
    // A selection by key fits the inline limit by construction. Otherwise
    // count it, and check the action can run, before anything is enqueued;
    // the largest post id of the same snapshot bounds a job's selection.
    let counted = if matches!(selector, Selector::Keys(_)) {
        None
    } else {
        let db = state.user_db(user_id).await?;
        let (checked, counted) = (core.clone(), selector.clone());
        let n = blocking(move || {
            db.read(|conn| {
                checked.check(conn)?;
                Ok::<_, RepoError>((bulk::count(conn, &counted)?, bulk::newest_id(conn)?))
            })
        })
        .await?;
        Some(n)
    };
    let selected = counted.map(|(n, _)| n);
    if selected.is_none_or(|n| n <= MAX_INLINE) {
        let written = library::write(state, user_id, action.reason(), move |tx| {
            // A delete's stamp is unique in the library (P1-11 review L1).
            let at = if delete {
                trash::new_stamp(tx, now)?
            } else {
                now
            };
            let applied = bulk::apply_stamped(tx, &selector, &core, at, now)?;
            let keys = changed_keys(tx, &applied.changed)?;
            Ok(Change {
                value: (applied, at),
                keys,
            })
        })
        .await?;
        let (applied, at) = written.value;
        let body = BulkResult {
            action,
            selected: applied.selected,
            changed: Some(applied.changed.len() as u64),
            // Only when posts moved (P1-11 review L2).
            deleted_at: (delete && !applied.changed.is_empty()).then_some(at),
            job: None,
        };
        return Ok(Json(body).into_response());
    }
    // A delete's job reserves its stamp now, so that no other delete takes
    // it before the job moves its posts.
    let at = if delete {
        reserve_stamp(state, user_id, now).await?
    } else {
        now
    };
    let max_id = counted.map_or(i64::MAX, |(_, newest)| newest);
    let payload = Payload::new(action, params, selection, at, max_id);
    let enqueued = job::enqueue(state.jobs(), user_id, &payload).await?;
    let body = BulkResult {
        action,
        selected: selected.unwrap_or_default(),
        changed: None,
        deleted_at: delete.then_some(at),
        job: Some(enqueued.job.into()),
    };
    Ok((StatusCode::ACCEPTED, Json(body)).into_response())
}

/// Reserves the stamp of a delete that a job runs later
/// ([`trash::reserve_stamp`]). The write changes no post, so it is not
/// announced.
pub(crate) async fn reserve_stamp(
    state: &AppState,
    user_id: &str,
    now: i64,
) -> Result<i64, ApiError> {
    let db = state.user_db(user_id).await?;
    blocking(move || db.write(|tx| trash::reserve_stamp(tx, now))).await
}

/// The `keys` of the `posts.changed` of a change to the posts `ids`: their
/// keys, or `None` ("any") past the event's cap.
///
/// # Errors
///
/// Database errors.
pub(crate) fn changed_keys(
    conn: &Connection,
    ids: &[i64],
) -> Result<Option<Vec<String>>, RepoError> {
    if ids.len() > MAX_EVENT_KEYS {
        return Ok(None);
    }
    posts::keys_of(conn, ids).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(collection_ids: Option<Vec<i64>>, collection_id: Option<i64>) -> BulkParams {
        BulkParams {
            collection_ids,
            collection_id,
        }
    }

    fn field(err: &ApiError) -> String {
        err.problem().errors[0].field.clone()
    }

    #[test]
    fn actions_take_their_own_parameters() {
        assert_eq!(BulkAction::Delete.resolve(None).unwrap(), Action::Delete);
        assert_eq!(
            BulkAction::ClearAiTags
                .resolve(Some(&BulkParams::default()))
                .unwrap(),
            Action::ClearAiTags
        );
        assert_eq!(
            BulkAction::AddToCollections
                .resolve(Some(&params(Some(vec![2, 1]), None)))
                .unwrap(),
            Action::AddToCollections(vec![2, 1])
        );
        assert_eq!(
            BulkAction::RemoveFromCollection
                .resolve(Some(&params(None, Some(4))))
                .unwrap(),
            Action::RemoveFromCollection(4)
        );
        let cases = [
            (BulkAction::AddToCollections, None, "params.collectionIds"),
            (
                BulkAction::AddToCollections,
                Some(params(Some(vec![]), None)),
                "params.collectionIds",
            ),
            (
                BulkAction::AddToCollections,
                Some(params(Some(vec![1]), Some(1))),
                "params.collectionId",
            ),
            (
                BulkAction::RemoveFromCollection,
                None,
                "params.collectionId",
            ),
            (
                BulkAction::RemoveFromCollection,
                Some(params(Some(vec![1]), Some(1))),
                "params.collectionIds",
            ),
            (
                BulkAction::Delete,
                Some(params(None, Some(1))),
                "params.collectionId",
            ),
            (
                BulkAction::Restore,
                Some(params(Some(vec![1]), None)),
                "params.collectionIds",
            ),
        ];
        for (action, given, named) in cases {
            let err = action.resolve(given.as_ref()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{action:?}");
            assert_eq!(field(&err), named, "{action:?} {given:?}");
        }
    }

    #[test]
    fn later_phases_are_not_available() {
        for action in [
            BulkAction::Analyze,
            BulkAction::FetchMedia,
            BulkAction::RemoveStoredMedia,
        ] {
            let err = action.resolve(None).unwrap_err();
            assert_eq!(err.code(), ErrorCode::NotAvailable);
            assert_eq!(err.status(), StatusCode::UNPROCESSABLE_ENTITY);
        }
    }

    #[test]
    fn wire_names_are_the_serde_names() {
        for action in [
            BulkAction::Delete,
            BulkAction::Restore,
            BulkAction::AddToCollections,
            BulkAction::RemoveFromCollection,
            BulkAction::ClearAiDescription,
            BulkAction::ClearAiTags,
            BulkAction::Analyze,
            BulkAction::FetchMedia,
            BulkAction::RemoveStoredMedia,
        ] {
            assert_eq!(serde_json::to_value(action).unwrap(), action.as_str());
        }
    }
}
