//! `/api/v1/uploads`: resumable uploads, tus 1.0 core + creation +
//! termination (plan §2.9), for the migration CLI (T9, P1-19), the web app
//! and API tokens (P4-08).
//!
//! | Request | Answer |
//! |---|---|
//! | `POST /uploads` with `Upload-Length` and `Upload-Metadata` | 201, `Location: /api/v1/uploads/{id}` |
//! | `HEAD /uploads/{id}` | 200 with `Upload-Offset` and `Upload-Length`: where to resume |
//! | `PATCH /uploads/{id}` with `Upload-Offset` and an `application/offset+octet-stream` body (≤16 MiB) | 204 with the new `Upload-Offset` |
//! | `DELETE /uploads/{id}` (tus termination) | 204: the upload and its bytes are gone |
//!
//! Every request carries `Tus-Resumable: 1.0.0` (412 otherwise). A `HEAD`
//! answer has no body, errors included: its status says what went wrong.
//!
//! **Who uploads what.** The purpose decides (`purpose` in
//! `Upload-Metadata`; the registry is [`UploadPurpose`]):
//!
//! | Purpose | Who | Largest | `sha256` | The bytes must be | A complete one waits |
//! |---|---|---|---|---|---|
//! | `migration-object` | a `migrate` token | 300 MiB | required | of the declared `ext` | 7 days |
//! | `migration-db` | a `migrate` token | 4 GiB | required | a SQLite database | 7 days |
//! | `bookmark-original` | a session or an `uploads` token | 200 MiB | optional | of a type of the store's allowlist, sniffed | 24 h |
//! | `bookmark-preview` | a session or an `uploads` token | 2 MiB | optional | a JPEG, PNG, GIF or WebP image | 24 h |
//! | `import` | a session or an `uploads` token | `SHELFY_IMPORT_MAX_GB` (10 GiB) | optional | a JSON document or a zip archive | 24 h |
//!
//! The routes take the three credentials ([`crate::routes::TOKEN_ROUTES`]);
//! every request checks the caller against the upload's purpose
//! ([`Caller`]), and any other pairing is 403. An unknown purpose is 422.
//! Another user's upload is 404. A session follows the CSRF rules
//! ([`crate::auth::csrf`]): `POST`, `PATCH` and `DELETE` carry
//! `X-Shelfy-Client: web` and the public URL as `Origin`, and `PATCH` bodies
//! stay `application/offset+octet-stream`. `HEAD` changes nothing and is
//! not checked, as browsers send no `Origin` on it.
//!
//! **Metadata.** `purpose`; `sha256`, the content's SHA-256 in lowercase hex;
//! for a migration object, `ext`, its type in the store's allowlist; and
//! `filename`, which tus clients send and the server keeps as a label
//! ([`clean_filename`]). Other keys (`filetype`) are ignored: a declared type
//! decides nothing where the server sniffs.
//!
//! **Completion.** When the last byte arrives, the server hashes the bytes
//! and checks them against the purpose. A declared SHA-256 that does not
//! match deletes the upload (422 `sha256`), and so do bytes the purpose does
//! not take: 422 `ext` when they are not of the declared type, 415
//! `unsupported_media_type` when they are of no type it takes. Then it
//! records the SHA-256 and the type it found (PG19: web clients need not
//! hash). The client starts a refused upload over.
//!
//! **Bytes.** They go to `<data>/work/uploads/<id>.part` (§2.5); a PATCH
//! that breaks off keeps what arrived, and `HEAD` tells the client where to
//! continue. One PATCH at a time per upload: a concurrent one gets 409.
//!
//! **Limits.** A user has at most [`MAX_UNFINISHED`] unfinished uploads, and
//! the uploads of purposes other than the migration's, unfinished or waiting
//! to be used, hold at most [`max_staged_bytes`] (the largest import plus
//! 1 GiB): a creation past either answers 409. A purpose that counts
//! against the quota refuses an upload that could not fit at once:
//! [`crate::quota::check`], against the user's quota (`quota_exceeded`) and
//! the media budget (`storage_full`); the consumer reserves for real.
//!
//! **Consumers.** A complete upload of a purpose other than the migration's
//! is used once: its consumer (P4-10 imports, P4-18 bookmarks, P2-14
//! archive objects) calls [`claim`], reads the bytes, then [`discard`]s them,
//! or [`release`]s the upload if it could not use it. A second claim, and
//! every tus request on a claimed upload, answers 409 `upload_consumed`.
//!
//! **Expiry.** Unfinished uploads expire 24 h after creation; complete ones
//! after their purpose's wait; claimed ones a week after the claim. Creating
//! an upload sweeps the user's expired unfinished ones; the hourly
//! housekeeping sweeps every user's ([`crate::migrations::housekeeping`]).

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
use shelfy_core::db::{ControlDb, DbError};
use shelfy_core::repo::RepoError;
use shelfy_media::{Digest, MediaKind};
use tokio::io::AsyncWriteExt as _;
use utoipa::{IntoParams, ToSchema};

