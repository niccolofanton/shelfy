//! The work of a `migrate` job (see [`super`] for the stages): validation,
//! the objects, then a replace (here) or a merge ([`super::merge`]).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::join_all;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::json;
use shelfy_core::db::{LIBRARY_FILE_NAME, UserDb, UserDbConfig};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::{self, NewNotification, Notification};
use shelfy_core::search::index;
use shelfy_media::kind::KindSet;
use shelfy_media::name::{Rendition, Variants};
use shelfy_media::pool::ImagePool;
use shelfy_media::refs::{self, ObjectMeta, Origin};
use shelfy_media::render::{RenderError, Rendered};
use shelfy_media::store::{IngestLimits, MediaStore, StagedObject, StoredObject, UserMedia};

use super::swap::{self, SwapError};
use super::validate::{self, BundleFacts, BundleLimits, BundleObject, Invalid};
use super::{
    ArchiveCounts, InstallMode, InstalledCounts, InstalledObjects, MigrationReport, MigrationStage,
    RenditionCounts, SizeStats, merge,
};
use crate::config::create_private_dir;
use crate::control::uploads::{self, Upload, UploadPurpose};
use crate::control::users;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::ChangeReason;
use crate::ids::now_ms;
use crate::jobs::migrate::Payload;
use crate::jobs::{JobContext, JobError, usage};
use crate::routes::uploads::remove_files;
use crate::state::blocking;

/// Objects stored and rendered per write transaction.
pub(super) const OBJECT_CHUNK: usize = 16;
/// `meta.key` prefix of the stored report; the job id follows.
pub const REPORT_META_PREFIX: &str = "migration.report:";
/// `notifications.kind` and `code` of a finished install.
pub const NOTIFICATION_KIND: &str = "migration";
pub const NOTIFICATION_CODE: &str = "migration.installed";

/// Overall progress at the start of each stage (the job's `progress`).
pub(super) mod at {
    pub const VALIDATING: f64 = 0.0;
    pub const OBJECTS: f64 = 0.05;
    pub const OBJECTS_END: f64 = 0.8;
    pub const INDEX: f64 = 0.8;
    pub const MERGING_END: f64 = 0.95;
    pub const REPORT: f64 = 0.9;
    pub const INSTALLING: f64 = 0.93;
}

/// The `meta` key of the report of job `id`.
#[must_use]
pub fn report_key(job_id: i64) -> String {
    format!("{REPORT_META_PREFIX}{job_id}")
}

/// The file the previous library of job `id` is kept in, next to `live`.
#[must_use]
pub fn previous_path(live: &Path, job_id: i64) -> PathBuf {
    live.with_file_name(format!(
        "{}{job_id}.sqlite",
        super::housekeeping::PREVIOUS_PREFIX
    ))
}

/// Runs one try of the install of `payload` to the end. The work directory
/// goes in every case; the uploads stay unless it succeeded.
///
/// # Errors
///
/// Why the try failed (see [`crate::jobs::migrate`]).
pub async fn run(ctx: &JobContext, payload: &Payload) -> Result<MigrationReport, JobError> {
    let started = Instant::now();
    let work = ctx
        .state()
        .config()
        .data_dir
        .migrations_dir()
        .join(ctx.id().to_string());
    let outcome = install(ctx, payload, &work, started).await;
    let removed = blocking(move || match fs::remove_dir_all(&work) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::internal(e)),
    })
    .await;
    if let Err(err) = removed {
        tracing::warn!(job_id = ctx.id(), error = %err, "cannot remove the install's work directory");
    }
    match &outcome {
        Ok(report) => tracing::info!(
            job_id = ctx.id(),
            user_id = %ctx.user_id(),
            mode = ?report.mode,
            posts = report.installed.posts.values().sum::<u64>(),
            objects = report.objects.total,
            rendered = report.renditions.rendered,
            duration_ms = report.duration_ms,
            "migration installed"
        ),
        Err(err) => tracing::warn!(
            job_id = ctx.id(),
            user_id = %ctx.user_id(),
            code = err.code(),
            transient = err.is_transient(),
            "migration try failed"
        ),
    }
    outcome
}

/// Where an object comes from.
#[derive(Clone, Debug)]
pub(super) enum Source {
    /// Already in the user's store, at this path.
    Stored(PathBuf),
    /// A complete upload, at this path.
    Upload(PathBuf),
}

/// An object to store, with what it is used for.
#[derive(Clone, Debug)]
pub(super) struct Item {
    pub(super) object: BundleObject,
    pub(super) source: Source,
    /// Shown in the grid: needs `g480`.
    pub(super) grid: bool,
    /// A cover: also needs its ThumbHash.
    pub(super) cover: bool,
}

/// An object ready to commit.
pub(super) struct Prepared {
    pub(super) item: Item,
    pub(super) staged: Option<StagedObject>,
    /// Render from this file.
    render: Option<PathBuf>,
}

