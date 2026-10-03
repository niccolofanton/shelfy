//! Authenticated, idempotent import admission and durable report reads.
use super::auth::no_store;
use crate::control::{
    jobs::{self, Inserted, NewJobRow},
    uploads::{self, ClaimRefusal, UploadPurpose},
};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Path};
use crate::imports::{Import, checkpoint_for};
use crate::jobs::{
    idempotency::IDEMPOTENCY_KEY,
    import::{self, Payload},
};
use crate::state::{AppState, blocking};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use utoipa::{IntoParams, ToSchema};

/// Required admission key; shared middleware validates and reserves it.
#[derive(IntoParams)]
#[into_params(parameter_in=Header)]
pub struct ImportIdempotencyHeader {
    /// Unique request key, 1–255 visible ASCII bytes; replayed for 24 hours.
    #[param(rename = "Idempotency-Key", nullable = false)]
    pub idempotency_key: String,
}

/// An import upload (never a client-provided path).
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StartImport {
    /// Complete upload with purpose `import`.
    pub upload_id: String,
}
/// Claims the upload and inserts the job atomically. Only a web session can
/// start an import; tokens authorized to upload cannot operate the library.
#[utoipa::path(post,path="/api/v1/imports",tag="imports",operation_id="startImport",params(ImportIdempotencyHeader),request_body=StartImport,responses((status=ACCEPTED,description="Import queued.",body=Import)))]
pub async fn start_import(
    user: CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<StartImport>,
) -> Result<Response, ApiError> {
    if !headers.contains_key(IDEMPOTENCY_KEY) {
        return Err(ApiError::new(ErrorCode::BadRequest)
            .with_detail("Idempotency-Key is required for an import"));
    }
    if body.upload_id.is_empty()
        || body.upload_id.len() > 64
        || !body.upload_id.bytes().all(|c| c.is_ascii_alphanumeric())
    {
        return Err(ApiError::invalid_field("uploadId", "must be an upload id"));
    }
    crate::quota::check(&state, user.id(), 0).await?;
    let control = Arc::clone(state.control());
    let uid = user.id().to_owned();
    let dir = state.config().data_dir.uploads_dir();
    let now = state.jobs().clock().now_ms();
    let row = blocking(move || {
        control.write(|tx| -> Result<_, ApiError> {
            let claimed = uploads::claim(
                tx,
                &uid,
                UploadPurpose::IMPORT,
                std::slice::from_ref(&body.upload_id),
                now,
            )?
            .map_err(|e| match e {
                ClaimRefusal::Consumed(_) => ApiError::new(ErrorCode::UploadConsumed),
                ClaimRefusal::Unusable(_) => ApiError::invalid_field(
                    "uploadId",
                    "must be a complete import upload of this user",
                ),
            })?;
            let upload = claimed.first().ok_or_else(ApiError::not_found)?;
            if !std::fs::metadata(uploads::file_path(&dir, &upload.id, true)).is_ok_and(|m| {
                m.is_file() && m.len() == u64::try_from(upload.length).unwrap_or(u64::MAX)
            }) {
                return Err(ApiError::invalid_field(
                    "uploadId",
                    "upload bytes are missing",
                ));
            }
            let payload = serde_json::to_string(&Payload {
                upload_id: upload.id.clone(),
            })
            .map_err(ApiError::internal)?;
            let dedupe = format!("upload:{}", upload.id);
            match jobs::insert(
                tx,
                &NewJobRow {
                    user_id: &uid,
                    kind: import::KIND,
                    dedupe_key: Some(&dedupe),
                    priority: 0,
                    payload_json: &payload,
                    max_attempts: 2,
                    run_at: now,
                },
                now,
            )? {
                Inserted::Created(row) => Ok(row),
                Inserted::Existing(_) => Err(ApiError::new(ErrorCode::UploadConsumed)),
            }
        })
    })
    .await?;
    state.jobs().admit_committed(&row);
    let id = row.id;
    let mut response = (
        StatusCode::ACCEPTED,
        Json(Import {
            job: row.into(),
            report: None,
        }),
    )
        .into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/imports/{id}")).map_err(ApiError::internal)?,
    );
    Ok(no_store(response))
}
/// Another user's job, an unknown id or a different job kind is 404. Reads
/// report on every state, including the already committed part of a cancel.
#[utoipa::path(get,path="/api/v1/imports/{id}",tag="imports",operation_id="getImport",params(("id"=i64,Path,description="Import job id.")),responses((status=OK,description="Import and committed report.",body=Import)))]
pub async fn get_import(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    let row = state
        .jobs()
        .get(user.id(), id)
        .await?
        .filter(|r| r.kind == import::KIND)
        .ok_or_else(ApiError::not_found)?;
    let library = state.user_db(user.id()).await?;
    let incarnation = row.incarnation.clone();
    let payload: Payload = serde_json::from_str(&row.payload_json).map_err(ApiError::internal)?;
    let report =
        blocking(move || library.read(|c| checkpoint_for(c, id, &incarnation, &payload.upload_id)))
            .await?
            .report;
    Ok(no_store(
        Json(Import {
            job: row.into(),
            report: Some(report),
        })
        .into_response(),
    ))
}
