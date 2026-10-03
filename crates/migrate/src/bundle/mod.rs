//! The migration bundle (plan §4.1 step 4, §4.2): a new-schema
//! `library.sqlite` built from the desktop library, plus the content-addressed
//! objects it references.
//!
//! The bundle is built from the decisions of the dry run
//! ([`crate::plan::plan_with_mapping`]): the same canonical keys, the same
//! duplicate groups and their kept rows, the same file lookups. So the bundle
//! and the plan always agree, and the reconciliation compares like with like.
//!
//! What it writes, by desktop table (§4.2; `spikes/01-legacy-mapping.md`):
//!
//! - `posts` → `posts` through `shelfy_core::repo::posts::insert`, with the
//!   slides, the AI layer (`ai_provider = 'desktop-local'`, schema 1) and the
//!   user layer. A manual AI edit (desktop model `manuale`) becomes the web's
//!   manual edit: model `manual`, no provider and no schema version. The
//!   cover is the first file present of `thumbnail_path`, `image_path` and
//!   `preview_path`, the desktop card's order. A file that is missing leaves
//!   its slot pending for the archive (OI-6: a missing kept video just means
//!   "not kept").
//! - Duplicate rows (same key) fold into the kept row: their collections and
//!   manual tags are unioned and their notes joined, each once (§4.2, the
//!   core's `ingest::duplicates`).
//! - The desktop settings found in its localStorage (language and asset
//!   types, [`crate::settings`]) become `settings` rows.
//! - `post_tags` and `post_entities` are carried verbatim, tiers included;
//!   files from before desktop schema v1 get them rebuilt from the JSON
//!   columns instead, as the desktop's own repair would.
//! - Captured sites become `web_captures` + `web_capture_assets`
//!   ([`web`]); `web_snapshots` become older captures with their frozen AI
//!   layer in `ai_snapshot_json`.
//! - Collections, memberships, aliases and clusters are carried with their
//!   ids remapped and their times in milliseconds.
//! - `meta` keeps the [`BundleSummary`] and, for manual posts, the legacy id.
//!
//! Derived data the server owns is left out or zeroed: renditions
//! (`variants = 0`), ThumbHash, and the FTS index, which the server rebuilds
//! at install anyway. Files are not copied: each object records the file to
//! upload it from.

pub mod objects;
pub mod summary;
pub mod web;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{self, Read as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use rusqlite::{Connection, Transaction, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use shelfy_core::ids::{Platform, ig};
use shelfy_core::ingest::archive::{
    self, ArchiveModes, ArchivePolicy, ArchiveState, Asset, PostFacts, SlideFacts,
};
use shelfy_core::ingest::duplicates;
use shelfy_core::legacy::convert::{
    Timestamp, cdn_url_expiry_ms, classify_timestamp, epoch_to_ms, is_local_path, json_string_array,
};
use shelfy_core::legacy::{
    CollectionRow, LegacyDb, PostCollectionRow, PostEntityRow, PostMediaRow, PostRow, PostTagRow,
    TagAliasRow, TagClusterMembershipRow, TagClusterRow, WebSnapshotRow,
};
use shelfy_core::repo::posts::{
    self, AiLayer, CAPTION_MAX_CHARS, MANUAL_AI_MODEL, NewMedia, NewPost,
};
use shelfy_core::schema::{self, Kind};
use shelfy_core::search::index;

use crate::files::FileState;
use crate::plan::{PlanMapping, PostMapping};
use crate::settings::DesktopSettings;
use objects::{ObjectTable, Role};
pub use summary::{BundleSummary, SUMMARY_META_KEY};
use web::Capture;

/// File name of the bundle's database.
pub const BUNDLE_DB_FILE: &str = "library.sqlite";
/// `posts.ai_provider` of an analysis made by the desktop's local model.
pub const DESKTOP_AI_PROVIDER: &str = "desktop-local";
/// `posts.ai_schema_version` of a desktop analysis.
pub const DESKTOP_AI_SCHEMA: i64 = 1;
/// `posts.ai_model` of a manual AI edit on the desktop (`analyze:updateManual`).
pub const DESKTOP_MANUAL_AI_MODEL: &str = "manuale";
/// `meta.key` prefix of a manual post's desktop id, for traceability (§4.2).
pub const LEGACY_ID_META_PREFIX: &str = "legacy_id:";
/// What joins the notes of merged duplicates.
pub const NOTE_SEPARATOR: &str = duplicates::NOTE_SEPARATOR;

/// How to build a bundle.
#[derive(Debug, Clone, Default)]
pub struct BundleOptions {
    /// Include kept videos (`--with-videos`).
    pub with_videos: bool,
    /// The library was read from a snapshot (OI-8).
    pub snapshot: bool,
    /// "Now", unix ms: the build time and the reference for URL expiry.
    pub now_ms: i64,
    /// The desktop settings to carry, when found.
    pub settings: Option<DesktopSettings>,
}

/// A built bundle.
#[derive(Debug, Clone)]
pub struct Bundle {
    /// The new-schema library.
    pub db_path: PathBuf,
    /// Its SHA-256 (lowercase hex) and size.
    pub db_sha256: String,
    pub db_bytes: u64,
    /// The distinct objects the library references.
    pub objects: Vec<BundleObject>,
    pub summary: BundleSummary,
}

/// One object of the bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BundleObject {
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// The store's extension for its type.
    pub ext: &'static str,
    pub bytes: u64,
    /// The desktop file to upload it from.
    pub path: PathBuf,
}

/// Builds the bundle of `db` into `dir` (its database is
/// `dir/library.sqlite`, replaced if present).
///
/// # Errors
///
/// The library cannot be read, or the bundle cannot be written.
pub fn build(
    db: &LegacyDb,
    mapping: &PlanMapping,
    dir: &Path,
    opts: &BundleOptions,
) -> anyhow::Result<Bundle> {
    let mut builder = Builder::new(db, mapping, opts);
    builder.draft()?;
    let path = dir.join(BUNDLE_DB_FILE);
    for suffix in ["", "-journal", "-wal", "-shm"] {
        let mut name = path.as_os_str().to_owned();
        name.push(suffix);
        match fs::remove_file(PathBuf::from(name)) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e).context("cannot replace the previous bundle"),
        }
    }
    let mut conn =
        Connection::open(&path).with_context(|| format!("cannot create {}", path.display()))?;
    schema::migrate(&mut conn, Kind::Library).context("cannot create the bundle schema")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    let objects = {
        let tx = conn.transaction()?;
        let objects = builder.write(&tx)?;
        tx.commit()?;
        objects
    };
    let broken: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
        r.get(0)
    })?;
    anyhow::ensure!(broken == 0, "the bundle breaks a foreign key");
    conn.close().map_err(|(_, e)| e)?;
    let (db_sha256, db_bytes) = sha256_file(&path)?;
    Ok(Bundle {
        db_path: path,
        db_sha256,
        db_bytes,
        objects,
        summary: builder.summary,
    })
}