/// What [`validated`] found: the bundle and how it joins the library.
pub(super) struct Checked {
    pub(super) facts: BundleFacts,
    pub(super) mode: InstallMode,
    pub(super) items: Vec<Item>,
    pub(super) media: UserMedia,
    /// The validated copy of the bundle's database.
    pub(super) work_db: PathBuf,
}

async fn install(
    ctx: &JobContext,
    payload: &Payload,
    work: &Path,
    started: Instant,
) -> Result<MigrationReport, JobError> {
    // A library locked for maintenance (a restore) is not touched: the job
    // waits for the unlock.
    refuse_locked(ctx)?;
    // A try that stopped after its install landed (a crash, a lost lease)
    // finds the report in the library: only the follow-ups are left.
    if let Some(report) = landed_report(ctx).await? {
        follow_up(ctx, &report).await?;
        return Ok(report);
    }
    let checked = validated(ctx, payload, work).await?;
    let report = match checked.mode {
        InstallMode::Replace => replace(ctx, checked, started).await?,
        InstallMode::Merge => merge::merge(ctx, checked, started).await?,
    };
    follow_up(ctx, &report).await?;
    Ok(MigrationReport {
        duration_ms: elapsed_ms(started),
        ..report
    })
}

/// Reports `stage` at the overall `progress`; stops a cancelled try.
pub(super) async fn stage(
    ctx: &JobContext,
    stage: MigrationStage,
    progress: f64,
) -> Result<(), JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::cancelled());
    }
    ctx.progress(Some(progress), Some(stage.as_str())).await;
    Ok(())
}

/// The report of this job, when an earlier try installed it already.
async fn landed_report(ctx: &JobContext) -> Result<Option<MigrationReport>, JobError> {
    let key = report_key(ctx.id());
    let library = ctx.state().config().data_dir.library_db(ctx.user_id());
    if !tokio::fs::try_exists(&library).await.unwrap_or(false) {
        return Ok(None);
    }
    let stored: Option<String> = ctx
        .user_db(move |db| {
            db.read(|c| {
                c.query_row("SELECT value FROM meta WHERE key = ?1", [&key], |r| {
                    r.get(0)
                })
                .optional()
                .map_err(shelfy_core::db::DbError::from)
            })
            .map_err(JobError::from)
        })
        .await?;
    Ok(stored.and_then(|json| serde_json::from_str(&json).ok()))
}

/// Step 1: the bundle checked and every object found, and how it joins the
/// web library.
async fn validated(ctx: &JobContext, payload: &Payload, work: &Path) -> Result<Checked, JobError> {
    stage(ctx, MigrationStage::Validating, at::VALIDATING).await?;
    let state = ctx.state();
    let data = state.config().data_dir.clone();
    let user_id = ctx.user_id().to_owned();
    let control = Arc::clone(state.control());
    let upload = {
        let (user_id, upload_id) = (user_id.clone(), payload.db_upload_id.clone());
        blocking(move || control.read(|c| uploads::get(c, &user_id, &upload_id))).await?
    }
    .filter(|u| u.is_complete() && u.purpose == Some(UploadPurpose::MigrationDb))
    .ok_or_else(|| {
        JobError::permanent(ErrorCode::ValidationFailed.as_str()).with_detail(
            "the bundle's database upload is gone: run shelfy-migrate run again to upload it",
        )
    })?;

    // A private copy of the uploaded database, checked.
    let work_db = work.join(LIBRARY_FILE_NAME);
    let facts = {
        let source = uploads::file_path(&data.uploads_dir(), &upload.id, true);
        let (work, work_db) = (work.to_path_buf(), work_db.clone());
        blocking(move || -> Result<BundleFacts, ApiError> {
            create_private_dir(&work).map_err(ApiError::internal)?;
            fs::copy(&source, &work_db).map_err(ApiError::internal)?;
            validate::validate(&work_db, &BundleLimits::default()).map_err(invalid_bundle)
        })
        .await?
    };

    // Replace an empty library; merge into one that is not, when asked to.
    let empty = live_is_empty(ctx).await?;
    let mode = match (empty, payload.merge) {
        (true, _) => InstallMode::Replace,
        (false, true) => InstallMode::Merge,
        (false, false) => return Err(not_empty().into()),
    };
    let media = MediaStore::new(data.users_dir())
        .user(&user_id)
        .map_err(ApiError::internal)?;
    let sources = locate(ctx, &media, &facts.objects).await?;
    check_quota(ctx, &facts, &sources, &work_db).await?;

    // The objects the grid shows, and the covers among them.
    let grid = {
        let work_db = work_db.clone();
        blocking(move || {
            let conn = open_read_only(&work_db)?;
            grid_objects(&conn).map_err(ApiError::from)
        })
        .await?
    };
    let items = facts
        .objects
        .iter()
        .zip(sources)
        .map(|(object, source)| {
            let cover = grid.get(&object.id).copied();
            Item {
                object: object.clone(),
                source,
                grid: cover.is_some(),
                cover: cover.unwrap_or(false),
            }
        })
        .collect();
    stage(ctx, MigrationStage::Validating, at::OBJECTS).await?;
    Ok(Checked {
        facts,
        mode,
        items,
        media,
        work_db,
    })
}

