//! Session-bound capture and recapture. Idempotency is the shared middleware.
use axum::extract::State;
use axum::http::StatusCode;
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::capture::{self, Options};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Path};
use crate::jobs::idempotency::IdempotencyHeader;
use crate::routes::jobs::Job;
use crate::state::{AppState, blocking};
use shelfy_core::repo::RepoError;

#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateSite {
    #[schema(max_length = 2048)]
    pub url: String,
    #[serde(default)]
    #[schema(minimum = 1, maximum = 8, default = 6)]
    pub max_pages: Option<u8>,
    #[serde(default)]
    pub single_page: bool,
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Recapture {
    #[serde(default)]
    #[schema(minimum = 1, maximum = 8, default = 6)]
    pub max_pages: Option<u8>,
    #[serde(default)]
    pub single_page: Option<bool>,
}
#[derive(Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SiteQueued {
    pub key: String,
    pub job: Job,
}

#[utoipa::path(post,path="/api/v1/sites",tag="websites",operation_id="createSite",params(IdempotencyHeader),request_body=CreateSite,responses((status=CREATED,description="Website capture queued.",body=SiteQueued),(status=OK,description="The site's active capture.",body=SiteQueued)))]
pub async fn create_site(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<CreateSite>,
) -> Result<(StatusCode, Json<SiteQueued>), ApiError> {
    let opts = Options {
        max_pages: request.max_pages.unwrap_or(6),
        single_page: request.single_page,
    };
    let (key, queued) = capture::enqueue_site(&state, user.id(), &request.url, opts).await?;
    Ok((
        if queued.created {
            StatusCode::CREATED
        } else {
            StatusCode::OK
        },
        Json(SiteQueued {
            key,
            job: queued.job.into(),
        }),
    ))
}

#[utoipa::path(post,path="/api/v1/sites/{key}/recapture",tag="websites",operation_id="recaptureSite",params(("key"=String,Path,description="Website key."),IdempotencyHeader),request_body=Recapture,responses((status=ACCEPTED,description="A new capture version queued.",body=SiteQueued)))]
pub async fn recapture(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(key): Path<String>,
    Json(request): Json<Recapture>,
) -> Result<(StatusCode, Json<SiteQueued>), ApiError> {
    let db = state.user_db(user.id()).await?;
    let found=blocking(move || db.read(|c| {
        c.query_row("SELECT coalesce(p.web_url,p.post_url),coalesce(json_extract(w.meta_json,'$.capture.singlePage'),0) FROM posts p LEFT JOIN web_captures w ON w.id=p.current_capture_id WHERE p.key=?1 AND p.platform='web' AND p.deleted_at IS NULL",[key],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?))).optional().map_err(RepoError::from)
    })).await?.ok_or_else(ApiError::not_found)?;
    let opts = Options {
        max_pages: request.max_pages.unwrap_or(6),
        single_page: request.single_page.unwrap_or(found.1),
    };
    let (key, queued) = capture::enqueue_site(&state, user.id(), &found.0, opts).await?;
    Ok((
        StatusCode::ACCEPTED,
        Json(SiteQueued {
            key,
            job: queued.job.into(),
        }),
    ))
}
