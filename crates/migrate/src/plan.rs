//! `shelfy-migrate plan`: the dry run of a desktop → web migration
//! (plan §4.1 step 3, SPIKE-1).
//!
//! Reads the desktop library through the read-only legacy reader, maps every
//! row to the web schema (plan §2.7, §4.2) without writing anything, and
//! reports: rows per table and their outcome, column coverage, canonical
//! identities and duplicate groups, local files, and site versions.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;

use shelfy_core::ids::{CanonicalId, Platform, ig, manual, pinterest, web, x};
use shelfy_core::ingest::duplicates::{self, Layers};
use shelfy_core::legacy::catalog::{self, Disposition, ValueType};
use shelfy_core::legacy::convert::{
    self, EpochClass, JsonArrayClass, Timestamp, classify_epoch_seconds, classify_json_array,
    classify_timestamp,
};
use shelfy_core::legacy::web::{capture_files, derived_facets};
use shelfy_core::legacy::{
    CollectionRow, ColumnStatus, DownloadRow, JobRow, LegacyDb, LegacyError, OpenMode,
    PostCollectionRow, PostEntityRow, PostFacetRow, PostMediaRow, PostRow, PostTagRow, TableStatus,
    TagAliasRow, TagClusterMembershipRow, TagClusterRow, WebSnapshotRow,
};

use crate::files::{FileClass, FileRefs, FileState, PathId, scan_orphans};
use crate::report::*;

/// Options of a dry run.
#[derive(Debug, Clone)]
pub struct PlanOptions {
    /// The desktop userData directory holding `assets/`; files are only
    /// checked when it is given.
    pub media_root: Option<PathBuf>,
    /// Leave keys and legacy ids out of the duplicate listing.
    pub redact: bool,
    /// "Now" in unix ms, for CDN URL expiry.
    pub now_ms: i64,
}

/// Runs the dry run. Reads only: the library is opened read-only by the
/// caller and files are only looked up.
pub fn plan(db: &LegacyDb, opts: &PlanOptions) -> Result<PlanReport, LegacyError> {
    plan_with_mapping(db, opts).map(|(report, _)| report)
}

/// Runs the dry run and also returns the decisions behind it, which `run`
/// builds the bundle from: so the bundle and the report always agree.
pub fn plan_with_mapping(
    db: &LegacyDb,
    opts: &PlanOptions,
) -> Result<(PlanReport, PlanMapping), LegacyError> {
    let mut planner = Planner::new(db, opts);
    planner.scan_posts()?;
    planner.scan_post_media()?;
    planner.deduplicate();
    planner.scan_collections()?;
    planner.scan_tags()?;
    planner.scan_web_snapshots()?;
    planner.scan_dropped_tables()?;
    planner.check_files();
    planner.finish()
}

/// The decisions of a dry run, by desktop row.
#[derive(Debug, Default)]
pub struct PlanMapping {
    /// Legacy post id → its canonical identity. Posts without one (errors)
    /// are absent.
    pub posts: HashMap<String, PostMapping>,
    /// Legacy collection id → the id of the collection it merges into (its
    /// own id when it is kept).
    pub collections: HashMap<i64, i64>,
    /// Every referenced file and what was found on disk.
    pub files: FileRefs,
}

/// The canonical identity of one desktop post.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostMapping {
    pub platform: Platform,
    /// `posts.key`.
    pub key: String,
    /// `posts.native_id`.
    pub native_id: String,
    /// False when the row is folded into another row of its duplicate group.
    pub kept: bool,
}

/// What the planner remembers about one post, by legacy id.
#[derive(Debug, Default)]
struct PostInfo {
    platform: Option<Platform>,
    /// `None`: no canonical key (an error).
    key: Option<String>,
    native_id: Option<String>,
    source: &'static str,
    archived_files: u64,
    has_capture: bool,
    has_ai: bool,
    has_user_layer: bool,
    has_note: bool,
    imported_at: Option<i64>,
    media_type: Option<String>,
    media_count: Option<i64>,
    has_legacy_media: bool,
    slides: u64,
    cover_paths: Vec<PathId>,
    /// `None`: no cover URL; `Some(None)`: a URL without an expiry.
    cover_url_expiry: Option<Option<i64>>,
    /// Kept after deduplication (false: folded into another row).
    kept: bool,
    derived_facets: Option<BTreeSet<(String, String)>>,
}

struct Planner<'a> {
    db: &'a LegacyDb,
    opts: &'a PlanOptions,
    posts: HashMap<String, PostInfo>,
    files: FileRefs,
    outcomes: BTreeMap<String, BTreeMap<String, u64>>,
    errors: Vec<String>,
    warnings: Vec<String>,
    posts_report: PostsReport,
    identity: IdentityReport,
    duplicates: DuplicatesReport,
    tags: TagsReport,
    web: WebReport,
    files_report: FilesReport,
    orphan_slides: u64,
    post_media_kinds: BTreeMap<String, u64>,
    collection_map: HashMap<i64, i64>,
}

impl<'a> Planner<'a> {
    fn new(db: &'a LegacyDb, opts: &'a PlanOptions) -> Self {
        Planner {
            db,
            opts,
            posts: HashMap::new(),
            files: FileRefs::default(),
            outcomes: BTreeMap::new(),
            errors: Vec::new(),
            warnings: Vec::new(),
            posts_report: PostsReport::default(),
            identity: IdentityReport::default(),
            duplicates: DuplicatesReport::default(),
            tags: TagsReport::default(),
            web: WebReport::default(),
            files_report: FilesReport::default(),
            orphan_slides: 0,
            post_media_kinds: BTreeMap::new(),
            collection_map: HashMap::new(),
        }
    }