/// The bundle's database, read-only, for a merge's reads.
pub(super) fn open_read_only(path: &Path) -> Result<Connection, ApiError> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(ApiError::internal)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA query_only = ON;")
        .map_err(ApiError::internal)?;
    Ok(conn)
}

/// Whether the user's live library has no posts and no collections.
async fn live_is_empty(ctx: &JobContext) -> Result<bool, JobError> {
    ctx.user_db(|db| {
        db.read(|c| swap::is_empty(c).map_err(shelfy_core::db::DbError::from))
            .map_err(JobError::from)
    })
    .await
}

/// 409 for a library that is not empty, without `--merge`.
pub(crate) fn not_empty() -> ApiError {
    ApiError::new(ErrorCode::Conflict).with_detail(
        "the web library is not empty: run shelfy-migrate run --merge to merge into it",
    )
}

/// Refuses a bundle whose new bytes would put the library over the user's
/// quota (0: unlimited, the owner).
async fn check_quota(
    ctx: &JobContext,
    facts: &BundleFacts,
    sources: &[Source],
    work_db: &Path,
) -> Result<(), JobError> {
    let control = Arc::clone(ctx.state().control());
    let user_id = ctx.user_id().to_owned();
    let quota = blocking(move || control.read(|c| users::usage(c, &user_id)))
        .await?
        .map_or(0, |u| u.quota_bytes);
    if quota <= 0 {
        return Ok(());
    }
    let used = live_usage(ctx).await?;
    let db_bytes = fs::metadata(work_db).map_or(0, |m| m.len());
    let new_bytes: u64 = facts
        .objects
        .iter()
        .zip(sources)
        .filter(|(_, source)| matches!(source, Source::Upload(_)))
        .map(|(object, _)| object.bytes)
        .sum::<u64>()
        + db_bytes;
    let after = i64::try_from(new_bytes)
        .unwrap_or(i64::MAX)
        .saturating_add(used);
    if after > quota {
        return Err(ApiError::new(ErrorCode::QuotaExceeded)
            .with_detail(format!(
                "the install needs {} bytes beyond the quota of {quota} bytes",
                after - quota
            ))
            .into());
    }
    Ok(())
}

/// Media plus database bytes of the user's live library.
pub(super) async fn live_usage(ctx: &JobContext) -> Result<i64, JobError> {
    ctx.user_db(|db| {
        db.read(|c| {
            let media: i64 = c.query_row(
                "SELECT coalesce(sum(bytes), 0) FROM media_objects",
                [],
                |r| r.get(0),
            )?;
            let pages: i64 = c.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let size: i64 = c.query_row("PRAGMA page_size", [], |r| r.get(0))?;
            Ok::<_, shelfy_core::db::DbError>(media.saturating_add(pages.saturating_mul(size)))
        })
        .map_err(JobError::from)
    })
    .await
}

