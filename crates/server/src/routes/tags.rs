//! Session-only tag explorer and atomic taxonomy edits.
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::extract::{Json, Path, Query};
use crate::library::{self, Change, event_keys};
use crate::state::{AppState, blocking};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use shelfy_core::tags::{explore, health, merge};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TagTier {
    General,
    Specific,
    Manual,
    #[default]
    All,
}
impl From<TagTier> for explore::Tier {
    fn from(t: TagTier) -> Self {
        match t {
            TagTier::General => Self::General,
            TagTier::Specific => Self::Specific,
            TagTier::Manual => Self::Manual,
            TagTier::All => Self::All,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct TagQuery {
    pub tier: Option<TagTier>,
    pub limit: Option<usize>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct TagLimit {
    pub limit: Option<usize>,
}
fn limit(n: Option<usize>, default: usize, max: usize) -> Result<usize, ApiError> {
    let n = n.unwrap_or(default);
    if n == 0 || n > max {
        return Err(ApiError::invalid_field("limit", "out of range"));
    }
    Ok(n)
}
#[derive(Debug, Serialize, ToSchema)]
pub struct CategoryCount {
    pub category: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ContentTypeCount {
    pub content_type: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct LanguageCount {
    pub language: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagOverview {
    pub total: u64,
    pub analyzed: u64,
    pub unanalyzed: u64,
    pub by_category: Vec<CategoryCount>,
    pub by_content_type: Vec<ContentTypeCount>,
    pub languages: Vec<LanguageCount>,
    pub unique_tags: u64,
    pub tagged_posts: u64,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagStat {
    pub tag: String,
    pub count: u64,
    pub last_used: Option<i64>,
    pub categories: Vec<CategoryCount>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TagStats {
    pub items: Vec<TagStat>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct EntityStat {
    pub entity: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct EntityStats {
    pub items: Vec<EntityStat>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct RelatedTag {
    pub tag: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct RelatedTags {
    pub items: Vec<RelatedTag>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagHealth {
    pub orphan_tags: Vec<RelatedTag>,
    pub rare_tags: usize,
    pub unanalyzed_posts: u64,
    pub untagged_posts: u64,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MergeSuggestion {
    pub canonical: String,
    pub variants: Vec<String>,
    pub total_count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct MergeSuggestions {
    pub items: Vec<MergeSuggestion>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TagsMerged {
    pub updated: usize,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RenameTag {
    pub from: String,
    pub to: String,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct MergeTags {
    pub sources: Vec<String>,
    pub target: String,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TagMatch {
    And,
    #[default]
    Or,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TagsToKeys {
    pub tags: Vec<String>,
    #[serde(default)]
    pub mode: TagMatch,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct TagPostKeys {
    pub keys: Vec<String>,
    pub truncated: bool,
}
#[derive(Serialize)]
struct Items<T> {
    items: T,
}

/// Serialize each view once per generation. Entries over 1 MiB stay uncached,
/// bounding the 64-entry cache's payload to 64 MiB across all users.
pub(super) async fn cached_read<T, F, P>(
    state: &AppState,
    user: &str,
    headers: &HeaderMap,
    view: &'static str,
    params: &P,
    read: F,
) -> Result<Response, ApiError>
where
    T: Serialize + Send + 'static,
    F: FnOnce(&rusqlite::Connection) -> shelfy_core::repo::Result<T> + Send + 'static,
    P: Serialize + Sync,
{
    let db = state.user_db(user).await?;
    let generation = db.generation();
    let etag = ETag::for_view(view, user, generation, params);
    if etag.matches(headers) {
        return Ok(etag.not_modified());
    }
    let digest = library::view_digest(view, params);
    let bytes = if let Some(bytes) = state
        .library_caches()
        .tag_views
        .get(user, generation, &digest)
    {
        bytes
    } else {
        let bytes = blocking(move || {
            db.read(|c| {
                let value = read(c)?;
                Ok::<_, shelfy_core::repo::RepoError>(axum::body::Bytes::from(
                    serde_json::to_vec(&value).expect("taxonomy DTOs serialize"),
                ))
            })
        })
        .await?;
        if bytes.len() <= 1024 * 1024 {
            state
                .library_caches()
                .tag_views
                .insert(user, generation, digest, bytes.clone());
        }
        bytes
    };
    Ok(etag.respond((
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        bytes,
    )))
}

#[utoipa::path(get,path="/api/v1/tags/overview",tag="ai",operation_id="getTagOverview",params(ConditionalHeaders),responses((status=OK,description="Live library AI and tag counters.",body=TagOverview,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn overview(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cached_read(
        &state,
        user.id(),
        &headers,
        "tags.overview",
        &(),
        explore::overview,
    )
    .await
}
#[utoipa::path(get,path="/api/v1/tags",tag="ai",operation_id="getTagStats",params(TagQuery,ConditionalHeaders),responses((status=OK,description="Tags by distinct live posts; default 200, maximum 500.",body=TagStats,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn list(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<TagQuery>,
) -> Result<Response, ApiError> {
    let n = limit(query.limit, 200, 500)?;
    let tier = query.tier.unwrap_or_default();
    cached_read(
        &state,
        user.id(),
        &headers,
        "tags.stats",
        &(tier, n),
        move |c| explore::stats(c, tier.into(), n).map(|items| Items { items }),
    )
    .await
}
#[utoipa::path(get,path="/api/v1/entities",tag="ai",operation_id="getEntityStats",params(TagLimit,ConditionalHeaders),responses((status=OK,description="Case-insensitive entities with their most frequent form; default 60, maximum 500.",body=EntityStats,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn entities(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<TagLimit>,
) -> Result<Response, ApiError> {
    let n = limit(query.limit, 60, 500)?;
    cached_read(&state, user.id(), &headers, "tags.entities", &n, move |c| {
        explore::entities(c, n).map(|items| Items { items })
    })
    .await
}
#[utoipa::path(get,path="/api/v1/tags/{tag}/related",tag="ai",operation_id="getRelatedTags",params(("tag"=String,Path),ConditionalHeaders),responses((status=OK,description="Top 12 co-occurring tags, resolving accepted aliases.",body=RelatedTags,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn related(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Path(tag): Path<String>,
) -> Result<Response, ApiError> {
    if tag.len() > 1024 {
        return Err(ApiError::invalid_field("tag", "too long"));
    }
    let tag = tag.trim().to_lowercase();
    let param = tag.clone();
    cached_read(
        &state,
        user.id(),
        &headers,
        "tags.related",
        &param,
        move |c| explore::related(c, &tag, 12).map(|items| Items { items }),
    )
    .await
}
#[utoipa::path(get,path="/api/v1/tags/health",tag="ai",operation_id="getTagHealth",params(ConditionalHeaders),responses((status=OK,description="Rare and orphan tags, unanalyzed and analyzed untagged posts.",body=TagHealth,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn health(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    cached_read(
        &state,
        user.id(),
        &headers,
        "tags.health",
        &(),
        health::health,
    )
    .await
}
#[utoipa::path(get,path="/api/v1/tags/merge-suggestions",tag="ai",operation_id="getTagMergeSuggestions",params(TagLimit,ConditionalHeaders),responses((status=OK,description="Accent-folded and edit-distance merge groups; default 30, maximum 100. Cached per library generation.",body=MergeSuggestions,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn suggestions(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<TagLimit>,
) -> Result<Response, ApiError> {
    let n = limit(query.limit, 30, 100)?;
    let db = state.user_db(user.id()).await?;
    let generation = db.generation();
    let etag = ETag::for_view("tags.merges", user.id(), generation, &n);
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let digest = library::view_digest("tags.merges", &());
    let items = if let Some(rows) =
        state
            .library_caches()
            .tag_merges
            .get(user.id(), generation, &digest)
    {
        rows
    } else {
        let rows = Arc::new(blocking(move || db.read(merge::suggestions)).await?);
        state
            .library_caches()
            .tag_merges
            .insert(user.id(), generation, digest, Arc::clone(&rows));
        rows
    };
    Ok(etag.respond(Json(Items {
        items: items.iter().take(n).cloned().collect::<Vec<_>>(),
    })))
}
#[utoipa::path(post,path="/api/v1/tags/rename",tag="ai",operation_id="renameTag",request_body=RenameTag,responses((status=OK,description="Atomic JSON, tag rows, membership, aliases and index rewrite.",body=TagsMerged)))]
pub async fn rename(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<RenameTag>,
) -> Result<Json<TagsMerged>, ApiError> {
    let written = library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        let result = merge::rename(tx, &body.from, &body.to, crate::ids::now_ms())?;
        Ok(Change {
            keys: event_keys(result.keys.clone()),
            value: result,
        })
    })
    .await?;
    Ok(Json(TagsMerged {
        updated: written.value.updated,
    }))
}
#[utoipa::path(post,path="/api/v1/tags/merge",tag="ai",operation_id="mergeTags",request_body=MergeTags,responses((status=OK,description="Atomic merge of up to 500 sources into one target.",body=TagsMerged)))]
pub async fn merge(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<MergeTags>,
) -> Result<Json<TagsMerged>, ApiError> {
    let written = library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        let result = merge::merge(tx, &body.sources, &body.target, crate::ids::now_ms())?;
        Ok(Change {
            keys: event_keys(result.keys.clone()),
            value: result,
        })
    })
    .await?;
    Ok(Json(TagsMerged {
        updated: written.value.updated,
    }))
}
#[utoipa::path(post,path="/api/v1/tags/post-keys",tag="ai",operation_id="getPostKeysByTags",request_body=TagsToKeys,responses((status=OK,description="Live post keys for AND/OR tags, capped at 10000. Bulk clients should submit a filter selector instead.",body=TagPostKeys)))]
pub async fn post_keys(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(body): Json<TagsToKeys>,
) -> Result<Json<explore::PostKeys>, ApiError> {
    if body.tags.len() > 500 || body.tags.iter().any(|s| s.len() > 1024) {
        return Err(ApiError::invalid_field(
            "tags",
            "at most 500 tags of at most 1024 bytes",
        ));
    }
    let db = state.user_db(user.id()).await?;
    let mode = match body.mode {
        TagMatch::And => explore::MatchMode::And,
        TagMatch::Or => explore::MatchMode::Or,
    };
    Ok(Json(
        blocking(move || db.read(|c| explore::post_keys(c, &body.tags, mode))).await?,
    ))
}
