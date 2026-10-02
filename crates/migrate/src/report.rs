//! The dry-run report of `shelfy-migrate plan`, serialized as-is by `--json`.
//!
//! Counts only, except the duplicate-group listing, which names the keys and
//! legacy ids involved unless the report is redacted.

use std::collections::BTreeMap;

use serde::Serialize;
use shelfy_core::legacy::{OpenMode, TableStatus};

use crate::settings::DesktopSettings;

/// Everything `plan` found. Nothing is written anywhere to produce it.
#[derive(Debug, Clone, Serialize)]
pub struct PlanReport {
    pub tool: String,
    pub dry_run: bool,
    pub redacted: bool,
    pub source: SourceReport,
    pub tables: Vec<TableReport>,
    pub coverage: CoverageReport,
    pub posts: PostsReport,
    pub identity: IdentityReport,
    pub duplicates: DuplicatesReport,
    pub tags: TagsReport,
    pub web: WebReport,
    pub files: FilesReport,
    /// The desktop settings that move to the web (OI-10); `None` without a
    /// media root.
    pub settings: Option<DesktopSettings>,
    /// Whether another process (the desktop app) had the library open, when
    /// checked (`plan` refuses unless `--allow-open`).
    pub desktop_open: Option<bool>,
    /// The web library and its quota, when a server was given.
    pub server: Option<ServerCheck>,
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub verdict: Verdict,
}

/// The web library and the quota against the upload (plan §4.1 step 3).
#[derive(Debug, Clone, Default, Serialize)]
pub struct ServerCheck {
    /// The web library has no posts or collections: a run replaces it;
    /// otherwise a run needs `--merge`.
    pub library_empty: bool,
    pub posts: u64,
    /// 0: unlimited.
    pub quota_bytes: i64,
    pub used_bytes: i64,
    /// What a run uploads at most: the default files, then the videos too.
    pub upload_bytes: u64,
    pub upload_bytes_with_videos: u64,
    /// Whether the use plus the upload stays within the quota.
    pub fits: bool,
    pub fits_with_videos: bool,
    /// An install already queued or running.
    pub active_job_id: Option<i64>,
}