    fn outcome(&mut self, table: &str, outcome: &str, n: u64) {
        if n > 0 {
            *self
                .outcomes
                .entry(table.to_owned())
                .or_default()
                .entry(outcome.to_owned())
                .or_default() += n;
        }
    }

    // ── posts ────────────────────────────────────────────────────────────

    fn scan_posts(&mut self) -> Result<(), LegacyError> {
        let db = self.db;
        db.stream(|p: PostRow| {
            self.add_post(p);
            Ok::<_, LegacyError>(())
        })
    }

    fn add_post(&mut self, p: PostRow) {
        let platform = p.platform();
        bump(&mut self.posts_report.by_platform, &p.platform);
        bump(
            &mut self.posts_report.by_media_type,
            p.media_type.as_deref().unwrap_or("null"),
        );

        // Dates.
        let ts = classify_timestamp(p.timestamp.as_deref());
        let counts = &mut self.posts_report.posted_at;
        match ts {
            Timestamp::Valid(_) => counts.valid += 1,
            Timestamp::Empty => counts.empty += 1,
            Timestamp::Null => counts.null += 1,
            Timestamp::Invalid => counts.invalid += 1,
        }
        if !matches!(ts, Timestamp::Valid(_))
            && platform == Some(Platform::Instagram)
            && p.shortcode
                .as_deref()
                .is_some_and(|sc| ig::date_from_shortcode(sc).is_some())
        {
            counts.undated_ig_datable_from_shortcode += 1;
        }
        bump(
            &mut self.posts_report.imported_at,
            epoch_name(classify_epoch_seconds(p.imported_at)),
        );

        // AI and user layers.
        let ai = &mut self.posts_report.ai;
        bump(&mut ai.status, p.ai_status.as_deref().unwrap_or("null"));
        let ai_fields = [
            &p.ai_description,
            &p.ai_status,
            &p.ai_model,
            &p.ai_category,
            &p.ai_content_type,
            &p.ai_language,
            &p.ai_save_reason,
            &p.ai_web_json,
        ];
        let has_ai_fields = ai_fields.iter().any(|f| non_empty(f).is_some())
            || p.ai_analyzed_at.is_some()
            || [&p.ai_tags, &p.ai_entities, &p.ai_keywords]
                .iter()
                .any(|f| !convert::json_string_array(f.as_deref()).is_empty());
        if has_ai_fields {
            ai.with_ai_fields += 1;
        }
        if p.ai_status.as_deref() == Some("analyzing") {
            ai.stuck_analyzing += 1;
        }
        if non_empty(&p.ai_web_json).is_some() {
            ai.with_ai_web_json += 1;
        }
        let has_ai = p.ai_status.as_deref() == Some("done")
            || non_empty(&p.ai_description).is_some()
            || !convert::json_string_array(p.ai_tags.as_deref()).is_empty();
        let has_note = non_empty(&p.user_note).is_some();
        let manual_tags = convert::json_string_array(p.user_tags.as_deref());
        if has_note {
            self.posts_report.user_notes += 1;
        }
        if !manual_tags.is_empty() {
            self.posts_report.user_tags += 1;
        }
        for (column, value) in [
            ("ai_tags", &p.ai_tags),
            ("ai_entities", &p.ai_entities),
            ("ai_keywords", &p.ai_keywords),
            ("user_tags", &p.user_tags),
        ] {
            let class = json_class_name(classify_json_array(value.as_deref()));
            bump(
                self.posts_report
                    .json_arrays
                    .entry(column.to_owned())
                    .or_default(),
                class,
            );
        }
        let ai_norms: HashSet<String> = convert::json_string_array(p.ai_tags.as_deref())
            .iter()
            .map(|t| t.to_lowercase())
            .collect();
        self.tags.manual_ai_collisions += manual_tags
            .iter()
            .map(|t| t.to_lowercase())
            .collect::<HashSet<_>>()
            .intersection(&ai_norms)
            .count() as u64;
        bump(
            &mut self.posts_report.thumb_blur,
            match p.thumb_blur.as_deref() {
                None => "null",
                Some("") => "ineligible",
                Some(_) => "data_uri",
            },
        );
        if platform == Some(Platform::Twitter)
            && p.post_url
                .as_deref()
                .is_some_and(|u| u.starts_with("https://x.com//status/"))
        {
            self.posts_report.x_status_urls_to_repair += 1;
        }

        // Identity.
        let (id, source) = self.identify(&p, platform);
        if platform == Some(Platform::Instagram)
            && p.shortcode
                .as_deref()
                .is_some_and(|sc| sc.chars().count() > 12)
        {
            self.identity.ig_long_shortcodes += 1;
        }

        // Files of the row.
        let mut info = PostInfo {
            platform,
            key: id.as_ref().map(|id| id.key().to_owned()),
            native_id: id.map(|id| id.native_id().to_owned()),
            source,
            has_ai,
            has_note,
            has_user_layer: has_note || !manual_tags.is_empty(),
            imported_at: p.imported_at,
            media_type: p.media_type.clone(),
            media_count: p.media_count,
            kept: true,
            ..PostInfo::default()
        };
        info.has_legacy_media = p.thumbnail_url.is_some()
            || p.thumbnail_path.is_some()
            || p.image_path.is_some()
            || p.video_path.is_some();
        for (class, value, archived, cover) in [
            (FileClass::Cover, &p.thumbnail_path, true, true),
            (FileClass::Preview, &p.preview_path, false, true),
            (FileClass::Image, &p.image_path, true, true),
            (FileClass::Video, &p.video_path, true, false),
        ] {
            if let Some(path) = non_empty(value) {
                let id = self.files.add(class, path);
                if archived {
                    info.archived_files += 1;
                }
                if cover {
                    info.cover_paths.push(id);
                }
            }
        }
        if platform == Some(Platform::Instagram) {
            info.cover_url_expiry = non_empty(&p.thumbnail_url).map(convert::cdn_url_expiry_ms);
        } else if non_empty(&p.thumbnail_url).is_some() {
            info.cover_url_expiry = Some(None);
        }

        // Sites.
        if platform == Some(Platform::Web) {
            self.web.sites += 1;
            let capture = capture_files(p.web_pages_json.as_deref(), p.web_meta_json.as_deref());
            self.web.pages += capture.pages as u64;
            self.web.pages_json_invalid += u64::from(capture.pages_json_invalid);
            self.web.meta_json_invalid += u64::from(capture.meta_json_invalid);
            info.has_capture = capture.pages > 0;
            if info.has_capture {
                self.web.captured += 1;
            } else {
                self.web.placeholders += 1;
            }
            for asset in &capture.refs {
                self.files.add(FileClass::Web(asset.role), &asset.path);
                bump(&mut self.web.assets_by_role, asset.role.as_str());
            }
        }
        if non_empty(&p.ai_web_json).is_some() {
            info.derived_facets =
                derived_facets(p.ai_web_json.as_deref()).map(|rows| rows.into_iter().collect());
        }

        if p.id.is_empty() {
            // Rows with a NULL or empty id cannot be referenced by children.
            if info.key.is_some() {
                *self
                    .identity
                    .unmappable
                    .entry("the legacy id is empty".to_owned())
                    .or_default() += 1;
            }
            self.outcome("posts", "unmappable", 1);
            return;
        }
        self.posts.insert(p.id, info);
    }

