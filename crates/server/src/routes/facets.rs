//! Conditional live-library facet values.
use crate::conditional::ConditionalHeaders;
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::state::AppState;
use axum::{extract::State, http::HeaderMap, response::Response};
use serde::Serialize;
use utoipa::ToSchema;
#[derive(Debug, Serialize, ToSchema)]
pub struct Facet {
    pub value: String,
    pub count: u64,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Facets {
    pub category: Vec<Facet>,
    pub content_type: Vec<Facet>,
    pub status: Vec<Facet>,
    pub language: Vec<Facet>,
}
#[utoipa::path(get,path="/api/v1/facets",tag="library",operation_id="getFacets",params(ConditionalHeaders),responses((status=OK,description="Category, content type, status (NULL as none), language values and counts of live posts.",body=Facets,headers(("ETag"=String),("Cache-Control"=String))),(status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn get(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    super::tags::cached_read(
        &state,
        user.id(),
        &headers,
        "tags.facets",
        &(),
        shelfy_core::tags::facets::facets,
    )
    .await
}