/// Steps 2–5 of a replace: the objects and the derived data in the new
/// library, its report, then the atomic swap.
async fn replace(
    ctx: &JobContext,
    checked: Checked,
    started: Instant,
) -> Result<MigrationReport, JobError> {
    let state = ctx.state();
    let user_id = ctx.user_id().to_owned();
    let Checked {
        facts,
        items,
        media,
        work_db,
        ..
    } = checked;

    // 2. The objects, recorded in the new library.
    let db = {
        let path = work_db.clone();
        let config = UserDbConfig {
            max_readers: 1,
            ..UserDbConfig::default()
        };
        blocking(move || UserDb::open(&path, &config).map(Arc::new)).await?
    };
    {
        let db = Arc::clone(&db);
        blocking(move || db.write(normalize)).await?;
    }
    let mut objects = InstalledObjects::default();
    let mut renditions = RenditionCounts {
        wanted: items.iter().filter(|i| i.grid).count() as u64,
        ..RenditionCounts::default()
    };
    let mut cover_sizes = Vec::new();
    stage(ctx, MigrationStage::Objects, at::OBJECTS).await?;
    let total = items.len().max(1);
    let mut done = 0usize;
    for chunk in items.chunks(OBJECT_CHUNK) {
        let (prepared, rendered) = prepare_chunk(ctx, &media, chunk).await?;
        count_chunk(
            &prepared,
            &rendered,
            &mut objects,
            &mut renditions,
            &mut cover_sizes,
        );
        let thumbhashes = {
            let (db, media) = (Arc::clone(&db), media.clone());
            let now = now_ms();
            blocking(move || db.write(|tx| commit(tx, &media, prepared, rendered, now))).await?
        };
        renditions.thumbhashes += thumbhashes;
        done += chunk.len();
        let progress = at::OBJECTS + (at::OBJECTS_END - at::OBJECTS) * done as f64 / total as f64;
        stage(ctx, MigrationStage::Objects, progress).await?;
    }
    renditions.cover_bytes = SizeStats::of(cover_sizes);

    // 3. Derived data: the search index and the archive state.
    stage(ctx, MigrationStage::Index, at::INDEX).await?;
    {
        let db = Arc::clone(&db);
        let now = now_ms();
        blocking(move || {
            db.write(|tx| {
                index::rebuild(tx).map_err(RepoError::from)?;
                archive_states(tx, None, now).map_err(RepoError::from)
            })
        })
        .await?;
    }

    // 4. The report, stored in the new library.
    stage(ctx, MigrationStage::Report, at::REPORT).await?;
    let live = state.config().data_dir.library_db(&user_id);
    let previous = previous_path(&live, ctx.id());
    let settings = {
        let (live, work_db) = (live.clone(), work_db.clone());
        blocking(move || desktop_settings_taken(&work_db, &live).map_err(ApiError::internal))
            .await?
    };
    let report = {
        let db = Arc::clone(&db);
        let now = now_ms();
        let key = report_key(ctx.id());
        let report = MigrationReport {
            mode: InstallMode::Replace,
            bundle: facts.summary.clone(),
            objects,
            renditions,
            settings,
            previous: previous
                .file_name()
                .map(|n| n.to_string_lossy().into_owned()),
            duration_ms: elapsed_ms(started),
            ..MigrationReport::default()
        };
        blocking(move || {
            db.write(|tx| -> Result<MigrationReport, RepoError> {
                let report = MigrationReport {
                    installed: installed_counts(tx)?,
                    archive: archive_counts(tx, now)?,
                    ..report
                };
                store_report(tx, &key, &report)?;
                Ok(report)
            })
        })
        .await?
    };

    // 5. Replace the live library, unless an operator locked it meanwhile.
    stage(ctx, MigrationStage::Installing, at::INSTALLING).await?;
    refuse_locked(ctx)?;
    // Handles on the live library are dropped first, and again after the
    // swap: any handle opened in between saw the old library.
    state.user_dbs().evict(&user_id);
    {
        let db = Arc::try_unwrap(db)
            .map_err(|_| ApiError::internal(anyhow::anyhow!("the new library is still in use")))?;
        blocking(move || -> Result<(), ApiError> {
            db.checkpoint()?;
            drop(db);
            swap::replace_library(&work_db, &live, &previous).map_err(|err| match err {
                SwapError::NotEmpty => not_empty(),
                SwapError::Busy => ApiError::new(ErrorCode::Unavailable)
                    .with_retry_after(5)
                    .with_detail("the web library stayed locked: retry the install"),
                // An operator is restoring the library: retry after the unlock.
                SwapError::Locked => ApiError::user_locked(),
                other => ApiError::internal(other),
            })
        })
        .await?;
    }
    state.user_dbs().evict(&user_id);
    Ok(report)
}

/// [`JobError`] `user_locked` while an operator holds the user's library
/// locked for maintenance (a restore): the job waits for the unlock.
pub(super) fn refuse_locked(ctx: &JobContext) -> Result<(), JobError> {
    match ctx.state().user_dbs().is_locked(ctx.user_id()) {
        Ok(false) => Ok(()),
        Ok(true) => Err(ApiError::user_locked().into()),
        Err(err) => Err(err.into()),
    }
}

/// The follow-ups of an install that landed: open tabs refresh, the
/// notification (once), the storage is counted again, and the uploads are
/// consumed. Each is idempotent: a later try may repeat them.
async fn follow_up(ctx: &JobContext, report: &MigrationReport) -> Result<(), JobError> {
    let state = ctx.state();
    let user_id = ctx.user_id().to_owned();
    crate::library::announce(state.events(), &user_id, ChangeReason::Import, None);
    match notify_once(ctx, report).await {
        Ok(Some(created)) => {
            let notification = crate::events::model::Notification::from(created);
            state.events().notification(&user_id, &notification);
        }
        Ok(None) => {}
        Err(err) => {
            tracing::warn!(job_id = ctx.id(), error = %err, "cannot store the migration notification");
        }
    }
    // The storage the library uses (`GET /me/usage`) is counted again.
    if let Err(err) = usage::enqueue(ctx.jobs(), &user_id).await {
        tracing::warn!(job_id = ctx.id(), error = %err, "cannot enqueue the usage count");
    }
    // The uploads are consumed: every complete migration upload of the user
    // is now in the store, or belongs to no install.
    let control = Arc::clone(state.control());
    let uploads_dir = state.config().data_dir.uploads_dir();
    blocking(move || -> Result<(), ApiError> {
        let mut ids = Vec::new();
        for purpose in [UploadPurpose::MigrationObject, UploadPurpose::MigrationDb] {
            let done = control.read(|c| uploads::complete_of(c, &user_id, purpose))?;
            ids.extend(done.into_iter().map(|u| u.id));
        }
        remove_files(&uploads_dir, &ids);
        control.write(|tx| uploads::delete(tx, &ids))?;
        Ok(())
    })
    .await?;
    Ok(())
}