/// The SHA-256 (lowercase hex) and size of a file.
///
/// # Errors
///
/// The file cannot be read.
pub fn sha256_file(path: &Path) -> io::Result<(String, u64)> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        hasher.update(&buffer[..n]);
        size += n as u64;
    }
    let digest = shelfy_media::Digest::from_bytes(hasher.finalize().into());
    Ok((digest.to_string(), size))
}

/// A slide, with objects as indexes into the [`ObjectTable`].
#[derive(Debug, Clone)]
struct Slide {
    kind: &'static str,
    source_url: Option<String>,
    source_url_expires_at: Option<i64>,
    label: Option<String>,
    object: Option<usize>,
    video_object: Option<usize>,
}

/// A captured version, mapped, with what the capture row needs.
#[derive(Debug, Clone)]
struct Version {
    capture: Capture,
    captured_at: i64,
    created_at: i64,
    ai_snapshot: Option<String>,
}

/// A post ready to write, with objects as indexes.
struct Draft {
    /// The desktop ids of its group, the kept row first.
    legacy_ids: Vec<String>,
    post: NewPost,
    cover: Option<usize>,
    slides: Vec<Slide>,
    current: Option<Version>,
    older: Vec<Version>,
}

struct Builder<'a> {
    db: &'a LegacyDb,
    mapping: &'a PlanMapping,
    opts: &'a BundleOptions,
    /// The archive policy of the new library: the server's default modes and
    /// the desktop's asset types, which the bundle carries as its setting.
    archive: ArchivePolicy,
    objects: ObjectTable,
    drafts: Vec<Draft>,
    summary: BundleSummary,
}

impl<'a> Builder<'a> {
    fn new(db: &'a LegacyDb, mapping: &'a PlanMapping, opts: &'a BundleOptions) -> Self {
        let objects = ObjectTable::hash(&mapping.files, opts.with_videos);
        let mut summary = BundleSummary {
            tool: format!("shelfy-migrate {}", env!("CARGO_PKG_VERSION")),
            desktop_user_version: db.schema().user_version,
            with_videos: opts.with_videos,
            snapshot: opts.snapshot,
            built_at: opts.now_ms,
            ..BundleSummary::default()
        };
        let files = &mut summary.files;
        let refs = &mapping.files;
        files.referenced = refs.len() as u64;
        for id in 0..refs.len() {
            match refs.state(id) {
                FileState::Present { .. } => files.present += 1,
                FileState::Missing => {
                    files.missing += 1;
                    for class in refs.classes(id) {
                        *files.missing_by_class.entry(class.name()).or_default() += 1;
                    }
                }
                FileState::OutsideRoot => files.outside_root += 1,
                FileState::Unchecked => {}
            }
        }
        let counts = &objects.counts;
        files.videos_excluded = counts.excluded;
        files.videos_excluded_bytes = counts.excluded_bytes;
        files.unsupported = counts.unsupported;
        files.unreadable = counts.unreadable;
        files.hashed = counts.hashed;
        files.hashed_bytes = counts.hashed_bytes;
        let archive = ArchivePolicy {
            modes: ArchiveModes::default(),
            assets: opts
                .settings
                .as_ref()
                .and_then(|s| s.archive_asset_types)
                .unwrap_or_default(),
        };
        Builder {
            db,
            mapping,
            opts,
            archive,
            objects,
            drafts: Vec::new(),
            summary,
        }
    }

    // ── pass 1: drafts ───────────────────────────────────────────────────

    fn draft(&mut self) -> anyhow::Result<()> {
        let posts: Vec<PostRow> = self.db.read_all()?;
        let mut slides: HashMap<String, Vec<PostMediaRow>> = HashMap::new();
        for slide in self.db.read_all::<PostMediaRow>()? {
            slides.entry(slide.post_id.clone()).or_default().push(slide);
        }
        for list in slides.values_mut() {
            list.sort_by_key(|s| s.position);
        }
        let mut snapshots: HashMap<String, Vec<WebSnapshotRow>> = HashMap::new();
        for snapshot in self.db.read_all::<WebSnapshotRow>()? {
            if self.mapping.posts.contains_key(&snapshot.post_id) {
                snapshots
                    .entry(snapshot.post_id.clone())
                    .or_default()
                    .push(snapshot);
            } else {
                self.summary.repairs.orphan_rows += 1;
            }
        }

        // Rows folded into another row, by key, in desktop order.
        let mut folded: HashMap<&str, Vec<&PostRow>> = HashMap::new();
        for p in &posts {
            if let Some(m) = self.mapping.posts.get(&p.id) {
                *self
                    .summary
                    .posts
                    .read
                    .entry(m.platform.as_str().to_owned())
                    .or_default() += 1;
                if !m.kept {
                    folded.entry(m.key.as_str()).or_default().push(p);
                    self.summary.posts.merged += 1;
                }
            }
        }
        for p in &posts {
            let Some(m) = self.mapping.posts.get(&p.id) else {
                continue;
            };
            if !m.kept {
                continue;
            }
            let group = folded.remove(m.key.as_str()).unwrap_or_default();
            let mut draft = self.draft_post(p, m, &group, slides.get(&p.id).map(Vec::as_slice));
            // Site versions: the kept row's current capture, then the older
            // versions of every row of the group.
            if m.platform == Platform::Web {
                draft.current = self.current_version(p, draft.post.imported_at);
                for legacy_id in &draft.legacy_ids {
                    for snapshot in snapshots.remove(legacy_id).unwrap_or_default() {
                        if let Some(version) = self.older_version(&snapshot, draft.post.imported_at)
                        {
                            draft.older.push(version);
                        }
                    }
                }
            }
            *self
                .summary
                .posts
                .written
                .entry(m.platform.as_str().to_owned())
                .or_default() += 1;
            self.drafts.push(draft);
        }
        Ok(())
    }

