//! Merging a bundle into a library that is not empty (`shelfy-migrate run
//! --merge`; plan §4.1 step 5, §4.2; the merge rules of P1-10).
//!
//! The live library keeps working while it is merged into, so the merge is
//! made of short write transactions on it, each taking the handle from the
//! cache ([`JobContext::user_db`]):
//!
//! 1. **The previous library** is kept first as `library.prev-<job id>.sqlite`
//!    (a retry keeps the first copy, taken before anything was merged).
//! 2. **Objects**, 16 per transaction: each is stored and recorded in the live
//!    library (an object it has already is reused), with its `g480` and, for
//!    covers, its ThumbHash. The bundle's object ids are mapped to the live
//!    library's.
//! 3. **Collections**, one transaction: a bundle collection is the live one
//!    of the same platform folder (`platform`, `external_id`), else of the
//!    same name among the folders without one, else a new collection.
//! 4. **Posts**, 200 per transaction. A key the library lacks is inserted
//!    ([`posts::insert`], with the bundle's tag and entity rows, tiers
//!    included); a key it has merges with the duplicate policy
//!    ([`duplicates::merge_duplicate`]: the row with archived files wins, then
//!    the one with an analysis, then the one with a user layer; folders
//!    unite; notes join). Site versions are copied unless the post has one
//!    of the same capture time; the current one moves to the bundle's when
//!    its row won (or the post had none). The archive state of every touched
//!    post is derived again.
//! 5. **The rest**, one transaction: tag aliases and clusters (the library's
//!    win), settings (the library's win), the manual posts' legacy ids, the
//!    covers' ThumbHashes, the unreferenced-object stamps, and the report.
//!
//! Merging the same bundle again changes nothing: an inserted key now
//! merges with itself, and [`duplicates::merge_duplicate`] of the same row
//! adds nothing twice. So a try that stopped half way (a restart, a lost
//! lease) is finished by the next one.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::Value;
use shelfy_core::ingest::archive::{self, ArchivePolicy, Scope};
use shelfy_core::ingest::duplicates::{self, Duplicate};
use shelfy_core::repo::posts::{self, AiLayer, NewMedia, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_core::search::index;
use shelfy_media::refs;

use super::install::{
    self, Checked, at, commit_object, count_chunk, elapsed_ms, open_read_only, prepare_chunk,
    previous_path, refuse_locked, report_key, stage,
};
use super::swap;
use super::{
    InstallMode, InstalledObjects, MergeCounts, MigrationReport, MigrationStage, RenditionCounts,
    SizeStats,
};
use crate::error::ApiError;
use crate::ids::now_ms;
use crate::jobs::{JobContext, JobError};
use crate::state::blocking;

/// Posts merged per write transaction.
const POST_CHUNK: usize = 200;

/// Bundle id → live id, for objects, collections or captures.
type IdMap = HashMap<i64, i64>;

/// An object of the bundle, recorded in the live library.
struct Recorded {
    bundle_id: i64,
    live_id: i64,
    /// Its ThumbHash, for a cover rendered now.
    thumbhash: Option<Vec<u8>>,
}

/// A tag cluster of the bundle.
struct ClusterRow {
    id: i64,
    label: String,
    label_norm: Option<String>,
    status: String,
    run_id: Option<i64>,
}

/// Merges the validated bundle of `checked` into the user's live library.
///
/// # Errors
///
/// Why the try failed.
pub(super) async fn merge(
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

    // 1. The library as it was, kept for 7 days.
    refuse_locked(ctx)?;
    let live = state.config().data_dir.library_db(&user_id);
    let previous = previous_path(&live, ctx.id());
    {
        let (live, previous) = (live.clone(), previous.clone());
        blocking(move || -> Result<(), ApiError> {
            let conn = Connection::open_with_flags(
                &live,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
            )
            .map_err(ApiError::internal)?;
            conn.busy_timeout(std::time::Duration::from_secs(5))
                .map_err(ApiError::internal)?;
            swap::keep_previous(&conn, &previous).map_err(ApiError::internal)
        })
        .await?;
    }

    // 2. The objects, recorded in the live library.
    let mut objects = InstalledObjects::default();
    let mut renditions = RenditionCounts {
        wanted: items.iter().filter(|i| i.grid).count() as u64,
        ..RenditionCounts::default()
    };
    let mut cover_sizes = Vec::new();
    let mut object_ids: IdMap = HashMap::with_capacity(items.len());
    let mut thumbhashes: Vec<(i64, Vec<u8>)> = Vec::new();
    stage(ctx, MigrationStage::Objects, at::OBJECTS).await?;
    let total = items.len().max(1);
    let mut done = 0usize;
    for chunk in items.chunks(install::OBJECT_CHUNK) {
        let (prepared, rendered) = prepare_chunk(ctx, &media, chunk).await?;
        count_chunk(
            &prepared,
            &rendered,
            &mut objects,
            &mut renditions,
            &mut cover_sizes,
        );
        let media = media.clone();
        let recorded = ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<Vec<Recorded>, RepoError> {
                    let now = now_ms();
                    let mut out = Vec::with_capacity(prepared.len());
                    for (p, render) in prepared.into_iter().zip(rendered) {
                        let (bundle_id, cover) = (p.item.object.id, p.item.cover);
                        let (live_id, rendered) = commit_object(tx, &media, p, render, now)?;
                        let thumbhash = rendered.filter(|_| cover).map(|r| r.thumbhash);
                        out.push(Recorded {
                            bundle_id,
                            live_id,
                            thumbhash,
                        });
                    }
                    Ok(out)
                })
                .map_err(JobError::from)
            })
            .await?;
        for Recorded {
            bundle_id,
            live_id,
            thumbhash,
        } in recorded
        {
            object_ids.insert(bundle_id, live_id);
            if let Some(thumbhash) = thumbhash {
                thumbhashes.push((live_id, thumbhash));
            }
        }
        done += chunk.len();
        let progress = at::OBJECTS + (at::OBJECTS_END - at::OBJECTS) * done as f64 / total as f64;
        stage(ctx, MigrationStage::Objects, progress).await?;
    }
    renditions.cover_bytes = SizeStats::of(cover_sizes);
    let object_ids = Arc::new(object_ids);

    // 3. The collections.
    stage(ctx, MigrationStage::Merging, at::OBJECTS_END).await?;
    let mut counts = MergeCounts::default();
    let bundle_collections = {
        let work_db = work_db.clone();
        blocking(move || read_collections(&work_db)).await?
    };
    let (collection_ids, inserted, matched) = ctx
        .user_db(move |db| {
            db.write(|tx| map_collections(tx, &bundle_collections, now_ms()))
                .map_err(JobError::from)
        })
        .await?;
    counts.collections_inserted = inserted;
    counts.collections_matched = matched;
    let collection_ids = Arc::new(collection_ids);

    // 4. The posts.
    let policy = install::archive_policy(ctx, &work_db).await?;
    let post_ids: Vec<i64> = {
        let work_db = work_db.clone();
        blocking(move || -> Result<Vec<i64>, ApiError> {
            let conn = open_read_only(&work_db)?;
            let ids = conn
                .prepare("SELECT id FROM posts ORDER BY id")
                .and_then(|mut stmt| stmt.query_map([], |r| r.get(0))?.collect())
                .map_err(ApiError::internal)?;
            Ok(ids)
        })
        .await?
    };
    let total = post_ids.len().max(1);
    let mut done = 0usize;
    for chunk in post_ids.chunks(POST_CHUNK) {
        if ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        let bundle_posts = {
            let (work_db, chunk) = (work_db.clone(), chunk.to_vec());
            let (object_ids, collection_ids) =
                (Arc::clone(&object_ids), Arc::clone(&collection_ids));
            blocking(move || read_posts(&work_db, &chunk, &object_ids, &collection_ids)).await?
        };
        let chunk_counts = ctx
            .user_db(move |db| {
                db.write(|tx| merge_posts(tx, bundle_posts, &policy, now_ms()))
                    .map_err(JobError::from)
            })
            .await?;
        counts.add(&chunk_counts);
        done += chunk.len();
        let progress =
            at::OBJECTS_END + (at::MERGING_END - at::OBJECTS_END) * done as f64 / total as f64;
        stage(ctx, MigrationStage::Merging, progress).await?;
    }

    // 5. The rest, and the report.
    stage(ctx, MigrationStage::Report, at::MERGING_END).await?;
    let rest = {
        let work_db = work_db.clone();
        blocking(move || read_rest(&work_db)).await?
    };
    let key = report_key(ctx.id());
    let report = MigrationReport {
        mode: InstallMode::Merge,
        bundle: facts.summary.clone(),
        objects,
        renditions,
        previous: previous
            .file_name()
            .map(|n| n.to_string_lossy().into_owned()),
        duration_ms: elapsed_ms(started),
        ..MigrationReport::default()
    };
    let report = ctx
        .user_db(move |db| {
            db.write(|tx| -> Result<MigrationReport, RepoError> {
                let now = now_ms();
                let mut counts = counts;
                let settings = merge_rest(tx, &rest, &mut counts, now)?;
                let mut renditions = report.renditions.clone();
                for (object, thumbhash) in &thumbhashes {
                    renditions.thumbhashes +=
                        refs::set_cover_thumbhash(tx, *object, thumbhash, now)? as u64;
                }
                refs::restamp(tx, now)?;
                let report = MigrationReport {
                    installed: install::installed_counts(tx)?,
                    archive: install::archive_counts(tx, now)?,
                    merge: Some(counts),
                    renditions,
                    settings,
                    ..report
                };
                install::store_report(tx, &key, &report)?;
                Ok(report)
            })
            .map_err(JobError::from)
        })
        .await?;
    Ok(report)
}