/// Stores the notification of this job's install, unless an earlier try
/// stored it; returns it when stored now.
async fn notify_once(
    ctx: &JobContext,
    report: &MigrationReport,
) -> Result<Option<Notification>, JobError> {
    let new = notification(ctx.id(), report);
    let job_id = ctx.id();
    ctx.user_db(move |db| {
        db.write(|tx| -> Result<Option<Notification>, RepoError> {
            let exists: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM notifications WHERE code = ?1
                                AND json_extract(params_json, '$.jobId') = ?2)",
                params![NOTIFICATION_CODE, job_id],
                |r| r.get(0),
            )?;
            if exists {
                return Ok(None);
            }
            notifications::create(tx, &new, now_ms()).map(Some)
        })
        .map_err(JobError::from)
    })
    .await
}

/// Finds every object: in the user's store with the right size, or in a
/// complete upload of the same hash.
async fn locate(
    ctx: &JobContext,
    media: &UserMedia,
    objects: &[BundleObject],
) -> Result<Vec<Source>, JobError> {
    let state = ctx.state();
    let control = Arc::clone(state.control());
    let uploads_dir = state.config().data_dir.uploads_dir();
    let user_id = ctx.user_id().to_owned();
    let (media, objects) = (media.clone(), objects.to_vec());
    Ok(blocking(move || -> Result<Vec<Source>, ApiError> {
        let hashes: Vec<String> = objects.iter().map(|o| o.digest.to_string()).collect();
        let mut uploaded: HashMap<String, Upload> = HashMap::new();
        for batch in hashes.chunks(500) {
            let found = control.read(|c| {
                uploads::complete_by_sha256(c, &user_id, UploadPurpose::MigrationObject, batch)
            })?;
            for upload in found {
                uploaded.entry(upload.meta.sha256.clone()).or_insert(upload);
            }
        }
        let mut sources = Vec::with_capacity(objects.len());
        let mut missing = 0u64;
        for (object, hash) in objects.iter().zip(&hashes) {
            let stored = media.object_path(&object.digest, object.kind);
            if size_of(&stored) == Some(object.bytes) {
                sources.push(Source::Stored(stored));
                continue;
            }
            let upload = uploaded.get(hash).filter(|u| {
                u.meta.ext.as_deref() == Some(object.kind.ext())
                    && u64::try_from(u.length).ok() == Some(object.bytes)
            });
            match upload {
                Some(upload) => sources.push(Source::Upload(uploads::file_path(
                    &uploads_dir,
                    &upload.id,
                    true,
                ))),
                None => missing += 1,
            }
        }
        if missing > 0 {
            return Err(ApiError::new(ErrorCode::ValidationFailed).with_detail(format!(
                "{missing} objects of the bundle are neither stored nor uploaded: upload them first"
            )));
        }
        Ok(sources)
    })
    .await?)
}

/// Prepares one chunk of objects off the async workers, then renders what
/// the grid shows on the shared image pool.
pub(super) async fn prepare_chunk(
    ctx: &JobContext,
    media: &UserMedia,
    chunk: &[Item],
) -> Result<(Vec<Prepared>, Vec<Option<Result<Rendered, RenderError>>>), JobError> {
    if ctx.is_cancelled() {
        return Err(JobError::cancelled());
    }
    let prepared = {
        let (media, chunk) = (media.clone(), chunk.to_vec());
        blocking(move || {
            chunk
                .into_iter()
                .map(|item| prepare(&media, item))
                .collect::<Result<Vec<_>, ApiError>>()
        })
        .await?
    };
    ctx.heartbeat();
    let rendered = join_all(prepared.iter().map(|p| {
        let path = p.render.clone();
        async move {
            match path {
                Some(path) => Some(
                    ImagePool::shared()
                        .render_file(path, Rendition::G480.spec())
                        .await,
                ),
                None => None,
            }
        }
    }))
    .await;
    ctx.heartbeat();
    Ok((prepared, rendered))
}