    fn draft_post(
        &mut self,
        p: &PostRow,
        m: &PostMapping,
        folded: &[&PostRow],
        slides: Option<&[PostMediaRow]>,
    ) -> Draft {
        let now = self.opts.now_ms;
        let files = &self.mapping.files;
        let platform = repo_platform(m.platform);
        let repairs = &mut self.summary.repairs;

        // The kept row's date, else a folded row's (the core's duplicate
        // policy fills the survivor's gaps), else the shortcode's.
        let valid = |row: &PostRow| match classify_timestamp(row.timestamp.as_deref()) {
            Timestamp::Valid(ms) => Some(ms),
            _ => None,
        };
        let posted_at = match valid(p).or_else(|| folded.iter().find_map(|row| valid(row))) {
            Some(ms) => Some(ms),
            None if m.platform == Platform::Instagram => {
                let date = non_empty(&p.shortcode).and_then(ig::date_from_shortcode);
                if date.is_some() {
                    repairs.ig_dates_from_shortcode += 1;
                }
                date
            }
            None => None,
        };
        // A merged post keeps the earliest import, as the core's merge does.
        let imported_at = std::iter::once(p)
            .chain(folded.iter().copied())
            .filter_map(|row| epoch_to_ms(row.imported_at))
            .min()
            .unwrap_or_else(|| {
                repairs.imported_at_missing += 1;
                posted_at.unwrap_or(now)
            });
        let media_type = non_empty(&p.media_type).map(str::to_owned);

        let mut post = NewPost::new(
            m.key.clone(),
            platform,
            m.native_id.clone(),
            media_type.clone().unwrap_or_default(),
            imported_at,
        );
        post.shortcode = non_empty(&p.shortcode).map(str::to_owned);
        post.post_url = non_empty(&p.post_url).map(|url| {
            if m.platform == Platform::Twitter && url.starts_with("https://x.com//status/") {
                repairs.x_status_urls += 1;
                format!("https://x.com/i/status/{}", m.native_id)
            } else {
                url.to_owned()
            }
        });
        post.profile_url = non_empty(&p.profile_url).map(str::to_owned);
        post.author_username = non_empty(&p.author_username).map(str::to_owned);
        post.author_name = non_empty(&p.author_name).map(str::to_owned);
        post.caption = p.text.as_deref().map(|text| {
            if text.chars().count() > CAPTION_MAX_CHARS {
                repairs.captions_truncated += 1;
                text.chars().take(CAPTION_MAX_CHARS).collect()
            } else {
                text.to_owned()
            }
        });
        post.posted_at = posted_at;
        post.cover_url = non_empty(&p.thumbnail_url)
            .filter(|u| is_remote(u))
            .map(str::to_owned);
        post.cover_url_expires_at = post.cover_url.as_deref().and_then(cdn_url_expiry_ms);
        // The kept row's analysis, else the first folded row's (the core's
        // duplicate policy: an unanalyzed survivor takes the other's).
        post.ai = match folded
            .iter()
            .find(|row| !has_analysis(p) && has_analysis(row))
        {
            Some(row) => {
                self.summary.repairs.ai_from_duplicates += 1;
                self.ai_layer(row)
            }
            None => self.ai_layer(p),
        };
        post.web_url = non_empty(&p.web_url).map(str::to_owned);
        post.web_domain = non_empty(&p.web_domain).map(str::to_owned);
        post.web_final_url = non_empty(&p.web_final_url).map(str::to_owned);

        // The user layer: the kept row's verbatim; with folded rows, every
        // row's, joined and united by the core's duplicate policy (§4.2).
        if folded.is_empty() {
            post.user_note = p.user_note.clone().filter(|n| !n.is_empty());
            let mut seen_tags: HashSet<String> = HashSet::new();
            post.user_tags = json_string_array(p.user_tags.as_deref())
                .into_iter()
                .filter(|tag| seen_tags.insert(tag.to_lowercase()))
                .collect();
        } else {
            let rows: Vec<&PostRow> = std::iter::once(p).chain(folded.iter().copied()).collect();
            post.user_note =
                duplicates::join_notes(rows.iter().filter_map(|row| row.user_note.as_deref()));
            let tags: Vec<Vec<String>> = rows
                .iter()
                .map(|row| json_string_array(row.user_tags.as_deref()))
                .collect();
            post.user_tags = duplicates::union_tags(tags.iter().map(Vec::as_slice));
        }

        // The cover: the desktop card's order (thumbnail, image, preview).
        let video_post = media_type.as_deref() == Some("video");
        let cover_role = if video_post {
            Role::Poster
        } else {
            Role::Image
        };
        let cover = [
            (&p.thumbnail_path, cover_role),
            (&p.image_path, Role::Image),
            (&p.preview_path, Role::Preview),
        ]
        .into_iter()
        .find_map(|(path, role)| {
            non_empty(path).and_then(|raw| self.objects.use_path(files, raw, role))
        });

        // Slides; files before desktop schema v1 get slide 0 from the post
        // columns, as the desktop's repair v1 does.
        let backfilled;
        let rows = match slides {
            Some(rows) => rows,
            None if self.db.schema().user_version < 1 => {
                backfilled = backfill_slide(p);
                backfilled.as_slice()
            }
            None => &[],
        };
        let mut drafted = Vec::with_capacity(rows.len());
        for (position, row) in rows.iter().enumerate() {
            drafted.push(self.slide(p, m, row, position, cover));
        }
        if media_type.is_none() {
            post.media_type = derived_media_type(&drafted).to_owned();
        }

        // Archive state and cover counts (OI-6, OI-7).
        let covers = &mut self.summary.covers;
        if cover.is_some() {
            covers.stored += 1;
        } else if post.cover_url.is_none() {
            covers.none += 1;
        } else {
            match (m.platform, post.cover_url_expires_at) {
                (Platform::Instagram, Some(expiry)) if expiry <= now => covers.ig_expired += 1,
                (Platform::Instagram, Some(_)) => covers.ig_valid += 1,
                (Platform::Instagram, None) => covers.ig_no_expiry += 1,
                (Platform::Twitter, _) => covers.x += 1,
                (Platform::Pinterest, _) => covers.pinterest += 1,
                _ => covers.other += 1,
            }
        }
        let state = archive_state(&post, cover.is_some(), &drafted, &self.archive, now);
        post.archive_state = Some(state.as_str().to_owned());

        let mut legacy_ids = vec![p.id.clone()];
        legacy_ids.extend(folded.iter().map(|row| row.id.clone()));
        Draft {
            legacy_ids,
            post,
            cover,
            slides: drafted,
            current: None,
            older: Vec::new(),
        }
    }

