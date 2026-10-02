//! The install task (see [`super`] for the stages).

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use futures_util::future::join_all;
use rusqlite::{Connection, params};
use serde_json::json;
use shelfy_core::db::{DbError, LIBRARY_FILE_NAME, UserDb, UserDbConfig};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::NewNotification;
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
    ArchiveCounts, Install, InstalledCounts, InstalledObjects, MigrationReport, MigrationStage,
    RenditionCounts,
};
use crate::config::create_private_dir;
use crate::control::uploads::{self, Upload, UploadPurpose};
use crate::error::{ApiError, ErrorCode};
use crate::events::{self, model::ChangeReason};
use crate::ids::now_ms;
use crate::routes::uploads::remove_files;
use crate::state::{AppState, blocking};

/// Objects stored and rendered per write transaction.
const OBJECT_CHUNK: usize = 16;
/// `meta.key` of the stored report in the installed library.
pub const REPORT_META_KEY: &str = "migration.report";
/// `notifications.kind` and `code` of a finished install.
pub const NOTIFICATION_KIND: &str = "migration";
pub const NOTIFICATION_CODE: &str = "migration.installed";

/// Runs the install `id` of `db_upload` to the end and records the outcome
/// in `install`, also when the work panics (a bug): the install then fails
/// with `internal` instead of staying "running".
pub async fn run(state: AppState, install: Arc<Install>, id: String, db_upload: Upload) {
    let started = Instant::now();
    let work = {
        let (state, install, id) = (state.clone(), Arc::clone(&install), id.clone());
        tokio::spawn(
            async move { install_bundle(&state, &install, &id, &db_upload, started).await },
        )
    };
    let outcome = work.await.unwrap_or_else(|err| {
        Err(ApiError::internal(anyhow::anyhow!(
            "the install task failed: {err}"
        )))
    });
    let work = state.config().data_dir.migrations_dir().join(&id);
    let removed = blocking(move || match fs::remove_dir_all(&work) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(ApiError::internal(e)),
    })
    .await;
    if let Err(err) = removed {
        tracing::warn!(migration = %id, error = %err, "cannot remove the install's work directory");
    }
    match &outcome {
        Ok(report) => tracing::info!(
            migration = %id,
            user_id = %install.user_id,
            posts = report.installed.posts.values().sum::<u64>(),
            objects = report.objects.total,
            rendered = report.renditions.rendered,
            duration_ms = report.duration_ms,
            "migration installed"
        ),
        Err(err) => tracing::warn!(
            migration = %id,
            user_id = %install.user_id,
            code = %err.code(),
            "migration failed"
        ),
    }
    install.finish(outcome, now_ms());
}

/// Where an object comes from.
#[derive(Clone, Debug)]
enum Source {
    /// Already in the user's store, at this path.
    Stored(PathBuf),
    /// A complete upload, at this path.
    Upload(PathBuf),
}

/// An object to store, with what it is used for.
#[derive(Clone, Debug)]
struct Item {
    object: BundleObject,
    source: Source,
    /// Shown in the grid: needs `g480`.
    grid: bool,
    /// A cover: also needs its ThumbHash.
    cover: bool,
}

/// An object ready to commit.
struct Prepared {
    item: Item,
    staged: Option<StagedObject>,
    /// Render from this file.
    render: Option<PathBuf>,
}

