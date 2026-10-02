//! Installing a desktop library migrated with `shelfy-migrate` (plan §4.1
//! step 5, §4.3; T9: migration v0).
//!
//! The CLI uploads a bundle: CAS objects first, each one a tus upload, then
//! the bundle's `library.sqlite`. `POST /api/v1/migrations` starts the
//! install, which runs in the background ([`Installs`]) and reports through
//! `GET /api/v1/migrations/{id}`:
//!
//! 1. **validating**: the database is copied to
//!    `<data>/work/migrations/<id>/` and checked ([`validate`]): integrity,
//!    schema version and shape, limits, and every object row a stored type;
//!    every object must be in the user's store already or in a complete
//!    upload of the same hash.
//! 2. **objects**: each uploaded object is streamed into the user's store
//!    (hashed again: a mismatch fails the install), and the objects the grid
//!    shows (covers, slides 1–3, site heroes) get their `g480` rendition and,
//!    for covers, their ThumbHash, on the shared image pool. Files and rows
//!    change in the new library's write transactions, per the store's
//!    protocol (`shelfy_media::store`).
//! 3. **index**: the FTS index is rebuilt; derived data (renditions,
//!    ThumbHash, archive state) is always recomputed by the server, never
//!    trusted from the bundle.
//! 4. **report**: the reconciliation report ([`MigrationReport`]) is stored
//!    in the new library's `meta`.
//! 5. **installing**: the new library replaces the empty live one atomically
//!    ([`swap`]); the previous one is kept next to it. Open tabs get
//!    `posts.changed` (reason `import`) and `stats.changed`, and a
//!    `migration.installed` notification joins the user's activity.
//!
//! Then the consumed uploads and the work directory are removed. A failed
//! install leaves the live library untouched and keeps the uploads, so the
//! CLI can retry.
//!
//! **v0 scope.** No job system yet: the install is a task of this process,
//! and its status lives in memory (lost on restart; the uploads and the
//! stored report are not). P1-07 brings the job system and P1-19 moves the
//! install onto it as the `migrate` job kind, with `Idempotency-Key`,
//! `job.updated` progress, the merge into a non-empty library (`--merge`,
//! refused here with 409) and the 7-day retention of the previous library.
//! Archive work for posts whose files were missing is recorded per post
//! (`archive_state`, `cover_url_expires_at`) and counted in the report; the
//! archive workers arrive with P1-19 and P2.

pub mod install;
pub mod swap;
pub mod validate;

use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, PoisonError};

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::error::{ApiError, ErrorCode};

/// The install's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    /// Still working.
    Running,
    /// Installed; the report is ready.
    Succeeded,
    /// Stopped; the live library is unchanged.
    Failed,
}

/// What the install is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStage {
    /// Accepted, not started.
    Queued,
    /// Checking the bundle.
    Validating,
    /// Storing objects and rendering their grid images.
    Objects,
    /// Rebuilding the search index.
    Index,
    /// Counting and storing the report.
    Report,
    /// Replacing the live library.
    Installing,
    /// Finished (see `state`).
    Done,
}

/// Why an install failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationFailure {
    /// A stable error code (`validation_failed`, `conflict`, `internal`…).
    pub code: ErrorCode,
    /// Developer-facing detail; never content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub detail: Option<String>,
}

/// An install of a migration bundle.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    /// Install id (ULID).
    pub id: String,
    pub state: MigrationState,
    pub stage: MigrationStage,
    /// Progress of the stage, 0 to 1.
    pub progress: f64,
    /// When it started, unix ms.
    pub created_at: i64,
    /// When it ended, unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub finished_at: Option<i64>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<MigrationFailure>,
    /// The reconciliation report, once installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub report: Option<MigrationReport>,
}

/// The reconciliation report of an install (plan §4.3): counts only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationReport {
    /// What `shelfy-migrate` counted in the desktop library and wrote to the
    /// bundle, as it sent it.
    #[schema(value_type = Object)]
    pub bundle: serde_json::Value,
    pub installed: InstalledCounts,
    pub objects: InstalledObjects,
    pub renditions: RenditionCounts,
    pub archive: ArchiveCounts,
    /// How long the install took, ms.
    pub duration_ms: u64,
}

/// Rows of the installed library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalledCounts {
    /// Posts by platform.
    pub posts: BTreeMap<String, u64>,
    pub slides: u64,
    pub collections: u64,
    pub memberships: u64,
    pub post_tags: u64,
    pub post_entities: u64,
    pub tag_aliases: u64,
    pub tag_clusters: u64,
    pub web_captures: u64,
    pub web_capture_assets: u64,
    pub media_objects: u64,
}

/// The objects the install stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct InstalledObjects {
    pub total: u64,
    pub bytes: u64,
    /// Moved into the store from uploads.
    pub from_uploads: u64,
    /// Already in the store (an earlier install, or another post's file).
    pub already_stored: u64,
}