impl ServerCheck {
    /// The check of a library uploading `upload_bytes` (or, with videos,
    /// `with_videos`) against `preflight`.
    #[must_use]
    pub fn new(preflight: &crate::client::Preflight, upload_bytes: u64, with_videos: u64) -> Self {
        let fits = |bytes: u64| {
            preflight.quota_bytes <= 0
                || preflight
                    .used_bytes
                    .saturating_add(i64::try_from(bytes).unwrap_or(i64::MAX))
                    <= preflight.quota_bytes
        };
        ServerCheck {
            library_empty: preflight.library_empty,
            posts: preflight.posts,
            quota_bytes: preflight.quota_bytes,
            used_bytes: preflight.used_bytes,
            upload_bytes,
            upload_bytes_with_videos: upload_bytes + with_videos,
            fits: fits(upload_bytes),
            fits_with_videos: fits(upload_bytes + with_videos),
            active_job_id: preflight.active_job_id,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceReport {
    pub file_name: String,
    pub bytes: u64,
    pub user_version: i64,
    pub journal_mode: String,
    pub open_mode: OpenMode,
    /// Desktop one-shot data repairs (`migrate()` user_version gates) this
    /// file has not run yet; the migration applies them while mapping.
    pub repairs_pending: Vec<String>,
}

/// One table of the file: its rows and what happens to each of them.
#[derive(Debug, Clone, Serialize)]
pub struct TableReport {
    pub table: String,
    pub status: TableStatus,
    /// `COUNT(*)`.
    pub rows: u64,
    /// The web schema target, for a mapped table.
    pub target: Option<String>,
    /// Why the table is dropped, for a dropped table.
    pub dropped_reason: Option<String>,
    /// Outcome → rows. The outcomes of a table add up to `rows`.
    pub outcomes: BTreeMap<String, u64>,
    /// The outcomes add up to `rows`.
    pub accounted: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CoverageReport {
    /// Columns of the catalog plus any unknown column of the file.
    pub columns: usize,
    pub present: usize,
    pub mapped: usize,
    pub dropped: usize,
    /// Columns a later desktop migration added and this file predates.
    pub absent_optional: Vec<String>,
    pub missing_required: Vec<String>,
    /// Columns (and tables) of the file the catalog does not know: errors.
    pub unmapped: Vec<String>,
    /// Views and triggers (the desktop creates none).
    pub unexpected_objects: Vec<String>,
    /// Columns holding values of another storage class than declared.
    pub type_anomalies: Vec<TypeAnomaly>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TypeAnomaly {
    pub column: String,
    pub expected: String,
    pub found: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PostsReport {
    pub by_platform: BTreeMap<String, u64>,
    pub by_media_type: BTreeMap<String, u64>,
    /// `posts.timestamp` → `posted_at`.
    pub posted_at: TimestampCounts,
    /// `posts.imported_at` (seconds) → ms.
    pub imported_at: BTreeMap<String, u64>,
    pub ai: AiCounts,
    pub user_notes: u64,
    pub user_tags: u64,
    /// JSON-array columns by class (`null`, `empty`, `strings`, `mixed`, `invalid`).
    pub json_arrays: BTreeMap<String, BTreeMap<String, u64>>,
    /// `thumb_blur`: dropped, recomputed as ThumbHash.
    pub thumb_blur: BTreeMap<String, u64>,
    /// X `x.com//status/` URLs the desktop repair (migrate v1) rewrites.
    pub x_status_urls_to_repair: u64,
    /// Slides by target `post_media.kind` (slides of sites become `page`).
    pub slides_by_kind: BTreeMap<String, u64>,
    /// Posts whose `media_count` differs from their number of slides.
    pub media_count_mismatches: u64,
    /// Posts without any `post_media` row, by `media_type`.
    pub without_slides: BTreeMap<String, u64>,
    /// In a file before desktop repair v1: posts without slides that have a
    /// cover URL or a local file, which the repair turns into slide 0.
    pub without_slides_backfillable: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TimestampCounts {
    pub valid: u64,
    pub empty: u64,
    pub null: u64,
    pub invalid: u64,
    /// Undated Instagram posts whose date the shortcode yields (desktop
    /// repair v2/v3).
    pub undated_ig_datable_from_shortcode: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AiCounts {
    /// `ai_status` value (`null` for NULL) → posts.
    pub status: BTreeMap<String, u64>,
    /// Posts with any AI field set: they get `ai_provider = 'desktop-local'`.
    pub with_ai_fields: u64,
    /// `ai_status = 'analyzing'`: reset to NULL.
    pub stuck_analyzing: u64,
    pub with_ai_web_json: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct IdentityReport {
    pub by_platform: BTreeMap<String, PlatformIdentity>,
    /// Reason → posts without a canonical key: errors.
    pub unmappable: BTreeMap<String, u64>,
    /// Instagram rows keyed by pk (composite or bare): does `shortcode`
    /// decode to the same pk?
    pub ig_shortcode_check: ShortcodeCheck,
    /// Instagram rows whose shortcode is longer than a public post's 11–12
    /// characters (a private account's): their key is the decoded shortcode,
    /// where the extension keeps an `igsc_` alias (OI-9).
    pub ig_long_shortcodes: u64,
    /// Web rows: is the legacy `web:<sha1>` id reproduced by the desktop
    /// normalization of a stored URL? Validates the port of `normalizeWebUrl`.
    pub web_legacy_id_check: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PlatformIdentity {
    pub rows: u64,
    pub distinct_keys: u64,
    /// Which identifier the key came from (`composite`, `pk`, `shortcode`,
    /// `id`, `url`, `web_url`, …) → rows.
    pub sources: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ShortcodeCheck {
    pub checked: u64,
    pub consistent: u64,
    pub inconsistent: u64,
    pub no_shortcode: u64,
    pub undecodable_shortcode: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DuplicatesReport {
    /// Instagram rows that decode to the same pk.
    pub instagram: DuplicateSummary,
    /// Web rows whose scheme-less URL is the same (http/https twins).
    pub web: DuplicateSummary,
    /// Any other key shared by several rows.
    pub other: DuplicateSummary,
    /// Collections sharing `(platform, external_id)`.
    pub collections: DuplicateSummary,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct DuplicateSummary {
    pub groups: u64,
    pub rows_in_groups: u64,
    /// Rows folded into the kept row of their group.
    pub rows_merged: u64,
    /// Groups where more than one row has a note (they are concatenated).
    pub notes_to_concatenate: u64,
    pub listed: Vec<DuplicateGroup>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateGroup {
    /// The canonical key; `None` when redacted.
    pub key: Option<String>,
    pub members: Vec<DuplicateMember>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DuplicateMember {
    /// The legacy id; `None` when redacted.
    pub legacy_id: Option<String>,
    pub source: String,
    pub archived_files: u64,
    pub has_ai: bool,
    pub has_user_layer: bool,
    pub kept: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct TagsReport {
    /// `post_tags` rows by target `source:tier`.
    pub post_tags: BTreeMap<String, u64>,
    /// Tier values the catalog does not know (mapped as untiered AI tags).
    pub unknown_tiers: u64,
    /// Manual tags that are also AI tags of the same post: the desktop index
    /// (PK post, tag) kept one row; the web schema keeps both.
    pub manual_ai_collisions: u64,
    pub alias_status: BTreeMap<String, u64>,
    pub clusters: u64,
    pub cluster_memberships: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WebReport {
    pub sites: u64,
    /// Sites with a current capture → one `web_captures` row each.
    pub captured: u64,
    /// Sites without a capture (placeholders): no `web_captures` row.
    pub placeholders: u64,
    /// `web_snapshots` rows → older `web_captures` rows.
    pub snapshots: u64,
    /// Total `web_captures` rows after the migration.
    pub web_captures: u64,
    pub pages: u64,
    pub pages_json_invalid: u64,
    pub meta_json_invalid: u64,
    /// File references of every version, by `media_objects` role.
    pub assets_by_role: BTreeMap<String, u64>,
    /// … of which come from older versions (`web_snapshots`).
    pub snapshot_asset_refs: u64,
    pub facets: FacetCheck,
}

/// `post_facets` is dropped and rebuilt from `posts.ai_web_json`; this checks
/// that the rebuild reproduces every row.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FacetCheck {
    pub rows: u64,
    pub posts: u64,
    /// Rows the rebuild reproduces.
    pub derivable_rows: u64,
    /// Rows the rebuild would not produce: they would be lost.
    pub not_derivable_rows: u64,
    /// Rows the rebuild adds that the file lacks.
    pub extra_rows: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FilesReport {
    /// Whether the files were looked up under a media root.
    pub checked: bool,
    pub media_root: Option<String>,
    /// A common desktop userData prefix was found in the stored paths.
    pub legacy_root_detected: bool,
    /// Reference class → counts.
    pub classes: BTreeMap<String, FileClassCounts>,
    pub totals: FileClassCounts,
    pub upload: UploadEstimate,
    pub covers: CoverCounts,
    pub orphans: OrphanCounts,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct FileClassCounts {
    /// References (rows × columns pointing to a file).
    pub refs: u64,
    /// Distinct paths.
    pub files: u64,
    pub present: u64,
    pub missing: u64,
    /// Paths that are not under the detected desktop root (not checkable).
    pub outside_root: u64,
    pub bytes_present: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct UploadEstimate {
    /// Distinct present files uploaded by default.
    pub files_default: u64,
    pub bytes_default: u64,
    /// Distinct present files referenced only as videos (`--with-videos`).
    pub files_videos: u64,
    pub bytes_videos: u64,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CoverCounts {
    /// Posts with at least one present local cover (thumbnail, preview,
    /// image or screenshot).
    pub with_local_cover: u64,
    /// Platform → posts without one: they stay pending for re-archive.
    pub without_local_cover: BTreeMap<String, u64>,
    /// Instagram posts without a local cover, by the state of their CDN
    /// cover URL: `expired` (its `oe` signature is past), `valid`,
    /// `no_expiry` (no `oe` parameter) or `no_url`.
    pub ig_without_cover_url: BTreeMap<String, u64>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OrphanCounts {
    /// Files under `<media root>/assets` no row references.
    pub files: u64,
    pub bytes: u64,
    /// `assets/<dir>` → (files, bytes).
    pub by_dir: BTreeMap<String, (u64, u64)>,
    /// OS metadata files (`.DS_Store`, …) and symlinks, not counted.
    pub ignored: u64,
}

/// The SPIKE-1 pass criteria.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Verdict {
    /// Every row of every table has exactly one outcome.
    pub every_row_accounted: bool,
    /// Every duplicate group is listed.
    pub duplicate_groups_listed: bool,
    /// No column or table of the file is unknown.
    pub no_unmapped_column: bool,
    /// No error at all (includes unmappable posts and lossy drops).
    pub no_errors: bool,
    pub pass: bool,
}
