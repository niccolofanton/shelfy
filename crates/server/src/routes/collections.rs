//! `/api/v1/collections`: the user's collections ("folders" in the UI) with
//! their live counts, and their writes (plan §2.9 Collections, §1.2 #12).
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /collections` | every collection, in manual order, then creation order (conditional) |
//! | `POST /collections` | 201: a new collection |
//! | `PATCH /collections/{id}` | the collection renamed, recolored or moved in the manual order |
//! | `DELETE /collections/{id}` | the collection deleted; its posts stay (`trashed` is 0) |
//! | `POST /collections/{id}/posts` | the posts of a selector added (one statement, trash skipped) |
//! | `DELETE /collections/{id}/posts/{key}` | one post taken out (DATA-27) |
//! | `POST /collections/from-query` | 201: a new collection holding the posts of a selector |
//!
//! Another user's collection is a 404, like a missing one. Every write that
//! changes something announces `posts.changed` (`edit`; the posts whose
//! membership changed, `[]` when only collections changed, `null` past 200)
//! and `stats.changed`, so other tabs reload their folder list and counts.
//! Deleting a collection with its posts (`mode=withPosts`) comes in P1-11.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use shelfy_core::repo::collections::{self, CollectionPatch, DeleteMode, NewCollection};
use shelfy_core::repo::{RepoError, posts};
use utoipa::{IntoParams, ToSchema};

use super::model::{Collection, CollectionList};
use super::posts::MAX_KEY_BYTES;
use super::selector::PostSelector;
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::events::MAX_EVENT_KEYS;
use crate::extract::{Json, Path, Query};
use crate::ids::now_ms;
use crate::library::{self, event_keys};
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

/// A collection to create.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct NewCollectionRequest {
    /// The name, 1–200 characters once trimmed.
    pub name: String,
    /// The color, `#rgb` or `#rrggbb`; default `#3d5afe`.
    #[schema(nullable = false)]
    pub color: Option<String>,
}

impl NewCollectionRequest {
    fn into_new(self) -> NewCollection {
        NewCollection {
            name: self.name,
            color: self.color,
            ..NewCollection::default()
        }
    }
}

/// Creates a collection; it comes last in the manual order.
#[utoipa::path(
    post,
    path = "/api/v1/collections",
    tag = "library",
    operation_id = "createCollection",
    request_body = NewCollectionRequest,
    responses(
        (status = CREATED, description = "The new collection.", body = Collection),
    )
)]
pub async fn create_collection(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<NewCollectionRequest>,
) -> Result<Response, ApiError> {
    let new = request.into_new();
    let now = now_ms();
    let written = library::write(&state, user.id(), move |tx| {
        collections::create(tx, &new, now)
    })
    .await?;
    library::announce(&state, user.id(), Some(Vec::new()));
    Ok((StatusCode::CREATED, Json(Collection::from(written.value))).into_response())
}

/// Changes to a collection; absent fields are left alone.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CollectionUpdate {
    /// The new name, 1–200 characters once trimmed.
    #[schema(nullable = false)]
    pub name: Option<String>,
    /// The new color, `#rgb` or `#rrggbb`.
    #[schema(nullable = false)]
    pub color: Option<String>,
    /// The new place in the manual order, from 0; past the end means last.
    /// Every collection's `position` is renumbered from 0.
    #[schema(nullable = false)]
    pub position: Option<u32>,
}