/// Counts what a chunk stored and rendered.
pub(super) fn count_chunk(
    prepared: &[Prepared],
    rendered: &[Option<Result<Rendered, RenderError>>],
    objects: &mut InstalledObjects,
    renditions: &mut RenditionCounts,
    cover_sizes: &mut Vec<u64>,
) {
    for (p, r) in prepared.iter().zip(rendered) {
        objects.total += 1;
        objects.bytes += p.item.object.bytes;
        match p.item.source {
            Source::Upload(_) => objects.from_uploads += 1,
            Source::Stored(_) => objects.already_stored += 1,
        }
        if p.item.grid {
            match r {
                Some(Ok(rendered)) => {
                    renditions.rendered += 1;
                    if p.item.cover {
                        cover_sizes.push(rendered.webp.len() as u64);
                    }
                }
                Some(Err(err)) => {
                    renditions.failed += 1;
                    tracing::warn!(error = %err, "cannot render a migrated image");
                }
                None if !p.item.object.kind.is_renderable() => renditions.not_renderable += 1,
                None => renditions.existing += 1,
            }
        }
    }
}

/// Streams an uploaded object into the store's staging area and checks it
/// is what its row says; picks the file to render from. Blocking.
fn prepare(media: &UserMedia, item: Item) -> Result<Prepared, ApiError> {
    let object = &item.object;
    let staged = match &item.source {
        Source::Stored(_) => None,
        Source::Upload(path) => {
            let file = fs::File::open(path).map_err(ApiError::internal)?;
            let limits = IngestLimits {
                max_bytes: object.bytes,
                accept: KindSet::ALL,
            };
            let staged = media.ingest(file, limits).map_err(|err| {
                ApiError::new(ErrorCode::ValidationFailed)
                    .with_detail(format!("object {}: {err}", object.id))
            })?;
            if staged.digest() != object.digest
                || staged.kind() != object.kind
                || staged.size() != object.bytes
            {
                return Err(
                    ApiError::new(ErrorCode::ValidationFailed).with_detail(format!(
                        "object {}: the uploaded bytes do not match the bundle",
                        object.id
                    )),
                );
            }
            Some(staged)
        }
    };
    let render = if item.grid && object.kind.is_renderable() {
        match (&staged, &item.source) {
            (Some(staged), _) => Some(staged.path().to_path_buf()),
            (None, Source::Stored(path)) => {
                // A cover needs its ThumbHash even when the rendition exists.
                let rendition = media.rendition_path(&object.digest, Rendition::G480);
                (item.cover || !rendition.is_file()).then(|| path.clone())
            }
            (None, Source::Upload(_)) => None,
        }
    } else {
        None
    };
    Ok(Prepared {
        item,
        staged,
        render,
    })
}

/// Publishes one prepared object and its rendition and records it, in the
/// caller's write transaction (the store's protocol, rule 1). Returns its
/// row id and the rendition, if any.
pub(super) fn commit_object(
    tx: &Connection,
    media: &UserMedia,
    prepared: Prepared,
    render: Option<Result<Rendered, RenderError>>,
    now: i64,
) -> Result<(i64, Option<Rendered>), RepoError> {
    let object = &prepared.item.object;
    let rendered = render.and_then(Result::ok);
    let meta = ObjectMeta {
        width: rendered.as_ref().map(|r| r.source_width),
        height: rendered.as_ref().map(|r| r.source_height),
        ..ObjectMeta::new(object.role, Origin::Migration)
    };
    let id = match prepared.staged {
        Some(staged) => {
            let renditions: Vec<(Rendition, &[u8])> = rendered
                .iter()
                .map(|r| (Rendition::G480, r.webp.as_slice()))
                .collect();
            refs::publish_and_record(tx, media, staged, &renditions, &meta, now)?.0
        }
        None => {
            let stored = StoredObject {
                digest: object.digest,
                kind: object.kind,
                size: object.bytes,
                deduplicated: true,
            };
            let id = refs::record(tx, &stored, &meta, now)?;
            if let Some(r) = &rendered {
                let exists = media
                    .rendition_path(&object.digest, Rendition::G480)
                    .is_file();
                if exists {
                    refs::add_variants(tx, id, Variants::NONE.with(Rendition::G480))?;
                } else {
                    refs::record_rendition(
                        tx,
                        media,
                        id,
                        &object.digest,
                        Rendition::G480,
                        &r.webp,
                    )?;
                }
            }
            id
        }
    };
    Ok((id, rendered))
}

/// Commits a chunk into the new library of a replace, where the bundle's
/// row ids are kept. Returns how many posts got a ThumbHash.
fn commit(
    tx: &Connection,
    media: &UserMedia,
    prepared: Vec<Prepared>,
    rendered: Vec<Option<Result<Rendered, RenderError>>>,
    now: i64,
) -> Result<u64, RepoError> {
    let mut thumbhashes = 0u64;
    for (p, render) in prepared.into_iter().zip(rendered) {
        let (expected, cover) = (p.item.object.id, p.item.cover);
        let (id, rendered) = commit_object(tx, media, p, render, now)?;
        if id != expected {
            return Err(RepoError::Conflict("media object"));
        }
        if cover && let Some(r) = &rendered {
            thumbhashes += refs::set_cover_thumbhash(tx, id, &r.thumbhash, now)? as u64;
        }
    }
    Ok(thumbhashes)
}