    fn slide(
        &mut self,
        p: &PostRow,
        m: &PostMapping,
        row: &PostMediaRow,
        position: usize,
        cover: Option<usize>,
    ) -> Slide {
        let files = &self.mapping.files;
        let first = position == 0;
        let local = non_empty(&row.local_path);
        if m.platform == Platform::Web {
            let object = local.and_then(|raw| self.objects.use_path(files, raw, Role::Screenshot));
            return Slide {
                kind: "page",
                source_url: non_empty(&row.source_url).map(str::to_owned),
                source_url_expires_at: None,
                label: page_title(p.web_pages_json.as_deref(), position),
                object,
                video_object: None,
            };
        }
        let kind = match row.media_type.as_str() {
            "image" => "image",
            "video" => "video",
            "file" => "file",
            _ => {
                self.summary.repairs.unknown_slide_kinds += 1;
                "image"
            }
        };
        // A manual post keeps the original file's local path in source_url.
        let original = (m.platform == Platform::Manual)
            .then(|| non_empty(&row.source_url).filter(|s| is_local_path(s)))
            .flatten();
        let source_url = non_empty(&row.source_url)
            .filter(|u| is_remote(u))
            .map(str::to_owned);
        let source_url_expires_at = source_url.as_deref().and_then(cdn_url_expiry_ms);
        let (object, video_object) = match kind {
            "video" => {
                // The poster of slide 0 is the post's cover. The slide's own
                // file, else the post's kept video, is a video to keep, or a
                // still the desktop stored in its place: then it is the poster.
                let mut poster = if first { cover } else { None };
                let mut video = None;
                let mut missing = false;
                let candidates = [
                    original,
                    local,
                    first.then(|| non_empty(&p.video_path)).flatten(),
                ];
                for raw in candidates.into_iter().flatten() {
                    match self.objects.kind_of(files, raw) {
                        Some(found) if found.is_video() => {
                            if video.is_none() {
                                video = if original.is_some() {
                                    self.objects.use_original(files, raw)
                                } else {
                                    self.objects.use_path(files, raw, Role::Video)
                                };
                            }
                        }
                        Some(_) => {
                            if poster.is_none() {
                                poster = self.objects.use_path(files, raw, Role::Poster);
                            }
                        }
                        None => missing |= self.objects.is_missing(files, raw),
                    }
                }
                if video.is_none() && missing {
                    self.summary.repairs.videos_missing += 1;
                }
                (poster, video)
            }
            _ => {
                // The original of a manual upload, else the slide's file, else
                // the post's slide-0 image. A video found there is kept as one.
                let mut object = None;
                let mut video = None;
                let candidates = [
                    original,
                    local,
                    first.then(|| non_empty(&p.image_path)).flatten(),
                ];
                for raw in candidates.into_iter().flatten() {
                    let is_original = Some(raw) == original;
                    match self.objects.kind_of(files, raw) {
                        Some(found) if found.is_video() => {
                            if video.is_none() {
                                video = if is_original {
                                    self.objects.use_original(files, raw)
                                } else {
                                    self.objects.use_path(files, raw, Role::Video)
                                };
                            }
                        }
                        Some(_) if object.is_none() => {
                            object = if is_original {
                                self.objects.use_original(files, raw)
                            } else if kind == "file" {
                                self.objects.use_path(files, raw, Role::Preview)
                            } else {
                                self.objects.use_path(files, raw, Role::Image)
                            };
                        }
                        _ => {}
                    }
                }
                (object, video)
            }
        };
        Slide {
            kind,
            source_url,
            source_url_expires_at,
            label: None,
            object,
            video_object,
        }
    }

    fn ai_layer(&mut self, p: &PostRow) -> Option<AiLayer> {
        let tags = json_string_array(p.ai_tags.as_deref());
        let entities = json_string_array(p.ai_entities.as_deref());
        let keywords = json_string_array(p.ai_keywords.as_deref());
        let has_fields = [
            &p.ai_description,
            &p.ai_status,
            &p.ai_model,
            &p.ai_category,
            &p.ai_content_type,
            &p.ai_language,
            &p.ai_save_reason,
            &p.ai_web_json,
        ]
        .iter()
        .any(|f| non_empty(f).is_some())
            || p.ai_analyzed_at.is_some()
            || !tags.is_empty()
            || !entities.is_empty()
            || !keywords.is_empty();
        if !has_fields {
            return None;
        }
        let status = match p.ai_status.as_deref() {
            Some("analyzing") => {
                self.summary.repairs.ai_stuck_reset += 1;
                None
            }
            other => other.map(str::to_owned),
        };
        let web = non_empty(&p.ai_web_json).and_then(|raw| match serde_json::from_str(raw) {
            Ok(value) => Some(value),
            Err(_) => {
                self.summary.repairs.ai_web_json_invalid += 1;
                None
            }
        });
        // A manual edit is the user's own layer, as on the web: model
        // `manual`, attributed to no provider and no output schema.
        let manual = p.ai_model.as_deref().map(str::trim) == Some(DESKTOP_MANUAL_AI_MODEL);
        if manual {
            self.summary.repairs.manual_ai_edits += 1;
        }
        Some(AiLayer {
            status,
            provider: (!manual).then(|| DESKTOP_AI_PROVIDER.to_owned()),
            model: if manual {
                Some(MANUAL_AI_MODEL.to_owned())
            } else {
                p.ai_model.clone()
            },
            schema_version: (!manual).then_some(DESKTOP_AI_SCHEMA),
            description: p.ai_description.clone(),
            save_reason: p.ai_save_reason.clone(),
            language: p.ai_language.clone(),
            category: p.ai_category.clone(),
            content_type: p.ai_content_type.clone(),
            tags,
            general_tags: None,
            specific_tags: None,
            entities,
            keywords,
            web,
            analyzed_at: epoch_to_ms(p.ai_analyzed_at),
        })
    }

