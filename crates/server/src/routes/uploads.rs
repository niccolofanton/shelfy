//! `/api/v1/uploads`: resumable uploads, tus 1.0 core + creation (plan §2.9).
//!
//! T9 implements what the migration CLI needs:
//!
//! | Request | Answer |
//! |---|---|
//! | `POST /uploads` with `Upload-Length` and `Upload-Metadata` | 201, `Location: /api/v1/uploads/{id}` |
//! | `HEAD /uploads/{id}` | 200 with `Upload-Offset` and `Upload-Length`: where to resume |
//! | `PATCH /uploads/{id}` with `Upload-Offset` and an `application/offset+octet-stream` body (≤16 MiB) | 204 with the new `Upload-Offset` |
//! | `DELETE /uploads/{id}` (tus termination, P1-19) | 204: the upload and its bytes are gone |
//!
//! Every request carries `Tus-Resumable: 1.0.0` (412 otherwise). A `HEAD`
//! answer has no body, errors included: its status says what went wrong.
//!
//! **Metadata.** `purpose` is `migration-object` (one object of a bundle) or
//! `migration-db` (its database); `sha256` is the content's SHA-256 in
//! lowercase hex; an object also declares `ext`, its type in the store's
//! allowlist. The server checks all three when the last byte arrives: an
//! upload whose bytes do not match is deleted and answered 422, so the
//! client starts it over. Uploads that never finish expire after 24 h.
//!
//! **Bytes.** They go to `<data>/work/uploads/<id>.part` (§2.5); a PATCH
//! that breaks off keeps what arrived, and `HEAD` tells the client where to
//! continue. One PATCH at a time per upload: a concurrent one gets 409.
//!
//! **Auth.** A token with the `migrate` scope, and nothing else
//! ([`crate::routes::TOKEN_ROUTES`]); another user's upload is 404.
//! Bookmarks and imports (P4) add their own purposes, the `uploads` scope
//! and the session.
//!
//! **Expiry.** Creating an upload sweeps the user's expired ones; the hourly
//! housekeeping sweeps every user's, and the complete uploads no install
//! consumed within a week ([`crate::migrations::housekeeping`]).

use std::collections::HashSet;
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Extension;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use futures_util::StreamExt as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use shelfy_media::kind::SNIFF_LEN;
use shelfy_media::store::IngestLimits;
use shelfy_media::{Digest, MediaKind};
use tokio::io::AsyncWriteExt as _;
use utoipa::{IntoParams, ToSchema};

use super::auth::no_store;
use crate::auth::bearer::{TokenUser, scopes::Migrate};
use crate::control::uploads::{self, NewUpload, Upload, UploadMeta, UploadPurpose};
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Path as UrlPath};
use crate::ids::{new_ulid, now_ms};
use crate::state::{AppState, blocking};

/// The tus version the server speaks.
pub const TUS_VERSION: &str = "1.0.0";
/// `Tus-Resumable`.
pub const TUS_RESUMABLE: HeaderName = HeaderName::from_static("tus-resumable");
/// `Tus-Version`, sent with a 412.
pub const TUS_VERSION_HEADER: HeaderName = HeaderName::from_static("tus-version");
/// `Upload-Length`.
pub const UPLOAD_LENGTH: HeaderName = HeaderName::from_static("upload-length");
/// `Upload-Offset`.
pub const UPLOAD_OFFSET: HeaderName = HeaderName::from_static("upload-offset");
/// `Upload-Metadata`.
pub const UPLOAD_METADATA: HeaderName = HeaderName::from_static("upload-metadata");
/// The content type of a `PATCH` body.
pub const OFFSET_OCTET_STREAM: &str = "application/offset+octet-stream";

/// How long an unfinished upload is kept (§2.5: TTL 24 h).
pub const UPLOAD_TTL: Duration = Duration::from_secs(24 * 3600);
/// Unfinished uploads a user may have at once.
pub const MAX_UNFINISHED: u64 = 16;
/// Largest migration object: a kept video (§2.12).
pub const MAX_OBJECT_BYTES: u64 = IngestLimits::VIDEO.max_bytes;
/// Largest migration database.
pub const MAX_DATABASE_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// The first bytes of every SQLite database file.
const SQLITE_MAGIC: &[u8; 16] = b"SQLite format 3\0";