/// Clears what the server derives itself: renditions and ThumbHashes are
/// made again by the install.
fn normalize(tx: &rusqlite::Transaction<'_>) -> Result<(), RepoError> {
    tx.execute(
        "UPDATE media_objects SET variants = 0 WHERE variants <> 0",
        [],
    )?;
    tx.execute(
        "UPDATE posts SET thumbhash = NULL WHERE thumbhash IS NOT NULL",
        [],
    )?;
    Ok(())
}

/// The objects the grid shows, `true` for covers (plan §2.13: covers and
/// posters, image slides 0–3, site heroes).
pub(super) fn grid_objects(conn: &Connection) -> Result<HashMap<i64, bool>, RepoError> {
    let mut out: HashMap<i64, bool> = HashMap::new();
    let mut stmt = conn.prepare(
        "SELECT cover_object, 1 FROM posts WHERE cover_object IS NOT NULL
         UNION ALL
         SELECT object_id, 0 FROM post_media
          WHERE object_id IS NOT NULL AND kind = 'image' AND position <= 3
         UNION ALL
         SELECT hero_object, 0 FROM web_captures WHERE hero_object IS NOT NULL",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let cover: bool = row.get(1)?;
        let entry = out.entry(id).or_insert(false);
        *entry |= cover;
    }
    Ok(out)
}

/// Sets the `archive_state` of the posts `ids` (every post when `None`)
/// from what is stored (the rule of the bundle builder,
/// `shelfy_migrate::bundle`): `done` when nothing is left to store, `client`
/// when the Instagram cover URL has expired (an extension task), `partial`
/// when something is stored, else `pending`. Only the cover and image slides
/// count; videos are fetched on demand.
pub(super) fn archive_states(
    tx: &Connection,
    ids: Option<&[i64]>,
    now: i64,
) -> rusqlite::Result<usize> {
    let scope = ids.map(|ids| serde_json::to_string(ids).expect("ids serialize"));
    tx.execute(
        "UPDATE posts SET archive_state = CASE
           WHEN NOT (cover_object IS NULL AND cover_url IS NOT NULL)
                AND NOT EXISTS (SELECT 1 FROM post_media m
                                WHERE m.post_id = posts.id AND m.kind = 'image'
                                  AND m.object_id IS NULL AND m.source_url IS NOT NULL)
             THEN 'done'
           WHEN cover_object IS NULL AND cover_url IS NOT NULL AND platform = 'instagram'
                AND cover_url_expires_at IS NOT NULL AND cover_url_expires_at <= ?1
             THEN 'client'
           WHEN cover_object IS NOT NULL
                OR EXISTS (SELECT 1 FROM post_media m
                           WHERE m.post_id = posts.id AND m.object_id IS NOT NULL)
             THEN 'partial'
           ELSE 'pending' END
         WHERE ?2 IS NULL OR id IN (SELECT value FROM json_each(?2))",
        params![now, scope],
    )
}

fn count(tx: &Connection, sql: &str) -> rusqlite::Result<u64> {
    let n: i64 = tx.query_row(sql, [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Rows of the library, by table.
pub(super) fn installed_counts(tx: &Connection) -> Result<InstalledCounts, RepoError> {
    let table = |name: &str| count(tx, &format!("SELECT count(*) FROM {name}"));
    let mut posts = std::collections::BTreeMap::new();
    let mut stmt = tx.prepare("SELECT platform, count(*) FROM posts GROUP BY platform")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        posts.insert(
            row.get(0)?,
            u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
        );
    }
    Ok(InstalledCounts {
        posts,
        slides: table("post_media")?,
        collections: table("collections")?,
        memberships: table("post_collections")?,
        post_tags: table("post_tags")?,
        post_entities: table("post_entities")?,
        tag_aliases: table("tag_alias")?,
        tag_clusters: table("tag_cluster")?,
        web_captures: table("web_captures")?,
        web_capture_assets: table("web_capture_assets")?,
        media_objects: table("media_objects")?,
    })
}

/// The archive work of the library's posts, by class.
pub(super) fn archive_counts(tx: &Connection, now: i64) -> Result<ArchiveCounts, RepoError> {
    let mut counts = ArchiveCounts::default();
    let mut stmt =
        tx.prepare("SELECT archive_state, count(*) FROM posts GROUP BY archive_state")?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        counts.by_state.insert(
            row.get(0)?,
            u64::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
        );
    }
    let mut stmt = tx.prepare(
        "SELECT platform,
                CASE WHEN cover_url_expires_at IS NULL THEN 'none'
                     WHEN cover_url_expires_at <= ?1 THEN 'expired' ELSE 'valid' END,
                count(*)
         FROM posts WHERE cover_object IS NULL AND cover_url IS NOT NULL
         GROUP BY 1, 2",
    )?;
    let mut rows = stmt.query([now])?;
    while let Some(row) = rows.next()? {
        let platform: String = row.get(0)?;
        let expiry: String = row.get(1)?;
        let n = u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0);
        let slot = match (platform.as_str(), expiry.as_str()) {
            ("instagram", "valid") => &mut counts.ig_cover_valid,
            ("instagram", "expired") => &mut counts.ig_cover_expired,
            ("instagram", _) => &mut counts.ig_cover_no_expiry,
            ("twitter", _) => &mut counts.x_cover,
            ("pinterest", _) => &mut counts.pinterest_cover,
            _ => &mut counts.other_cover,
        };
        *slot += n;
    }
    counts.image_slides_pending = count(
        tx,
        "SELECT count(*) FROM post_media
         WHERE kind = 'image' AND object_id IS NULL AND source_url IS NOT NULL",
    )?;
    Ok(counts)
}