impl MergeCounts {
    fn add(&mut self, other: &MergeCounts) {
        for (platform, posts) in &other.posts {
            let slot = self.posts.entry(platform.clone()).or_default();
            slot.inserted += posts.inserted;
            slot.merged += posts.merged;
        }
        self.replaced += other.replaced;
        self.unchanged += other.unchanged;
        self.ai_filled += other.ai_filled;
        self.dates_filled += other.dates_filled;
        self.notes_joined += other.notes_joined;
        self.tags_added += other.tags_added;
        self.memberships_added += other.memberships_added;
        self.memberships_present += other.memberships_present;
        self.captures_added += other.captures_added;
        self.captures_present += other.captures_present;
    }
}

// ── Reading the bundle ───────────────────────────────────────────────────────

/// A collection of the bundle.
#[derive(Clone, Debug)]
struct BundleCollection {
    id: i64,
    name: String,
    color: String,
    platform: Option<String>,
    external_id: Option<String>,
    source_name: Option<String>,
    created_at: i64,
}

fn read_collections(path: &Path) -> Result<Vec<BundleCollection>, ApiError> {
    let conn = open_read_only(path)?;
    let rows = conn
        .prepare(
            "SELECT id, name, color, platform, external_id, source_name, created_at
             FROM collections ORDER BY id",
        )
        .and_then(|mut stmt| {
            stmt.query_map([], |r| {
                Ok(BundleCollection {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    color: r.get(2)?,
                    platform: r.get(3)?,
                    external_id: r.get(4)?,
                    source_name: r.get(5)?,
                    created_at: r.get(6)?,
                })
            })?
            .collect()
        })
        .map_err(ApiError::internal)?;
    Ok(rows)
}

