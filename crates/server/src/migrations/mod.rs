//! Installing a desktop library migrated with `shelfy-migrate` (plan §4.1
//! step 5, §4.3; T9: migration v0; P1-19: the `migrate` job).
//!
//! The CLI uploads a bundle: CAS objects first, each one a tus upload, then
//! the bundle's `library.sqlite`. `POST /api/v1/migrations` enqueues a
//! `migrate` job ([`crate::jobs::migrate`]: one try at a time per user, two
//! tries, a 60-minute lease) that installs it; `GET /api/v1/migrations/{id}`
//! and the user's `job.updated` events follow it. The stages
//! ([`MigrationStage`], the job's `stage`):
//!
//! 1. **validating**: the database is copied to
//!    `<data>/work/migrations/<job id>/` and checked ([`validate`]): integrity,
//!    schema version and shape, limits, and every object row a stored type;
//!    every object must be in the user's store already or in a complete
//!    upload of the same hash, and the new bytes must fit the quota. An empty
//!    web library is **replaced**; a library with posts or collections is
//!    **merged** into when the CLI asked for it (`--merge`), and refused
//!    otherwise.
//! 2. **objects**: each uploaded object is streamed into the user's store
//!    (hashed again: a mismatch fails the install), and the objects the grid
//!    shows (covers, slides 1–3, site heroes) get their `g480` rendition and,
//!    for covers, their ThumbHash, on the shared image pool. Files and rows
//!    change in one library's write transactions, per the store's protocol
//!    (`shelfy_media::store`): the new library's for a replace, the live
//!    one's for a merge.
//! 3. **index** (replace): the FTS index is rebuilt and the archive state of
//!    every post derived; derived data (renditions, ThumbHash, archive state)
//!    is always recomputed by the server, never trusted from the bundle.
//!    **merging** (merge): the bundle's posts join the live library through
//!    the core's merge rules ([`merge`], P1-10).
//! 4. **report**: the reconciliation report ([`MigrationReport`]) is stored
//!    in the library's `meta` (`migration.report:<job id>`).
//! 5. **installing** (replace): the new library replaces the empty live one
//!    atomically ([`swap`]).
//!
//! The previous library is kept next to the live one as
//! `library.prev-<job id>.sqlite` (a merge keeps the library as it was before
//! the merge) for 7 days ([`housekeeping`]). Then open tabs get
//! `posts.changed` (reason `import`) and `stats.changed`, a
//! `migration.installed` notification with the reconciliation joins the
//! user's activity, a `usage.recompute` job counts the storage again
//! ([`crate::jobs::usage`]), and the consumed uploads and the work directory
//! are removed. A failed install leaves the live library untouched (a merge
//! that stopped half way is finished by the next try, which merges the same
//! rows again without changing them twice) and keeps the uploads, so the job
//! can be retried. An install whose library is locked for maintenance waits
//! for the unlock (`user_locked`, a transient error).
//!
//! Archive work for posts whose files were missing is recorded per post
//! (`archive_state`, `cover_url_expires_at`) and counted in the report. The
//! states come from the core's rule (`shelfy_core::ingest::archive`), which
//! the bundle builder applies too, with the asset types the library ends with
//! (`install::archive_policy`); the archive workers arrive with P2.

pub mod housekeeping;
pub mod install;
pub mod merge;
pub mod swap;
pub mod validate;

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::control::jobs::JobRow;
use crate::error::ErrorCode;
use crate::events::model::JobState;

/// The install's state.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MigrationState {
    /// Queued or working; also while waiting for a retry.
    Running,
    /// Installed; the report is ready.
    Succeeded,
    /// Stopped for good (or cancelled); the live library is unchanged, or
    /// merged into only in part.
    Failed,
}

/// What the install is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum MigrationStage {
    /// Accepted, not started (or waiting for its next try).
    Queued,
    /// Checking the bundle.
    Validating,
    /// Storing objects and rendering their grid images.
    Objects,
    /// Merging the posts into a library that is not empty.
    Merging,
    /// Rebuilding the search index.
    Index,
    /// Counting and storing the report.
    Report,
    /// Replacing the live library.
    Installing,
    /// Finished (see `state`).
    Done,
}