/// Upload ids with a `PATCH` in progress in this process.
#[derive(Debug, Default)]
pub struct UploadLocks {
    writing: Mutex<HashSet<String>>,
}

impl UploadLocks {
    /// Marks `id` as being written; `None` when it already is.
    #[must_use]
    pub fn try_lock(self: &Arc<Self>, id: &str) -> Option<UploadGuard> {
        let mut writing = self.writing.lock().unwrap_or_else(PoisonError::into_inner);
        writing.insert(id.to_owned()).then(|| UploadGuard {
            locks: Arc::clone(self),
            id: id.to_owned(),
        })
    }
}

/// Releases an upload when dropped.
#[derive(Debug)]
pub struct UploadGuard {
    locks: Arc<UploadLocks>,
    id: String,
}

impl Drop for UploadGuard {
    fn drop(&mut self) {
        self.locks
            .writing
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// An upload, as `POST /uploads` answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct UploadCreated {
    /// Upload id (ULID); the upload's URL is `/api/v1/uploads/{id}`.
    pub id: String,
    /// Declared length in bytes.
    pub length: u64,
    /// Bytes received so far (0).
    pub offset: u64,
}

/// The tus headers of `POST /uploads`, for the OpenAPI document; the handler
/// reads them itself.
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct CreationHeaders {
    /// `1.0.0`.
    #[param(rename = "Tus-Resumable")]
    pub tus_resumable: String,
    /// Length of the whole upload, in bytes.
    #[param(rename = "Upload-Length")]
    pub upload_length: u64,
    /// Comma-separated `key base64(value)` pairs: `purpose`
    /// (`migration-object` or `migration-db`), `sha256` (lowercase hex) and,
    /// for an object, `ext`.
    #[param(rename = "Upload-Metadata")]
    pub upload_metadata: String,
}

/// The tus header of `HEAD /uploads/{id}`, for the OpenAPI document.
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct TusHeaders {
    /// `1.0.0`.
    #[param(rename = "Tus-Resumable")]
    pub tus_resumable: String,
}

/// The tus headers of `PATCH /uploads/{id}`, for the OpenAPI document.
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct AppendHeaders {
    /// `1.0.0`.
    #[param(rename = "Tus-Resumable")]
    pub tus_resumable: String,
    /// Where the body goes: the upload's current offset.
    #[param(rename = "Upload-Offset")]
    pub upload_offset: u64,
}

/// Creates an upload (tus creation).
///
/// Answers 201 with `Location: /api/v1/uploads/{id}`. 412 without
/// `Tus-Resumable: 1.0.0`; 400 without a valid `Upload-Length`; 422 for bad
/// metadata; 413 when the length is over the purpose's limit (300 MiB for an
/// object, 4 GiB for a database); 409 with too many unfinished uploads.
#[utoipa::path(
    post,
    path = "/api/v1/uploads",
    tag = "migration",
    operation_id = "createUpload",
    security(("bearer" = ["migrate"])),
    params(CreationHeaders),
    responses(
        (
            status = CREATED,
            description = "The upload exists; send its bytes with PATCH.",
            body = UploadCreated,
            headers(
                ("Location" = String, description = "`/api/v1/uploads/{id}`."),
                ("Tus-Resumable" = String, description = "`1.0.0`."),
            )
        ),
    )
)]
pub async fn create_upload(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_tus(&headers)?;
    let length = header_u64(&headers, &UPLOAD_LENGTH)?
        .filter(|&n| n > 0)
        .ok_or_else(|| {
            ApiError::new(ErrorCode::BadRequest)
                .with_detail("Upload-Length must be a positive integer")
        })?;
    let (purpose, meta) = parse_metadata(&headers)?;
    let limit = match purpose {
        UploadPurpose::MigrationObject => MAX_OBJECT_BYTES,
        UploadPurpose::MigrationDb => MAX_DATABASE_BYTES,
    };
    if length > limit {
        return Err(
            ApiError::new(ErrorCode::PayloadTooLarge).with_detail(format!(
                "Upload-Length is over {limit} bytes for this purpose"
            )),
        );
    }
    let length = i64::try_from(length).map_err(ApiError::internal)?;

    let user_id = caller.id().to_owned();
    let dir = state.config().data_dir.uploads_dir();
    let control = Arc::clone(state.control());
    let id = new_ulid();
    let now = now_ms();
    let expires_at = now.saturating_add(crate::auth::millis(UPLOAD_TTL));
    let created = {
        let id = id.clone();
        blocking(move || -> Result<(), ApiError> {
            sweep_expired(&control, &dir, &user_id, now)?;
            let unfinished = control.read(|c| uploads::count_unfinished(c, &user_id, now))?;
            if unfinished >= MAX_UNFINISHED {
                return Err(ApiError::new(ErrorCode::Conflict).with_detail(
                    "too many unfinished uploads: finish or wait for them to expire",
                ));
            }
            fs::create_dir_all(&dir).map_err(ApiError::internal)?;
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(uploads::file_path(&dir, &id, false))
                .map_err(ApiError::internal)?;
            let new = NewUpload {
                id: &id,
                user_id: &user_id,
                purpose,
                length,
                meta: &meta,
                expires_at,
            };
            control.write(|tx| uploads::insert(tx, &new, now))?;
            Ok(())
        })
    };
    created.await?;
    let mut response = (
        StatusCode::CREATED,
        Json(UploadCreated {
            id: id.clone(),
            length: u64::try_from(length).unwrap_or(0),
            offset: 0,
        }),
    )
        .into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/uploads/{id}")).map_err(ApiError::internal)?,
    );
    headers.insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    Ok(no_store(response))
}

