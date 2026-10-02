//! `/api/v1/migrations`: installing a desktop library moved with
//! `shelfy-migrate` (plan §2.9, §4.1; the install itself is in
//! [`crate::migrations`]).
//!
//! | Route | Answer |
//! |---|---|
//! | `POST /migrations/missing-objects` `{objects: [{sha256, ext, bytes}]}` (≤500) | `{missing: [sha256]}`: what to upload |
//! | `POST /migrations` `{dbUploadId, merge}` | 202 and the install; `Location: /api/v1/migrations/{id}` |
//! | `GET /migrations/{id}` | the install's state, stage, progress and, once done, its report |
//!
//! An object is "present" when the user's store holds it with the declared
//! size, or when a complete migration upload has its hash; anything else is
//! missing. Auth: a `migrate` token only ([`crate::routes::TOKEN_ROUTES`]);
//! another user's install is 404.

use std::collections::HashSet;
use std::sync::Arc;

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, MediaKind};
use utoipa::ToSchema;

use super::auth::no_store;
use crate::auth::bearer::{TokenUser, scopes::Migrate};
use crate::control::uploads::{self, UploadPurpose};
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Path};
use crate::ids::{new_ulid, now_ms};
use crate::migrations::{Installs, Migration, install};
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
    /// Merge into a library that is not empty. Not supported yet: such a
    /// library answers 409 either way.
    #[serde(default)]
    pub merge: bool,
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
                uploads::complete_by_sha256(c, &user_id, UploadPurpose::MigrationObject, &hashes)
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

/// Starts installing an uploaded bundle (plan §4.1 step 5).
///
/// Answers 202 with the install, polled at `Location`. 422 when the upload
/// is not a complete bundle database of this user; 409 when the web library
/// is not empty (merging arrives with P1-19) or another install runs.
#[utoipa::path(
    post,
    path = "/api/v1/migrations",
    tag = "migration",
    operation_id = "startMigration",
    security(("bearer" = ["migrate"])),
    request_body = StartMigration,
    responses(
        (
            status = ACCEPTED,
            description = "The install started.",
            body = Migration,
            headers(("Location" = String, description = "`/api/v1/migrations/{id}`."))
        ),
    )
)]
pub async fn start_migration(
    caller: TokenUser<Migrate>,
    State(state): State<AppState>,
    Extension(installs): Extension<Arc<Installs>>,
    Json(body): Json<StartMigration>,
) -> Result<Response, ApiError> {
    let user_id = caller.id().to_owned();
    let control = Arc::clone(state.control());
    let upload = {
        let (user_id, upload_id) = (user_id.clone(), body.db_upload_id.clone());
        blocking(move || control.read(|c| uploads::get(c, &user_id, &upload_id))).await?
    }
    .filter(|u| u.is_complete() && u.purpose == Some(UploadPurpose::MigrationDb))
    .ok_or_else(|| {
        ApiError::invalid_field(
            "dbUploadId",
            "must be a complete upload of the bundle's database",
        )
    })?;
    install::ensure_empty(&state, &user_id).await?;
    let id = new_ulid();
    let job = installs.begin(&id, &user_id, now_ms())?;
    let status = job.status();
    tokio::spawn(install::run(state.clone(), job, id.clone(), upload));
    let mut response = (StatusCode::ACCEPTED, Json(status)).into_response();
    response.headers_mut().insert(
        header::LOCATION,
        HeaderValue::from_str(&format!("/api/v1/migrations/{id}")).map_err(ApiError::internal)?,
    );
    Ok(no_store(response))
}

/// The state of an install, and its report once it succeeded. The install
/// runs in this server process: after a restart its id is unknown (404),
/// while the report stays in the installed library.
#[utoipa::path(
    get,
    path = "/api/v1/migrations/{id}",
    tag = "migration",
    operation_id = "getMigration",
    security(("bearer" = ["migrate"])),
    params(("id" = String, Path, description = "Install id.")),
    responses(
        (status = OK, description = "The install.", body = Migration),
    )
)]
pub async fn get_migration(
    caller: TokenUser<Migrate>,
    Extension(installs): Extension<Arc<Installs>>,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let install = installs
        .get(&id, caller.id())
        .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
    Ok(no_store(Json(install.status()).into_response()))
}
