//! `GET /api/v1/posts` (the gallery list) and `GET /api/v1/posts/{key}` (one
//! post), plan §2.9. Both are conditional ([`crate::conditional`]).
//!
//! The filters mirror the desktop's gallery filters (`buildPostFilter`), with
//! the core's documented changes (`shelfy_core::repo::posts`). Two tag filters
//! coexist, as on the desktop: `tag` is the gallery's tag chip and always
//! filters; `tags` + `tagMode` are the tags of the AI views, which in `or` mode
//! with `q` widen the search instead of filtering it.

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use shelfy_core::repo::posts::{self, PageRequest, PostFilter, SourceBucket};
use utoipa::{IntoParams, ToSchema};

use super::listing::{
    self, Cursors, MAX_QUERY_CHARS, MAX_VALUE_CHARS, MatchMode, PostSort, YesNo, check_count,
    check_text, check_values, flag, page_size,
};
use super::model::{MediaType, Platform, Post, PostDetail, PostPage};
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Path, Query};
use crate::state::{AppState, blocking};

/// Longest post key (plan §2.8); longer keys cannot exist.
const MAX_KEY_BYTES: usize = 200;

/// Websites or social posts.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum PostSource {
    /// Websites only (platform `web`).
    Web,
    /// Everything else, manual bookmarks included.
    Social,
}

/// Filters, order and paging of `GET /api/v1/posts`. Every filter is
/// optional; the ones given combine with AND.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct PostsQuery {
    /// Only posts of this platform.
    pub platform: Option<Platform>,
    /// Only websites (`web`) or only everything else (`social`).
    pub source: Option<PostSource>,
    /// Only posts in this collection (its `id`).
    pub collection: Option<i64>,
    /// Only posts of these kinds. Repeatable: any of them.
    #[param(style = Form, explode)]
    pub media_type: Vec<MediaType>,
    /// Only posts with (`yes`) or without (`no`) a stored object.
    pub stored: Option<YesNo>,
    /// Only posts with (`yes`) or without (`no`) AI tags; manual tags do not
    /// count.
    pub ai_tagged: Option<YesNo>,
    /// Only posts with this AI status.
    pub ai_status: Option<String>,
    /// Only posts with this tag (the gallery's tag chip; always a filter).
    pub tag: Option<String>,
    /// Tags of the AI views, combined by `tagMode`. Repeatable. With `q` in
    /// `or` mode they widen the search: a post with one of the tags or a text
    /// match is listed, and its tags add to its score.
    #[param(style = Form, explode)]
    pub tags: Vec<String>,
    /// How `tags` combine. Default `or`.
    pub tag_mode: Option<MatchMode>,
    /// Only posts with this AI entity.
    pub entity: Option<String>,
    /// Only posts with this AI category.
    pub category: Option<String>,
    /// Only posts with this AI content type.
    pub content_type: Option<String>,
    /// Free-text search over tags, keywords, entities, description, note,
    /// caption, author and page text (plan §2.14). At most 500 characters.
    pub q: Option<String>,
    /// Suggested concepts: more search terms, combined with `q` by
    /// `conceptMode`. Repeatable.
    #[param(style = Form, explode)]
    pub concept: Vec<String>,
    /// How `q` and the concepts combine. Default `or`.
    pub concept_mode: Option<MatchMode>,
    /// Order. Default `relevance` with search text (`q` or `concept`),
    /// `newest` without.
    pub sort: Option<PostSort>,
    /// List the trash instead of the library (`true` or `1`).
    #[serde(deserialize_with = "flag")]
    pub trash: Option<bool>,
    /// Page size, 1–200 (larger values are clamped). Default 60.
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page, with the same filters and sort.
    pub cursor: Option<String>,
    /// Add `total`, the number of matching posts, to the response.
    #[serde(deserialize_with = "flag")]
    pub include_total: Option<bool>,
}

impl PostsQuery {
    /// Refuses over-long text and too many values (422 `validation_failed`).
    fn validate(&self) -> Result<(), ApiError> {
        check_text("q", self.q.as_deref(), MAX_QUERY_CHARS)?;
        for (field, value) in [
            ("aiStatus", &self.ai_status),
            ("tag", &self.tag),
            ("entity", &self.entity),
            ("category", &self.category),
            ("contentType", &self.content_type),
        ] {
            check_text(field, value.as_deref(), MAX_VALUE_CHARS)?;
        }
        check_values("tags", &self.tags)?;
        check_values("concept", &self.concept)?;
        check_count("mediaType", self.media_type.len())
    }