/// The `g480` renditions and ThumbHashes of the install.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RenditionCounts {
    /// Objects the grid shows: covers, slides 1–3, site heroes.
    pub wanted: u64,
    pub rendered: u64,
    /// Already rendered in the store.
    pub existing: u64,
    /// Images the pipeline could not decode.
    pub failed: u64,
    /// Types the pipeline does not decode (AVIF, videos, PDF).
    pub not_renderable: u64,
    /// Posts whose ThumbHash was set.
    pub thumbhashes: u64,
}

/// Archive work left for the workers (P1-19, P2), by class (OI-6, OI-7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveCounts {
    /// Posts by `archive_state`.
    pub by_state: BTreeMap<String, u64>,
    /// Instagram covers to archive whose signed URL is still valid: archive
    /// them first, before they expire.
    pub ig_cover_valid: u64,
    /// Instagram covers whose URL has expired: extension `refresh_media`
    /// tasks.
    pub ig_cover_expired: u64,
    /// Instagram covers whose URL has no expiry.
    pub ig_cover_no_expiry: u64,
    /// X covers to archive (they do not expire).
    pub x_cover: u64,
    pub pinterest_cover: u64,
    pub other_cover: u64,
    /// Image slides without a stored image.
    pub image_slides_pending: u64,
}

/// One install, shared by its task and the status route.
#[derive(Debug)]
pub struct Install {
    /// The user whose library it fills.
    pub user_id: String,
    status: Mutex<Migration>,
}

impl Install {
    /// The current status.
    #[must_use]
    pub fn status(&self) -> Migration {
        self.status
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Moves to `stage` at `progress`.
    pub fn set_stage(&self, stage: MigrationStage, progress: f64) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        status.stage = stage;
        status.progress = progress.clamp(0.0, 1.0);
    }

    fn finish(&self, outcome: Result<MigrationReport, ApiError>, now: i64) {
        let mut status = self.status.lock().unwrap_or_else(PoisonError::into_inner);
        status.stage = MigrationStage::Done;
        status.finished_at = Some(now);
        match outcome {
            Ok(report) => {
                status.state = MigrationState::Succeeded;
                status.progress = 1.0;
                status.report = Some(report);
            }
            Err(err) => {
                status.state = MigrationState::Failed;
                let problem = err.problem();
                status.error = Some(MigrationFailure {
                    code: problem.code,
                    detail: problem.detail,
                });
            }
        }
    }

    fn is_running(&self) -> bool {
        self.status().state == MigrationState::Running
    }
}

/// The installs of this process, running and recent.
#[derive(Debug, Default)]
pub struct Installs {
    all: Mutex<VecDeque<(String, Arc<Install>)>>,
}

/// Finished installs remembered for their status route.
const KEEP_FINISHED: usize = 32;

impl Installs {
    /// Registers a new install for `user_id` unless one is running for them.
    ///
    /// # Errors
    ///
    /// 409 `conflict` while another install of the user runs.
    pub fn begin(&self, id: &str, user_id: &str, now: i64) -> Result<Arc<Install>, ApiError> {
        let mut all = self.all.lock().unwrap_or_else(PoisonError::into_inner);
        if all
            .iter()
            .any(|(_, install)| install.user_id == user_id && install.is_running())
        {
            return Err(ApiError::new(ErrorCode::Conflict)
                .with_detail("an install of this library is already running"));
        }
        let install = Arc::new(Install {
            user_id: user_id.to_owned(),
            status: Mutex::new(Migration {
                id: id.to_owned(),
                state: MigrationState::Running,
                stage: MigrationStage::Queued,
                progress: 0.0,
                created_at: now,
                finished_at: None,
                error: None,
                report: None,
            }),
        });
        all.push_back((id.to_owned(), Arc::clone(&install)));
        while all.len() > KEEP_FINISHED {
            match all.iter().position(|(_, i)| !i.is_running()) {
                Some(oldest) => {
                    all.remove(oldest);
                }
                None => break,
            }
        }
        Ok(install)
    }

    /// The install `id` of `user_id`; another user's reads as missing.
    #[must_use]
    pub fn get(&self, id: &str, user_id: &str) -> Option<Arc<Install>> {
        self.all
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .find(|(key, install)| key == id && install.user_id == user_id)
            .map(|(_, install)| Arc::clone(install))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_running_install_per_user() {
        let installs = Installs::default();
        let first = installs.begin("A", "U1", 1).unwrap();
        let err = installs.begin("B", "U1", 2).unwrap_err();
        assert_eq!(err.code(), ErrorCode::Conflict);
        installs.begin("C", "U2", 2).unwrap();
        assert!(installs.get("A", "U2").is_none(), "another user's install");
        assert_eq!(installs.get("A", "U1").unwrap().status().id, "A");

        first.set_stage(MigrationStage::Objects, 0.5);
        assert_eq!(first.status().stage, MigrationStage::Objects);
        first.finish(Err(ApiError::new(ErrorCode::ValidationFailed)), 3);
        let failed = first.status();
        assert_eq!(failed.state, MigrationState::Failed);
        assert_eq!(failed.finished_at, Some(3));
        assert_eq!(failed.error.unwrap().code, ErrorCode::ValidationFailed);
        installs.begin("D", "U1", 4).unwrap();
    }
}