    /// The canonical identity of a post and which identifier it came from.
    fn identify(
        &mut self,
        p: &PostRow,
        platform: Option<Platform>,
    ) -> (Option<CanonicalId>, &'static str) {
        let result: Result<(CanonicalId, &'static str), String> = match platform {
            Some(Platform::Instagram) => match ig::parse_legacy_id(&p.id, p.shortcode.as_deref()) {
                Ok(decoded) => {
                    if decoded.form != ig::LegacyIdForm::Shortcode {
                        let check = &mut self.identity.ig_shortcode_check;
                        check.checked += 1;
                        match p.shortcode.as_deref().filter(|s| !s.is_empty()) {
                            None => check.no_shortcode += 1,
                            Some(sc) => match ig::MediaPk::from_shortcode(sc) {
                                Ok(pk) if pk == decoded.pk => check.consistent += 1,
                                Ok(_) => check.inconsistent += 1,
                                Err(_) => check.undecodable_shortcode += 1,
                            },
                        }
                    }
                    Ok((decoded.pk.canonical(), decoded.form.as_str()))
                }
                Err(e) => Err(format!("instagram: {e}")),
            },
            Some(Platform::Twitter) => x::from_legacy(&p.id, p.post_url.as_deref())
                .map(|id| {
                    let source = if id.native_id() == p.id { "id" } else { "url" };
                    (id, source)
                })
                .map_err(|e| format!("twitter: {e}")),
            Some(Platform::Pinterest) => pinterest::from_legacy(&p.id, p.post_url.as_deref())
                .map(|id| {
                    let source = match (
                        id.native_id() == p.id,
                        pinterest::is_numeric(id.native_id()),
                    ) {
                        (true, true) => "id",
                        (true, false) => "id_non_numeric",
                        (false, _) => "url",
                    };
                    (id, source)
                })
                .map_err(|e| format!("pinterest: {e}")),
            Some(Platform::Web) => {
                let urls = [
                    ("web_url", &p.web_url),
                    ("web_final_url", &p.web_final_url),
                    ("post_url", &p.post_url),
                ];
                let reproduced = urls
                    .iter()
                    .find(|(_, url)| non_empty(url).is_some_and(|u| web::legacy_post_id(u) == p.id))
                    .map_or("not_reproduced", |(name, _)| *name);
                bump(&mut self.identity.web_legacy_id_check, reproduced);
                match urls
                    .iter()
                    .find_map(|(name, url)| non_empty(url).map(|u| (*name, u)))
                {
                    Some((name, url)) => web::from_url(url)
                        .map(|id| (id, name))
                        .map_err(|e| format!("web: {e}")),
                    None => Err("web: the site has no URL".to_owned()),
                }
            }
            Some(Platform::Manual) => {
                let imported_ms = convert::epoch_to_ms(p.imported_at).unwrap_or(0);
                manual::from_legacy(&p.id, imported_ms)
                    .map(|id| (id, "new_ulid"))
                    .map_err(|e| format!("manual: {e}"))
            }
            None => Err(
                "the platform is not one of instagram, twitter, pinterest, web, manual".to_owned(),
            ),
        };
        let platform_name = platform.map_or("unknown", Platform::as_str);
        let entry = self
            .identity
            .by_platform
            .entry(platform_name.to_owned())
            .or_default();
        entry.rows += 1;
        match result {
            Ok((id, source)) => {
                bump(&mut entry.sources, source);
                (Some(id), source)
            }
            Err(reason) => {
                bump(&mut entry.sources, "unmappable");
                *self.identity.unmappable.entry(reason).or_default() += 1;
                (None, "unmappable")
            }
        }
    }

    // ── post_media ───────────────────────────────────────────────────────

