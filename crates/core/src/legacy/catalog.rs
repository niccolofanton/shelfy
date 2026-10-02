//! The desktop schema, column by column, and where each column goes.
//!
//! Source of truth: `SCHEMA` and `migrate()` in `electron/db.ts`
//! (`SCHEMA_VERSION = 3`). Every table and column the desktop can create is
//! listed here with:
//!
//! - its [`Presence`]: in the base `CREATE TABLE`, or added later by an
//!   `ALTER TABLE … ADD COLUMN` (an older file may lack it; it then reads as
//!   the value the `ALTER` would have filled in);
//! - its [`Disposition`]: the target in the web schema (plan §2.7, §4.2) or
//!   the reason it is dropped.
//!
//! The reader selects columns in the order listed here, and the migration
//! plan reports any column of a file that this catalog does not know as
//! unmapped.

use serde::Serialize;

/// The SQLite value a column is expected to hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    Text,
    Integer,
    Real,
}

/// When a column exists in a desktop file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Presence {
    /// In the table's `CREATE TABLE` since the table exists.
    Base,
    /// Added by `migrate()` with `ALTER TABLE … ADD COLUMN`. In a file that
    /// predates it, the column reads as `absent` (an SQL literal: the
    /// `ALTER`'s default).
    Added { absent: &'static str },
}

/// What happens to a table or column in the migration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Disposition {
    /// Carried to `target` (web schema, plan §2.7) following `rule`.
    Mapped {
        target: &'static str,
        rule: &'static str,
    },
    /// Not carried, on purpose.
    Dropped { reason: &'static str },
}

impl Disposition {
    pub fn is_mapped(&self) -> bool {
        matches!(self, Disposition::Mapped { .. })
    }
}

/// One desktop column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ColumnSpec {
    pub name: &'static str,
    pub value_type: ValueType,
    pub presence: Presence,
    pub disposition: Disposition,
}

/// One desktop table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct TableSpec {
    pub name: &'static str,
    /// Only `posts` is required: any other table may be missing from an old
    /// or partial file, and then reads as empty.
    pub required: bool,
    pub disposition: Disposition,
    pub columns: &'static [ColumnSpec],
}

impl TableSpec {
    pub fn column(&self, name: &str) -> Option<&'static ColumnSpec> {
        self.columns.iter().find(|c| c.name == name)
    }
}

/// Every desktop table, in dependency order (parents first).
pub const TABLES: &[TableSpec] = &[
    POSTS,
    POST_MEDIA,
    COLLECTIONS,
    POST_COLLECTIONS,
    POST_TAGS,
    POST_ENTITIES,
    POST_FACETS,
    TAG_ALIAS,
    TAG_CLUSTER,
    TAG_CLUSTER_MEMBERSHIP,
    WEB_SNAPSHOTS,
    JOBS,
    DOWNLOADS,
];

/// The catalog entry of a desktop table.
pub fn table(name: &str) -> Option<&'static TableSpec> {
    TABLES.iter().find(|t| t.name == name)
}

/// SQLite's own tables (`sqlite_sequence`, `sqlite_stat1`, …): bookkeeping,
/// never user data. `sqlite_sequence` holds the AUTOINCREMENT counters, which
/// the web schema does not need (rows are renumbered on install).
pub fn is_sqlite_internal(table: &str) -> bool {
    table.starts_with("sqlite_")
}

/// The reason SQLite-internal tables are dropped.
pub const SQLITE_INTERNAL_REASON: &str =
    "SQLite bookkeeping (AUTOINCREMENT counters, statistics); the web schema renumbers rows";

const fn col(
    name: &'static str,
    value_type: ValueType,
    presence: Presence,
    disposition: Disposition,
) -> ColumnSpec {
    ColumnSpec {
        name,
        value_type,
        presence,
        disposition,
    }
}

const fn mapped(target: &'static str, rule: &'static str) -> Disposition {
    Disposition::Mapped { target, rule }
}

const fn dropped(reason: &'static str) -> Disposition {
    Disposition::Dropped { reason }
}

use Presence::Base;
use ValueType::{Integer, Real, Text};

/// Added by `ALTER TABLE … ADD COLUMN x TYPE` (no default): absent reads as NULL.
const ADDED: Presence = Presence::Added { absent: "NULL" };

const VERBATIM: &str = "verbatim";
const SECONDS_TO_MS: &str = "unix seconds → ms";
const VIA_POST_KEY: &str = "the parent post's new id (merged duplicates point to the kept post)";

// The tables below keep one line per column; rustfmt would spread each over
// several lines.