    fn current_version(&mut self, p: &PostRow, imported_at: i64) -> Option<Version> {
        let capture = web::map_capture(
            p.web_pages_json.as_deref(),
            p.web_meta_json.as_deref(),
            None,
            web::SiteJson {
                palette: p.web_palette_json.as_deref(),
                fonts: p.web_fonts_json.as_deref(),
                tech: p.web_tech_json.as_deref(),
                awards: p.web_awards_json.as_deref(),
            },
            &self.mapping.files,
            &mut self.objects,
        )?;
        self.summary.repairs.site_json_invalid += capture.site_json_invalid;
        let captured_at = epoch_to_ms(p.web_captured_at).unwrap_or(imported_at);
        Some(Version {
            capture,
            captured_at,
            created_at: captured_at,
            ai_snapshot: None,
        })
    }

    fn older_version(&mut self, s: &WebSnapshotRow, imported_at: i64) -> Option<Version> {
        let capture = web::map_capture(
            s.web_pages_json.as_deref(),
            s.web_meta_json.as_deref(),
            non_empty(&s.title),
            web::SiteJson {
                palette: s.web_palette_json.as_deref(),
                fonts: s.web_fonts_json.as_deref(),
                tech: s.web_tech_json.as_deref(),
                awards: s.web_awards_json.as_deref(),
            },
            &self.mapping.files,
            &mut self.objects,
        )?;
        self.summary.repairs.site_json_invalid += capture.site_json_invalid;
        let captured_at = epoch_to_ms(s.captured_at).unwrap_or(imported_at);
        let web = non_empty(&s.ai_web_json).and_then(|raw| serde_json::from_str::<Value>(raw).ok());
        let ai = json!({
            "description": s.ai_description,
            "tags": json_string_array(s.ai_tags_json.as_deref()),
            "model": s.ai_model,
            "status": s.ai_status,
            "analyzedAt": epoch_to_ms(s.ai_analyzed_at),
            "category": s.ai_category,
            "contentType": s.ai_content_type,
            "entities": json_string_array(s.ai_entities_json.as_deref()),
            "keywords": json_string_array(s.ai_keywords_json.as_deref()),
            "language": s.ai_language,
            "saveReason": s.ai_save_reason,
            "web": web,
        });
        Some(Version {
            capture,
            captured_at,
            created_at: epoch_to_ms(s.created_at).unwrap_or(captured_at),
            ai_snapshot: Some(ai.to_string()),
        })
    }

    // ── pass 2: the database ─────────────────────────────────────────────