impl MigrationStage {
    /// The job's `stage` code.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Validating => "validating",
            Self::Objects => "objects",
            Self::Merging => "merging",
            Self::Index => "index",
            Self::Report => "report",
            Self::Installing => "installing",
            Self::Done => "done",
        }
    }

    /// The stage of a job's `stage` code.
    #[must_use]
    pub fn parse(code: &str) -> Option<Self> {
        [
            Self::Queued,
            Self::Validating,
            Self::Objects,
            Self::Merging,
            Self::Index,
            Self::Report,
            Self::Installing,
            Self::Done,
        ]
        .into_iter()
        .find(|stage| stage.as_str() == code)
    }
}

/// How the bundle joined the web library.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum InstallMode {
    /// The web library was empty: the bundle replaced it atomically.
    #[default]
    Replace,
    /// The web library had posts or collections: the bundle was merged into
    /// it (`--merge`).
    Merge,
}

/// Why an install failed.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationFailure {
    /// A stable code: an API error code (`validation_failed`, `conflict`,
    /// `quota_exceeded`, `user_locked`, `internal`…) or a job code
    /// (`lease_expired`, `cancelled`).
    pub code: String,
    /// Developer-facing detail of a refused bundle (`validation_failed`,
    /// `conflict`, `quota_exceeded`); never content.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub detail: Option<String>,
}

/// An install of a migration bundle: a `migrate` job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Migration {
    /// Install id: the job id, as text.
    pub id: String,
    /// The `migrate` job (`GET /jobs`, `job.updated`).
    pub job_id: i64,
    pub state: MigrationState,
    pub stage: MigrationStage,
    /// Progress of the whole install, 0 to 1.
    pub progress: f64,
    /// Whether the CLI asked to merge into a library that is not empty.
    pub merge: bool,
    /// Tries that ended without success; a transient failure is tried again.
    pub attempts: u32,
    /// Tries allowed.
    pub max_attempts: u32,
    /// When it was accepted, unix ms.
    pub created_at: i64,
    /// When it ended, unix ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub finished_at: Option<i64>,
    /// Why it failed, or why its last try failed before a retry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub error: Option<MigrationFailure>,
    /// The reconciliation report, once installed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub report: Option<MigrationReport>,
}

/// Error codes whose job detail is a curated, content-free message of the
/// install, sent to the CLI. Other details (internal errors) stay in the
/// job row.
const PUBLIC_DETAIL_CODES: [&str; 3] = ["validation_failed", "conflict", "quota_exceeded"];

impl Migration {
    /// The install of the `migrate` job `row`, with its `report` once it
    /// succeeded.
    #[must_use]
    pub fn from_job(row: &JobRow, report: Option<MigrationReport>) -> Self {
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload_json).unwrap_or(serde_json::Value::Null);
        let merge = payload
            .get("merge")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let state = match row.state {
            JobState::Queued | JobState::Running => MigrationState::Running,
            JobState::Succeeded => MigrationState::Succeeded,
            JobState::Failed | JobState::Cancelled => MigrationState::Failed,
        };
        let stage = match row.state {
            JobState::Queued => MigrationStage::Queued,
            JobState::Running => row
                .stage
                .as_deref()
                .and_then(MigrationStage::parse)
                .unwrap_or(MigrationStage::Queued),
            JobState::Succeeded | JobState::Failed | JobState::Cancelled => MigrationStage::Done,
        };
        let error = match (row.state, &row.error_code) {
            (JobState::Cancelled, _) => Some(MigrationFailure {
                code: "cancelled".to_owned(),
                detail: None,
            }),
            (_, Some(code)) if row.state != JobState::Succeeded => Some(MigrationFailure {
                code: code.clone(),
                detail: PUBLIC_DETAIL_CODES
                    .contains(&code.as_str())
                    .then(|| public_detail(code, row.error_detail.as_deref()))
                    .flatten(),
            }),
            _ => None,
        };
        let progress = match row.state {
            JobState::Succeeded => 1.0,
            _ => row.progress.unwrap_or(0.0).clamp(0.0, 1.0),
        };
        Self {
            id: row.id.to_string(),
            job_id: row.id,
            state,
            stage,
            progress,
            merge,
            attempts: row.attempts,
            max_attempts: row.max_attempts,
            created_at: row.created_at,
            finished_at: row.finished_at,
            error,
            report,
        }
    }
}