use super::auth::no_store;
use crate::auth::bearer::Scope;
use crate::auth::caller::Caller;
use crate::config::Config;
use crate::control::uploads::{
    self, ClaimRefusal, Content, ContentRefusal, Found, HashRule, NewUpload, Terminated, Upload,
    UploadMeta, UploadPurpose, clean_filename,
};
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Path as UrlPath};
use crate::ids::{new_ulid, now_ms};
use crate::quota;
use crate::state::{AppState, blocking};

pub use crate::control::uploads::{MAX_DATABASE_BYTES, MAX_OBJECT_BYTES};

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

/// The token scopes the upload routes take ([`crate::routes::TOKEN_ROUTES`]):
/// those of every purpose's uploaders. The routes take the session too.
pub const UPLOAD_SCOPES: &[Scope] = &[Scope::Uploads, Scope::Migrate];

/// How long an unfinished upload is kept (§2.5: TTL 24 h).
pub const UPLOAD_TTL: Duration = Duration::from_secs(24 * 3600);
/// Unfinished uploads a user may have at once.
pub const MAX_UNFINISHED: u64 = 16;
/// What the staging cap allows beyond the largest import: a bookmark's files
/// (12 of at most 500 MiB in total, §1.2 #16) and their previews.
pub const STAGING_HEADROOM: u64 = 1024 * 1024 * 1024;