    fn scan_post_media(&mut self) -> Result<(), LegacyError> {
        let db = self.db;
        db.stream(|m: PostMediaRow| {
            let Some(info) = self.posts.get_mut(&m.post_id) else {
                self.orphan_slides += 1;
                return Ok::<_, LegacyError>(());
            };
            info.slides += 1;
            let kind = if info.platform == Some(Platform::Web) {
                "page"
            } else {
                match m.media_type.as_str() {
                    "image" | "video" | "file" => m.media_type.as_str(),
                    _ => "unknown",
                }
            };
            *self.post_media_kinds.entry(kind.to_owned()).or_default() += 1;
            if let Some(path) = non_empty(&m.local_path) {
                let class = match m.media_type.as_str() {
                    "video" => FileClass::SlideVideo,
                    "file" => FileClass::SlideFile,
                    _ => FileClass::SlideImage,
                };
                self.files.add(class, path);
                info.archived_files += 1;
            }
            if info.platform == Some(Platform::Manual)
                && let Some(original) =
                    non_empty(&m.source_url).filter(|s| convert::is_local_path(s))
            {
                self.files.add(FileClass::ManualOriginal, original);
            }
            Ok(())
        })?;
        Ok(())
    }

    // ── duplicates ───────────────────────────────────────────────────────

    fn deduplicate(&mut self) {
        let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for (id, info) in &self.posts {
            if let Some(key) = &info.key {
                groups.entry(key.clone()).or_default().push(id.clone());
            }
        }
        let mut distinct: BTreeMap<String, u64> = BTreeMap::new();
        for (key, mut ids) in groups {
            let platform = self.posts[&ids[0]]
                .platform
                .map_or("unknown", Platform::as_str);
            *distinct.entry(platform.to_owned()).or_default() += 1;
            if ids.len() < 2 {
                continue;
            }
            // The core's duplicate policy picks the kept row (§4.2); among
            // equals the first wins, so the rows go in tie-break order.
            ids.sort_by(|a, b| {
                let (pa, pb) = (&self.posts[a], &self.posts[b]);
                tie_break(pa).cmp(&tie_break(pb)).then_with(|| a.cmp(b))
            });
            let ranks: Vec<Layers> = ids.iter().map(|id| rank_layers(&self.posts[id])).collect();
            let kept = duplicates::survivor(&ranks).unwrap_or(0);
            ids[..=kept].rotate_right(1);
            let summary = match key.split('_').next() {
                Some("ig") => &mut self.duplicates.instagram,
                Some("web") => &mut self.duplicates.web,
                _ => &mut self.duplicates.other,
            };
            summary.groups += 1;
            summary.rows_in_groups += ids.len() as u64;
            summary.rows_merged += ids.len() as u64 - 1;
            if ids.iter().filter(|id| self.posts[*id].has_note).count() > 1 {
                summary.notes_to_concatenate += 1;
            }
            let mut members = Vec::new();
            for (i, id) in ids.iter().enumerate() {
                let info = self.posts.get_mut(id).expect("grouped ids exist");
                info.kept = i == 0;
                members.push(DuplicateMember {
                    legacy_id: (!self.opts.redact).then(|| id.clone()),
                    source: info.source.to_owned(),
                    archived_files: info.archived_files,
                    has_ai: info.has_ai,
                    has_user_layer: info.has_user_layer,
                    kept: i == 0,
                });
            }
            summary.listed.push(DuplicateGroup {
                key: (!self.opts.redact).then_some(key),
                members,
            });
        }
        for (platform, n) in distinct {
            self.identity
                .by_platform
                .entry(platform)
                .or_default()
                .distinct_keys = n;
        }

        // Row outcomes of posts and of their slides.
        let (mut insert, mut merge, mut unmappable) = (0, 0, 0);
        let (mut s_insert, mut s_merge, mut s_unmappable) = (0, 0, 0);
        let (mut mismatches, mut backfillable) = (0, 0);
        let mut without: BTreeMap<String, u64> = BTreeMap::new();
        let before_repair_v1 = self.db.schema().user_version < 1;
        for info in self.posts.values() {
            match (&info.key, info.kept) {
                (None, _) => {
                    unmappable += 1;
                    s_unmappable += info.slides;
                }
                (Some(_), true) => {
                    insert += 1;
                    s_insert += info.slides;
                }
                (Some(_), false) => {
                    merge += 1;
                    s_merge += info.slides;
                }
            }
            if info.slides == 0 {
                bump(&mut without, info.media_type.as_deref().unwrap_or("null"));
                if before_repair_v1 && info.has_legacy_media {
                    backfillable += 1;
                }
            } else if info.media_count.unwrap_or(1) != info.slides as i64 {
                mismatches += 1;
            }
        }
        self.outcome("posts", "insert", insert);
        self.outcome("posts", "merge", merge);
        self.outcome("posts", "unmappable", unmappable);
        self.outcome("post_media", "insert", s_insert);
        self.outcome("post_media", "merge", s_merge);
        self.outcome("post_media", "unmappable_parent", s_unmappable);
        self.outcome("post_media", "orphan", self.orphan_slides);
        self.posts_report.media_count_mismatches = mismatches;
        self.posts_report.without_slides = without;
        self.posts_report.without_slides_backfillable = backfillable;
    }