/// Renames, recolors or moves a collection. A platform folder keeps its
/// link.
#[utoipa::path(
    patch,
    path = "/api/v1/collections/{id}",
    tag = "library",
    operation_id = "updateCollection",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = CollectionUpdate,
    responses(
        (status = OK, description = "The collection after the change.", body = Collection),
    )
)]
pub async fn update_collection(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(update): Json<CollectionUpdate>,
) -> Result<Json<Collection>, ApiError> {
    // The core keeps the old name for a blank one (desktop semantics); the
    // API says so instead. Blank as JavaScript's `trim` sees it.
    let blank = |c: char| c.is_whitespace() || c == '\u{feff}';
    if update
        .name
        .as_deref()
        .is_some_and(|name| name.trim_matches(blank).is_empty())
    {
        return Err(ApiError::invalid_field("name", "is blank"));
    }
    let written = library::write(&state, user.id(), move |tx| {
        let mut collection = if update.name.is_some() || update.color.is_some() {
            let patch = CollectionPatch {
                name: update.name,
                color: update.color,
            };
            collections::update(tx, id, &patch)?
        } else {
            collections::get(tx, id)?.ok_or(RepoError::NotFound)?
        };
        if let Some(position) = update.position {
            let index = usize::try_from(position).unwrap_or(usize::MAX);
            collection = collections::move_to(tx, id, index)?;
        }
        Ok(collection)
    })
    .await?;
    if written.changed {
        library::announce(&state, user.id(), Some(Vec::new()));
    }
    Ok(Json(Collection::from(written.value)))
}

/// What happens to the posts of a deleted collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub enum CollectionDeleteMode {
    /// Only the collection goes; its posts stay in the library (default).
    /// Moving them to the trash with it (`withPosts`) comes later.
    #[default]
    Label,
}

/// How to delete a collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct DeleteCollectionQuery {
    /// What happens to the posts; default `label`.
    pub mode: Option<CollectionDeleteMode>,
}

/// The outcome of deleting a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionDeleted {
    /// Posts moved to the trash with the collection: 0 with `mode=label`.
    pub trashed: u64,
}

/// Deletes a collection. Its posts stay in the library, out of the
/// collection.
#[utoipa::path(
    delete,
    path = "/api/v1/collections/{id}",
    tag = "library",
    operation_id = "deleteCollection",
    params(
        ("id" = i64, Path, description = "The collection's id."),
        DeleteCollectionQuery,
    ),
    responses(
        (status = OK, description = "The collection is gone.", body = CollectionDeleted),
    )
)]
pub async fn delete_collection(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Query(query): Query<DeleteCollectionQuery>,
) -> Result<Json<CollectionDeleted>, ApiError> {
    // The one mode until `withPosts` (P1-11).
    let CollectionDeleteMode::Label = query.mode.unwrap_or_default();
    let written = library::write(&state, user.id(), move |tx| {
        let members = collections::member_keys(tx, id, MAX_EVENT_KEYS + 1)?;
        let trashed = collections::delete(tx, id, DeleteMode::KeepPosts, now_ms())?;
        Ok((members, trashed))
    })
    .await?;
    let (members, trashed) = written.value;
    library::announce(&state, user.id(), event_keys(members));
    Ok(Json(CollectionDeleted {
        trashed: u64::try_from(trashed).unwrap_or(u64::MAX),
    }))
}

/// The posts to add.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct AddPostsRequest {
    /// Which posts.
    pub selector: PostSelector,
}

/// The outcome of adding posts to a collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionPostsAdded {
    /// Posts that joined the collection; trashed posts and posts already in
    /// it are not counted.
    pub added: u64,
    /// The collection, with its new count.
    pub collection: Collection,
}

/// Adds posts to a collection: a list of keys, or every post a filter
/// lists (minus some), however many, in one step. Trashed posts and posts
/// already in the collection are skipped.
#[utoipa::path(
    post,
    path = "/api/v1/collections/{id}/posts",
    tag = "library",
    operation_id = "addCollectionPosts",
    params(("id" = i64, Path, description = "The collection's id.")),
    request_body = AddPostsRequest,
    responses(
        (status = OK, description = "What was added.", body = CollectionPostsAdded),
    )
)]
pub async fn add_collection_posts(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(request): Json<AddPostsRequest>,
) -> Result<Json<CollectionPostsAdded>, ApiError> {
    let selector = request.selector.resolve("selector")?;
    let now = now_ms();
    let written = library::write(&state, user.id(), move |tx| {
        let added = collections::add_selected(tx, id, &selector, now)?;
        let collection = collections::get(tx, id)?.ok_or(RepoError::NotFound)?;
        let keys = added_keys(tx, &added)?;
        Ok((added.len(), collection, keys))
    })
    .await?;
    let (added, collection, keys) = written.value;
    if written.changed {
        library::announce(&state, user.id(), keys);
    }
    Ok(Json(CollectionPostsAdded {
        added: u64::try_from(added).unwrap_or(u64::MAX),
        collection: collection.into(),
    }))
}