/// Where an upload stands, so a client can resume it (tus core).
///
/// 200 with `Upload-Offset` and `Upload-Length`; no body, errors included:
/// 412 without `Tus-Resumable: 1.0.0`, 404 for an unknown, expired or
/// another user's upload.
#[utoipa::path(
    head,
    path = "/api/v1/uploads/{id}",
    tag = "migration",
    operation_id = "getUploadOffset",
    security(("bearer" = ["migrate"])),
    params(("id" = String, Path, description = "Upload id."), TusHeaders),
    responses(
        (
            status = OK,
            description = "Where the upload stands; no body.",
            headers(
                ("Upload-Offset" = u64, description = "Bytes received: continue from here."),
                ("Upload-Length" = u64, description = "Declared length."),
                ("Tus-Resumable" = String, description = "`1.0.0`."),
            )
        ),
    )
)]
pub async fn upload_offset(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_tus(&headers)?;
    let upload = find(&state, caller.id(), &id).await?;
    let mut response = StatusCode::OK.into_response();
    let headers = response.headers_mut();
    headers.insert(UPLOAD_OFFSET, HeaderValue::from(upload.offset));
    headers.insert(UPLOAD_LENGTH, HeaderValue::from(upload.length));
    headers.insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    Ok(no_store(response))
}