/// A site version of the bundle, its object ids already the live library's.
#[derive(Clone, Debug)]
struct BundleCapture {
    id: i64,
    captured_at: i64,
    requested_url: Option<String>,
    final_url: Option<String>,
    status: String,
    partial: i64,
    engine: Option<String>,
    viewport: Option<String>,
    title: Option<String>,
    palette_json: Option<String>,
    fonts_json: Option<String>,
    tech_json: Option<String>,
    awards_json: Option<String>,
    meta_json: Option<String>,
    pages_json: Option<String>,
    traits_json: Option<String>,
    hero_object: Option<i64>,
    favicon_object: Option<i64>,
    ai_snapshot_json: Option<String>,
    created_at: i64,
    assets: Vec<CaptureAsset>,
}

#[derive(Clone, Debug)]
struct CaptureAsset {
    page_index: i64,
    role: String,
    seq: i64,
    object_id: i64,
    css_top: Option<i64>,
    css_height: Option<i64>,
}

/// A tag row of the bundle.
#[derive(Clone, Debug)]
struct TagRow {
    norm: String,
    form: String,
    source: String,
    tier: Option<String>,
}

/// A post of the bundle, ready to merge: object and collection ids are the
/// live library's.
#[derive(Clone, Debug)]
struct BundlePost {
    post: NewPost,
    /// Its folders, with when the post joined each.
    collections: Vec<(i64, i64)>,
    captures: Vec<BundleCapture>,
    /// The bundle id of its current site version.
    current_capture: Option<i64>,
    tags: Vec<TagRow>,
    entities: Vec<(String, String)>,
}