/// The most bytes a user's uploads of purposes other than the migration's
/// ([`UploadPurpose::staged`]) may hold at once, unfinished or waiting to be
/// used: the largest import plus [`STAGING_HEADROOM`].
#[must_use]
pub fn max_staged_bytes(config: &Config) -> u64 {
    config.import_max_bytes.saturating_add(STAGING_HEADROOM)
}

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
    /// (`migration-object`, `migration-db`, `bookmark-original`,
    /// `bookmark-preview` or `import`); `sha256` (lowercase hex; required for
    /// the migration's purposes); for a migration object, `ext`; optionally
    /// `filename`, kept as a label.
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
/// metadata or an unknown purpose; 403 when the purpose is not the caller's
/// (the migration's need a `migrate` token; bookmarks and imports a session
/// or an `uploads` token); 413 over the purpose's size; 409 with too many
/// unfinished uploads, or too many bytes waiting; 403 `quota_exceeded` when
/// a bookmark could not fit in the quota. A session sends `X-Shelfy-Client:
/// web` and the public `Origin`.
#[utoipa::path(
    post,
    path = "/api/v1/uploads",
    tag = "uploads",
    operation_id = "createUpload",
    security(("session" = []), ("bearer" = ["uploads"]), ("bearer" = ["migrate"])),
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
    caller: Caller,
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
    let metadata = Metadata::read(&headers)?;
    let purpose = metadata.purpose()?;
    require_uploader(&caller, Some(purpose))?;
    let meta = metadata.declared(purpose)?;
    let limit = purpose.byte_limit(state.config().import_max_bytes);
    if length > limit {
        return Err(
            ApiError::new(ErrorCode::PayloadTooLarge).with_detail(format!(
                "Upload-Length is over {limit} bytes for this purpose"
            )),
        );
    }
    if purpose.quota {
        // Before the bytes are sent: the consumer would refuse them after
        // they all arrived. It reserves for real when it stores them (P4
        // lane rule 6); this holds nothing.
        quota::check(&state, caller.id(), length).await?;
    }
    let length = i64::try_from(length).map_err(ApiError::internal)?;

    let user_id = caller.id().to_owned();
    let dir = state.config().data_dir.uploads_dir();
    let max_staged = max_staged_bytes(state.config());
    let control = Arc::clone(state.control());
    let id = new_ulid();
    let now = now_ms();
    let expires_at = now.saturating_add(crate::auth::millis(UPLOAD_TTL));
    let created = {
        let id = id.clone();
        blocking(move || -> Result<(), ApiError> {
            sweep_expired(&control, &dir, &user_id, now)?;
            fs::create_dir_all(&dir).map_err(ApiError::internal)?;
            let part = uploads::file_path(&dir, &id, false);
            fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&part)
                .map_err(ApiError::internal)?;
            let new = NewUpload {
                id: &id,
                user_id: &user_id,
                purpose,
                length,
                meta: &meta,
                expires_at,
            };
            // The caps and the insert in one transaction: two creations at
            // once cannot both slip under a cap.
            let admitted = control.write(|tx| -> Result<(), ApiError> {
                if uploads::count_unfinished(tx, &user_id, now)? >= MAX_UNFINISHED {
                    return Err(ApiError::new(ErrorCode::Conflict).with_detail(
                        "too many unfinished uploads: finish or wait for them to expire",
                    ));
                }
                if purpose.staged
                    && uploads::staged_bytes(tx, &user_id, now)?
                        .saturating_add(length.unsigned_abs())
                        > max_staged
                {
                    return Err(ApiError::new(ErrorCode::Conflict).with_detail(
                        "the uploads waiting to be used hold too many bytes: use or delete them",
                    ));
                }
                uploads::insert(tx, &new, now)?;
                Ok(())
            });
            if admitted.is_err() {
                remove_files(&dir, std::slice::from_ref(&id));
            }
            admitted
        })
    };
    created.await?;
    let mut response = (
        StatusCode::CREATED,
        Json(UploadCreated {
            id: id.clone(),
            length: length.unsigned_abs(),
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
/// another user's upload, 403 when its purpose is not the caller's, 409 once
/// a consumer used it.
#[utoipa::path(
    head,
    path = "/api/v1/uploads/{id}",
    tag = "uploads",
    operation_id = "getUploadOffset",
    security(("session" = []), ("bearer" = ["uploads"]), ("bearer" = ["migrate"])),
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
    caller: Caller,
    State(state): State<AppState>,
    UrlPath(id): UrlPath<String>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    require_tus(&headers)?;
    let upload = find(&state, &caller, &id).await?;
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
/// them against the purpose: a declared SHA-256 that does not match deletes
/// the upload (422 `sha256`), and so do bytes of another type (422 `ext`
/// against a declared type, 415 `unsupported_media_type` otherwise).
#[utoipa::path(
    patch,
    path = "/api/v1/uploads/{id}",
    tag = "uploads",
    operation_id = "appendUpload",
    security(("session" = []), ("bearer" = ["uploads"]), ("bearer" = ["migrate"])),
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
    caller: Caller,
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
    let upload = find(&state, &caller, &id).await?;
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
/// user's upload; 403 when its purpose is not the caller's; 409 while a
/// `PATCH` writes it, and once a consumer used it (`upload_consumed`).
#[utoipa::path(
    delete,
    path = "/api/v1/uploads/{id}",
    tag = "uploads",
    operation_id = "deleteUpload",
    security(("session" = []), ("bearer" = ["uploads"]), ("bearer" = ["migrate"])),
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
    caller: Caller,
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
    require_uploader(&caller, upload.purpose)?;
    let dir = state.config().data_dir.uploads_dir();
    let control = Arc::clone(state.control());
    let user_id = caller.id().to_owned();
    blocking(move || -> Result<(), ApiError> {
        match control.write(|tx| uploads::terminate(tx, &user_id, &upload.id))? {
            Terminated::Deleted => {
                remove_files(&dir, std::slice::from_ref(&upload.id));
                Ok(())
            }
            Terminated::Missing => Err(ApiError::not_found()),
            Terminated::Consumed => Err(consumed(&upload.id)),
        }
    })
    .await?;
    let mut response = StatusCode::NO_CONTENT.into_response();
    response
        .headers_mut()
        .insert(TUS_RESUMABLE, HeaderValue::from_static(TUS_VERSION));
    Ok(no_store(response))
}

/// A complete upload that a consumer claimed ([`claim`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claimed {
    /// Upload id.
    pub id: String,
    /// Where the bytes are until [`discard`] or [`release`]: read them,
    /// never write, move or delete them.
    pub path: PathBuf,
    /// Their size, in bytes.
    pub length: u64,
    /// Their SHA-256, checked or computed when the upload completed.
    pub sha256: Digest,
    /// What they are, as the purpose's check found them.
    pub found: Found,
    /// The file name the client gave, cleaned: untrusted, a label at most.
    pub filename: Option<String>,
}

impl Claimed {
    /// The media type, for a media purpose.
    #[must_use]
    pub fn media_kind(&self) -> Option<MediaKind> {
        match self.found {
            Found::Media(kind) => Some(kind),
            _ => None,
        }
    }
}

/// Why [`claim`] took nothing.
#[derive(Debug)]
pub enum ClaimError {
    /// The upload `id` is not a complete upload of the user with the
    /// purpose, within its wait: missing, another user's, unfinished,
    /// expired, of another purpose, or without its bytes.
    Unusable {
        /// The upload.
        id: String,
    },
    /// The upload `id` was used before (or appears twice).
    Consumed {
        /// The upload.
        id: String,
    },
    /// The control database failed.
    Failed(ApiError),
}

impl ClaimError {
    /// The problem to answer: 422 `validation_failed` on `field` of the
    /// consumer's request for an unusable upload, 409 `upload_consumed` for a
    /// used one.
    #[must_use]
    pub fn into_problem(self, field: &str) -> ApiError {
        match self {
            Self::Unusable { id } => ApiError::invalid_field(
                field,
                format!("{id} is not a complete upload of the right purpose"),
            ),
            Self::Consumed { id } => consumed(&id),
            Self::Failed(err) => err,
        }
    }
}

impl From<ClaimError> for ApiError {
    /// [`ClaimError::into_problem`] on the field `uploadId`.
    fn from(err: ClaimError) -> Self {
        err.into_problem("uploadId")
    }
}

impl From<DbError> for ClaimError {
    fn from(err: DbError) -> Self {
        Self::Failed(err.into())
    }
}

impl From<RepoError> for ClaimError {
    fn from(err: RepoError) -> Self {
        Self::Failed(err.into())
    }
}

/// Claims the complete uploads `ids` of `user_id` with `purpose` for one
/// consumer, all or none, and returns them in the order of `ids`. Each is
/// then used: a second claim, and every tus request on it, answers 409
/// `upload_consumed`. The bytes stay at [`Claimed::path`] until the consumer
/// calls [`discard`] (used) or [`release`] (not used after all); a crashed
/// consumer's are deleted a week after the claim.
///
/// # Errors
///
/// [`ClaimError`]; nothing is claimed then.
pub async fn claim(
    state: &AppState,
    user_id: &str,
    purpose: UploadPurpose,
    ids: &[String],
) -> Result<Vec<Claimed>, ClaimError> {
    let control = Arc::clone(state.control());
    let dir = state.config().data_dir.uploads_dir();
    let (user_id, ids) = (user_id.to_owned(), ids.to_vec());
    let now = now_ms();
    let claimed = tokio::task::spawn_blocking(move || {
        control.write(|tx| {
            let claimed = match uploads::claim(tx, &user_id, purpose, &ids, now)? {
                Ok(claimed) => claimed,
                Err(ClaimRefusal::Unusable(id)) => return Err(ClaimError::Unusable { id }),
                Err(ClaimRefusal::Consumed(id)) => return Err(ClaimError::Consumed { id }),
            };
            // An error rolls the claim back.
            claimed
                .into_iter()
                .map(|upload| claimed_of(&dir, upload))
                .collect::<Result<Vec<_>, _>>()
        })
    })
    .await;
    claimed.unwrap_or_else(|join| {
        Err(ClaimError::Failed(ApiError::internal(anyhow::anyhow!(
            "claiming uploads failed: {join}"
        ))))
    })
}

/// A claimed upload as its consumer sees it; unusable without its bytes.
fn claimed_of(dir: &Path, upload: Upload) -> Result<Claimed, ClaimError> {
    let path = uploads::file_path(dir, &upload.id, true);
    let (Some(sha256), Some(found)) = (upload.digest(), upload.found()) else {
        return Err(ClaimError::Unusable { id: upload.id });
    };
    if !fs::metadata(&path).is_ok_and(|m| m.is_file()) {
        return Err(ClaimError::Unusable { id: upload.id });
    }
    Ok(Claimed {
        length: u64::try_from(upload.length).unwrap_or(0),
        filename: upload.meta.filename,
        id: upload.id,
        path,
        sha256,
        found,
    })
}

/// Gives back claimed uploads `ids` of `user_id` that their consumer could
/// not use (it failed before storing anything): they can be claimed again
/// until they expire.
///
/// # Errors
///
/// The control database failed.
pub async fn release(state: &AppState, user_id: &str, ids: &[String]) -> Result<(), ApiError> {
    let control = Arc::clone(state.control());
    let (user_id, ids) = (user_id.to_owned(), ids.to_vec());
    blocking(move || control.write(|tx| uploads::release(tx, &user_id, &ids))).await?;
    Ok(())
}

/// Deletes the bytes of claimed uploads `ids` of `user_id` once their
/// consumer is done with them. The rows stay a week, so a reuse still
/// answers `upload_consumed`. Uploads that are not claimed are left alone.
///
/// # Errors
///
/// The control database failed.
pub async fn discard(state: &AppState, user_id: &str, ids: &[String]) -> Result<(), ApiError> {
    let control = Arc::clone(state.control());
    let dir = state.config().data_dir.uploads_dir();
    let (user_id, ids) = (user_id.to_owned(), ids.to_vec());
    blocking(move || -> Result<(), ApiError> {
        let claimed = control.write(|tx| uploads::discard(tx, &user_id, &ids))?;
        remove_files(&dir, &claimed);
        Ok(())
    })
    .await
}

/// 409 `upload_consumed`: a consumer used upload `id`.
fn consumed(id: &str) -> ApiError {
    ApiError::new(ErrorCode::UploadConsumed).with_detail(format!(
        "upload {id} was used already: upload the file again"
    ))
}

/// 403 unless `caller` may upload `purpose` (an unknown purpose: nobody).
fn require_uploader(caller: &Caller, purpose: Option<UploadPurpose>) -> Result<(), ApiError> {
    match purpose {
        Some(purpose) if caller.is_one_of(purpose.uploaders.session, purpose.uploaders.scopes) => {
            Ok(())
        }
        Some(purpose) => {
            let who = match (
                purpose.uploaders.session,
                purpose.uploaders.scopes.is_empty(),
            ) {
                (true, true) => "a session".to_owned(),
                (true, false) => format!(
                    "a session or a token with the {} scope",
                    purpose.uploaders.scopes
                ),
                (false, _) => format!("a token with the {} scope", purpose.uploaders.scopes),
            };
            Err(ApiError::new(ErrorCode::Forbidden)
                .with_detail(format!("{} uploads need {who}", purpose.as_str())))
        }
        None => Err(ApiError::new(ErrorCode::Forbidden)
            .with_detail("this upload's purpose is unknown to this server")),
    }
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

/// Checks the completed bytes against the upload's purpose, then moves them
/// out of `.part` and marks the upload complete with what was found.
async fn finish(state: &AppState, dir: &Path, upload: &Upload) -> Result<(), ApiError> {
    let part = uploads::file_path(dir, &upload.id, false);
    let inspected = {
        let (part, upload) = (part.clone(), upload.clone());
        blocking(move || inspect(&part, &upload).map_err(ApiError::internal)).await?
    };
    let meta = match inspected {
        Ok(meta) => meta,
        Err(problem) => {
            forget(state, dir, upload).await;
            return Err(problem);
        }
    };
    let control = Arc::clone(state.control());
    let (dir, upload) = (dir.to_path_buf(), upload.clone());
    blocking(move || -> Result<(), ApiError> {
        let done = uploads::file_path(&dir, &upload.id, true);
        fs::rename(&part, &done).map_err(ApiError::internal)?;
        sync_dir(&dir).map_err(ApiError::internal)?;
        control.write(|tx| uploads::complete(tx, &upload.id, &meta, now_ms()))?;
        Ok(())
    })
    .await
}

/// Hashes the bytes at `path` and checks them against `upload`'s purpose:
/// `Ok(Ok(meta))` with the hash and the type found, or `Ok(Err(problem))`
/// when they are not what the upload declared or its purpose takes.
fn inspect(path: &Path, upload: &Upload) -> io::Result<Result<UploadMeta, ApiError>> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut head = Vec::with_capacity(Content::HEAD_LEN);
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let n = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        if head.len() < Content::HEAD_LEN {
            let wanted = (Content::HEAD_LEN - head.len()).min(n);
            head.extend_from_slice(&buffer[..wanted]);
        }
        hasher.update(&buffer[..n]);
    }
    file.sync_all()?;
    let digest = Digest::from_bytes(hasher.finalize().into()).to_string();
    if !upload.meta.sha256.is_empty() && digest != upload.meta.sha256 {
        return Ok(Err(ApiError::invalid_field(
            "sha256",
            "the uploaded bytes have another SHA-256: upload them again",
        )));
    }
    let Some(purpose) = upload.purpose else {
        return Ok(Err(ApiError::invalid_field(
            "purpose",
            "this server does not know the upload's purpose",
        )));
    };
    let found = match purpose.content.check(upload.meta.ext.as_deref(), &head) {
        Ok(found) => found,
        Err(ContentRefusal::NotDeclared) => {
            return Ok(Err(ApiError::invalid_field(
                "ext",
                "the uploaded bytes are not of the declared type",
            )));
        }
        Err(ContentRefusal::Unsupported) => {
            return Ok(Err(ApiError::new(ErrorCode::UnsupportedMediaType)
                .with_detail(
                    "the uploaded bytes are of no type this purpose takes",
                )));
        }
    };
    Ok(Ok(UploadMeta {
        sha256: digest,
        ext: found.ext().map(str::to_owned),
        ..upload.meta.clone()
    }))
}