    /// Writes everything in `tx`; returns the objects to upload.
    fn write(&mut self, tx: &Transaction<'_>) -> anyhow::Result<Vec<BundleObject>> {
        let now = self.opts.now_ms;
        // Aliases first: tag rows rebuilt from JSON resolve through them.
        self.write_aliases(tx)?;
        let (row_ids, upload) = self.write_objects(tx)?;
        let collections = self.write_collections(tx)?;

        let mut post_ids: HashMap<String, i64> = HashMap::new();
        let rebuild_tags = self.db.schema().user_version < 1;
        let tag_rows = group_by_post(self.db.read_all::<PostTagRow>()?, |r| &r.post_id);
        let entity_rows = group_by_post(self.db.read_all::<PostEntityRow>()?, |r| &r.post_id);
        let drafts = std::mem::take(&mut self.drafts);
        for draft in &drafts {
            let mut post = draft.post.clone();
            post.cover_object = draft.cover.map(|i| row_ids[i]);
            post.media = draft
                .slides
                .iter()
                .map(|s| NewMedia {
                    kind: s.kind.to_owned(),
                    source_url: s.source_url.clone(),
                    source_url_expires_at: s.source_url_expires_at,
                    label: s.label.clone(),
                    object_id: s.object.map(|i| row_ids[i]),
                    video_object_id: s.video_object.map(|i| row_ids[i]),
                    ..NewMedia::default()
                })
                .collect();
            let id = posts::insert(tx, &post, now)
                .with_context(|| format!("cannot write post {}", post.key))?;
            self.summary.rows.slides += post.media.len() as u64;
            if !rebuild_tags {
                self.replace_tag_rows(tx, id, draft, &tag_rows, &entity_rows)?;
            }
            for version in draft.older.iter() {
                self.write_capture(tx, id, version, &row_ids)?;
            }
            if let Some(version) = &draft.current {
                let capture_id = self.write_capture(tx, id, version, &row_ids)?;
                tx.execute(
                    "UPDATE posts SET current_capture_id = ?2 WHERE id = ?1",
                    params![id, capture_id],
                )?;
                index::reindex_post(tx, id)?;
            }
            if post.platform == shelfy_core::repo::Platform::Manual {
                tx.execute(
                    "INSERT INTO meta (key, value) VALUES (?1, ?2)",
                    params![
                        format!("{LEGACY_ID_META_PREFIX}{}", post.key),
                        draft.legacy_ids[0]
                    ],
                )?;
            }
            for legacy_id in &draft.legacy_ids {
                post_ids.insert(legacy_id.clone(), id);
            }
        }
        if rebuild_tags {
            self.summary.rows.post_tags = count(tx, "post_tags")?;
            self.summary.rows.post_entities = count(tx, "post_entities")?;
        }

        // Memberships of every row of a group go to the kept post.
        let mut memberships = 0u64;
        self.db.stream(|pc: PostCollectionRow| {
            let post = post_ids.get(&pc.post_id);
            let collection = self
                .mapping
                .collections
                .get(&pc.collection_id)
                .and_then(|kept| collections.get(kept));
            match (post, collection) {
                (Some(&post), Some(&collection)) => {
                    let added = tx
                        .prepare_cached(
                            "INSERT OR IGNORE INTO post_collections (post_id, collection_id, added_at)
                             VALUES (?1, ?2, ?3)",
                        )?
                        .execute(params![post, collection, epoch_to_ms(pc.added_at).unwrap_or(now)])?;
                    memberships += added as u64;
                }
                _ => self.summary.repairs.orphan_rows += 1,
            }
            Ok::<_, anyhow::Error>(())
        })?;
        self.summary.rows.memberships = memberships;
        self.write_clusters(tx)?;
        if let Some(settings) = &self.opts.settings {
            for (key, value) in settings.rows() {
                tx.execute(
                    "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, ?2, ?3)",
                    params![key, value, now],
                )?;
                self.summary.settings.push(key.to_owned());
            }
        }

        for object in &upload {
            *self
                .summary
                .objects
                .by_role
                .entry(object.1.to_owned())
                .or_default() += 1;
        }
        let upload: Vec<BundleObject> = upload.into_iter().map(|(object, _)| object).collect();
        self.summary.objects.count = upload.len() as u64;
        self.summary.objects.bytes = upload.iter().map(|o| o.bytes).sum();
        let summary = serde_json::to_string(&self.summary)?;
        tx.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2)",
            params![SUMMARY_META_KEY, summary],
        )?;
        Ok(upload)
    }

    /// Inserts the objects some row uses; returns their row ids (by object
    /// index; 0 for unused objects) and the upload list with each role.
    #[allow(clippy::type_complexity)]
    fn write_objects(
        &mut self,
        tx: &Transaction<'_>,
    ) -> anyhow::Result<(Vec<i64>, Vec<(BundleObject, &'static str)>)> {
        let now = self.opts.now_ms;
        let mut row_ids = vec![0i64; self.objects.objects.len()];
        let mut upload = Vec::new();
        let mut insert = tx.prepare(
            "INSERT INTO media_objects (sha256, ext, mime, bytes, role, variants, origin, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, 0, 'migration', ?6)",
        )?;
        for (index, object) in self.objects.objects.iter().enumerate() {
            let Some(role) = object.role else {
                continue; // only rows of folded duplicates used it
            };
            insert.execute(params![
                &object.digest.as_bytes()[..],
                object.kind.ext(),
                object.kind.mime(),
                i64::try_from(object.bytes).unwrap_or(i64::MAX),
                role.as_str(),
                now
            ])?;
            row_ids[index] = tx.last_insert_rowid();
            upload.push((
                BundleObject {
                    sha256: object.digest.to_string(),
                    ext: object.kind.ext(),
                    bytes: object.bytes,
                    path: object.path.clone(),
                },
                role.as_str(),
            ));
        }
        Ok((row_ids, upload))
    }

    /// Inserts the kept collections; returns legacy id → new id.
    fn write_collections(&mut self, tx: &Transaction<'_>) -> anyhow::Result<HashMap<i64, i64>> {
        let now = self.opts.now_ms;
        let mut ids = HashMap::new();
        for c in self.db.read_all::<CollectionRow>()? {
            if self.mapping.collections.get(&c.id) != Some(&c.id) {
                self.summary.rows.collections_merged += 1;
                continue;
            }
            let name = c.name.trim();
            let (name, color) = (
                if name.is_empty() {
                    "Untitled".to_owned()
                } else {
                    name.chars().take(200).collect()
                },
                valid_color(&c.color),
            );
            if c.name.trim().is_empty() || color.is_none() {
                self.summary.repairs.collections_fixed += 1;
            }
            let external_id = non_empty(&c.external_id);
            let platform = c
                .platform
                .as_deref()
                .and_then(Platform::parse)
                .filter(|_| external_id.is_some())
                .map(Platform::as_str);
            tx.execute(
                "INSERT INTO collections (name, color, platform, external_id, source_name, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    name,
                    color.unwrap_or_else(|| shelfy_core::repo::collections::DEFAULT_COLOR.to_owned()),
                    platform,
                    external_id,
                    non_empty(&c.ig_name),
                    epoch_to_ms(c.created_at).unwrap_or(now)
                ],
            )?;
            ids.insert(c.id, tx.last_insert_rowid());
            self.summary.rows.collections += 1;
        }
        Ok(ids)
    }

    /// Replaces the tag and entity rows `posts::insert` rebuilt from the JSON
    /// columns with the desktop's own rows (tiers included).
    fn replace_tag_rows(
        &mut self,
        tx: &Transaction<'_>,
        id: i64,
        draft: &Draft,
        tag_rows: &HashMap<String, Vec<PostTagRow>>,
        entity_rows: &HashMap<String, Vec<PostEntityRow>>,
    ) -> anyhow::Result<()> {
        tx.execute("DELETE FROM post_tags WHERE post_id = ?1", [id])?;
        tx.execute("DELETE FROM post_entities WHERE post_id = ?1", [id])?;
        for legacy_id in &draft.legacy_ids {
            for t in tag_rows.get(legacy_id).into_iter().flatten() {
                let (source, tier) = match t.tier.as_deref() {
                    Some("manual") => ("manual", None),
                    Some(tier @ ("general" | "specific")) => ("ai", Some(tier)),
                    _ => ("ai", None),
                };
                self.summary.rows.post_tags += tx.execute(
                    "INSERT OR IGNORE INTO post_tags (post_id, tag_norm, tag_form, source, tier)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![id, t.tag_norm, t.tag_form, source, tier],
                )? as u64;
            }
            for e in entity_rows.get(legacy_id).into_iter().flatten() {
                self.summary.rows.post_entities += tx.execute(
                    "INSERT OR IGNORE INTO post_entities (post_id, ent_norm, ent_form)
                     VALUES (?1, ?2, ?3)",
                    params![id, e.ent_norm, e.ent_form],
                )? as u64;
            }
        }
        index::reindex_post(tx, id)?;
        Ok(())
    }

    fn write_capture(
        &mut self,
        tx: &Transaction<'_>,
        post_id: i64,
        version: &Version,
        row_ids: &[i64],
    ) -> anyhow::Result<i64> {
        let c = &version.capture;
        tx.execute(
            "INSERT INTO web_captures (post_id, captured_at, requested_url, final_url, status, partial,
                                       engine, viewport, title, palette_json, fonts_json, tech_json,
                                       awards_json, meta_json, pages_json, traits_json, hero_object,
                                       favicon_object, ai_snapshot_json, created_at)
             SELECT ?1, ?2, web_url, web_final_url, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, ?18
             FROM posts WHERE id = ?1",
            params![
                post_id,
                version.captured_at,
                web::STATUS_DONE,
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
                c.hero.map(|i| row_ids[i]),
                c.favicon.map(|i| row_ids[i]),
                version.ai_snapshot,
                version.created_at
            ],
        )?;
        let capture_id = tx.last_insert_rowid();
        self.summary.rows.web_captures += 1;
        for asset in &c.assets {
            self.summary.rows.web_capture_assets += tx.execute(
                "INSERT OR IGNORE INTO web_capture_assets (capture_id, page_index, role, seq, object_id,
                                                          css_top, css_height)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    capture_id,
                    asset.page_index,
                    asset.role,
                    asset.seq,
                    row_ids[asset.object],
                    asset.css_top,
                    asset.css_height
                ],
            )? as u64;
        }
        Ok(capture_id)
    }

    fn write_aliases(&mut self, tx: &Transaction<'_>) -> anyhow::Result<()> {
        let now = self.opts.now_ms;
        for a in self.db.read_all::<TagAliasRow>()? {
            let status = match a.status.as_str() {
                "accepted" => "accepted",
                _ => "proposed",
            };
            self.summary.rows.tag_aliases += tx.execute(
                "INSERT OR IGNORE INTO tag_alias (alias_norm, canonical_norm, canonical_form, status,
                                                  created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![a.alias_norm, a.canonical_norm, a.canonical_form, status, now],
            )? as u64;
        }
        Ok(())
    }

    fn write_clusters(&mut self, tx: &Transaction<'_>) -> anyhow::Result<()> {
        let now = self.opts.now_ms;
        let mut ids = HashSet::new();
        for c in self.db.read_all::<TagClusterRow>()? {
            let created_at = epoch_to_ms(c.created_at).unwrap_or(now);
            let status = if c.status.trim().is_empty() {
                "proposed"
            } else {
                c.status.as_str()
            };
            let added = tx.execute(
                "INSERT OR IGNORE INTO tag_cluster (id, label, label_norm, status, run_id, created_at,
                                                    updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    c.id,
                    c.label,
                    c.label_norm,
                    status,
                    c.run_id,
                    created_at,
                    epoch_to_ms(c.updated_at).unwrap_or(created_at)
                ],
            )?;
            if added > 0 {
                ids.insert(c.id);
                self.summary.rows.tag_clusters += 1;
            }
        }
        for m in self.db.read_all::<TagClusterMembershipRow>()? {
            if !ids.contains(&m.cluster_id) {
                self.summary.repairs.orphan_rows += 1;
                continue;
            }
            self.summary.rows.tag_cluster_memberships += tx.execute(
                "INSERT OR IGNORE INTO tag_cluster_membership (tag_norm, cluster_id) VALUES (?1, ?2)",
                params![m.tag_norm, m.cluster_id],
            )? as u64;
        }
        Ok(())
    }
}