/// Appends bytes to an upload at its offset (tus core).
///
/// The body (`application/offset+octet-stream`, at most 16 MiB) goes at
/// `Upload-Offset`, which must be the upload's current offset (409
/// otherwise, with the current `Upload-Offset`). Bytes that arrive before a
/// connection breaks are kept. When the last byte arrives the server checks
/// the declared SHA-256 and type: a mismatch deletes the upload (422).
#[utoipa::path(
    patch,
    path = "/api/v1/uploads/{id}",
    tag = "migration",
    operation_id = "appendUpload",
    security(("bearer" = ["migrate"])),
    params(("id" = String, Path, description = "Upload id."), AppendHeaders),
    request_body(
        content = Vec<u8>,
        content_type = "application/offset+octet-stream",
        description = "The next bytes of the upload."
    ),
    responses(
        (
            status = NO_CONTENT,
            description = "The bytes are stored.",
            headers(
                ("Upload-Offset" = u64, description = "The new offset."),
                ("Tus-Resumable" = String, description = "`1.0.0`."),
            )
        ),
    )
)]
pub async fn append_upload(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Extension(locks): Extension<Arc<UploadLocks>>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    require_tus(&headers)?;
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !content_type.eq_ignore_ascii_case(OFFSET_OCTET_STREAM) {
        return Err(ApiError::new(ErrorCode::UnsupportedMediaType)
            .with_detail("the body must be application/offset+octet-stream"));
    }
    let offset = header_u64(&headers, &UPLOAD_OFFSET)?.ok_or_else(|| {
        ApiError::new(ErrorCode::BadRequest).with_detail("Upload-Offset is required")
    })?;
    let Some(_writing) = locks.try_lock(&id) else {
        return Err(ApiError::new(ErrorCode::Conflict)
            .with_detail("another request is writing this upload"));
    };
    let upload = find(&state, caller.id(), &id).await?;
    let length = u64::try_from(upload.length).unwrap_or(0);
    let current = u64::try_from(upload.offset).unwrap_or(0);
    if upload.is_complete() {
        // A repeated final request: nothing left to write.
        return if offset == length {
            Ok(appended(length))
        } else {
            Err(offset_conflict(length))
        };
    }
    if offset != current {
        return Err(offset_conflict(current));
    }

    let dir = state.config().data_dir.uploads_dir();
    let part = uploads::file_path(&dir, &id, false);
    let file = {
        let part = part.clone();
        blocking(move || open_at(&part, current)).await?
    };
    let Some(file) = file else {
        // The row outlived its file (a crash during cleanup): drop both.
        forget(&state, &dir, &upload).await;
        return Err(ApiError::not_found());
    };
    let mut file = tokio::fs::File::from_std(file);
    let mut written = 0u64;
    let mut failure = None;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = match chunk {
            Ok(chunk) => chunk,
            Err(_) => {
                failure = Some(ApiError::new(ErrorCode::BadRequest).with_detail(
                    "the body ended early: HEAD the upload and continue from its Upload-Offset",
                ));
                break;
            }
        };
        if current + written + chunk.len() as u64 > length {
            failure = Some(
                ApiError::new(ErrorCode::PayloadTooLarge)
                    .with_detail("the body goes past Upload-Length"),
            );
            break;
        }
        if let Err(e) = file.write_all(&chunk).await {
            failure = Some(ApiError::internal(e));
            break;
        }
        written += chunk.len() as u64;
    }
    // What arrived is kept, also when the body broke off.
    file.sync_data().await.map_err(ApiError::internal)?;
    drop(file);
    let next = current + written;
    if written > 0 {
        let control = Arc::clone(state.control());
        let (id, from, to) = (
            id.clone(),
            upload.offset,
            i64::try_from(next).map_err(ApiError::internal)?,
        );
        let moved =
            blocking(move || control.write(|tx| uploads::advance(tx, &id, from, to))).await?;
        if !moved {
            return Err(offset_conflict(current));
        }
    }
    if let Some(err) = failure {
        return Err(err);
    }
    if next == length {
        finish(&state, &dir, &upload).await?;
    }
    Ok(appended(next))
}

/// Terminates an upload (tus termination): its row and its bytes are
/// removed, whether it was complete or not.
///
/// 204; 412 without `Tus-Resumable: 1.0.0`; 404 for an unknown or another
/// user's upload; 409 while a `PATCH` writes it.
#[utoipa::path(
    delete,
    path = "/api/v1/uploads/{id}",
    tag = "migration",
    operation_id = "deleteUpload",
    security(("bearer" = ["migrate"])),
    params(("id" = String, Path, description = "Upload id."), TusHeaders),
    responses(
        (
            status = NO_CONTENT,
            description = "The upload is gone.",
            headers(("Tus-Resumable" = String, description = "`1.0.0`.")),
        ),
    )
)]
pub async fn delete_upload(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Extension(locks): Extension<Arc<UploadLocks>>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_tus(&headers)?;
    let Some(_writing) = locks.try_lock(&id) else {
        return Err(ApiError::new(ErrorCode::Conflict)
            .with_detail("another request is writing this upload"));
    };
    let control = Arc::clone(state.control());
    let (user_id, upload_id) = (caller.id().to_owned(), id.clone());
    let upload = blocking(move || control.read(|c| uploads::get(c, &user_id, &upload_id)))
        .await?
        .ok_or_else(ApiError::not_found)?;
    let dir = state.config().data_dir.uploads_dir();
    let control = Arc::clone(state.control());
    blocking(move || -> Result<(), ApiError> {
        control.write(|tx| uploads::delete(tx, std::slice::from_ref(&upload.id)))?;
        remove_files(&dir, std::slice::from_ref(&upload.id));
        Ok(())
    })
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    Ok(no_store(response))
}