/// The upload `id` of the caller's user, when the caller may work on it:
/// 404 for an unknown, expired or another user's upload; 403 when its
/// purpose is not the caller's; 409 `upload_consumed` once claimed.
async fn find(state: &AppState, caller: &Caller, id: &str) -> Result<Upload, ApiError> {
    let control = Arc::clone(state.control());
    let (user_id, id) = (caller.id().to_owned(), id.to_owned());
    let found = blocking(move || control.read(|c| uploads::get(c, &user_id, &id))).await?;
    let upload = found.ok_or_else(ApiError::not_found)?;
    require_uploader(caller, upload.purpose)?;
    if upload.is_consumed() {
        return Err(consumed(&upload.id));
    }
    if !upload.is_live(now_ms()) {
        return Err(ApiError::not_found());
    }
    Ok(upload)
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
fn sweep_expired(control: &ControlDb, dir: &Path, user_id: &str, now: i64) -> Result<(), ApiError> {
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

/// The pairs of `Upload-Metadata` the server reads, decoded.
#[derive(Debug, Default)]
struct Metadata {
    purpose: Option<String>,
    sha256: Option<String>,
    ext: Option<String>,
    filename: Option<String>,
}

impl Metadata {
    /// Decodes `Upload-Metadata`. 422 for a repeated key or a value that is
    /// not base64 UTF-8.
    fn read(headers: &HeaderMap) -> Result<Self, ApiError> {
        let raw = headers
            .get(&UPLOAD_METADATA)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default();
        let mut metadata = Self::default();
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
                "purpose" => metadata.purpose = Some(value),
                "sha256" => metadata.sha256 = Some(value),
                "ext" => metadata.ext = Some(value),
                "filename" => metadata.filename = Some(value),
                _ => {} // tus clients may send more (filetype): ignored
            }
        }
        Ok(metadata)
    }

    /// The purpose; 422 when it is missing or unknown.
    fn purpose(&self) -> Result<UploadPurpose, ApiError> {
        self.purpose
            .as_deref()
            .and_then(UploadPurpose::parse)
            .ok_or_else(|| {
                let known: Vec<&str> = UploadPurpose::ALL.iter().map(|p| p.as_str()).collect();
                ApiError::invalid_field("purpose", format!("must be one of {}", known.join(", ")))
            })
    }

    /// What the client declares for an upload of `purpose`; 422 when it
    /// breaks the purpose's rules.
    fn declared(self, purpose: UploadPurpose) -> Result<UploadMeta, ApiError> {
        let sha256 = match (self.sha256, purpose.sha256) {
            (Some(sha256), _) if Digest::parse_hex(&sha256).is_some() => sha256,
            (Some(_), _) => {
                return Err(ApiError::invalid_field(
                    "sha256",
                    "must be 64 lowercase hex digits",
                ));
            }
            (None, HashRule::Required) => {
                return Err(ApiError::invalid_field(
                    "sha256",
                    "is required for this purpose",
                ));
            }
            (None, HashRule::Optional) => String::new(),
        };
        let ext = match purpose.content {
            Content::DeclaredMedia(kinds) => Some(
                self.ext
                    .filter(|e| MediaKind::from_ext(e).is_some_and(|k| kinds.contains(k)))
                    .ok_or_else(|| ApiError::invalid_field("ext", "must be a stored media type"))?,
            ),
            Content::Media(_) | Content::Sqlite | Content::JsonOrZip => None,
        };
        Ok(UploadMeta {
            sha256,
            ext,
            filename: self.filename.as_deref().and_then(clean_filename),
            consumed_at: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::bearer::ScopeSet;

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

    fn parse(raw: &str) -> Result<(UploadPurpose, UploadMeta), ApiError> {
        let metadata = Metadata::read(&headers(&[("upload-metadata", raw)]))?;
        let purpose = metadata.purpose()?;
        Ok((purpose, metadata.declared(purpose)?))
    }

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn metadata_names_a_purpose_a_hash_and_a_type() {
        let (purpose, parsed) = parse(&meta(&[
            ("purpose", "migration-object"),
            ("sha256", SHA),
            ("ext", "jpg"),
        ]))
        .unwrap();
        assert_eq!(purpose, UploadPurpose::MIGRATION_OBJECT);
        assert_eq!(parsed.sha256, SHA);
        assert_eq!(parsed.ext.as_deref(), Some("jpg"));

        let (_, db) = parse(&meta(&[
            ("purpose", "migration-db"),
            ("sha256", SHA),
            ("filename", "x"),
        ]))
        .unwrap();
        assert_eq!((db.ext, db.filename.as_deref()), (None, Some("x")));

        // The web's purposes need no hash, and their type is sniffed.
        let (purpose, web) = parse(&meta(&[
            ("purpose", "bookmark-original"),
            ("ext", "svg"),
            ("filename", "../holiday.jpg"),
            ("filetype", "image/svg+xml"),
        ]))
        .unwrap();
        assert_eq!(purpose, UploadPurpose::BOOKMARK_ORIGINAL);
        assert_eq!(
            web,
            UploadMeta {
                sha256: String::new(),
                ext: None,
                filename: Some("holiday.jpg".into()),
                consumed_at: None,
            }
        );
        let (_, hashed) = parse(&meta(&[("purpose", "import"), ("sha256", SHA)])).unwrap();
        assert_eq!(hashed.sha256, SHA);

        for bad in [
            meta(&[("purpose", "bookmark"), ("sha256", SHA)]),
            meta(&[("sha256", SHA)]),
            meta(&[("purpose", "migration-object"), ("sha256", SHA)]),
            meta(&[("purpose", "migration-object"), ("ext", "jpg")]),
            meta(&[
                ("purpose", "migration-object"),
                ("sha256", SHA),
                ("ext", "svg"),
            ]),
            meta(&[("purpose", "migration-db"), ("sha256", &SHA.to_uppercase())]),
            meta(&[("purpose", "import"), ("sha256", "abc")]),
            format!("purpose {},purpose x", STANDARD.encode("migration-db")),
            "purpose !!!".to_owned(),
            String::new(),
        ] {
            let err = parse(&bad).unwrap_err();
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

    #[test]
    fn the_routes_take_what_the_purposes_take() {
        assert_eq!(ScopeSet::of(UPLOAD_SCOPES), UploadPurpose::token_scopes());
        assert!(UploadPurpose::any_takes_sessions());
        let rows: Vec<_> = crate::routes::TOKEN_ROUTES
            .iter()
            .filter(|(_, path, ..)| path.starts_with("/api/v1/uploads"))
            .collect();
        assert_eq!(rows.len(), 4, "POST, HEAD, PATCH and DELETE");
        for (method, path, scopes, session) in rows {
            assert_eq!(
                ScopeSet::of(scopes),
                UploadPurpose::token_scopes(),
                "{method} {path}"
            );
            assert!(*session, "{method} {path}");
        }
    }

    #[test]
    fn each_purpose_names_who_may_upload_it() {
        use crate::auth::bearer::TokenPrincipal;
        use crate::current_user::CurrentUser;

        let session = Caller::session(CurrentUser::new("U"));
        let web = Caller::with_token(TokenPrincipal::for_tests("U", &[Scope::Uploads]));
        let cli = Caller::with_token(TokenPrincipal::for_tests("U", &[Scope::Migrate]));
        let extension = Caller::with_token(TokenPrincipal::for_tests(
            "U",
            &[Scope::Ingest, Scope::Tasks, Scope::Lookup],
        ));
        for purpose in [UploadPurpose::MIGRATION_OBJECT, UploadPurpose::MIGRATION_DB] {
            assert!(require_uploader(&cli, Some(purpose)).is_ok());
            for refused in [&session, &web, &extension] {
                let err = require_uploader(refused, Some(purpose)).unwrap_err();
                assert_eq!(err.code(), ErrorCode::Forbidden);
                assert!(err.problem().detail.unwrap().contains("migrate scope"));
            }
        }
        for purpose in [
            UploadPurpose::BOOKMARK_ORIGINAL,
            UploadPurpose::BOOKMARK_PREVIEW,
            UploadPurpose::IMPORT,
        ] {
            assert!(require_uploader(&session, Some(purpose)).is_ok());
            assert!(require_uploader(&web, Some(purpose)).is_ok());
            for refused in [&cli, &extension] {
                let err = require_uploader(refused, Some(purpose)).unwrap_err();
                assert_eq!(err.code(), ErrorCode::Forbidden, "{}", purpose.as_str());
            }
        }
        assert_eq!(
            require_uploader(&session, None).unwrap_err().code(),
            ErrorCode::Forbidden
        );
    }

    #[test]
    fn a_claim_error_names_the_consumers_field() {
        let unusable = ClaimError::Unusable { id: "U1".into() }.into_problem("files[0].upload");
        assert_eq!(unusable.code(), ErrorCode::ValidationFailed);
        assert_eq!(unusable.problem().errors[0].field, "files[0].upload");
        let used = ApiError::from(ClaimError::Consumed { id: "U1".into() });
        assert_eq!(
            (used.code(), used.status()),
            (ErrorCode::UploadConsumed, StatusCode::CONFLICT)
        );
    }
}