#[rustfmt::skip]
pub const POSTS: TableSpec = TableSpec {
    name: "posts",
    required: true,
    disposition: mapped("posts, web_captures, media_objects", "one post per canonical key; duplicates merge (§4.2); a captured site also yields its current web_captures row"),
    columns: &[
        col("id", Text, Base, mapped("posts.key, posts.native_id", "canonical identity (§2.8): IG `<pk>_<owner>`, pk or shortcode → `ig_<pk>`; tweet id → `x_<id>`; pin id → `pin_<id>`; `web:<sha1>` → `web_<sha1:20>` of the scheme-less URL; `manual:<uuid>` → `m_<ulid>` (legacy id kept in meta)")),
        col("platform", Text, Base, mapped("posts.platform", "verbatim (instagram, twitter, pinterest, web, manual)")),
        col("shortcode", Text, Base, mapped("posts.shortcode", "verbatim; '' → NULL")),
        col("post_url", Text, Base, mapped("posts.post_url", "verbatim; '' → NULL; X `x.com//status/` repaired as in desktop migrate v1")),
        col("profile_url", Text, Base, mapped("posts.profile_url", "verbatim; '' → NULL")),
        col("author_username", Text, Base, mapped("posts.author_username", "verbatim; '' → NULL")),
        col("author_name", Text, Base, mapped("posts.author_name", "verbatim; '' → NULL")),
        col("text", Text, Base, mapped("posts.caption", "verbatim (≤ 20 000 chars)")),
        col("thumbnail_url", Text, Base, mapped("posts.cover_url, posts.cover_url_expires_at", "verbatim; expiry from the IG `oe` parameter")),
        col("media_type", Text, Base, mapped("posts.media_type", "verbatim; NULL → derived from the slides")),
        col("timestamp", Text, Base, mapped("posts.posted_at, posts.sort_ts", "ISO 8601 → ms; '', NULL or invalid → NULL; sort_ts = COALESCE(posted_at, imported_at)")),
        col("thumbnail_path", Text, Base, mapped("media_objects, posts.cover_object", "file hashed into CAS; missing file → archive_state pending")),
        col("preview_path", Text, ADDED, mapped("media_objects, posts.cover_object", "640 px auto cover; CAS; the cover when no downloaded cover exists")),
        col("image_path", Text, Base, mapped("media_objects, post_media.object_id", "the slide-0 image (same file as post_media position 0); CAS")),
        col("video_path", Text, Base, mapped("media_objects, post_media.video_object_id", "kept video of slide 0; CAS only with --with-videos")),
        col("media_count", Integer, Presence::Added { absent: "1" }, mapped("posts.media_count", "recomputed from the slides")),
        col("imported_at", Integer, Base, mapped("posts.imported_at", SECONDS_TO_MS)),
        col("ai_description", Text, ADDED, mapped("posts.ai_description", VERBATIM)),
        col("ai_tags", Text, ADDED, mapped("posts.ai_tags_json", "verbatim JSON array")),
        col("ai_status", Text, ADDED, mapped("posts.ai_status", "verbatim; 'analyzing' (stuck) → NULL, as desktop DATA-47")),
        col("ai_model", Text, ADDED, mapped("posts.ai_model", "verbatim; analyzed rows get ai_provider 'desktop-local' and ai_schema_version 1")),
        col("ai_analyzed_at", Integer, ADDED, mapped("posts.ai_analyzed_at", SECONDS_TO_MS)),
        col("ai_category", Text, ADDED, mapped("posts.ai_category", VERBATIM)),
        col("ai_content_type", Text, ADDED, mapped("posts.ai_content_type", VERBATIM)),
        col("ai_entities", Text, ADDED, mapped("posts.ai_entities_json", "verbatim JSON array")),
        col("ai_keywords", Text, ADDED, mapped("posts.ai_keywords_json", "verbatim JSON array")),
        col("ai_language", Text, ADDED, mapped("posts.ai_language", VERBATIM)),
        col("ai_save_reason", Text, ADDED, mapped("posts.ai_save_reason", VERBATIM)),
        col("ai_web_json", Text, ADDED, mapped("posts.ai_web_json", "verbatim JSON object (source of the rebuilt facets)")),
        col("user_note", Text, ADDED, mapped("posts.user_note", "verbatim; merged duplicates concatenate notes")),
        col("user_tags", Text, ADDED, mapped("posts.user_tags_json", "verbatim JSON array")),
        col("web_url", Text, ADDED, mapped("posts.web_url, web_captures.requested_url", "verbatim; the URL the web identity is computed from")),
        col("web_domain", Text, ADDED, mapped("posts.web_domain", VERBATIM)),
        col("web_final_url", Text, ADDED, mapped("posts.web_final_url, web_captures.final_url", VERBATIM)),
        col("web_palette_json", Text, ADDED, mapped("web_captures.palette_json", "current capture, verbatim")),
        col("web_fonts_json", Text, ADDED, mapped("web_captures.fonts_json", "current capture, verbatim")),
        col("web_tech_json", Text, ADDED, mapped("web_captures.tech_json", "current capture, verbatim")),
        col("web_awards_json", Text, ADDED, mapped("web_captures.awards_json", "current capture, verbatim")),
        col("web_pages_json", Text, ADDED, mapped("web_captures.pages_json, web_capture_assets", "current capture; file paths (screenshot, hero, chunks, sections, footer) → CAS assets, the JSON keeps text and probes")),
        col("web_meta_json", Text, ADDED, mapped("web_captures.meta_json, traits_json, engine, viewport, favicon_object, web_capture_assets", "current capture; og image, favicon and scroll video → CAS; traits and capture settings split out")),
        col("web_captured_at", Integer, ADDED, mapped("web_captures.captured_at", "unix seconds → ms; a site without pages is a placeholder: no capture row")),
        col("thumb_blur", Text, ADDED, dropped("~24 px JPEG data URI; replaced by posts.thumbhash, recomputed from the cover")),
    ],
};