/// The 204 of a stored chunk.
fn appended(offset: u64) -> Response {
    let mut response = StatusCode::NO_CONTENT.into_response();
    let headers = response.headers_mut();
    headers.insert(UPLOAD_OFFSET, HeaderValue::from(offset));
    headers.insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    no_store(response)
}

/// 409 for a wrong `Upload-Offset`, telling the right one.
fn offset_conflict(offset: u64) -> ApiError {
    ApiError::new(ErrorCode::Conflict).with_detail(format!("Upload-Offset must be {offset}"))
}

/// Checks the completed bytes against the declared hash and type, then
/// moves them out of `.part` and marks the upload complete.
async fn finish(state: &AppState, dir: &Path, upload: &Upload) -> Result<(), ApiError> {
    let part = uploads::file_path(dir, &upload.id, false);
    let checked = {
        let (part, upload) = (part.clone(), upload.clone());
        blocking(move || check_content(&part, &upload).map_err(ApiError::internal)).await?
    };
    if let Err(problem) = checked {
        forget(state, dir, upload).await;
        return Err(problem);
    }
    let control = Arc::clone(state.control());
    let (dir, upload) = (dir.to_path_buf(), upload.clone());
    blocking(move || -> Result<(), ApiError> {
        let done = uploads::file_path(&dir, &upload.id, true);
        fs::rename(&part, &done).map_err(ApiError::internal)?;
        sync_dir(&dir).map_err(ApiError::internal)?;
        control.write(|tx| uploads::complete(tx, &upload.id, now_ms()))?;
        Ok(())
    })
    .await
}

/// Whether the file holds what the upload declared: `Ok(Err(problem))` when
/// it does not.
fn check_content(path: &Path, upload: &Upload) -> io::Result<Result<(), ApiError>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut head = Vec::with_capacity(SNIFF_LEN);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if head.len() < SNIFF_LEN {
            let wanted = (SNIFF_LEN - head.len()).min(n);
            head.extend_from_slice(&buffer[..wanted]);
        }
        hasher.update(&buffer[..n]);
    }
    file.sync_all()?;
    let digest = Digest::from_bytes(hasher.finalize().into());
    if digest.to_string() != upload.meta.sha256 {
        return Ok(Err(ApiError::invalid_field(
            "sha256",
            "the uploaded bytes have another SHA-256: upload them again",
        )));
    }
    let typed = match upload.purpose {
        Some(UploadPurpose::MigrationObject) => {
            let declared = upload.meta.ext.as_deref().and_then(MediaKind::from_ext);
            declared.is_some() && MediaKind::sniff(&head) == declared
        }
        Some(UploadPurpose::MigrationDb) => head.starts_with(SQLITE_MAGIC),
        None => false,
    };
    if !typed {
        return Ok(Err(ApiError::invalid_field(
            "ext",
            "the uploaded bytes are not of the declared type",
        )));
    }
    Ok(Ok(()))
}

/// The upload `id` of `user_id`, unexpired if unfinished; 404 otherwise.
async fn find(state: &AppState, user_id: &str, id: &str) -> Result<Upload, ApiError> {
    let control = Arc::clone(state.control());
    let (user_id, id) = (user_id.to_owned(), id.to_owned());
    let found = blocking(move || control.read(|c| uploads::get(c, &user_id, &id))).await?;
    match found {
        Some(upload) if upload.is_complete() || upload.expires_at > now_ms() => Ok(upload),
        _ => Err(ApiError::not_found()),
    }
}