fn json_strings(raw: Option<String>) -> Vec<String> {
    raw.and_then(|raw| serde_json::from_str::<Vec<Value>>(&raw).ok())
        .map(|items| {
            items
                .into_iter()
                .filter_map(|v| match v {
                    Value::String(s) => Some(s),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The posts `ids` of the bundle at `path`, with their slides, layers,
/// folders and site versions.
fn read_posts(
    path: &Path,
    ids: &[i64],
    objects: &IdMap,
    collections: &IdMap,
) -> Result<Vec<BundlePost>, ApiError> {
    let conn = open_read_only(path)?;
    read_posts_from(&conn, ids, objects, collections).map_err(|err| match err {
        RepoError::Db(_) => ApiError::internal(err),
        other => ApiError::from(other),
    })
}

fn read_posts_from(
    conn: &Connection,
    ids: &[i64],
    objects: &IdMap,
    collections: &IdMap,
) -> Result<Vec<BundlePost>, RepoError> {
    let scope = serde_json::to_string(ids).expect("ids serialize");
    let object = |id: Option<i64>| -> Result<Option<i64>, RepoError> {
        id.map(|id| {
            objects
                .get(&id)
                .copied()
                .ok_or(RepoError::Conflict("a bundle object that was not stored"))
        })
        .transpose()
    };

    let mut posts: BTreeMap<i64, BundlePost> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT id, key, platform, native_id, shortcode, post_url, profile_url, author_username,
                author_name, caption, media_type, posted_at, imported_at, cover_object, cover_url,
                cover_url_expires_at, archive_state, ai_status, ai_provider, ai_model,
                ai_schema_version, ai_description, ai_save_reason, ai_language, ai_category,
                ai_content_type, ai_tags_json, ai_entities_json, ai_keywords_json, ai_web_json,
                ai_analyzed_at, user_note, user_tags_json, web_url, web_domain, web_final_url,
                current_capture_id
         FROM posts WHERE id IN (SELECT value FROM json_each(?1)) ORDER BY id",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        let id: i64 = r.get(0)?;
        let platform: Platform = r.get(2)?;
        let mut post = NewPost::new(
            r.get::<_, String>(1)?,
            platform,
            r.get::<_, String>(3)?,
            r.get::<_, String>(10)?,
            r.get(12)?,
        );
        post.shortcode = r.get(4)?;
        post.post_url = r.get(5)?;
        post.profile_url = r.get(6)?;
        post.author_username = r.get(7)?;
        post.author_name = r.get(8)?;
        post.caption = r.get(9)?;
        post.posted_at = r.get(11)?;
        post.cover_object = object(r.get(13)?)?;
        post.cover_url = r.get(14)?;
        post.cover_url_expires_at = r.get(15)?;
        post.archive_state = r.get(16)?;
        let ai = AiLayer {
            status: r.get(17)?,
            provider: r.get(18)?,
            model: r.get(19)?,
            schema_version: r.get(20)?,
            description: r.get(21)?,
            save_reason: r.get(22)?,
            language: r.get(23)?,
            category: r.get(24)?,
            content_type: r.get(25)?,
            tags: json_strings(r.get(26)?),
            general_tags: None,
            specific_tags: None,
            entities: json_strings(r.get(27)?),
            keywords: json_strings(r.get(28)?),
            web: r
                .get::<_, Option<String>>(29)?
                .and_then(|raw| serde_json::from_str(&raw).ok()),
            analyzed_at: r.get(30)?,
        };
        post.ai = (ai != AiLayer::default()).then_some(ai);
        post.user_note = r.get(31)?;
        post.user_tags = json_strings(r.get(32)?);
        post.web_url = r.get(33)?;
        post.web_domain = r.get(34)?;
        post.web_final_url = r.get(35)?;
        posts.insert(
            id,
            BundlePost {
                post,
                collections: Vec::new(),
                captures: Vec::new(),
                current_capture: r.get(36)?,
                tags: Vec::new(),
                entities: Vec::new(),
            },
        );
    }
    drop(rows);
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT post_id, kind, source_url, source_url_expires_at, video_url, video_url_expires_at,
                width, height, duration_ms, label, object_id, video_object_id
         FROM post_media WHERE post_id IN (SELECT value FROM json_each(?1))
         ORDER BY post_id, position",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        let Some(post) = posts.get_mut(&r.get::<_, i64>(0)?) else {
            continue;
        };
        post.post.media.push(NewMedia {
            kind: r.get(1)?,
            source_url: r.get(2)?,
            source_url_expires_at: r.get(3)?,
            video_url: r.get(4)?,
            video_url_expires_at: r.get(5)?,
            width: r.get(6)?,
            height: r.get(7)?,
            duration_ms: r.get(8)?,
            label: r.get(9)?,
            object_id: object(r.get(10)?)?,
            video_object_id: object(r.get(11)?)?,
        });
    }
    drop(rows);
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT post_id, tag_norm, tag_form, source, tier FROM post_tags
         WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY post_id, tag_norm, source",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        if let Some(post) = posts.get_mut(&r.get::<_, i64>(0)?) {
            post.tags.push(TagRow {
                norm: r.get(1)?,
                form: r.get(2)?,
                source: r.get(3)?,
                tier: r.get(4)?,
            });
        }
    }
    drop(rows);
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT post_id, ent_norm, ent_form FROM post_entities
         WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY post_id, ent_norm",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        if let Some(post) = posts.get_mut(&r.get::<_, i64>(0)?) {
            post.entities.push((r.get(1)?, r.get(2)?));
        }
    }
    drop(rows);
    drop(stmt);

    let mut stmt = conn.prepare(
        "SELECT post_id, collection_id, added_at FROM post_collections
         WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY post_id, collection_id",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        let collection =
            collections
                .get(&r.get::<_, i64>(1)?)
                .copied()
                .ok_or(RepoError::Conflict(
                    "a bundle collection that was not merged",
                ))?;
        if let Some(post) = posts.get_mut(&r.get::<_, i64>(0)?) {
            post.collections.push((collection, r.get(2)?));
        }
    }
    drop(rows);
    drop(stmt);

    let mut captures: BTreeMap<i64, (i64, BundleCapture)> = BTreeMap::new();
    let mut stmt = conn.prepare(
        "SELECT id, post_id, captured_at, requested_url, final_url, status, partial, engine,
                viewport, title, palette_json, fonts_json, tech_json, awards_json, meta_json,
                pages_json, traits_json, hero_object, favicon_object, ai_snapshot_json, created_at
         FROM web_captures WHERE post_id IN (SELECT value FROM json_each(?1)) ORDER BY id",
    )?;
    let mut rows = stmt.query([&scope])?;
    while let Some(r) = rows.next()? {
        let capture = BundleCapture {
            id: r.get(0)?,
            captured_at: r.get(2)?,
            requested_url: r.get(3)?,
            final_url: r.get(4)?,
            status: r.get(5)?,
            partial: r.get(6)?,
            engine: r.get(7)?,
            viewport: r.get(8)?,
            title: r.get(9)?,
            palette_json: r.get(10)?,
            fonts_json: r.get(11)?,
            tech_json: r.get(12)?,
            awards_json: r.get(13)?,
            meta_json: r.get(14)?,
            pages_json: r.get(15)?,
            traits_json: r.get(16)?,
            hero_object: object(r.get(17)?)?,
            favicon_object: object(r.get(18)?)?,
            ai_snapshot_json: r.get(19)?,
            created_at: r.get(20)?,
            assets: Vec::new(),
        };
        captures.insert(capture.id, (r.get(1)?, capture));
    }
    drop(rows);
    drop(stmt);
    if !captures.is_empty() {
        let capture_ids: Vec<i64> = captures.keys().copied().collect();
        let scope = serde_json::to_string(&capture_ids).expect("ids serialize");
        let mut stmt = conn.prepare(
            "SELECT capture_id, page_index, role, seq, object_id, css_top, css_height
             FROM web_capture_assets WHERE capture_id IN (SELECT value FROM json_each(?1))
             ORDER BY capture_id, page_index, role, seq",
        )?;
        let mut rows = stmt.query([&scope])?;
        while let Some(r) = rows.next()? {
            if let Some((_, capture)) = captures.get_mut(&r.get::<_, i64>(0)?) {
                capture.assets.push(CaptureAsset {
                    page_index: r.get(1)?,
                    role: r.get(2)?,
                    seq: r.get(3)?,
                    object_id: object(Some(r.get(4)?))?.unwrap_or_default(),
                    css_top: r.get(5)?,
                    css_height: r.get(6)?,
                });
            }
        }
    }
    for (_, (post_id, capture)) in captures {
        if let Some(post) = posts.get_mut(&post_id) {
            post.captures.push(capture);
        }
    }

    // The AI tags' tiers, so a merge that takes the bundle's analysis keeps
    // them.
    for post in posts.values_mut() {
        let tier = |name: &str| -> Vec<String> {
            post.tags
                .iter()
                .filter(|t| t.source == "ai" && t.tier.as_deref() == Some(name))
                .map(|t| t.form.clone())
                .collect()
        };
        let (general, specific) = (tier("general"), tier("specific"));
        if let Some(ai) = post.post.ai.as_mut()
            && !(general.is_empty() && specific.is_empty())
        {
            ai.general_tags = Some(general);
            ai.specific_tags = Some(specific);
        }
    }
    Ok(posts.into_values().collect())
}