async fn install_bundle(
    state: &AppState,
    install: &Install,
    id: &str,
    db_upload: &Upload,
    started: Instant,
) -> Result<MigrationReport, ApiError> {
    let data = state.config().data_dir.clone();
    let user_id = install.user_id.clone();

    // 1. Validate a private copy of the uploaded database.
    install.set_stage(MigrationStage::Validating, 0.0);
    let work = data.migrations_dir().join(id);
    let work_db = work.join(LIBRARY_FILE_NAME);
    let facts = {
        let source = uploads::file_path(&data.uploads_dir(), &db_upload.id, true);
        let (work, work_db) = (work.clone(), work_db.clone());
        blocking(move || -> Result<BundleFacts, ApiError> {
            create_private_dir(&work).map_err(ApiError::internal)?;
            fs::copy(&source, &work_db).map_err(ApiError::internal)?;
            validate::validate(&work_db, &BundleLimits::default()).map_err(invalid_bundle)
        })
        .await?
    };
    ensure_empty(state, &user_id).await?;
    let media = MediaStore::new(data.users_dir())
        .user(&user_id)
        .map_err(ApiError::internal)?;
    let sources = locate(state, &media, &user_id, &facts.objects).await?;
    install.set_stage(MigrationStage::Validating, 1.0);

    // 2. Store the objects and render what the grid shows.
    let db = {
        let path = work_db.clone();
        let config = UserDbConfig {
            max_readers: 1,
            ..UserDbConfig::default()
        };
        blocking(move || UserDb::open(&path, &config).map(Arc::new)).await?
    };
    let grid = {
        let db = Arc::clone(&db);
        blocking(move || db.write(|tx| normalize(tx).and_then(|()| grid_objects(tx)))).await?
    };
    let items: Vec<Item> = facts
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
    let mut objects = InstalledObjects::default();
    let mut renditions = RenditionCounts {
        wanted: items.iter().filter(|i| i.grid).count() as u64,
        ..RenditionCounts::default()
    };
    install.set_stage(MigrationStage::Objects, 0.0);
    let total = items.len().max(1);
    let mut done = 0usize;
    for chunk in items.chunks(OBJECT_CHUNK) {
        if state.shutdown_token().is_cancelled() {
            return Err(ApiError::new(ErrorCode::Unavailable)
                .with_detail("the server is stopping: retry the install"));
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
        for (p, r) in prepared.iter().zip(&rendered) {
            objects.total += 1;
            objects.bytes += p.item.object.bytes;
            match p.item.source {
                Source::Upload(_) => objects.from_uploads += 1,
                Source::Stored(_) => objects.already_stored += 1,
            }
            if p.item.grid {
                match r {
                    Some(Ok(_)) => renditions.rendered += 1,
                    Some(Err(err)) => {
                        renditions.failed += 1;
                        tracing::warn!(error = %err, "cannot render a migrated image");
                    }
                    None if !p.item.object.kind.is_renderable() => renditions.not_renderable += 1,
                    None => renditions.existing += 1,
                }
            }
        }
        let thumbhashes = {
            let (db, media) = (Arc::clone(&db), media.clone());
            let now = now_ms();
            blocking(move || db.write(|tx| commit(tx, &media, prepared, rendered, now))).await?
        };
        renditions.thumbhashes += thumbhashes;
        done += chunk.len();
        install.set_stage(MigrationStage::Objects, done as f64 / total as f64);
    }

    // 3. Derived data: the search index and the archive state.
    install.set_stage(MigrationStage::Index, 0.0);
    {
        let db = Arc::clone(&db);
        let now = now_ms();
        blocking(move || {
            db.write(|tx| {
                index::rebuild(tx).map_err(RepoError::from)?;
                archive_states(tx, now).map_err(RepoError::from)
            })
        })
        .await?;
    }

    // 4. The report, stored in the new library.
    install.set_stage(MigrationStage::Report, 0.0);
    let report = {
        let db = Arc::clone(&db);
        let now = now_ms();
        let bundle = facts.summary.clone();
        let duration_ms = elapsed_ms(started);
        blocking(move || {
            db.write(|tx| -> Result<MigrationReport, RepoError> {
                let report = MigrationReport {
                    bundle,
                    installed: installed_counts(tx)?,
                    objects,
                    renditions,
                    archive: archive_counts(tx, now)?,
                    duration_ms,
                };
                store_report(tx, &report)?;
                Ok(report)
            })
        })
        .await?
    };

    // 5. Replace the live library.
    install.set_stage(MigrationStage::Installing, 0.0);
    {
        let db = Arc::try_unwrap(db)
            .map_err(|_| ApiError::internal(anyhow::anyhow!("the new library is still in use")))?;
        let live = data.library_db(&user_id);
        let previous = live.with_file_name(format!("library.prev-{id}.sqlite"));
        blocking(move || -> Result<(), ApiError> {
            db.checkpoint()?;
            drop(db);
            swap::replace_library(&work_db, &live, &previous).map_err(|err| match err {
                SwapError::NotEmpty => not_empty(),
                SwapError::Busy => ApiError::new(ErrorCode::Unavailable)
                    .with_retry_after(5)
                    .with_detail("the web library stayed locked: retry the install"),
                other => ApiError::internal(other),
            })
        })
        .await?;
    }
    state.user_dbs().evict(&user_id);

    // Open tabs refresh, and the notification joins the user's activity.
    state
        .events()
        .posts_changed(&user_id, ChangeReason::Import, None);
    state.events().stats_changed(&user_id);
    if let Err(err) = events::notify(state, &user_id, notification(&report)).await {
        tracing::warn!(migration = %id, error = %err, "cannot store the migration notification");
    }

    // The uploads are consumed.
    let control = Arc::clone(state.control());
    let uploads_dir = data.uploads_dir();
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
    Ok(MigrationReport {
        duration_ms: elapsed_ms(started),
        ..report
    })
}

/// 409 for a library that is not empty: v0 only replaces an empty one.
pub(crate) fn not_empty() -> ApiError {
    ApiError::new(ErrorCode::Conflict)
        .with_detail("the web library is not empty: merging into it (--merge) arrives with P1-19")
}

/// Refuses unless the user's live library is empty.
pub(crate) async fn ensure_empty(state: &AppState, user_id: &str) -> Result<(), ApiError> {
    let live = state.user_db(user_id).await?;
    let empty = blocking(move || {
        live.read(|c| swap::is_empty(c).map_err(|e| ApiError::from(DbError::from(e))))
    })
    .await?;
    if empty { Ok(()) } else { Err(not_empty()) }
}

/// Finds every object: in the user's store with the right size, or in a
/// complete upload of the same hash.
async fn locate(
    state: &AppState,
    media: &UserMedia,
    user_id: &str,
    objects: &[BundleObject],
) -> Result<Vec<Source>, ApiError> {
    let control = Arc::clone(state.control());
    let uploads_dir = state.config().data_dir.uploads_dir();
    let (media, user_id, objects) = (media.clone(), user_id.to_owned(), objects.to_vec());
    blocking(move || -> Result<Vec<Source>, ApiError> {
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
    .await
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

/// Publishes the prepared objects and their renditions and records them, in
/// the new library's write transaction (the store's protocol, rule 1).
/// Returns how many posts got a ThumbHash.
fn commit(
    tx: &Connection,
    media: &UserMedia,
    prepared: Vec<Prepared>,
    rendered: Vec<Option<Result<Rendered, RenderError>>>,
    now: i64,
) -> Result<u64, RepoError> {
    let mut thumbhashes = 0u64;
    for (p, render) in prepared.into_iter().zip(rendered) {
        let object = &p.item.object;
        let rendered = render.and_then(Result::ok);
        let meta = ObjectMeta {
            width: rendered.as_ref().map(|r| r.source_width),
            height: rendered.as_ref().map(|r| r.source_height),
            ..ObjectMeta::new(object.role, Origin::Migration)
        };
        let id = match p.staged {
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
        if id != object.id {
            return Err(RepoError::Conflict("media object"));
        }
        if p.item.cover
            && let Some(r) = &rendered
        {
            thumbhashes += refs::set_cover_thumbhash(tx, id, &r.thumbhash, now)? as u64;
        }
    }
    Ok(thumbhashes)
}

/// Clears what the server derives itself: renditions and ThumbHashes are
/// made again by the install.
fn normalize(tx: &Connection) -> Result<(), RepoError> {
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
fn grid_objects(tx: &Connection) -> Result<HashMap<i64, bool>, RepoError> {
    let mut out: HashMap<i64, bool> = HashMap::new();
    let mut stmt = tx.prepare(
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

/// Sets every post's `archive_state` from what is stored (the rule of the
/// bundle builder, `shelfy_migrate::bundle`): `done` when nothing is left to
/// store, `client` when the Instagram cover URL has expired (an extension
/// task), `partial` when something is stored, else `pending`. Only the cover
/// and image slides count; videos are fetched on demand.
fn archive_states(tx: &Connection, now: i64) -> rusqlite::Result<usize> {
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
           ELSE 'pending' END",
        [now],
    )
}

fn count(tx: &Connection, sql: &str) -> rusqlite::Result<u64> {
    let n: i64 = tx.query_row(sql, [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

fn installed_counts(tx: &Connection) -> Result<InstalledCounts, RepoError> {
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

fn archive_counts(tx: &Connection, now: i64) -> Result<ArchiveCounts, RepoError> {
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

/// Stores the whole report in the new library's `meta`.
fn store_report(tx: &Connection, report: &MigrationReport) -> Result<(), RepoError> {
    let json = serde_json::to_string(report).expect("the report serializes");
    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)",
        params![REPORT_META_KEY, json],
    )?;
    Ok(())
}

/// The notification of an install: its headline counts.
fn notification(report: &MigrationReport) -> NewNotification {
    let pending = report
        .archive
        .by_state
        .iter()
        .filter(|(state, _)| state.as_str() != "done")
        .map(|(_, n)| n)
        .sum::<u64>();
    let mut params = serde_json::Map::new();
    params.insert(
        "posts".into(),
        json!(report.installed.posts.values().sum::<u64>()),
    );
    params.insert("objects".into(), json!(report.objects.total));
    params.insert("renditions".into(), json!(report.renditions.rendered));
    params.insert("archivePending".into(), json!(pending));
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

fn elapsed_ms(since: Instant) -> u64 {
    u64::try_from(since.elapsed().as_millis()).unwrap_or(u64::MAX)
}