/// Stores the whole report in the library's `meta` under `key`.
pub(super) fn store_report(
    tx: &Connection,
    key: &str,
    report: &MigrationReport,
) -> Result<(), RepoError> {
    let json = serde_json::to_string(report).expect("the report serializes");
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![key, json],
    )?;
    Ok(())
}

/// The bundle's settings keys that the live library does not have: the
/// desktop settings a replace takes (the web library's own win).
fn desktop_settings_taken(bundle: &Path, live: &Path) -> rusqlite::Result<Vec<String>> {
    let keys = |path: &Path| -> rusqlite::Result<Vec<String>> {
        if !path.is_file() {
            return Ok(Vec::new());
        }
        let conn = Connection::open_with_flags(
            path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.prepare("SELECT key FROM settings ORDER BY key")?
            .query_map([], |r| r.get(0))?
            .collect()
    };
    let live_keys = keys(live)?;
    Ok(keys(bundle)?
        .into_iter()
        .filter(|key| !live_keys.contains(key))
        .collect())
}

/// The notification of an install: its reconciliation (plan §4.3: counts
/// in, counts out, merges, missing files).
fn notification(job_id: i64, report: &MigrationReport) -> NewNotification {
    let bundle = &report.bundle;
    let sum = |value: &serde_json::Value| -> u64 {
        value
            .as_object()
            .map(|m| m.values().filter_map(serde_json::Value::as_u64).sum())
            .unwrap_or(0)
    };
    let desktop_posts = sum(&bundle["posts"]["read"]);
    let bundle_posts = sum(&bundle["posts"]["written"]);
    let (installed, inserted, merged) = match &report.merge {
        Some(m) => {
            let inserted: u64 = m.posts.values().map(|p| p.inserted).sum();
            let merged: u64 = m.posts.values().map(|p| p.merged).sum();
            (inserted + merged, inserted, merged)
        }
        None => {
            let installed = report.installed.posts.values().sum::<u64>();
            (installed, installed, 0)
        }
    };
    let pending = report
        .archive
        .by_state
        .iter()
        .filter(|(state, _)| state.as_str() != "done")
        .map(|(_, n)| n)
        .sum::<u64>();
    let mut params = serde_json::Map::new();
    params.insert("jobId".into(), json!(job_id));
    params.insert(
        "mode".into(),
        json!(match report.mode {
            InstallMode::Replace => "replace",
            InstallMode::Merge => "merge",
        }),
    );
    params.insert("desktopPosts".into(), json!(desktop_posts));
    params.insert("bundlePosts".into(), json!(bundle_posts));
    params.insert("installedPosts".into(), json!(installed));
    params.insert("inserted".into(), json!(inserted));
    params.insert("mergedIntoExisting".into(), json!(merged));
    params.insert(
        "duplicatesMerged".into(),
        json!(bundle["posts"]["merged"].as_u64().unwrap_or(0)),
    );
    params.insert(
        "filesMissing".into(),
        json!(bundle["files"]["missing"].as_u64().unwrap_or(0)),
    );
    params.insert("objects".into(), json!(report.objects.total));
    params.insert("renditions".into(), json!(report.renditions.rendered));
    params.insert("archivePending".into(), json!(pending));
    params.insert(
        // The posts the desktop counted all landed.
        "matches".into(),
        json!(
            bundle_posts == installed
                && desktop_posts == bundle_posts + bundle["posts"]["merged"].as_u64().unwrap_or(0)
        ),
    );
    NewNotification {
        kind: NOTIFICATION_KIND.to_owned(),
        code: NOTIFICATION_CODE.to_owned(),
        params,
        target: None,
    }
}

fn invalid_bundle(err: Invalid) -> ApiError {
    match err {
        Invalid::Bundle(reason) => ApiError::new(ErrorCode::ValidationFailed).with_detail(reason),
        Invalid::Sqlite(_) => ApiError::new(ErrorCode::ValidationFailed)
            .with_detail("the bundle database cannot be read"),
    }
}

fn size_of(path: &Path) -> Option<u64> {
    fs::metadata(path)
        .ok()
        .filter(|m| m.is_file())
        .map(|m| m.len())
}

pub(super) fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