/// The keys of the posts added, for the event; `None` past the event's cap.
fn added_keys(conn: &rusqlite::Connection, ids: &[i64]) -> Result<Option<Vec<String>>, RepoError> {
    if ids.len() > MAX_EVENT_KEYS {
        return Ok(None);
    }
    posts::keys_of(conn, ids).map(Some)
}

/// The outcome of taking a post out of a collection.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionPostRemoved {
    /// Whether the post was in the collection.
    pub removed: bool,
    /// The collection, with its new count.
    pub collection: Collection,
}

/// Takes one post out of a collection (§1.2 #12). Taking out a post that is
/// not in it changes nothing.
#[utoipa::path(
    delete,
    path = "/api/v1/collections/{id}/posts/{key}",
    tag = "library",
    operation_id = "removeCollectionPost",
    params(
        ("id" = i64, Path, description = "The collection's id."),
        ("key" = String, Path, description = "The post's key."),
    ),
    responses(
        (status = OK, description = "What was removed.", body = CollectionPostRemoved),
    )
)]
pub async fn remove_collection_post(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, key)): Path<(i64, String)>,
) -> Result<Json<CollectionPostRemoved>, ApiError> {
    if key.len() > MAX_KEY_BYTES {
        return Err(ApiError::not_found());
    }
    let lookup = key.clone();
    let written = library::write(&state, user.id(), move |tx| {
        let collection = collections::get(tx, id)?.ok_or(RepoError::NotFound)?;
        let post = posts::id_for_key(tx, &lookup)?.ok_or(RepoError::NotFound)?;
        if !collections::remove_post(tx, post, collection.id)? {
            return Ok((false, collection));
        }
        Ok((true, collections::get(tx, id)?.ok_or(RepoError::NotFound)?))
    })
    .await?;
    let (removed, collection) = written.value;
    if written.changed {
        library::announce(&state, user.id(), Some(vec![key]));
    }
    Ok(Json(CollectionPostRemoved {
        removed,
        collection: collection.into(),
    }))
}

/// A collection to create with posts.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct CollectionFromQuery {
    /// The name, 1–200 characters once trimmed.
    pub name: String,
    /// The color, `#rgb` or `#rrggbb`; default `#3d5afe`.
    #[schema(nullable = false)]
    pub color: Option<String>,
    /// The posts to put in it: usually the current gallery filter.
    pub selector: PostSelector,
}

/// Creates a collection holding the posts of a selector, in one step: "save
/// this view as a folder". Trashed posts are skipped.
#[utoipa::path(
    post,
    path = "/api/v1/collections/from-query",
    tag = "library",
    operation_id = "createCollectionFromQuery",
    request_body = CollectionFromQuery,
    responses(
        (status = CREATED, description = "The new collection.", body = CollectionPostsAdded),
    )
)]
pub async fn create_collection_from_query(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<CollectionFromQuery>,
) -> Result<Response, ApiError> {
    let selector = request.selector.resolve("selector")?;
    let new = NewCollection {
        name: request.name,
        color: request.color,
        ..NewCollection::default()
    };
    let now = now_ms();
    let written = library::write(&state, user.id(), move |tx| {
        let created = collections::create(tx, &new, now)?;
        let added = collections::add_selected(tx, created.id, &selector, now)?;
        let collection = collections::get(tx, created.id)?.ok_or(RepoError::NotFound)?;
        let keys = if added.is_empty() {
            Some(Vec::new())
        } else {
            added_keys(tx, &added)?
        };
        Ok((added.len(), collection, keys))
    })
    .await?;
    let (added, collection, keys) = written.value;
    library::announce(&state, user.id(), keys);
    let body = CollectionPostsAdded {
        added: u64::try_from(added).unwrap_or(u64::MAX),
        collection: collection.into(),
    };
    Ok((StatusCode::CREATED, Json(body)).into_response())
}
