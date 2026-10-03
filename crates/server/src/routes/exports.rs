//! Session-only export creation, listing, deletion and resumable downloads.
use crate::control::exports::{self as rows, Export};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::exports;
use crate::extract::{Json, Path};
use crate::ids::now_ms;
use crate::jobs::idempotency::{IDEMPOTENCY_KEY, IdempotencyHeader};
use crate::routes::media::preconditions::{self, Outcome};
use crate::state::{AppState, blocking};
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use std::io::SeekFrom;
use std::sync::Arc;
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
async fn owned(state: &AppState, user: &str, id: &str) -> Result<Export, ApiError> {
    if !exports::valid_id(id) {
        return Err(ApiError::not_found());
    }
    let (control, user, id) = (Arc::clone(state.control()), user.to_owned(), id.to_owned());
    blocking(move || control.read(|c| rows::get(c, &user, &id, now_ms())))
        .await?
        .ok_or_else(ApiError::not_found)
}
#[utoipa::path(post, path="/api/v1/exports", tag="exports", operation_id="startExport", security(("session"=[])), params(IdempotencyHeader), responses((status=ACCEPTED, description="Export queued or existing live bundle.", body=Export)))]
pub async fn start_export(
    user: CurrentUser,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    if !headers.contains_key(IDEMPOTENCY_KEY) {
        return Err(ApiError::invalid_field("Idempotency-Key", "is required"));
    }
    let export = exports::enqueue(&state, user.id()).await?;
    let location = format!("/api/v1/jobs/{}", export.job_id);
    let mut response = (StatusCode::ACCEPTED, Json(export)).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&location).map_err(ApiError::internal)?,
    );
    Ok(no_store(response))
}
#[utoipa::path(get, path="/api/v1/exports", tag="exports", operation_id="listExports", security(("session"=[])), responses((status=OK, description="Live bundles, at most one.", body=Vec<Export>)))]
pub async fn list_exports(
    user: CurrentUser,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let (control, user) = (Arc::clone(state.control()), user.id().to_owned());
    let exports = blocking(move || control.read(|c| rows::list(c, &user, now_ms()))).await?;
    Ok(no_store(Json(exports).into_response()))
}
#[utoipa::path(delete, path="/api/v1/exports/{id}", tag="exports", operation_id="deleteExport", security(("session"=[])), params(("id"=String, Path)), responses((status=NO_CONTENT, description="Bundle deleted and worker cancelled.")))]
pub async fn delete_export(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let export = owned(&state, user.id(), &id).await?;
    let (control, uid) = (Arc::clone(state.control()), user.id().to_owned());
    blocking(move || control.write(|tx| rows::mark_deleted(tx, &uid, &id, now_ms()))).await?;
    if let Some(job) = state.jobs().get(user.id(), export.job_id).await?
        && matches!(
            job.state,
            crate::events::model::JobState::Queued | crate::events::model::JobState::Running
        )
    {
        state.jobs().cancel(user.id(), export.job_id).await?;
    }
    exports::sweep(&state, now_ms()).await;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
#[utoipa::path(get, path="/api/v1/exports/{id}/download", tag="exports", operation_id="downloadExport", security(("session"=[])), params(("id"=String, Path), ("Range"=Option<String>, Header, description="Single byte range.")), responses((status=OK, description="ZIP64 bundle.", content_type="application/zip"), (status=PARTIAL_CONTENT, description="Requested byte range.", content_type="application/zip"), (status=RANGE_NOT_SATISFIABLE, description="Unsatisfiable range.")))]
pub async fn download_export(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(id): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let export = owned(&state, user.id(), &id).await?;
    let size = export
        .bytes
        .ok_or_else(|| ApiError::new(ErrorCode::Conflict).with_detail("export is not ready"))?;
    let path = exports::file(&exports::directory(&state, user.id()), &id);
    let mut file = tokio::fs::File::open(path).await.map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            ApiError::not_found()
        } else {
            ApiError::internal(e)
        }
    })?;
    if file.metadata().await.map_err(ApiError::internal)?.len() != size {
        return Err(ApiError::internal(anyhow::anyhow!(
            "export size changed on disk"
        )));
    }
    let etag = format!("\"export-{id}\"");
    let (status, start, len) = match preconditions::evaluate(&method, &headers, &etag, size) {
        Outcome::Full => (StatusCode::OK, 0, size),
        Outcome::Partial { start, end } => (StatusCode::PARTIAL_CONTENT, start, end - start + 1),
        Outcome::NotModified => return Ok(no_store(StatusCode::NOT_MODIFIED.into_response())),
        Outcome::PreconditionFailed => {
            return Ok(no_store(StatusCode::PRECONDITION_FAILED.into_response()));
        }
        Outcome::RangeNotSatisfiable => {
            return Ok(no_store(
                (
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    [(header::CONTENT_RANGE, format!("bytes */{size}"))],
                )
                    .into_response(),
            ));
        }
    };
    file.seek(SeekFrom::Start(start))
        .await
        .map_err(ApiError::internal)?;
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        Body::from_stream(ReaderStream::with_capacity(file.take(len), 64 * 1024))
    };
    let mut response = Response::new(body);
    *response.status_mut() = status;
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/zip"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    h.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    h.insert(
        header::ETAG,
        HeaderValue::from_str(&etag).map_err(ApiError::internal)?,
    );
    // HTTP-date formatting is locale independent; the file name keeps its date.
    let created =
        std::time::UNIX_EPOCH + std::time::Duration::from_millis(export.created_at.max(0) as u64);
    let date = httpdate::fmt_http_date(created);
    let month = match &date[8..11] {
        "Jan" => "01",
        "Feb" => "02",
        "Mar" => "03",
        "Apr" => "04",
        "May" => "05",
        "Jun" => "06",
        "Jul" => "07",
        "Aug" => "08",
        "Sep" => "09",
        "Oct" => "10",
        "Nov" => "11",
        _ => "12",
    };
    let date = format!("{}-{month}-{}", &date[12..16], &date[5..7]);
    h.insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"shelfy-export-{date}.zip\""
        ))
        .map_err(ApiError::internal)?,
    );
    if status == StatusCode::PARTIAL_CONTENT {
        h.insert(
            header::CONTENT_RANGE,
            HeaderValue::from_str(&format!("bytes {start}-{}/{size}", start + len - 1))
                .map_err(ApiError::internal)?,
        );
    }
    Ok(no_store(response))
}