    /// The new key of a legacy post id: `Err(outcome)` when the row has no
    /// target (`orphan` or `unmappable_parent`).
    fn parent_key(&self, legacy_id: &str) -> Result<&str, &'static str> {
        match self.posts.get(legacy_id) {
            None => Err("orphan"),
            Some(PostInfo { key: None, .. }) => Err("unmappable_parent"),
            Some(PostInfo { key: Some(k), .. }) => Ok(k),
        }
    }

    // ── collections ──────────────────────────────────────────────────────

    fn scan_collections(&mut self) -> Result<(), LegacyError> {
        let collections: Vec<CollectionRow> = self.db.read_all()?;
        let mut by_external: BTreeMap<(String, String), Vec<i64>> = BTreeMap::new();
        let mut id_map: HashMap<i64, i64> = HashMap::new();
        for c in &collections {
            id_map.insert(c.id, c.id);
            if let Some(external) = non_empty(&c.external_id) {
                by_external
                    .entry((c.platform.clone().unwrap_or_default(), external.to_owned()))
                    .or_default()
                    .push(c.id);
            }
        }
        let mut merged = 0;
        for ((platform, external), mut ids) in by_external {
            if ids.len() < 2 {
                continue;
            }
            ids.sort_unstable();
            let summary = &mut self.duplicates.collections;
            summary.groups += 1;
            summary.rows_in_groups += ids.len() as u64;
            summary.rows_merged += ids.len() as u64 - 1;
            merged += ids.len() as u64 - 1;
            for &id in &ids[1..] {
                id_map.insert(id, ids[0]);
            }
            summary.listed.push(DuplicateGroup {
                key: (!self.opts.redact).then(|| format!("{platform}:{external}")),
                members: ids
                    .iter()
                    .enumerate()
                    .map(|(i, id)| DuplicateMember {
                        legacy_id: (!self.opts.redact).then(|| id.to_string()),
                        source: "external_id".to_owned(),
                        archived_files: 0,
                        has_ai: false,
                        has_user_layer: false,
                        kept: i == 0,
                    })
                    .collect(),
            });
        }
        self.outcome("collections", "insert", collections.len() as u64 - merged);
        self.outcome("collections", "merge", merged);
        self.collection_map = id_map.clone();

        let mut seen: HashSet<(String, i64)> = HashSet::new();
        let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        let db = self.db;
        db.stream(|m: PostCollectionRow| {
            let outcome = match (self.parent_key(&m.post_id), id_map.get(&m.collection_id)) {
                (Err(outcome), _) => outcome,
                (Ok(_), None) => "orphan",
                (Ok(key), Some(&collection)) => {
                    if seen.insert((key.to_owned(), collection)) {
                        "insert"
                    } else {
                        "merge"
                    }
                }
            };
            *counts.entry(outcome).or_default() += 1;
            Ok::<_, LegacyError>(())
        })?;
        for (outcome, n) in counts {
            self.outcome("post_collections", outcome, n);
        }
        Ok(())
    }

    // ── tags, entities, facets, aliases, clusters ─────────────────────────

    fn scan_tags(&mut self) -> Result<(), LegacyError> {
        let db = self.db;

        let mut seen: HashSet<(String, String, &'static str)> = HashSet::new();
        let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        db.stream(|t: PostTagRow| {
            let (source, label) = match t.tier.as_deref() {
                Some("manual") => ("manual", "manual"),
                Some("general") => ("ai", "ai:general"),
                Some("specific") => ("ai", "ai:specific"),
                None => ("ai", "ai:untiered"),
                Some(_) => {
                    self.tags.unknown_tiers += 1;
                    ("ai", "ai:untiered")
                }
            };
            bump(&mut self.tags.post_tags, label);
            let outcome = match self.parent_key(&t.post_id) {
                Err(outcome) => outcome,
                Ok(key) => {
                    if seen.insert((key.to_owned(), t.tag_norm.clone(), source)) {
                        "insert"
                    } else {
                        "merge"
                    }
                }
            };
            *counts.entry(outcome).or_default() += 1;
            Ok::<_, LegacyError>(())
        })?;
        for (outcome, n) in std::mem::take(&mut counts) {
            self.outcome("post_tags", outcome, n);
        }

        let mut seen: HashSet<(String, String)> = HashSet::new();
        db.stream(|e: PostEntityRow| {
            let outcome = match self.parent_key(&e.post_id) {
                Err(outcome) => outcome,
                Ok(key) => {
                    if seen.insert((key.to_owned(), e.ent_norm.clone())) {
                        "insert"
                    } else {
                        "merge"
                    }
                }
            };
            *counts.entry(outcome).or_default() += 1;
            Ok::<_, LegacyError>(())
        })?;
        for (outcome, n) in std::mem::take(&mut counts) {
            self.outcome("post_entities", outcome, n);
        }

        // post_facets: dropped, rebuilt from ai_web_json. Check the rebuild.
        let mut file_facets: BTreeMap<String, BTreeSet<(String, String)>> = BTreeMap::new();
        db.stream(|f: PostFacetRow| {
            let outcome = match self.posts.get(&f.post_id) {
                None => "orphan",
                Some(info) => {
                    let derivable = info
                        .derived_facets
                        .as_ref()
                        .is_some_and(|d| d.contains(&(f.facet.clone(), f.value.clone())));
                    if derivable {
                        "rebuilt"
                    } else {
                        "not_derivable"
                    }
                }
            };
            *counts.entry(outcome).or_default() += 1;
            file_facets
                .entry(f.post_id)
                .or_default()
                .insert((f.facet, f.value));
            Ok::<_, LegacyError>(())
        })?;
        let check = &mut self.web.facets;
        check.rows = counts.values().sum();
        check.posts = file_facets.len() as u64;
        check.derivable_rows = counts.get("rebuilt").copied().unwrap_or(0);
        check.not_derivable_rows = counts.get("not_derivable").copied().unwrap_or(0);
        check.extra_rows = self
            .posts
            .iter()
            .filter_map(|(id, info)| {
                let derived = info.derived_facets.as_ref()?;
                let existing = file_facets.get(id);
                Some(
                    derived
                        .iter()
                        .filter(|row| existing.is_none_or(|e| !e.contains(*row)))
                        .count() as u64,
                )
            })
            .sum();
        for (outcome, n) in std::mem::take(&mut counts) {
            self.outcome("post_facets", outcome, n);
        }

        let aliases: Vec<TagAliasRow> = db.read_all()?;
        for a in &aliases {
            bump(&mut self.tags.alias_status, &a.status);
        }
        self.outcome("tag_alias", "insert", aliases.len() as u64);

        let clusters: Vec<TagClusterRow> = db.read_all()?;
        let cluster_ids: HashSet<i64> = clusters.iter().map(|c| c.id).collect();
        self.tags.clusters = clusters.len() as u64;
        self.outcome("tag_cluster", "insert", clusters.len() as u64);
        let memberships: Vec<TagClusterMembershipRow> = db.read_all()?;
        self.tags.cluster_memberships = memberships.len() as u64;
        let orphans = memberships
            .iter()
            .filter(|m| !cluster_ids.contains(&m.cluster_id))
            .count() as u64;
        self.outcome(
            "tag_cluster_membership",
            "insert",
            memberships.len() as u64 - orphans,
        );
        self.outcome("tag_cluster_membership", "orphan", orphans);
        Ok(())
    }

    // ── site versions ────────────────────────────────────────────────────

    fn scan_web_snapshots(&mut self) -> Result<(), LegacyError> {
        let db = self.db;
        let mut counts: BTreeMap<&'static str, u64> = BTreeMap::new();
        db.stream(|s: WebSnapshotRow| {
            let outcome = match self.parent_key(&s.post_id) {
                Err(outcome) => outcome,
                Ok(_) => "insert",
            };
            *counts.entry(outcome).or_default() += 1;
            if outcome == "insert" {
                let capture =
                    capture_files(s.web_pages_json.as_deref(), s.web_meta_json.as_deref());
                self.web.pages_json_invalid += u64::from(capture.pages_json_invalid);
                self.web.meta_json_invalid += u64::from(capture.meta_json_invalid);
                for asset in &capture.refs {
                    self.files.add(FileClass::Web(asset.role), &asset.path);
                    bump(&mut self.web.assets_by_role, asset.role.as_str());
                    self.web.snapshot_asset_refs += 1;
                }
            }
            Ok::<_, LegacyError>(())
        })?;
        self.web.snapshots = counts.get("insert").copied().unwrap_or(0);
        self.web.web_captures = self.web.captured + self.web.snapshots;
        for (outcome, n) in counts {
            self.outcome("web_snapshots", outcome, n);
        }
        Ok(())
    }

    // ── dropped tables ───────────────────────────────────────────────────

    fn scan_dropped_tables(&mut self) -> Result<(), LegacyError> {
        let db = self.db;
        let mut jobs = 0;
        let mut unfinished = 0;
        db.stream(|j: JobRow| {
            jobs += 1;
            if !matches!(j.status.as_str(), "done" | "cancelled" | "error") {
                unfinished += 1;
            }
            Ok::<_, LegacyError>(())
        })?;
        self.outcome("jobs", "dropped", jobs);
        if unfinished > 0 {
            self.warnings.push(format!(
                "{unfinished} unfinished desktop jobs are dropped: the web derives pending work from per-item state"
            ));
        }
        let downloads = db.read_all::<DownloadRow>()?.len() as u64;
        self.outcome("downloads", "dropped", downloads);
        Ok(())
    }

    // ── files ────────────────────────────────────────────────────────────

    fn check_files(&mut self) {
        let Some(root) = self.opts.media_root.clone() else {
            let (classes, totals) = self.files.counts();
            self.files_report.classes = classes;
            self.files_report.totals = totals;
            return;
        };
        let referenced = self.files.check(&root);
        self.files_report.checked = true;
        self.files_report.media_root = Some(if self.opts.redact {
            "(redacted)".to_owned()
        } else {
            root.display().to_string()
        });
        self.files_report.legacy_root_detected = self.files.legacy_root_detected();
        let (classes, totals) = self.files.counts();
        self.files_report.classes = classes;
        self.files_report.totals = totals;
        self.files_report.upload = self.files.upload_estimate();
        self.files_report.orphans = scan_orphans(&root, &referenced);

        let covers = &mut self.files_report.covers;
        for info in self.posts.values().filter(|i| i.key.is_some() && i.kept) {
            let has_cover = info
                .cover_paths
                .iter()
                .any(|&id| matches!(self.files.state(id), FileState::Present { .. }));
            if has_cover {
                covers.with_local_cover += 1;
                continue;
            }
            let platform = info.platform.map_or("unknown", Platform::as_str);
            *covers
                .without_local_cover
                .entry(platform.to_owned())
                .or_default() += 1;
            if info.platform == Some(Platform::Instagram) {
                let state = match info.cover_url_expiry {
                    None => "no_url",
                    Some(None) => "no_expiry",
                    Some(Some(expiry)) if expiry <= self.opts.now_ms => "expired",
                    Some(Some(_)) => "valid",
                };
                bump(&mut covers.ig_without_cover_url, state);
            }
        }
    }

    // ── report ───────────────────────────────────────────────────────────

    fn finish(mut self) -> Result<(PlanReport, PlanMapping), LegacyError> {
        let schema = self.db.schema();
        let coverage = schema.coverage();

        // Tables: catalog order, then the file's other tables.
        let mut tables = Vec::new();
        for t in &coverage.tables {
            let rows = self.db.row_count(&t.table)?;
            let mut outcomes = self.outcomes.remove(&t.table).unwrap_or_default();
            match t.status {
                TableStatus::SqliteInternal if rows > 0 => {
                    outcomes.insert("dropped".to_owned(), rows);
                }
                TableStatus::Unmapped if rows > 0 => {
                    outcomes.insert("unmapped".to_owned(), rows);
                }
                _ => {}
            }
            let accounted = outcomes.values().sum::<u64>() == rows;
            let (target, dropped_reason) = match t.disposition {
                Some(Disposition::Mapped { target, .. }) => (Some(target.to_owned()), None),
                Some(Disposition::Dropped { reason }) => (None, Some(reason.to_owned())),
                None => (None, None),
            };
            tables.push(TableReport {
                table: t.table.clone(),
                status: t.status,
                rows,
                target,
                dropped_reason,
                outcomes,
                accounted,
            });
        }

        // Column coverage.
        let mut cov = CoverageReport {
            columns: coverage.columns.len(),
            present: coverage.count(ColumnStatus::Present),
            ..CoverageReport::default()
        };
        for c in &coverage.columns {
            let name = format!("{}.{}", c.table, c.column);
            match c.status {
                ColumnStatus::Present => match c.disposition {
                    Some(Disposition::Mapped { .. }) => cov.mapped += 1,
                    Some(Disposition::Dropped { .. }) => cov.dropped += 1,
                    None => {}
                },
                ColumnStatus::AbsentOptional => cov.absent_optional.push(name),
                ColumnStatus::AbsentRequired => {}
                ColumnStatus::Unmapped => cov.unmapped.push(name),
            }
        }
        for t in coverage.unmapped_tables() {
            cov.unmapped.push(format!("{} (table)", t.table));
        }
        cov.missing_required = coverage.missing_required();
        cov.unexpected_objects = coverage
            .other_objects
            .iter()
            .map(|o| format!("{} {}", o.kind, o.name))
            .collect();
        for spec in catalog::TABLES {
            let classes = self.db.storage_classes(spec.name)?;
            for c in spec.columns {
                let Some(found) = classes.get(c.name) else {
                    continue;
                };
                let expected: &[&str] = match c.value_type {
                    ValueType::Text => &["null", "text"],
                    ValueType::Integer => &["null", "integer"],
                    ValueType::Real => &["null", "real", "integer"],
                };
                if found.keys().any(|k| !expected.contains(&k.as_str())) {
                    cov.type_anomalies.push(TypeAnomaly {
                        column: format!("{}.{}", spec.name, c.name),
                        expected: expected[1].to_owned(),
                        found: found.clone(),
                    });
                }
            }
        }

        // Errors and warnings.
        let mut errors = std::mem::take(&mut self.errors);
        let mut warnings = std::mem::take(&mut self.warnings);
        for name in &cov.unmapped {
            errors.push(format!("unmapped column or table: {name}"));
        }
        for name in &cov.missing_required {
            errors.push(format!("missing required table or column: {name}"));
        }
        for (reason, n) in &self.identity.unmappable {
            errors.push(format!("{n} posts have no canonical key ({reason})"));
        }
        for t in &tables {
            if !t.accounted {
                errors.push(format!(
                    "{}: {} rows but {} outcomes",
                    t.table,
                    t.rows,
                    t.outcomes.values().sum::<u64>()
                ));
            }
            if let Some(n) = t.outcomes.get("orphan") {
                warnings.push(format!(
                    "{}: {n} rows reference a missing parent and are dropped",
                    t.table
                ));
            }
        }
        if self.web.facets.not_derivable_rows > 0 {
            errors.push(format!(
                "{} post_facets rows cannot be rebuilt from ai_web_json",
                self.web.facets.not_derivable_rows
            ));
        }
        if self.db.open_mode() == OpenMode::SharedReadOnly {
            warnings.push(
                "a -wal file is present: the desktop app may be running (or a reader left it); the plan reads one consistent snapshot, but close Shelfy before `run`"
                    .to_owned(),
            );
        }
        let user_version = schema.user_version;
        let mut repairs_pending = Vec::new();
        if user_version < 1 {
            repairs_pending.push(
                "v1: X `x.com//status/` URLs, post_media backfill from the post columns, post_tags/post_entities backfill"
                    .to_owned(),
            );
        }
        if user_version < 2 {
            repairs_pending.push("v2: Instagram dates from shortcodes".to_owned());
        }
        if user_version < 3 {
            repairs_pending.push("v3: undated posts fall back to imported_at".to_owned());
        }
        if !repairs_pending.is_empty() {
            warnings.push(format!(
                "user_version {user_version}: {} desktop data repairs pending; the migration applies them",
                repairs_pending.len()
            ));
        }
        for anomaly in &cov.type_anomalies {
            warnings.push(format!(
                "{} holds values of another type than {} (read leniently)",
                anomaly.column, anomaly.expected
            ));
        }
        if !cov.unexpected_objects.is_empty() {
            warnings.push(format!(
                "{} views or triggers the desktop does not create",
                cov.unexpected_objects.len()
            ));
        }
        let ts = &self.posts_report.posted_at;
        if ts.invalid > 0 {
            warnings.push(format!(
                "{} posts have an unparseable timestamp: posted_at NULL",
                ts.invalid
            ));
        }
        let check = &self.identity.ig_shortcode_check;
        if check.inconsistent + check.undecodable_shortcode > 0 {
            warnings.push(format!(
                "{} Instagram rows have a shortcode that does not decode to their pk (the id wins)",
                check.inconsistent + check.undecodable_shortcode
            ));
        }
        if let Some(n) = self.identity.web_legacy_id_check.get("not_reproduced") {
            warnings.push(format!(
                "{n} web ids are not reproduced from a stored URL (the new key is recomputed anyway)"
            ));
        }
        if self.tags.unknown_tiers > 0 {
            warnings.push(format!(
                "{} post_tags rows have an unknown tier: mapped as untiered AI tags",
                self.tags.unknown_tiers
            ));
        }
        if self.web.pages_json_invalid + self.web.meta_json_invalid > 0 {
            warnings.push(format!(
                "{} web capture JSON values are invalid",
                self.web.pages_json_invalid + self.web.meta_json_invalid
            ));
        }
        let invalid_arrays: u64 = self
            .posts_report
            .json_arrays
            .values()
            .filter_map(|m| m.get("invalid"))
            .sum();
        if invalid_arrays > 0 {
            warnings.push(format!(
                "{invalid_arrays} JSON array values are invalid: read as empty, as on the desktop"
            ));
        }
        let totals = &self.files_report.totals;
        if self.files_report.checked {
            if totals.missing > 0 {
                warnings.push(format!(
                    "{} referenced files are missing: their posts stay pending for re-archive",
                    totals.missing
                ));
            }
            if totals.outside_root > 0 {
                warnings.push(format!(
                    "{} referenced paths are outside the desktop root and were not checked",
                    totals.outside_root
                ));
            }
        } else {
            warnings.push("no media root: files were not checked".to_owned());
        }

        // Verdict.
        let blocking = ["unmapped", "unmappable", "unmappable_parent"];
        let every_row_accounted = tables
            .iter()
            .all(|t| t.accounted && blocking.iter().all(|o| !t.outcomes.contains_key(*o)));
        let dups = &self.duplicates;
        let duplicate_groups_listed = [&dups.instagram, &dups.web, &dups.other, &dups.collections]
            .iter()
            .all(|d| d.listed.len() as u64 == d.groups);
        let no_unmapped_column = cov.unmapped.is_empty() && cov.missing_required.is_empty();
        let no_errors = errors.is_empty();
        let verdict = Verdict {
            every_row_accounted,
            duplicate_groups_listed,
            no_unmapped_column,
            no_errors,
            pass: every_row_accounted && duplicate_groups_listed && no_unmapped_column && no_errors,
        };

        let path = self.db.path();
        let source = SourceReport {
            file_name: path
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            bytes: std::fs::metadata(path).map(|m| m.len()).unwrap_or(0),
            user_version,
            journal_mode: schema.journal_mode.clone(),
            open_mode: self.db.open_mode(),
            repairs_pending,
        };
        let mut posts = self.posts_report;
        posts.slides_by_kind = self.post_media_kinds;
        let report = PlanReport {
            tool: format!("shelfy-migrate {}", env!("CARGO_PKG_VERSION")),
            dry_run: true,
            redacted: self.opts.redact,
            source,
            tables,
            coverage: cov,
            posts,
            identity: self.identity,
            duplicates: self.duplicates,
            tags: self.tags,
            web: self.web,
            files: self.files_report,
            settings: self.opts.media_root.as_deref().map(crate::settings::read),
            desktop_open: None,
            server: None,
            errors,
            warnings,
            verdict,
        };
        let mapping = PlanMapping {
            posts: self
                .posts
                .into_iter()
                .filter_map(|(legacy_id, info)| {
                    let mapping = PostMapping {
                        platform: info.platform?,
                        key: info.key?,
                        native_id: info.native_id?,
                        kept: info.kept,
                    };
                    Some((legacy_id, mapping))
                })
                .collect(),
            collections: self.collection_map,
            files: self.files,
        };
        Ok((report, mapping))
    }
}