/// The detail of a job error without the `<code>: ` prefix that an API
/// error's text carries.
fn public_detail(code: &str, detail: Option<&str>) -> Option<String> {
    let detail = detail?;
    let detail = detail
        .strip_prefix(code)
        .and_then(|rest| rest.strip_prefix(": "))
        .unwrap_or(detail);
    (!detail.is_empty()).then(|| detail.to_owned())
}

/// The reconciliation report of an install (plan §4.3): counts only.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct MigrationReport {
    /// Whether the bundle replaced an empty library or was merged into one.
    pub mode: InstallMode,
    /// What `shelfy-migrate` counted in the desktop library and wrote to the
    /// bundle, as it sent it.
    #[schema(value_type = Object)]
    pub bundle: serde_json::Value,
    /// Rows of the web library after the install.
    pub installed: InstalledCounts,
    /// What the merge did, for a merge.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub merge: Option<MergeCounts>,
    pub objects: InstalledObjects,
    pub renditions: RenditionCounts,
    pub archive: ArchiveCounts,
    /// The desktop settings the library took (`language`,
    /// `archiveAssetTypes`); settings the web library had already win.
    pub settings: Vec<String>,
    /// The file the previous library is kept in for 7 days, next to the
    /// live one.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub previous: Option<String>,
    /// How long the install took, ms.
    pub duration_ms: u64,
}

/// Rows of the installed library.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
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

/// What a merge did with the bundle's rows (plan §4.2: the duplicate policy
/// of P1-10 for posts the library already had).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct MergeCounts {
    /// The bundle's posts by platform: inserted as new posts, or merged into
    /// the post with the same key.
    pub posts: BTreeMap<String, MergedPosts>,
    /// Merged posts whose bundle row won (more archived files, an analysis,
    /// a user layer): its media and AI layers replaced the stored ones.
    pub replaced: u64,
    /// Merged posts that did not change.
    pub unchanged: u64,
    /// Merged posts that took the bundle's analysis or date.
    pub ai_filled: u64,
    pub dates_filled: u64,
    /// Merged posts whose notes were joined, and manual tags added.
    pub notes_joined: u64,
    pub tags_added: u64,
    /// The bundle's collections: new, or the library's of the same folder
    /// (`platform` and `external_id`, else the same name).
    pub collections_inserted: u64,
    pub collections_matched: u64,
    /// The bundle's memberships: new, or already there.
    pub memberships_added: u64,
    pub memberships_present: u64,
    /// The bundle's site versions: new, or already there (same post and
    /// capture time).
    pub captures_added: u64,
    pub captures_present: u64,
    pub aliases_added: u64,
    pub clusters_added: u64,
    pub cluster_memberships_added: u64,
}

impl MergeCounts {
    /// The bundle's posts that landed, by platform: inserted plus merged.
    #[must_use]
    pub fn landed(&self) -> BTreeMap<String, u64> {
        self.posts
            .iter()
            .map(|(platform, p)| (platform.clone(), p.inserted + p.merged))
            .collect()
    }
}

/// The bundle's posts of one platform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct MergedPosts {
    pub inserted: u64,
    pub merged: u64,
}

/// The objects the install stored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
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
#[serde(rename_all = "camelCase", default)]
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
    /// Sizes of the covers' `g480` renditions rendered by this install (plan
    /// §6.2 budget: p50 ≤35 KB, p95 ≤60 KB).
    #[serde(skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub cover_bytes: Option<SizeStats>,
}

/// A distribution of sizes, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
pub struct SizeStats {
    pub count: u64,
    pub p50: u64,
    pub p95: u64,
    pub max: u64,
}

impl SizeStats {
    /// The distribution of `sizes` (nearest rank); `None` when empty.
    #[must_use]
    pub fn of(mut sizes: Vec<u64>) -> Option<Self> {
        if sizes.is_empty() {
            return None;
        }
        sizes.sort_unstable();
        let rank = |p: usize| -> u64 {
            // Nearest rank: the smallest value with at least p % of the
            // values at or below it.
            let n = sizes.len();
            let index = (p * n).div_ceil(100).clamp(1, n) - 1;
            sizes[index]
        };
        Some(Self {
            count: sizes.len() as u64,
            p50: rank(50),
            p95: rank(95),
            max: sizes[sizes.len() - 1],
        })
    }
}