/// Deletes an upload's row and file, best effort.
async fn forget(state: &AppState, dir: &Path, upload: &Upload) {
    let control = Arc::clone(state.control());
    let (dir, upload) = (dir.to_path_buf(), upload.clone());
    let removed = blocking(move || -> Result<(), ApiError> {
        remove_files(&dir, std::slice::from_ref(&upload.id));
        control.write(|tx| uploads::delete(tx, &[upload.id]))?;
        Ok(())
    })
    .await;
    if let Err(err) = removed {
        tracing::warn!(error = %err, "cannot delete a refused upload");
    }
}

/// Deletes `user_id`'s unfinished uploads past their expiry, rows and files.
/// Blocking.
fn sweep_expired(
    control: &shelfy_core::db::ControlDb,
    dir: &Path,
    user_id: &str,
    now: i64,
) -> Result<(), ApiError> {
    let expired = control.read(|c| uploads::expired(c, user_id, now))?;
    if expired.is_empty() {
        return Ok(());
    }
    remove_files(dir, &expired);
    control.write(|tx| uploads::delete(tx, &expired))?;
    Ok(())
}

/// Removes the files of the uploads `ids`, in progress or complete; missing
/// files are fine. Blocking.
pub fn remove_files(dir: &Path, ids: &[String]) {
    for id in ids {
        for complete in [false, true] {
            match fs::remove_file(uploads::file_path(dir, id, complete)) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => tracing::warn!(error = %e, "cannot remove an upload file"),
            }
        }
    }
}

/// Opens the `.part` file for appending at `offset`, cutting bytes past it
/// (written before a crash, never acknowledged). `None` when it is gone.
fn open_at(path: &PathBuf, offset: u64) -> Result<Option<fs::File>, ApiError> {
    use std::io::{Seek as _, SeekFrom};
    let mut file = match fs::OpenOptions::new().write(true).open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(ApiError::internal(e)),
    };
    let size = file.metadata().map_err(ApiError::internal)?.len();
    if size != offset {
        file.set_len(offset).map_err(ApiError::internal)?;
    }
    file.seek(SeekFrom::Start(offset))
        .map_err(ApiError::internal)?;
    Ok(Some(file))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(dir)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Ok(())
    }
}

/// 412 unless the request speaks tus 1.0.0.
fn require_tus(headers: &HeaderMap) -> Result<(), ApiError> {
    if headers
        .get(&TUS_RESUMABLE)
        .is_some_and(|v| v.as_bytes() == TUS_VERSION.as_bytes())
    {
        Ok(())
    } else {
        Err(ApiError::from_status(StatusCode::PRECONDITION_FAILED)
            .with_detail("this server speaks tus 1.0.0: send Tus-Resumable: 1.0.0"))
    }
}

/// A header holding a non-negative integer; `None` when absent.
fn header_u64(headers: &HeaderMap, name: &HeaderName) -> Result<Option<u64>, ApiError> {
    let Some(value) = headers.get(name) else {
        return Ok(None);
    };
    value
        .to_str()
        .ok()
        .filter(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|v| v.parse().ok())
        .map(Some)
        .ok_or_else(|| {
            ApiError::new(ErrorCode::BadRequest)
                .with_detail(format!("{name} must be a non-negative integer"))
        })
}