/// What the last transaction takes from the bundle.
struct Rest {
    aliases: Vec<(String, String, String, String)>,
    clusters: Vec<ClusterRow>,
    cluster_memberships: Vec<(String, i64)>,
    settings: Vec<(String, String)>,
    legacy_ids: Vec<(String, String)>,
}

fn read_rest(path: &Path) -> Result<Rest, ApiError> {
    let conn = open_read_only(path)?;
    let read = || -> rusqlite::Result<Rest> {
        Ok(Rest {
            aliases: conn
                .prepare(
                    "SELECT alias_norm, canonical_norm, canonical_form, status FROM tag_alias
                     ORDER BY alias_norm",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?,
            clusters: conn
                .prepare(
                    "SELECT id, label, label_norm, status, run_id FROM tag_cluster ORDER BY id",
                )?
                .query_map([], |r| {
                    Ok(ClusterRow {
                        id: r.get(0)?,
                        label: r.get(1)?,
                        label_norm: r.get(2)?,
                        status: r.get(3)?,
                        run_id: r.get(4)?,
                    })
                })?
                .collect::<rusqlite::Result<_>>()?,
            cluster_memberships: conn
                .prepare(
                    "SELECT tag_norm, cluster_id FROM tag_cluster_membership ORDER BY tag_norm",
                )?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?,
            settings: conn
                .prepare("SELECT key, value_json FROM settings ORDER BY key")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?,
            legacy_ids: conn
                .prepare("SELECT key, value FROM meta WHERE key LIKE 'legacy_id:%' ORDER BY key")?
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<_>>()?,
        })
    };
    read().map_err(ApiError::internal)
}

// ── Writing the live library ─────────────────────────────────────────────────

/// Maps each bundle collection to a live one, creating what is missing.
/// Returns the map and how many were inserted and matched.
fn map_collections(
    tx: &Connection,
    bundle: &[BundleCollection],
    now: i64,
) -> Result<(IdMap, u64, u64), RepoError> {
    let mut ids = IdMap::with_capacity(bundle.len());
    let (mut inserted, mut matched) = (0, 0);
    for c in bundle {
        let found: Option<i64> = match (&c.platform, &c.external_id) {
            (Some(platform), Some(external)) => tx
                .query_row(
                    "SELECT id FROM collections WHERE platform = ?1 AND external_id = ?2",
                    params![platform, external],
                    |r| r.get(0),
                )
                .optional()?,
            _ => tx
                .query_row(
                    "SELECT id FROM collections WHERE external_id IS NULL
                       AND lower(trim(name)) = lower(trim(?1))
                     ORDER BY id LIMIT 1",
                    [&c.name],
                    |r| r.get(0),
                )
                .optional()?,
        };
        let id = match found {
            Some(id) => {
                matched += 1;
                id
            }
            None => {
                tx.execute(
                    "INSERT INTO collections (name, color, platform, external_id, source_name,
                                              created_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        c.name,
                        c.color,
                        c.platform,
                        c.external_id,
                        c.source_name,
                        c.created_at.min(now)
                    ],
                )?;
                inserted += 1;
                tx.last_insert_rowid()
            }
        };
        ids.insert(c.id, id);
    }
    Ok((ids, inserted, matched))
}