/// Archive work left for the workers (P2), by class (OI-6, OI-7).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", default)]
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

/// What `GET /migrations/preflight` tells the CLI before it uploads.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MigrationPreflight {
    /// The web library has no posts and no collections: a bundle replaces
    /// it. Otherwise a bundle needs `--merge`.
    pub library_empty: bool,
    /// Posts in the web library, trash included.
    pub posts: u64,
    /// The quota in bytes, media plus database; 0 means unlimited.
    pub quota_bytes: i64,
    /// What the library uses now, media plus database, in bytes.
    pub used_bytes: i64,
    /// The largest object an upload may hold (a kept video).
    pub max_object_bytes: u64,
    /// The largest bundle database.
    pub max_database_bytes: u64,
    /// The `migrate` job already queued or running, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub active_job_id: Option<i64>,
}

/// Whether an error code is one whose job detail the CLI gets.
#[must_use]
pub fn has_public_detail(code: ErrorCode) -> bool {
    PUBLIC_DETAIL_CODES.contains(&code.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(state: JobState) -> JobRow {
        JobRow {
            id: 7,
            user_id: "U".into(),
            kind: "migrate".into(),
            dedupe_key: Some("migrate".into()),
            state,
            priority: 100,
            payload_json: r#"{"dbUploadId":"X","merge":true}"#.into(),
            attempts: 0,
            max_attempts: 2,
            run_at: 1,
            lease_until: None,
            progress: Some(0.5),
            stage: Some("objects".into()),
            error_code: None,
            error_detail: None,
            created_at: 1,
            updated_at: 2,
            finished_at: None,
        }
    }

    #[test]
    fn a_job_reads_as_an_install() {
        let running = Migration::from_job(&row(JobState::Running), None);
        assert_eq!(running.id, "7");
        assert_eq!(running.state, MigrationState::Running);
        assert_eq!(running.stage, MigrationStage::Objects);
        assert!(running.merge);
        assert_eq!(running.progress, 0.5);
        let queued = Migration::from_job(&row(JobState::Queued), None);
        assert_eq!(queued.stage, MigrationStage::Queued);

        let mut failed = row(JobState::Failed);
        failed.error_code = Some("validation_failed".into());
        failed.error_detail = Some("validation_failed: 2 objects are missing".into());
        let failed = Migration::from_job(&failed, None);
        assert_eq!(failed.state, MigrationState::Failed);
        assert_eq!(failed.stage, MigrationStage::Done);
        let error = failed.error.unwrap();
        assert_eq!(error.code, "validation_failed");
        assert_eq!(error.detail.as_deref(), Some("2 objects are missing"));

        let mut internal = row(JobState::Failed);
        internal.error_code = Some("internal".into());
        internal.error_detail = Some("disk I/O error at /data/x".into());
        let internal = Migration::from_job(&internal, None);
        assert_eq!(internal.error.unwrap().detail, None, "never sent");

        let cancelled = Migration::from_job(&row(JobState::Cancelled), None);
        assert_eq!(cancelled.error.unwrap().code, "cancelled");
        let done = Migration::from_job(&row(JobState::Succeeded), None);
        assert_eq!((done.progress, done.error), (1.0, None));
        for stage in [
            MigrationStage::Queued,
            MigrationStage::Merging,
            MigrationStage::Done,
        ] {
            assert_eq!(MigrationStage::parse(stage.as_str()), Some(stage));
        }
    }

    #[test]
    fn sizes_take_the_nearest_rank() {
        assert_eq!(SizeStats::of(Vec::new()), None);
        let stats = SizeStats::of((1..=100).rev().collect()).unwrap();
        assert_eq!(
            (stats.count, stats.p50, stats.p95, stats.max),
            (100, 50, 95, 100)
        );
        let one = SizeStats::of(vec![7]).unwrap();
        assert_eq!((one.p50, one.p95, one.max), (7, 7, 7));
    }
}