/// The purpose and the declared content of `Upload-Metadata`.
fn parse_metadata(headers: &HeaderMap) -> Result<(UploadPurpose, UploadMeta), ApiError> {
    let raw = headers
        .get(&UPLOAD_METADATA)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let mut purpose = None;
    let mut sha256 = None;
    let mut ext = None;
    let mut seen = HashSet::new();
    for pair in raw.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (key, value) = pair.split_once(' ').unwrap_or((pair, ""));
        if !seen.insert(key) {
            return Err(ApiError::invalid_field(
                "Upload-Metadata",
                "a key is repeated",
            ));
        }
        let value = STANDARD
            .decode(value.trim())
            .ok()
            .and_then(|bytes| String::from_utf8(bytes).ok())
            .ok_or_else(|| {
                ApiError::invalid_field("Upload-Metadata", "values must be base64 UTF-8")
            })?;
        match key {
            "purpose" => purpose = Some(value),
            "sha256" => sha256 = Some(value),
            "ext" => ext = Some(value),
            _ => {} // tus clients may send more (filename, filetype): ignored
        }
    }
    let purpose = purpose
        .as_deref()
        .and_then(UploadPurpose::parse)
        .ok_or_else(|| {
            ApiError::invalid_field("purpose", "must be migration-object or migration-db")
        })?;
    let sha256 = sha256
        .filter(|s| Digest::parse_hex(s).is_some())
        .ok_or_else(|| ApiError::invalid_field("sha256", "must be 64 lowercase hex digits"))?;
    let ext = match purpose {
        UploadPurpose::MigrationObject => Some(
            ext.filter(|e| MediaKind::from_ext(e).is_some())
                .ok_or_else(|| ApiError::invalid_field("ext", "must be a stored media type"))?,
        ),
        UploadPurpose::MigrationDb => None,
    };
    Ok((purpose, UploadMeta { sha256, ext }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        map
    }

    fn meta(pairs: &[(&str, &str)]) -> String {
        pairs
            .iter()
            .map(|(k, v)| format!("{k} {}", STANDARD.encode(v)))
            .collect::<Vec<_>>()
            .join(",")
    }

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn metadata_names_a_purpose_a_hash_and_a_type() {
        let ok = headers(&[(
            "upload-metadata",
            &meta(&[
                ("purpose", "migration-object"),
                ("sha256", SHA),
                ("ext", "jpg"),
            ]),
        )]);
        let (purpose, parsed) = parse_metadata(&ok).unwrap();
        assert_eq!(purpose, UploadPurpose::MigrationObject);
        assert_eq!(parsed.sha256, SHA);
        assert_eq!(parsed.ext.as_deref(), Some("jpg"));

        let db = headers(&[(
            "upload-metadata",
            &meta(&[
                ("purpose", "migration-db"),
                ("sha256", SHA),
                ("filename", "x"),
            ]),
        )]);
        assert_eq!(parse_metadata(&db).unwrap().1.ext, None);

        for bad in [
            meta(&[("purpose", "bookmark"), ("sha256", SHA)]),
            meta(&[("purpose", "migration-object"), ("sha256", SHA)]),
            meta(&[
                ("purpose", "migration-object"),
                ("sha256", SHA),
                ("ext", "svg"),
            ]),
            meta(&[("purpose", "migration-db"), ("sha256", &SHA.to_uppercase())]),
            format!("purpose {},purpose x", STANDARD.encode("migration-db")),
            "purpose !!!".to_owned(),
            String::new(),
        ] {
            let err = parse_metadata(&headers(&[("upload-metadata", &bad)])).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{bad}");
        }
    }

    #[test]
    fn tus_headers_are_checked() {
        assert!(require_tus(&headers(&[("tus-resumable", "1.0.0")])).is_ok());
        for bad in [headers(&[]), headers(&[("tus-resumable", "0.2.2")])] {
            assert_eq!(
                require_tus(&bad).unwrap_err().status(),
                StatusCode::PRECONDITION_FAILED
            );
        }
        let h = headers(&[("upload-offset", "42"), ("upload-length", "-1")]);
        assert_eq!(header_u64(&h, &UPLOAD_OFFSET).unwrap(), Some(42));
        assert!(header_u64(&h, &UPLOAD_LENGTH).is_err());
        assert_eq!(header_u64(&h, &UPLOAD_METADATA).unwrap(), None);
    }

    #[test]
    fn one_patch_at_a_time_per_upload() {
        let locks = Arc::new(UploadLocks::default());
        let guard = locks.try_lock("U1").unwrap();
        assert!(locks.try_lock("U1").is_none());
        assert!(locks.try_lock("U2").is_some());
        drop(guard);
        assert!(locks.try_lock("U1").is_some());
    }
}