/// The web schema's platform of a canonical id.
fn repo_platform(platform: Platform) -> shelfy_core::repo::Platform {
    use shelfy_core::repo::Platform as P;
    match platform {
        Platform::Instagram => P::Instagram,
        Platform::Twitter => P::Twitter,
        Platform::Pinterest => P::Pinterest,
        Platform::Web => P::Web,
        Platform::Manual => P::Manual,
    }
}

/// The archive state of a drafted post: the core's rule
/// (`shelfy_core::ingest::archive`, P2 contract C10) on what the bundle
/// stores. Nothing has been fetched yet, so nothing has failed. The server
/// derives the states again at install, with the same rule.
fn archive_state(
    post: &NewPost,
    cover_stored: bool,
    slides: &[Slide],
    policy: &ArchivePolicy,
    now: i64,
) -> ArchiveState {
    let facts = PostFacts {
        platform: post.platform,
        media_type: post.media_type.clone(),
        state: ArchiveState::Pending,
        cover: Asset {
            stored: cover_stored,
            has_url: post.cover_url.is_some(),
            expires_at: post.cover_url_expires_at,
            failed: false,
        },
        slides: slides
            .iter()
            .map(|s| SlideFacts {
                image: s.kind == "image",
                asset: Asset {
                    stored: s.object.is_some(),
                    has_url: s.source_url.is_some(),
                    expires_at: s.source_url_expires_at,
                    failed: false,
                },
                video_stored: s.video_object.is_some(),
            })
            .collect(),
    };
    archive::state(&facts, policy, now)
}

/// Whether a desktop row holds an AI analysis, as the core's duplicate
/// policy counts one: status `done`, a description or AI tags.
fn has_analysis(row: &PostRow) -> bool {
    row.ai_status.as_deref() == Some("done")
        || non_empty(&row.ai_description).is_some()
        || !json_string_array(row.ai_tags.as_deref()).is_empty()
}

/// `media_type` of a post that has none: from its slides.
fn derived_media_type(slides: &[Slide]) -> &'static str {
    match slides {
        [] => "text",
        [one] => match one.kind {
            "video" => "video",
            "file" => "file",
            "page" => "website",
            _ => "image",
        },
        _ => "carousel",
    }
}