#[rustfmt::skip]
pub const POST_MEDIA: TableSpec = TableSpec {
    name: "post_media",
    required: false,
    disposition: mapped("post_media, media_objects", "one slide per (post, position)"),
    columns: &[
        col("post_id", Text, Base, mapped("post_media.post_id", VIA_POST_KEY)),
        col("position", Integer, Base, mapped("post_media.position", VERBATIM)),
        col("media_type", Text, Base, mapped("post_media.kind", "image, video, file; slides of web posts → page")),
        col("source_url", Text, Base, mapped("post_media.source_url, post_media.object_id", "remote URL verbatim (+ IG `oe` expiry); manual posts hold the original file's local path → CAS object")),
        col("local_path", Text, Base, mapped("media_objects, post_media.object_id, post_media.video_object_id", "file hashed into CAS; videos only with --with-videos")),
    ],
};

#[rustfmt::skip]
pub const COLLECTIONS: TableSpec = TableSpec {
    name: "collections",
    required: false,
    disposition: mapped("collections", "duplicates on (platform, external_id) merge"),
    columns: &[
        col("id", Integer, Base, mapped("collections.id", "renumbered; old → new id map for memberships")),
        col("name", Text, Base, mapped("collections.name", VERBATIM)),
        col("color", Text, Base, mapped("collections.color", VERBATIM)),
        col("created_at", Integer, Base, mapped("collections.created_at", SECONDS_TO_MS)),
        col("platform", Text, ADDED, mapped("collections.platform", VERBATIM)),
        col("external_id", Text, ADDED, mapped("collections.external_id", VERBATIM)),
        col("ig_name", Text, ADDED, mapped("collections.source_name", "renamed column, verbatim")),
    ],
};

#[rustfmt::skip]
pub const POST_COLLECTIONS: TableSpec = TableSpec {
    name: "post_collections",
    required: false,
    disposition: mapped("post_collections", "memberships of merged posts and collections are unioned"),
    columns: &[
        col("post_id", Text, Base, mapped("post_collections.post_id", VIA_POST_KEY)),
        col("collection_id", Integer, Base, mapped("post_collections.collection_id", "the collection's new id")),
        col("added_at", Integer, Base, mapped("post_collections.added_at", SECONDS_TO_MS)),
    ],
};

#[rustfmt::skip]
pub const POST_TAGS: TableSpec = TableSpec {
    name: "post_tags",
    required: false,
    disposition: mapped("post_tags", "one row per (post, tag, source)"),
    columns: &[
        col("post_id", Text, Base, mapped("post_tags.post_id", VIA_POST_KEY)),
        col("tag_norm", Text, Base, mapped("post_tags.tag_norm", VERBATIM)),
        col("tag_form", Text, Base, mapped("post_tags.tag_form", VERBATIM)),
        col("tier", Text, ADDED, mapped("post_tags.source, post_tags.tier", "manual → source 'manual'; general/specific → source 'ai' with the tier; NULL → source 'ai', tier NULL")),
    ],
};