/// Merges one chunk of posts into the live library, and derives the archive
/// state of every post it touched under `policy`.
fn merge_posts(
    tx: &Connection,
    posts: Vec<BundlePost>,
    policy: &ArchivePolicy,
    now: i64,
) -> Result<MergeCounts, RepoError> {
    let mut counts = MergeCounts::default();
    let mut touched = Vec::with_capacity(posts.len());
    for bundle in posts {
        let platform = bundle.post.platform.as_str().to_owned();
        let collection_ids: Vec<i64> = bundle.collections.iter().map(|(id, _)| *id).collect();
        match posts::id_for_key(tx, &bundle.post.key)? {
            Some(id) => {
                let before: u64 = count_memberships(tx, id, &collection_ids)?;
                let duplicate = Duplicate {
                    post: bundle.post.clone(),
                    collections: collection_ids.clone(),
                    captures: bundle.captures.len() as u64,
                };
                let done = duplicates::merge_duplicate(tx, id, &duplicate, now)?;
                counts.posts.entry(platform).or_default().merged += 1;
                counts.replaced += u64::from(done.replaced);
                counts.ai_filled += u64::from(done.ai_filled);
                counts.dates_filled += u64::from(done.date_filled);
                counts.notes_joined += u64::from(done.notes_joined);
                counts.tags_added += done.tags_added as u64;
                counts.memberships_added += done.collections_added as u64;
                counts.memberships_present += before;
                let (added, present) = copy_captures(
                    tx,
                    id,
                    &bundle.captures,
                    bundle.current_capture,
                    if done.replaced {
                        Current::Replace
                    } else {
                        Current::IfNone
                    },
                )?;
                counts.captures_added += added;
                counts.captures_present += present;
                if !done.changed() && added == 0 {
                    counts.unchanged += 1;
                }
                touched.push(id);
            }
            None => {
                let id = posts::insert(tx, &bundle.post, now)?;
                replace_tag_rows(tx, id, &bundle.tags, &bundle.entities)?;
                let mut add = tx.prepare_cached(
                    "INSERT INTO post_collections (post_id, collection_id, added_at)
                     VALUES (?1, ?2, ?3) ON CONFLICT (post_id, collection_id) DO NOTHING",
                )?;
                for (collection, added_at) in &bundle.collections {
                    let n = add.execute(params![id, collection, added_at])? as u64;
                    counts.memberships_added += n;
                    counts.memberships_present += 1 - n;
                }
                let (added, present) = copy_captures(
                    tx,
                    id,
                    &bundle.captures,
                    bundle.current_capture,
                    Current::Replace,
                )?;
                counts.captures_added += added;
                counts.captures_present += present;
                counts.posts.entry(platform).or_default().inserted += 1;
                touched.push(id);
            }
        }
    }
    archive::refresh_states(tx, Scope::Posts(&touched), policy, now)?;
    Ok(counts)
}