    /// The core filter.
    fn filter(&self) -> PostFilter {
        PostFilter {
            platform: self.platform.map(Into::into),
            source: self.source.map(|s| match s {
                PostSource::Web => SourceBucket::Web,
                PostSource::Social => SourceBucket::Social,
            }),
            collection_id: self.collection,
            media_types: self
                .media_type
                .iter()
                .map(|m| m.as_str().to_owned())
                .collect(),
            stored: self.stored.map(YesNo::is_yes),
            ai_tagged: self.ai_tagged.map(YesNo::is_yes),
            analyzed: None,
            ai_status: self.ai_status.clone(),
            tag: self.tag.clone(),
            tags: self.tags.clone(),
            tag_mode: self.tag_mode.unwrap_or_default().into(),
            entity: self.entity.clone(),
            category: self.category.clone(),
            content_type: self.content_type.clone(),
            q: self.q.clone(),
            concepts: self.concept.clone(),
            concept_mode: self.concept_mode.unwrap_or_default().into(),
            date_from: None,
            date_to: None,
            trash: self.trash.unwrap_or(false),
        }
    }
}

/// One page of the library (or of the trash), newest first by default.
///
/// Browsing orders page by keyset; relevance pages within the first 1,000
/// results. The response is conditional: send the `ETag` back in
/// `If-None-Match` and an unchanged page answers 304.
#[utoipa::path(
    get,
    path = "/api/v1/posts",
    tag = "library",
    operation_id = "listPosts",
    params(PostsQuery, ConditionalHeaders),
    responses(
        (
            status = OK,
            description = "One page of posts.",
            body = PostPage,
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
pub async fn list_posts(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<PostsQuery>,
) -> Result<Response, ApiError> {
    query.validate()?;
    let filter = query.filter();
    let sort = PostSort::effective(query.sort, filter.has_text());
    let limit = page_size(query.limit);
    let include_total = query.include_total.unwrap_or(false);
    // What identifies the result set (the cursor fingerprint), and the whole
    // request (the ETag).
    let filters = PostsQuery {
        sort: Some(sort),
        limit: None,
        cursor: None,
        include_total: None,
        ..query.clone()
    };
    let cursors = Cursors::new("posts", &filters);
    let cursor = query
        .cursor
        .as_deref()
        .map(|text| cursors.decode(text))
        .transpose()?;

    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view(
        "posts.list",
        user.id(),
        db.generation(),
        &(&filters, limit, &query.cursor, include_total),
    );
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let page = PageRequest {
        sort: sort.into(),
        limit,
        cursor,
    };
    let result = listing::fetch(db, filter, page, include_total).await?;
    let body = PostPage {
        items: result.page.items.into_iter().map(Post::from).collect(),
        next_cursor: result.page.next_cursor.map(|c| cursors.encode(&c)),
        total: result.total,
    };
    Ok(etag.respond(Json(body)))
}

/// One post with everything about it, trashed or not.
#[utoipa::path(
    get,
    path = "/api/v1/posts/{key}",
    tag = "library",
    operation_id = "getPost",
    params(
        ("key" = String, Path, description = "The post's key, for example `ig_3141592653589793238`."),
        ConditionalHeaders,
    ),
    responses(
        (
            status = OK,
            description = "The post.",
            body = PostDetail,
            headers(
                ("ETag" = String, description = "Weak ETag of this post in this library state."),
                ("Cache-Control" = String, description = "`private, no-cache`."),
            )
        ),
        (
            status = NOT_MODIFIED,
            description = "The post is unchanged since the ETag in `If-None-Match`; no body.",
            headers(("ETag" = String, description = "The same ETag."))
        ),
    )
)]
pub async fn get_post(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(key): Path<String>,
) -> Result<Response, ApiError> {
    if key.len() > MAX_KEY_BYTES {
        return Err(ApiError::not_found());
    }
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view("posts.get", user.id(), db.generation(), &key);
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let detail = blocking(move || db.read(|conn| posts::get(conn, &key)))
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(etag.respond(Json(PostDetail::from(detail))))
}