#[rustfmt::skip]
pub const POST_ENTITIES: TableSpec = TableSpec {
    name: "post_entities",
    required: false,
    disposition: mapped("post_entities", VERBATIM),
    columns: &[
        col("post_id", Text, Base, mapped("post_entities.post_id", VIA_POST_KEY)),
        col("ent_norm", Text, Base, mapped("post_entities.ent_norm", VERBATIM)),
        col("ent_form", Text, Base, mapped("post_entities.ent_form", VERBATIM)),
    ],
};

/// Why `post_facets` is not copied.
const FACETS_DERIVED: &str = "derived index of posts.ai_web_json.facets (desktop applyAiAnalysis); rebuilt from ai_web_json, which is carried verbatim";

#[rustfmt::skip]
pub const POST_FACETS: TableSpec = TableSpec {
    name: "post_facets",
    required: false,
    disposition: dropped(FACETS_DERIVED),
    columns: &[
        col("post_id", Text, Base, dropped(FACETS_DERIVED)),
        col("facet", Text, Base, dropped(FACETS_DERIVED)),
        col("value", Text, Base, dropped(FACETS_DERIVED)),
    ],
};

#[rustfmt::skip]
pub const TAG_ALIAS: TableSpec = TableSpec {
    name: "tag_alias",
    required: false,
    disposition: mapped("tag_alias", "verbatim; created_at = install time"),
    columns: &[
        col("alias_norm", Text, Base, mapped("tag_alias.alias_norm", VERBATIM)),
        col("canonical_norm", Text, Base, mapped("tag_alias.canonical_norm", VERBATIM)),
        col("canonical_form", Text, Base, mapped("tag_alias.canonical_form", VERBATIM)),
        col("status", Text, Presence::Added { absent: "'accepted'" }, mapped("tag_alias.status", VERBATIM)),
    ],
};

#[rustfmt::skip]
pub const TAG_CLUSTER: TableSpec = TableSpec {
    name: "tag_cluster",
    required: false,
    disposition: mapped("tag_cluster", VERBATIM),
    columns: &[
        col("id", Integer, Base, mapped("tag_cluster.id", VERBATIM)),
        col("label", Text, Base, mapped("tag_cluster.label", VERBATIM)),
        col("label_norm", Text, Base, mapped("tag_cluster.label_norm", VERBATIM)),
        col("status", Text, Base, mapped("tag_cluster.status", VERBATIM)),
        col("run_id", Integer, Base, mapped("tag_cluster.run_id", "verbatim (already ms)")),
        col("created_at", Integer, Base, mapped("tag_cluster.created_at", SECONDS_TO_MS)),
        col("updated_at", Integer, Base, mapped("tag_cluster.updated_at", SECONDS_TO_MS)),
    ],
};

#[rustfmt::skip]
pub const TAG_CLUSTER_MEMBERSHIP: TableSpec = TableSpec {
    name: "tag_cluster_membership",
    required: false,
    disposition: mapped("tag_cluster_membership", VERBATIM),
    columns: &[
        col("tag_norm", Text, Base, mapped("tag_cluster_membership.tag_norm", VERBATIM)),
        col("cluster_id", Integer, Base, mapped("tag_cluster_membership.cluster_id", VERBATIM)),
    ],
};

/// Where the AI layer of an archived site version goes.
const SNAPSHOT_AI: &str = "web_captures.ai_snapshot_json";
const SNAPSHOT_AI_RULE: &str = "frozen AI layer of the version, verbatim inside the JSON";