/// How many of `collections` the post is in already.
fn count_memberships(tx: &Connection, post_id: i64, collections: &[i64]) -> Result<u64, RepoError> {
    if collections.is_empty() {
        return Ok(0);
    }
    let scope = serde_json::to_string(collections).expect("ids serialize");
    let n: i64 = tx.query_row(
        "SELECT count(*) FROM post_collections
         WHERE post_id = ?1 AND collection_id IN (SELECT value FROM json_each(?2))",
        params![post_id, scope],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// The bundle's own tag and entity rows of a new post (tiers included),
/// instead of those [`posts::insert`] rebuilt from its JSON columns.
fn replace_tag_rows(
    tx: &Connection,
    post_id: i64,
    tags: &[TagRow],
    entities: &[(String, String)],
) -> Result<(), RepoError> {
    if tags.is_empty() && entities.is_empty() {
        return Ok(());
    }
    tx.execute("DELETE FROM post_tags WHERE post_id = ?1", [post_id])?;
    tx.execute("DELETE FROM post_entities WHERE post_id = ?1", [post_id])?;
    let mut tag = tx.prepare_cached(
        "INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, source, tier)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?;
    for t in tags {
        tag.execute(params![post_id, t.norm, t.form, t.source, t.tier])?;
    }
    let mut entity = tx.prepare_cached(
        "INSERT OR IGNORE INTO post_entities (post_id, ent_norm, ent_form) VALUES (?1, ?2, ?3)",
    )?;
    for (norm, form) in entities {
        entity.execute(params![post_id, norm, form])?;
    }
    index::reindex_post(tx, post_id)?;
    Ok(())
}

/// Where the current site version of a merged post goes.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Current {
    /// To the bundle's: a new post, or a merged one whose bundle row won.
    Replace,
    /// To the bundle's only when the post has none.
    IfNone,
}

/// Copies the site versions of a bundle post to the live post `post_id`,
/// skipping those it has (same capture time). Returns how many were added
/// and how many were there.
fn copy_captures(
    tx: &Connection,
    post_id: i64,
    captures: &[BundleCapture],
    current: Option<i64>,
    policy: Current,
) -> Result<(u64, u64), RepoError> {
    if captures.is_empty() {
        return Ok((0, 0));
    }
    let (mut added, mut present) = (0, 0);
    let mut ids = IdMap::new();
    for c in captures {
        let existing: Option<i64> = tx
            .query_row(
                "SELECT id FROM web_captures WHERE post_id = ?1 AND captured_at = ?2
                 ORDER BY id LIMIT 1",
                params![post_id, c.captured_at],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(id) = existing {
            present += 1;
            ids.insert(c.id, id);
            continue;
        }
        tx.execute(
            "INSERT INTO web_captures (post_id, captured_at, requested_url, final_url, status,
                                       partial, engine, viewport, title, palette_json,
                                       fonts_json, tech_json, awards_json, meta_json, pages_json,
                                       traits_json, hero_object, favicon_object,
                                       ai_snapshot_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                     ?18, ?19, ?20)",
            params![
                post_id,
                c.captured_at,
                c.requested_url,
                c.final_url,
                c.status,
                c.partial,
                c.engine,
                c.viewport,
                c.title,
                c.palette_json,
                c.fonts_json,
                c.tech_json,
                c.awards_json,
                c.meta_json,
                c.pages_json,
                c.traits_json,
                c.hero_object,
                c.favicon_object,
                c.ai_snapshot_json,
                c.created_at
            ],
        )?;
        let id = tx.last_insert_rowid();
        let mut asset = tx.prepare_cached(
            "INSERT OR IGNORE INTO web_capture_assets (capture_id, page_index, role, seq,
                                                      object_id, css_top, css_height)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )?;
        for a in &c.assets {
            asset.execute(params![
                id,
                a.page_index,
                a.role,
                a.seq,
                a.object_id,
                a.css_top,
                a.css_height
            ])?;
        }
        added += 1;
        ids.insert(c.id, id);
    }
    if let Some(current) = current.and_then(|bundle_id| ids.get(&bundle_id).copied()) {
        let sql = match policy {
            Current::Replace => {
                "UPDATE posts SET current_capture_id = ?2 WHERE id = ?1
                   AND current_capture_id IS NOT ?2"
            }
            Current::IfNone => {
                "UPDATE posts SET current_capture_id = ?2 WHERE id = ?1
                   AND current_capture_id IS NULL"
            }
        };
        if tx.execute(sql, params![post_id, current])? > 0 {
            index::reindex_post(tx, post_id)?;
        }
    }
    Ok((added, present))
}

/// Tag aliases and clusters, settings and legacy ids; returns the setting
/// keys the library took from the desktop.
fn merge_rest(
    tx: &Connection,
    rest: &Rest,
    counts: &mut MergeCounts,
    now: i64,
) -> Result<Vec<String>, RepoError> {
    for (alias, canonical_norm, canonical_form, status) in &rest.aliases {
        counts.aliases_added += tx.execute(
            "INSERT OR IGNORE INTO tag_alias (alias_norm, canonical_norm, canonical_form, status,
                                              created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![alias, canonical_norm, canonical_form, status, now],
        )? as u64;
    }
    let mut clusters = IdMap::new();
    for ClusterRow {
        id,
        label,
        label_norm,
        status,
        run_id,
    } in &rest.clusters
    {
        let existing: Option<i64> = match label_norm {
            Some(norm) => tx
                .query_row(
                    "SELECT id FROM tag_cluster WHERE label_norm = ?1 ORDER BY id LIMIT 1",
                    [norm],
                    |r| r.get(0),
                )
                .optional()?,
            None => None,
        };
        let live = match existing {
            Some(live) => live,
            None => {
                tx.execute(
                    "INSERT INTO tag_cluster (label, label_norm, status, run_id, created_at,
                                              updated_at)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                    params![label, label_norm, status, run_id, now],
                )?;
                counts.clusters_added += 1;
                tx.last_insert_rowid()
            }
        };
        clusters.insert(*id, live);
    }
    for (tag, cluster) in &rest.cluster_memberships {
        if let Some(live) = clusters.get(cluster) {
            counts.cluster_memberships_added += tx.execute(
                "INSERT OR IGNORE INTO tag_cluster_membership (tag_norm, cluster_id)
                 VALUES (?1, ?2)",
                params![tag, live],
            )? as u64;
        }
    }
    let mut taken = Vec::new();
    for (key, value) in &rest.settings {
        if tx.execute(
            "INSERT OR IGNORE INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)",
            params![key, value, now],
        )? > 0
        {
            taken.push(key.clone());
        }
    }
    for (key, value) in &rest.legacy_ids {
        tx.execute(
            "INSERT OR IGNORE INTO meta (key, value) VALUES (?1, ?2)",
            params![key, value],
        )?;
    }
    Ok(taken)
}