/// What the core's duplicate policy ranks a member by (plan §4.2: archived
/// files, a site capture counting as one, then AI, then the user layer).
fn rank_layers(info: &PostInfo) -> Layers {
    Layers {
        archived_files: info.archived_files + u64::from(info.has_capture),
        ai: info.has_ai,
        user: info.has_user_layer,
    }
}

/// The order of equally ranked members: the richest id form, then the
/// oldest import (S1-3).
fn tie_break(info: &PostInfo) -> impl Ord {
    let form = match info.source {
        "composite" => 0,
        "pk" => 1,
        "shortcode" => 2,
        _ => 0,
    };
    (form, info.imported_at.unwrap_or(i64::MAX))
}

fn bump(map: &mut BTreeMap<String, u64>, key: &str) {
    *map.entry(key.to_owned()).or_default() += 1;
}

fn non_empty(value: &Option<String>) -> Option<&str> {
    value.as_deref().filter(|s| !s.trim().is_empty())
}

fn epoch_name(class: EpochClass) -> &'static str {
    match class {
        EpochClass::Null => "null",
        EpochClass::Seconds => "seconds",
        EpochClass::Milliseconds => "milliseconds",
        EpochClass::Implausible => "implausible",
    }
}

fn json_class_name(class: JsonArrayClass) -> &'static str {
    match class {
        JsonArrayClass::Null => "null",
        JsonArrayClass::Empty => "empty",
        JsonArrayClass::Strings => "strings",
        JsonArrayClass::Mixed => "mixed",
        JsonArrayClass::Invalid => "invalid",
    }
}
