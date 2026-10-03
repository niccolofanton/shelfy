//! Session-authenticated cluster review. Runs/providers belong to P3-16.
use super::tag_aliases::ReviewStatus;
use crate::conditional::{ConditionalHeaders, ETag};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::extract::{Json, Path};
use crate::library::{self, Change};
use crate::state::{AppState, blocking};
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::Response;
use serde::{Deserialize, Serialize};
use shelfy_core::tags::clusters;
use utoipa::ToSchema;

#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagCluster {
    pub id: i64,
    pub label: String,
    pub status: ReviewStatus,
    pub top_tag: String,
    pub tags: Vec<String>,
    pub post_count: u64,
    pub run_id: i64,
}
impl From<clusters::Cluster> for TagCluster {
    fn from(c: clusters::Cluster) -> Self {
        Self {
            id: c.id,
            label: c.label,
            status: c.status.into(),
            top_tag: c.top_tag,
            tags: c.tags,
            post_count: c.post_count,
            run_id: c.run_id,
        }
    }
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterList {
    pub items: Vec<TagCluster>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterUpdated {
    pub updated: usize,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct ClusterTagRemoved {
    pub removed: usize,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClusterDecision {
    Accepted,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ClusterPatch {
    #[schema(nullable = false)]
    pub status: Option<ClusterDecision>,
    #[schema(nullable = false)]
    pub label: Option<String>,
}
#[utoipa::path(get,path="/api/v1/tag-clusters",tag="ai",operation_id="listTagClusters",params(ConditionalHeaders),
    responses((status=OK,description="Top 24 clusters by distinct live posts.",body=ClusterList,headers(("ETag"=String),("Cache-Control"=String))),
    (status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn list_clusters(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view("tags.clusters", user.id(), db.generation(), &());
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let rows = blocking(move || db.read(|c| clusters::list(c, 24))).await?;
    Ok(etag.respond(Json(ClusterList {
        items: rows.into_iter().map(Into::into).collect(),
    })))
}
#[utoipa::path(patch,path="/api/v1/tag-clusters/{id}",tag="ai",operation_id="updateTagCluster",
    params(("id"=i64,Path)),request_body=ClusterPatch,responses((status=OK,description="Cluster reviewed.",body=ClusterUpdated)))]
pub async fn update_cluster(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
    Json(patch): Json<ClusterPatch>,
) -> Result<Json<ClusterUpdated>, ApiError> {
    if patch.status.is_none() && patch.label.is_none() {
        return Err(ApiError::invalid_field("patch", "no change supplied"));
    }
    library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        clusters::review(
            tx,
            id,
            patch.status.is_some(),
            patch.label.as_deref(),
            crate::ids::now_ms(),
        )?;
        Ok(Change::collections(()))
    })
    .await?;
    Ok(Json(ClusterUpdated { updated: 1 }))
}
#[utoipa::path(delete,path="/api/v1/tag-clusters/{id}",tag="ai",operation_id="dismissTagCluster",
    params(("id"=i64,Path)),responses((status=OK,description="Cluster dismissed.",body=ClusterUpdated)))]
pub async fn dismiss_cluster(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<ClusterUpdated>, ApiError> {
    library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        clusters::dismiss(tx, id)?;
        Ok(Change::collections(()))
    })
    .await?;
    Ok(Json(ClusterUpdated { updated: 1 }))
}
#[utoipa::path(delete,path="/api/v1/tag-clusters/{id}/tags/{tag}",tag="ai",operation_id="removeClusterTag",
    params(("id"=i64,Path),("tag"=String,Path)),responses((status=OK,description="Tag removed from cluster.",body=ClusterTagRemoved)))]
pub async fn remove_cluster_tag(
    State(state): State<AppState>,
    user: CurrentUser,
    Path((id, tag)): Path<(i64, String)>,
) -> Result<Json<ClusterTagRemoved>, ApiError> {
    if tag.len() > 1024 {
        return Err(ApiError::invalid_field("tag", "too long"));
    }
    let result = library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        clusters::remove_tag(tx, id, &tag).map(Change::collections)
    })
    .await?;
    Ok(Json(ClusterTagRemoved {
        removed: result.value,
    }))
}
