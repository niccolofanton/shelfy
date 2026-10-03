//! `/api/v1/migrations`: installing a desktop library moved with
//! `shelfy-migrate` (plan §2.9, §4.1; the install itself is the `migrate`
//! job, [`crate::migrations`]).
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /migrations/preflight` | whether the library is empty, the quota and the use: what `plan` and `run` check before uploading |
//! | `POST /migrations/missing-objects` `{objects: [{sha256, ext, bytes}]}` (≤500) | `{missing: [sha256]}`: what to upload |
//! | `POST /migrations` `{dbUploadId, merge}` (`Idempotency-Key`) | 202 and the install; `Location: /api/v1/migrations/{id}` |
//! | `GET /migrations/{id}` | the install's state, stage, progress and, once done, its report |
//!
//! An object is "present" when the user's store holds it with the declared
//! size, or when a complete migration upload has its hash; anything else is
//! missing. Auth: a `migrate` token only ([`crate::routes::TOKEN_ROUTES`]);
//! another user's install is 404. The install's progress also reaches the
//! user's open tabs as `job.updated` (kind `migrate`).

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use rusqlite::OptionalExtension as _;
use serde::{Deserialize, Serialize};
use shelfy_core::db::DbError;
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, MediaKind};
use utoipa::ToSchema;

use super::auth::no_store;
use super::uploads::{MAX_DATABASE_BYTES, MAX_OBJECT_BYTES};
use crate::auth::bearer::{TokenUser, scopes::Migrate};
use crate::control::jobs::JobRow;
use crate::control::uploads::{self, UploadPurpose};
use crate::control::users;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::JobState;
use crate::extract::{Json, Path};
use crate::jobs::idempotency::IdempotencyHeader;
use crate::jobs::migrate::{self as job, Payload};
use crate::migrations::install::{self, report_key};
use crate::migrations::swap;
use crate::migrations::{Migration, MigrationPreflight, MigrationReport};
use crate::state::{AppState, blocking};

/// Objects per `POST /migrations/missing-objects`.
pub const MAX_OBJECTS_PER_REQUEST: usize = 500;

/// An object of a bundle.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    /// SHA-256 of the content, lowercase hex.
    pub sha256: String,
    /// Extension of its type in the store's allowlist (`jpg`, `webp`, `mp4`…).
    pub ext: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// Body of `POST /migrations/missing-objects`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MissingObjectsRequest {
    /// The bundle's objects, at most 500 per request.
    pub objects: Vec<ObjectRef>,
}

/// Answer of `POST /migrations/missing-objects`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MissingObjects {
    /// The hashes the server does not have, in request order.
    pub missing: Vec<String>,
}

/// Body of `POST /migrations`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StartMigration {
    /// The complete upload (purpose `migration-db`) of the bundle's database.
    pub db_upload_id: String,
    /// Merge into a library that is not empty. An empty library is replaced
    /// either way.
    #[serde(default)]
    pub merge: bool,
}

/// What the library looks like before a migration: whether it is empty (a
/// bundle replaces it) or needs `--merge`, and the quota.
#[utoipa::path(
    get,
    path = "/api/v1/migrations/preflight",
    tag = "migration",
    operation_id = "getMigrationPreflight",
    security(("bearer" = ["migrate"])),
    responses(
        (status = OK, description = "The library and its quota.", body = MigrationPreflight),
    )
)]
pub async fn migration_preflight(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
) -> Result<Response, ApiError> {
    let user_id = caller.id().to_owned();
    let library = state.user_db(&user_id).await?;
    let (empty, posts, used) = blocking(move || {
        library.read(|c| -> Result<(bool, u64, i64), DbError> {
            let empty = swap::is_empty(c)?;
            let posts: i64 = c.query_row("SELECT count(*) FROM posts", [], |r| r.get(0))?;
            let media: i64 = c.query_row(
                "SELECT coalesce(sum(bytes), 0) FROM media_objects",
                [],
                |r| r.get(0),
            )?;
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            Ok((
                empty,
                u64::try_from(posts).unwrap_or(0),
                media.saturating_add(pages.saturating_mul(size)),
            ))
        })
    })
    .await?;
    let control = Arc::clone(state.control());
    let (quota, active) = {
        let user_id = user_id.clone();
        blocking(move || -> Result<(i64, Option<i64>), ApiError> {
            let quota = control
                .read(|c| users::usage(c, &user_id))?
                .map_or(0, |u| u.quota_bytes);
            let active = control.read(|c| {
                c.query_row(
                    "SELECT id FROM jobs WHERE user_id = ?1 AND kind = ?2
                       AND state IN ('queued', 'running') ORDER BY id LIMIT 1",
                    rusqlite::params![user_id, job::KIND],
                    |r| r.get(0),
                )
                .optional()
                .map_err(shelfy_core::repo::RepoError::from)
            })?;
            Ok((quota, active))
        })
        .await?
    };
    Ok(no_store(
        Json(MigrationPreflight {
            library_empty: empty,
            posts,
            quota_bytes: quota,
            used_bytes: used,
            max_object_bytes: MAX_OBJECT_BYTES,
            max_database_bytes: MAX_DATABASE_BYTES,
            active_job_id: active,
        })
        .into_response(),
    ))
}

/// Which objects of a bundle the server lacks, so the CLI uploads only those.
///
/// 422 for more than 500 objects, a malformed hash or a type outside the
/// store's allowlist.
#[utoipa::path(
    post,
    path = "/api/v1/migrations/missing-objects",
    tag = "migration",
    operation_id = "findMissingObjects",
    security(("bearer" = ["migrate"])),
    request_body = MissingObjectsRequest,
    responses(
        (status = OK, description = "The objects to upload.", body = MissingObjects),
    )
)]
pub async fn find_missing_objects(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Json(body): Json<MissingObjectsRequest>,
) -> Result<Response, ApiError> {
    if body.objects.len() > MAX_OBJECTS_PER_REQUEST {
        return Err(ApiError::invalid_field(
            "objects",
            "at most 500 per request",
        ));
    }
    let mut parsed = Vec::with_capacity(body.objects.len());
    for object in &body.objects {
        let digest = Digest::parse_hex(&object.sha256)
            .ok_or_else(|| ApiError::invalid_field("objects.sha256", "must be lowercase hex"))?;
        let kind = MediaKind::from_ext(&object.ext)
            .ok_or_else(|| ApiError::invalid_field("objects.ext", "not a stored media type"))?;
        parsed.push((object.sha256.clone(), digest, kind, object.bytes));
    }
    let media = MediaStore::new(state.config().data_dir.users_dir())
        .user(caller.id())
        .map_err(ApiError::internal)?;
    let control = Arc::clone(state.control());
    let user_id = caller.id().to_owned();
    let missing = blocking(move || -> Result<Vec<String>, ApiError> {
        let hashes: Vec<String> = parsed.iter().map(|p| p.0.clone()).collect();
        let uploaded: HashSet<String> = control
            .read(|c| {
                uploads::complete_by_sha256(c, &user_id, UploadPurpose::MIGRATION_OBJECT, &hashes)
            })?
            .into_iter()
            .map(|u| u.meta.sha256)
            .collect();
        Ok(parsed
            .into_iter()
            .filter(|(hash, digest, kind, bytes)| {
                let stored = std::fs::metadata(media.object_path(digest, *kind))
                    .is_ok_and(|m| m.is_file() && m.len() == *bytes);
                !stored && !uploaded.contains(hash)
            })
            .map(|(hash, ..)| hash)
            .collect())
    })
    .await?;
    Ok(no_store(Json(MissingObjects { missing }).into_response()))
}

/// Starts installing an uploaded bundle (plan §4.1 step 5): enqueues the
/// `migrate` job.
///
/// Answers 202 with the install, polled at `Location`; its progress also
/// arrives as `job.updated`. 422 when the upload is not a complete bundle
/// database of this user; 409 when the web library is not empty and `merge`
/// is false, or another install of the library is queued or running. Send an
/// `Idempotency-Key`: a repeat gets the first answer back.
#[utoipa::path(
    post,
    path = "/api/v1/migrations",
    tag = "migration",
    operation_id = "startMigration",
    security(("bearer" = ["migrate"])),
    params(IdempotencyHeader),
    request_body = StartMigration,
    responses(
        (
            status = ACCEPTED,
            description = "The install is queued.",
            body = Migration,
            headers(("Location" = String, description = "`/api/v1/migrations/{id}`."))
        ),
    )
)]
pub async fn start_migration(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Json(body): Json<StartMigration>,
) -> Result<Response, ApiError> {
    let user_id = caller.id().to_owned();
    let control = Arc::clone(state.control());
    {
        let (user_id, upload_id) = (user_id.clone(), body.db_upload_id.clone());
        blocking(move || control.read(|c| uploads::get(c, &user_id, &upload_id))).await?
    }
    .filter(|u| u.is_complete() && u.purpose == Some(UploadPurpose::MIGRATION_DB))
    .ok_or_else(|| {
        ApiError::invalid_field(
            "dbUploadId",
            "must be a complete upload of the bundle's database",
        )
    })?;
    if !body.merge {
        let library = state.user_db(&user_id).await?;
        let empty = blocking(move || {
            library.read(|c| swap::is_empty(c).map_err(|e| ApiError::from(DbError::from(e))))
        })
        .await?;
        if !empty {
            return Err(install::not_empty());
        }
    }
    let payload = Payload {
        db_upload_id: body.db_upload_id,
        merge: body.merge,
    };
    let enqueued = job::enqueue(state.jobs(), &user_id, &payload).await?;
    let same = serde_json::from_str::<Payload>(&enqueued.job.payload_json).ok() == Some(payload);
    if !enqueued.created && !same {
        return Err(ApiError::new(ErrorCode::Conflict).with_detail(
            "another install of this library is queued or running: wait for it to end",
        ));
    }
    let migration = Migration::from_job(&enqueued.job, None);
    let mut response = (StatusCode::ACCEPTED, Json(migration)).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/migrations/{}", enqueued.job.id))
            .map_err(ApiError::internal)?,
    );
    Ok(no_store(response))
}

/// The state of an install, and its report once it succeeded.
#[utoipa::path(
    get,
    path = "/api/v1/migrations/{id}",
    tag = "migration",
    operation_id = "getMigration",
    security(("bearer" = ["migrate"])),
    params(("id" = String, Path, description = "Install id (the `migrate` job's id).")),
    responses(
        (status = OK, description = "The install.", body = Migration),
    )
)]
pub async fn get_migration(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let row: JobRow = match id.parse::<i64>() {
        Ok(id) => state.jobs().get(caller.id(), id).await?,
        Err(_) => None,
    }
    .filter(|row| row.kind == job::KIND)
    .ok_or_else(ApiError::not_found)?;
    let report = if row.state == JobState::Succeeded {
        stored_report(&state, caller.id(), row.id).await?
    } else {
        None
    };
    Ok(no_store(
        Json(Migration::from_job(&row, report)).into_response(),
    ))
}

/// The report job `id` stored in `user_id`'s library, if it is still there
/// (a later install replaces the library).
async fn stored_report(
    state: &AppState,
    user_id: &str,
    job_id: i64,
) -> Result<Option<MigrationReport>, ApiError> {
    let library = state.user_db(user_id).await?;
    let key = report_key(job_id);
    let stored: Option<String> = blocking(move || {
        library.read(|c| {
            c.query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| {
                r.get(0)
            })
            .optional()
            .map_err(DbError::from)
        })
    })
    .await?;
    Ok(stored.and_then(|json| serde_json::from_str(&json).ok()))
}