#[rustfmt::skip]
pub const WEB_SNAPSHOTS: TableSpec = TableSpec {
    name: "web_snapshots",
    required: false,
    disposition: mapped("web_captures, web_capture_assets", "one older version of a site per row"),
    columns: &[
        col("id", Integer, Base, mapped("web_captures.id", "renumbered")),
        col("post_id", Text, Base, mapped("web_captures.post_id", VIA_POST_KEY)),
        col("captured_at", Integer, Base, mapped("web_captures.captured_at", SECONDS_TO_MS)),
        col("title", Text, Base, mapped("web_captures.title", VERBATIM)),
        col("web_pages_json", Text, Base, mapped("web_captures.pages_json, web_capture_assets", "file paths → CAS assets; the JSON keeps text and probes")),
        col("web_palette_json", Text, Base, mapped("web_captures.palette_json", VERBATIM)),
        col("web_fonts_json", Text, Base, mapped("web_captures.fonts_json", VERBATIM)),
        col("web_tech_json", Text, Base, mapped("web_captures.tech_json", VERBATIM)),
        col("web_awards_json", Text, Base, mapped("web_captures.awards_json", VERBATIM)),
        col("web_meta_json", Text, Base, mapped("web_captures.meta_json, traits_json, engine, viewport, favicon_object, web_capture_assets", "as posts.web_meta_json")),
        col("ai_description", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_tags_json", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_model", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_status", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_analyzed_at", Integer, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_category", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_content_type", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_entities_json", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_keywords_json", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_language", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("ai_save_reason", Text, Base, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
        col("created_at", Integer, Base, mapped("web_captures.created_at", SECONDS_TO_MS)),
        col("ai_web_json", Text, ADDED, mapped(SNAPSHOT_AI, SNAPSHOT_AI_RULE)),
    ],
};

/// Why the job queue mirror is not copied.
const JOBS_DROPPED: &str = "desktop queue mirror; the web derives pending work from per-item state and keeps jobs in the control DB (§2.12)";

#[rustfmt::skip]
pub const JOBS: TableSpec = TableSpec {
    name: "jobs",
    required: false,
    disposition: dropped(JOBS_DROPPED),
    columns: &[
        col("kind", Text, Base, dropped(JOBS_DROPPED)),
        col("key", Text, Base, dropped(JOBS_DROPPED)),
        col("post_id", Text, Base, dropped(JOBS_DROPPED)),
        col("payload", Text, Base, dropped(JOBS_DROPPED)),
        col("status", Text, Base, dropped(JOBS_DROPPED)),
        col("progress", Real, Base, dropped(JOBS_DROPPED)),
        col("error", Text, Base, dropped(JOBS_DROPPED)),
        col("attempts", Integer, Base, dropped(JOBS_DROPPED)),
        col("created_at", Integer, Base, dropped(JOBS_DROPPED)),
        col("updated_at", Integer, Base, dropped(JOBS_DROPPED)),
    ],
};

/// Why `downloads` is not copied.
const DOWNLOADS_DROPPED: &str = "dead table, never written or read (DATA-57)";

#[rustfmt::skip]
pub const DOWNLOADS: TableSpec = TableSpec {
    name: "downloads",
    required: false,
    disposition: dropped(DOWNLOADS_DROPPED),
    columns: &[
        col("id", Integer, Base, dropped(DOWNLOADS_DROPPED)),
        col("post_id", Text, Base, dropped(DOWNLOADS_DROPPED)),
        col("asset_type", Text, Base, dropped(DOWNLOADS_DROPPED)),
        col("status", Text, Base, dropped(DOWNLOADS_DROPPED)),
        col("progress", Real, Base, dropped(DOWNLOADS_DROPPED)),
        col("error", Text, Base, dropped(DOWNLOADS_DROPPED)),
        col("started_at", Integer, Base, dropped(DOWNLOADS_DROPPED)),
        col("completed_at", Integer, Base, dropped(DOWNLOADS_DROPPED)),
    ],
};

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn names_are_unique() {
        let tables: BTreeSet<_> = TABLES.iter().map(|t| t.name).collect();
        assert_eq!(tables.len(), TABLES.len());
        for t in TABLES {
            let columns: BTreeSet<_> = t.columns.iter().map(|c| c.name).collect();
            assert_eq!(columns.len(), t.columns.len(), "{}", t.name);
        }
    }

    #[test]
    fn only_posts_is_required() {
        let required: Vec<_> = TABLES
            .iter()
            .filter(|t| t.required)
            .map(|t| t.name)
            .collect();
        assert_eq!(required, ["posts"]);
    }

    #[test]
    fn every_column_has_a_disposition_text() {
        for t in TABLES {
            for c in t.columns {
                match c.disposition {
                    Disposition::Mapped { target, rule } => {
                        assert!(
                            !target.is_empty() && !rule.is_empty(),
                            "{}.{}",
                            t.name,
                            c.name
                        )
                    }
                    Disposition::Dropped { reason } => assert!(!reason.is_empty()),
                }
            }
        }
    }

    #[test]
    fn dropped_tables_drop_every_column() {
        for t in TABLES.iter().filter(|t| !t.disposition.is_mapped()) {
            assert!(
                t.columns.iter().all(|c| !c.disposition.is_mapped()),
                "{}",
                t.name
            );
        }
    }

    #[test]
    fn counts_match_the_desktop_schema() {
        // 13 tables and 121 columns in `electron/db.ts` (SCHEMA + migrate()).
        assert_eq!(TABLES.len(), 13);
        assert_eq!(TABLES.iter().map(|t| t.columns.len()).sum::<usize>(), 121);
        assert_eq!(POSTS.columns.len(), 42);
        assert!(is_sqlite_internal("sqlite_sequence"));
        assert!(!is_sqlite_internal("posts"));
    }
}