/// Slide 0 of a post from before desktop schema v1, built from its columns
/// as the desktop's repair v1 does.
fn backfill_slide(p: &PostRow) -> Vec<PostMediaRow> {
    let has_media = p.thumbnail_url.is_some()
        || p.image_path.is_some()
        || p.thumbnail_path.is_some()
        || p.video_path.is_some();
    if !has_media {
        return Vec::new();
    }
    let media_type = if p.media_type.as_deref() == Some("video") {
        "video"
    } else {
        "image"
    };
    vec![PostMediaRow {
        post_id: p.id.clone(),
        position: 0,
        media_type: media_type.to_owned(),
        source_url: p.thumbnail_url.clone(),
        local_path: p
            .image_path
            .clone()
            .or_else(|| p.thumbnail_path.clone())
            .or_else(|| p.video_path.clone()),
    }]
}

/// The title of page `index` of a site's pages JSON.
fn page_title(pages_json: Option<&str>, index: usize) -> Option<String> {
    let pages: Value = serde_json::from_str(pages_json?).ok()?;
    pages
        .get(index)?
        .get("title")?
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_owned)
}

/// `#rgb` or `#rrggbb`, lowercased; `None` otherwise.
fn valid_color(color: &str) -> Option<String> {
    let color = color.trim();
    let hex = color.strip_prefix('#')?;
    (matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("#{}", hex.to_ascii_lowercase()))
}

fn is_remote(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://")
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.trim().is_empty())
}

fn group_by_post<R>(rows: Vec<R>, key: impl Fn(&R) -> &String) -> HashMap<String, Vec<R>> {
    let mut out: HashMap<String, Vec<R>> = HashMap::new();
    for row in rows {
        out.entry(key(&row).clone()).or_default().push(row);
    }
    out
}

fn count(conn: &Connection, table: &str) -> rusqlite::Result<u64> {
    let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Counts of the rows of a bundle's database, by table: what `run` checks
/// against its summary before uploading.
///
/// # Errors
///
/// The database cannot be read.
pub fn table_counts(path: &Path) -> anyhow::Result<BTreeMap<String, u64>> {
    let conn = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut out = BTreeMap::new();
    for table in [
        "posts",
        "post_media",
        "media_objects",
        "collections",
        "post_collections",
        "post_tags",
        "post_entities",
        "web_captures",
        "web_capture_assets",
    ] {
        out.insert(table.to_owned(), count(&conn, table)?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn slide(kind: &'static str, object: Option<usize>, url: bool) -> Slide {
        Slide {
            kind,
            source_url: url.then(|| "https://cdn.example/x.jpg".to_owned()),
            source_url_expires_at: None,
            label: None,
            object,
            video_object: None,
        }
    }

    /// A drafted post with a cover URL expiring at `expires_at`, if any.
    fn post(
        platform: shelfy_core::repo::Platform,
        media_type: &str,
        cover_url: bool,
        expires_at: Option<i64>,
    ) -> NewPost {
        let mut post = NewPost::new("k_1", platform, "1", media_type, 0);
        post.cover_url = cover_url.then(|| "https://cdn.example/c.jpg".to_owned());
        post.cover_url_expires_at = expires_at;
        post
    }

    fn state(post: &NewPost, cover_stored: bool, slides: &[Slide]) -> &'static str {
        archive_state(post, cover_stored, slides, &ArchivePolicy::default(), 1_000).as_str()
    }

    #[test]
    fn archive_states_follow_what_is_left_to_store() {
        use shelfy_core::repo::Platform::{Instagram, Twitter, Web};
        // Nothing to store: a text post, or everything stored.
        assert_eq!(
            state(&post(Instagram, "text", false, None), false, &[]),
            "done"
        );
        let stored = post(Instagram, "image", true, Some(10));
        assert_eq!(
            state(&stored, true, &[slide("image", Some(0), true)]),
            "done"
        );
        // Videos are on demand: a stored poster is enough.
        let video = post(Instagram, "video", true, None);
        assert_eq!(
            state(&video, true, &[slide("video", Some(0), true)]),
            "done"
        );
        // An expired IG cover needs the extension.
        let expired = post(Instagram, "image", true, Some(999));
        assert_eq!(state(&expired, false, &[]), "client");
        let valid = post(Instagram, "image", true, Some(1_001));
        assert_eq!(state(&valid, false, &[]), "pending");
        assert_eq!(
            state(&post(Twitter, "image", true, Some(1)), false, &[]),
            "pending"
        );
        // Something stored, something not.
        let carousel = post(Instagram, "carousel", true, None);
        let slides = [slide("image", Some(0), true), slide("image", None, true)];
        assert_eq!(state(&carousel, true, &slides), "partial");
        // The desktop's asset types: without images, the cover is enough.
        let mut covers_only = ArchivePolicy::default();
        covers_only.assets.image = false;
        assert_eq!(
            archive_state(&carousel, true, &slides, &covers_only, 1_000),
            ArchiveState::Done
        );
        // An Instagram post with no media at all waits for the server's
        // hydration (L17); a site that stores nothing is a link.
        assert_eq!(
            state(&post(Instagram, "image", false, None), false, &[]),
            "pending"
        );
        assert_eq!(
            state(&post(Web, "website", false, None), false, &[]),
            "link_only"
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(valid_color("#3D5AFE").as_deref(), Some("#3d5afe"));
        assert_eq!(valid_color("red"), None);
        assert!(is_remote("HTTPS://x.test/a"));
        assert!(!is_remote("/Users/x/assets/a.jpg"));
        assert_eq!(derived_media_type(&[]), "text");
        assert_eq!(
            derived_media_type(&[slide("image", None, true), slide("video", None, true)]),
            "carousel"
        );
        assert_eq!(
            page_title(Some(r#"[{"title":"Home"},{"title":" "}]"#), 0).as_deref(),
            Some("Home")
        );
        assert_eq!(page_title(Some(r#"[{"title":" "}]"#), 0), None);
    }
}
