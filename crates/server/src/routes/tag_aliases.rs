//! Alias review rewrites and reindexes posts inside a single library::write.
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
use shelfy_core::repo::posts;
use shelfy_core::tags::{Status, aliases};
use utoipa::{IntoParams, ToSchema};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReviewStatus {
    Proposed,
    Accepted,
}
impl From<Status> for ReviewStatus {
    fn from(s: Status) -> Self {
        match s {
            Status::Proposed => Self::Proposed,
            Status::Accepted => Self::Accepted,
        }
    }
}
impl From<ReviewStatus> for Status {
    fn from(s: ReviewStatus) -> Self {
        match s {
            ReviewStatus::Proposed => Self::Proposed,
            ReviewStatus::Accepted => Self::Accepted,
        }
    }
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, IntoParams)]
#[serde(deny_unknown_fields)]
#[into_params(parameter_in=Query)]
pub struct AliasQuery {
    pub status: Option<ReviewStatus>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TagAlias {
    pub alias_norm: String,
    pub alias_form: String,
    pub canonical_norm: String,
    pub canonical_form: String,
    pub status: ReviewStatus,
    pub count: u64,
}
impl From<aliases::Alias> for TagAlias {
    fn from(a: aliases::Alias) -> Self {
        Self {
            alias_norm: a.alias_norm,
            alias_form: a.alias_form,
            canonical_norm: a.canonical_norm,
            canonical_form: a.canonical_form,
            status: a.status.into(),
            count: a.count,
        }
    }
}
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasList {
    pub items: Vec<TagAlias>,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasesAccepted {
    pub accepted: usize,
    pub rewritten: usize,
}
#[derive(Debug, Serialize, ToSchema)]
pub struct AliasDismissed {
    pub ok: bool,
}
#[utoipa::path(get,path="/api/v1/tag-aliases",tag="ai",operation_id="listTagAliases",params(AliasQuery,ConditionalHeaders),
    responses((status=OK,description="User aliases by live post count.",body=AliasList,headers(("ETag"=String),("Cache-Control"=String))),
    (status=NOT_MODIFIED,description="Unchanged.")))]
pub async fn list_aliases(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Query(query): Query<AliasQuery>,
) -> Result<Response, ApiError> {
    let db = state.user_db(user.id()).await?;
    let etag = ETag::for_view("tags.aliases", user.id(), db.generation(), &query);
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    let rows =
        blocking(move || db.read(|c| aliases::list(c, query.status.map(Into::into)))).await?;
    Ok(etag.respond(Json(AliasList {
        items: rows.into_iter().map(Into::into).collect(),
    })))
}
#[utoipa::path(post,path="/api/v1/tag-aliases/{alias}/accept",tag="ai",operation_id="acceptTagAlias",
    params(("alias"=String,Path)),responses((status=OK,description="Alias accepted and affected posts reindexed.",body=AliasesAccepted)))]
pub async fn accept_alias(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(alias): Path<String>,
) -> Result<Json<AliasesAccepted>, ApiError> {
    if alias.len() > 1024 {
        return Err(ApiError::invalid_field("alias", "too long"));
    }
    let result = library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        let r = aliases::accept(tx, &alias)?;
        let keys = event_keys(posts::keys_of(tx, &r.post_ids)?);
        Ok(Change { value: r, keys })
    })
    .await?;
    Ok(Json(AliasesAccepted {
        accepted: result.value.accepted,
        rewritten: result.value.rewritten,
    }))
}
#[utoipa::path(post,path="/api/v1/tag-aliases/accept-all",tag="ai",operation_id="acceptAllTagAliases",
    responses((status=OK,description="All proposed aliases accepted atomically.",body=AliasesAccepted)))]
pub async fn accept_all_aliases(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Json<AliasesAccepted>, ApiError> {
    let result = library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        let r = aliases::accept_all(tx)?;
        let keys = event_keys(posts::keys_of(tx, &r.post_ids)?);
        Ok(Change { value: r, keys })
    })
    .await?;
    Ok(Json(AliasesAccepted {
        accepted: result.value.accepted,
        rewritten: result.value.rewritten,
    }))
}
#[utoipa::path(post,path="/api/v1/tag-aliases/{alias}/dismiss",tag="ai",operation_id="dismissTagAlias",
    params(("alias"=String,Path)),responses((status=OK,description="Proposal dismissed; post tags unchanged.",body=AliasDismissed)))]
pub async fn dismiss_alias(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(alias): Path<String>,
) -> Result<Json<AliasDismissed>, ApiError> {
    if alias.len() > 1024 {
        return Err(ApiError::invalid_field("alias", "too long"));
    }
    library::write(&state, user.id(), ChangeReason::Edit, move |tx| {
        aliases::dismiss(tx, &alias)?;
        Ok(Change::collections(()))
    })
    .await?;
    Ok(Json(AliasDismissed { ok: true }))
}
