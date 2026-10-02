//! What a bundle holds, counted: the "in" side of the reconciliation report
//! (plan §4.3).
//!
//! The summary travels inside the bundle (`meta` row
//! [`SUMMARY_META_KEY`]); the server stores it with its install report, next
//! to what it counted in the installed library, and `run` prints both. It
//! holds counts only: no keys, captions, URLs or paths.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// `meta.key` of the summary in the bundle's database.
pub const SUMMARY_META_KEY: &str = "migration.summary";

/// Counts of a bundle, as built by `shelfy-migrate run`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleSummary {
    /// `shelfy-migrate <version>`.
    pub tool: String,
    /// `PRAGMA user_version` of the desktop library.
    pub desktop_user_version: i64,
    /// Whether kept videos were included (`--with-videos`).
    pub with_videos: bool,
    /// Whether the library was read from a snapshot (it had a `-wal` file).
    pub snapshot: bool,
    /// When the bundle was built, unix ms.
    pub built_at: i64,
    pub posts: PostCounts,
    pub rows: RowCounts,
    pub files: FileCounts,
    pub objects: ObjectCounts,
    pub covers: CoverCounts,
    pub repairs: RepairCounts,
}

/// Posts in and out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PostCounts {
    /// Desktop rows by platform.
    pub read: BTreeMap<String, u64>,
    /// Posts in the bundle by platform (after merging duplicates).
    pub written: BTreeMap<String, u64>,
    /// Desktop rows folded into another row of their duplicate group.
    pub merged: u64,
}

/// Rows written to the other tables of the bundle.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RowCounts {
    pub slides: u64,
    pub collections: u64,
    /// Desktop collections folded into another (same platform folder).
    pub collections_merged: u64,
    pub memberships: u64,
    pub post_tags: u64,
    pub post_entities: u64,
    pub tag_aliases: u64,
    pub tag_clusters: u64,
    pub tag_cluster_memberships: u64,
    pub web_captures: u64,
    pub web_capture_assets: u64,
}

/// The files the desktop rows reference, by distinct path.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileCounts {
    /// Distinct referenced paths.
    pub referenced: u64,
    pub present: u64,
    pub missing: u64,
    /// Missing paths by reference class (`video`, `cover`, …).
    pub missing_by_class: BTreeMap<String, u64>,
    /// Paths outside the detected desktop root (not looked up).
    pub outside_root: u64,
    /// Present files referenced only as videos, left out without
    /// `--with-videos`.
    pub videos_excluded: u64,
    pub videos_excluded_bytes: u64,
    /// Present files whose type is not in the store's allowlist.
    pub unsupported: u64,
    /// Present files that could not be read.
    pub unreadable: u64,
    /// Files hashed into the bundle.
    pub hashed: u64,
    pub hashed_bytes: u64,
}

/// The distinct objects of the bundle (`media_objects` rows).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectCounts {
    pub count: u64,
    pub bytes: u64,
    /// Objects by `media_objects.role`.
    pub by_role: BTreeMap<String, u64>,
}

/// Covers of the bundle's posts: stored, or what archiving them needs
/// (OI-6, OI-7). The archive workers arrive with P1-19 and P2.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CoverCounts {
    /// Posts whose cover is a bundle object.
    pub stored: u64,
    /// Instagram covers to archive whose signed URL is still valid.
    pub ig_valid: u64,
    /// Instagram covers whose signed URL has expired: extension
    /// `refresh_media` tasks.
    pub ig_expired: u64,
    /// Instagram covers whose URL has no expiry.
    pub ig_no_expiry: u64,
    /// X covers to archive (they do not expire).
    pub x: u64,
    /// Pinterest covers to archive.
    pub pinterest: u64,
    /// Other platforms' covers to archive.
    pub other: u64,
    /// Posts without a cover and without a cover URL.
    pub none: u64,
}

/// Values the migration repaired or could not carry, counted.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepairCounts {
    /// X `x.com//status/` URLs rewritten (desktop repair v1).
    pub x_status_urls: u64,
    /// Instagram dates taken from the shortcode (desktop repair v2).
    pub ig_dates_from_shortcode: u64,
    /// Posts without a usable `imported_at` (the bundle time is used).
    pub imported_at_missing: u64,
    /// Captions cut at 20,000 characters.
    pub captions_truncated: u64,
    /// Slides of an unknown type, stored as images.
    pub unknown_slide_kinds: u64,
    /// Video slides whose kept video is missing on disk (OI-6): "video not
    /// kept", not an error.
    pub videos_missing: u64,
    /// `ai_status = 'analyzing'` reset (desktop DATA-47).
    pub ai_stuck_reset: u64,
    /// `ai_web_json` values that are not JSON, dropped.
    pub ai_web_json_invalid: u64,
    /// Collections whose name was blank or whose color was invalid.
    pub collections_fixed: u64,
    /// Rows referencing a missing parent, dropped.
    pub orphan_rows: u64,
}
