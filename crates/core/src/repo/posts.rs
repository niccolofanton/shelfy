//! Posts: the gallery list (desktop filter set, keyset pagination, FTS5
//! search), post detail, the lookup of saved posts by platform id, and the
//! write primitives that keep the derived tag rows and the search index
//! consistent: the user layer ([`update_user_content`]) and the AI layer, as
//! a whole ([`set_ai`]) or field by field ([`update_ai`], the desktop's
//! `updateAiAnalysis`, behind the manual AI edit).
//!
//! Filter semantics mirror the desktop's `buildPostFilter` (`electron/db.ts`),
//! with these deliberate changes:
//!
//! - search uses the FTS5 indexes (plan D10) instead of `LIKE` + a JS
//!   function: a post matches when any content term prefix-matches one of its
//!   indexed columns (`posts_fts`), or, for a term of 3 or more characters,
//!   appears anywhere in its text (`posts_infix`, a trigram index: `design`
//!   in `#productdesign`, as the desktop's `LIKE '%term%'` found it); the
//!   relevance score is the per-term model of [`crate::search::query`]
//!   (clamped term weights over bm25's frequency part, whole tokens above
//!   prefixes above infixes) plus an exact-tag and a phrase bonus (§2.14,
//!   tuned by SPIKE-5 and P1-05);
//! - relevance pages are offsets into a ranking of at most
//!   [`RELEVANCE_WINDOW`] posts ([`rank`], [`ranked_page`]), which the API
//!   computes once per search and keeps as a snapshot;
//! - "downloaded" becomes [`PostFilter::stored`]: the post has at least one
//!   archived object (cover, slide, poster or kept video);
//! - trashed posts are hidden unless [`PostFilter::trash`] asks for them;
//! - `media_types` takes several values (UI-34);
//! - a blank search is no search (the desktop treated `" "` as a phrase), and
//!   a search without any letter or digit (`"!!!"`) matches nothing, since FTS5
//!   cannot index punctuation (the desktop matched it as a substring);
//! - the desktop's unused `missingOnly` is `stored: Some(false)`; the single
//!   `tag` and the `tags` list keep their separate desktop semantics.

use std::collections::HashMap;
use std::fmt;

use rusqlite::types::{FromSql, Value};
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;

use super::{
    ObjectRef, Platform, RepoError, Result, conflict_on_unique, id_list, json_array_or_null,
    json_strings, json_value, object_columns, object_ref_at, tags,
};
use crate::ids::ig;
use crate::search::query::{self, RELEVANCE_WINDOW, TextQuery};
use crate::search::{index, terms::js_trim};

/// Page size when the caller does not choose one (plan §2.9).
pub const DEFAULT_PAGE_SIZE: u32 = 60;
/// Largest page size (plan §2.9).
pub const MAX_PAGE_SIZE: u32 = 200;
/// Longest caption accepted, in characters (plan §2.7).
pub const CAPTION_MAX_CHARS: usize = 20_000;

/// Sort order of a list.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Sort {
    /// Most recent first (`sort_ts` descending).
    #[default]
    Newest,
    /// Oldest first.
    Oldest,
    /// Best search score first, then newest. Without search text it falls back
    /// to [`Sort::Newest`].
    Relevance,
}

/// How several tags, or several search blocks, combine.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Mode {
    /// Any of them.
    #[default]
    Or,
    /// All of them.
    And,
}

/// The desktop's `source` bucket.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceBucket {
    /// Websites only (`platform = 'web'`).
    Web,
    /// Everything else, manual bookmarks included (`platform <> 'web'`).
    Social,
}

/// Filters of the gallery list, its count and its bulk selection.
///
/// Every field is optional and the active ones combine with AND.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PostFilter {
    /// One platform.
    pub platform: Option<Platform>,
    /// Websites or social posts.
    pub source: Option<SourceBucket>,
    /// Members of one collection.
    pub collection_id: Option<i64>,
    /// Any of these media types (`image`, `images`, `carousel`, `video`, …).
    pub media_types: Vec<String>,
    /// Has at least one archived object (`Some(true)`) or none (`Some(false)`).
    pub stored: Option<bool>,
    /// Has at least one AI tag (manual tags do not count).
    pub ai_tagged: Option<bool>,
    /// AI analysis finished (`ai_status = 'done'`) or not.
    pub analyzed: Option<bool>,
    /// Exact `ai_status`.
    pub ai_status: Option<String>,
    /// One tag, always a hard filter: the gallery's tag chip (desktop `tag`).
    pub tag: Option<String>,
    /// Tags of the AI views (desktop `tags[]`), combined by `tag_mode`. With
    /// search text in [`Mode::Or`] they join the search instead of filtering:
    /// a post matching a tag or the text is listed, and tags add to the score.
    pub tags: Vec<String>,
    /// How `tags` combine.
    pub tag_mode: Mode,
    /// Has this AI entity.
    pub entity: Option<String>,
    /// Exact `ai_category`.
    pub category: Option<String>,
    /// Exact `ai_content_type`.
    pub content_type: Option<String>,
    /// Free-text search.
    pub q: Option<String>,
    /// Suggested concepts: extra search blocks, combined with the text by
    /// `concept_mode`.
    pub concepts: Vec<String>,
    /// How the search blocks combine.
    pub concept_mode: Mode,
    /// `sort_ts` at or after this time (unix ms).
    pub date_from: Option<i64>,
    /// `sort_ts` before this time (unix ms).
    pub date_to: Option<i64>,
    /// List the trash instead of the library.
    pub trash: bool,
}

impl PostFilter {
    /// Whether the filter has search text, which makes [`Sort::Relevance`] apply.
    /// The desktop ranks by relevance whenever this is true.
    #[must_use]
    pub fn has_text(&self) -> bool {
        non_blank(self.q.as_deref()).is_some() || !clean_concepts(&self.concepts).is_empty()
    }
}

/// A position in a list. Its text form (`Display`, [`Cursor::parse`]) is what
/// the API hands out as an opaque cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cursor {
    /// After this post in [`Sort::Newest`] order.
    Newest {
        /// `sort_ts` of the last post of the previous page.
        sort_ts: i64,
        /// Internal id of that post.
        id: i64,
    },
    /// After this post in [`Sort::Oldest`] order.
    Oldest {
        /// `sort_ts` of the last post of the previous page.
        sort_ts: i64,
        /// Internal id of that post.
        id: i64,
    },
    /// Relevance results from this offset on.
    Relevance {
        /// Results already returned.
        offset: u32,
    },
}

impl Cursor {
    /// Parses the text form.
    ///
    /// # Errors
    ///
    /// [`RepoError::InvalidCursor`] for anything [`Cursor`]'s `Display` did not
    /// produce.
    pub fn parse(text: &str) -> Result<Self> {
        let mut parts = text.split('.');
        let kind = parts.next();
        let nums: Vec<&str> = parts.collect();
        let int = |s: &str| s.parse::<i64>().map_err(|_| RepoError::InvalidCursor);
        match (kind, nums.as_slice()) {
            (Some("n"), [ts, id]) => Ok(Self::Newest {
                sort_ts: int(ts)?,
                id: int(id)?,
            }),
            (Some("o"), [ts, id]) => Ok(Self::Oldest {
                sort_ts: int(ts)?,
                id: int(id)?,
            }),
            (Some("r"), [offset]) => Ok(Self::Relevance {
                offset: offset.parse().map_err(|_| RepoError::InvalidCursor)?,
            }),
            _ => Err(RepoError::InvalidCursor),
        }
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Newest { sort_ts, id } => write!(f, "n.{sort_ts}.{id}"),
            Self::Oldest { sort_ts, id } => write!(f, "o.{sort_ts}.{id}"),
            Self::Relevance { offset } => write!(f, "r.{offset}"),
        }
    }
}

/// Which page of a list to return.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    /// Order of the list.
    pub sort: Sort,
    /// Page size, clamped to `1..=MAX_PAGE_SIZE`.
    pub limit: u32,
    /// Where the previous page ended; `None` for the first page.
    pub cursor: Option<Cursor>,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self {
            sort: Sort::Newest,
            limit: DEFAULT_PAGE_SIZE,
            cursor: None,
        }
    }
}

/// One page of results.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Page<T> {
    /// The results.
    pub items: Vec<T>,
    /// Cursor of the next page; `None` on the last one.
    #[serde(serialize_with = "serialize_cursor")]
    pub next_cursor: Option<Cursor>,
}

fn serialize_cursor<S: serde::Serializer>(
    c: &Option<Cursor>,
    s: S,
) -> std::result::Result<S::Ok, S::Error> {
    match c {
        Some(c) => s.collect_str(c),
        None => s.serialize_none(),
    }
}

/// A post as the gallery shows it. Heavy fields (entities, keywords, the AI web
/// catalog, capture pages) are only in [`PostDetail`].
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostSummary {
    /// Internal row id, never exposed by the API (posts are addressed by key).
    #[serde(skip)]
    pub id: i64,
    /// Public id (plan §2.8).
    pub key: String,
    /// Source platform.
    pub platform: Platform,
    /// Instagram shortcode.
    pub shortcode: Option<String>,
    /// Link to the original post (for websites: the final URL).
    pub post_url: Option<String>,
    /// Link to the author's profile.
    pub profile_url: Option<String>,
    /// Author handle (for websites: the domain).
    pub author_username: Option<String>,
    /// Author display name (for websites: the title).
    pub author_name: Option<String>,
    /// Caption.
    pub caption: Option<String>,
    /// `image`, `images`, `carousel`, `video`, `text`, `website` or `file`.
    pub media_type: String,
    /// Number of slides.
    pub media_count: i64,
    /// Publication time (unix ms), when known.
    pub posted_at: Option<i64>,
    /// When the post entered the library.
    pub imported_at: i64,
    /// Sort key: `posted_at`, else `imported_at`.
    pub sort_ts: i64,
    /// Archived cover.
    pub cover: Option<ObjectRef>,
    /// Remote cover URL (may expire).
    pub cover_url: Option<String>,
    /// ThumbHash placeholder bytes.
    pub thumbhash: Option<Vec<u8>>,
    /// Archive progress: `pending`, `partial`, `done`, `failed`, `client`, `link_only`.
    pub archive_state: String,
    /// AI lifecycle status.
    pub ai_status: Option<String>,
    /// AI description.
    pub ai_description: Option<String>,
    /// AI category.
    pub ai_category: Option<String>,
    /// AI content type.
    pub ai_content_type: Option<String>,
    /// AI-detected language.
    pub ai_language: Option<String>,
    /// AI guess of why the post was saved.
    pub ai_save_reason: Option<String>,
    /// AI tags as produced (display forms).
    pub ai_tags: Vec<String>,
    /// When the AI analysis finished.
    pub ai_analyzed_at: Option<i64>,
    /// The user's note.
    pub user_note: Option<String>,
    /// The user's tags (display forms).
    pub user_tags: Vec<String>,
    /// Website URL as saved.
    pub web_url: Option<String>,
    /// Website domain.
    pub web_domain: Option<String>,
    /// Website URL after redirects.
    pub web_final_url: Option<String>,
    /// Last change of the row.
    pub updated_at: i64,
    /// When the post went to the trash.
    pub deleted_at: Option<i64>,
    /// Slides, in order.
    pub media: Vec<PostMedia>,
    /// Collections the post belongs to.
    pub collection_ids: Vec<i64>,
    /// The current capture of a website.
    pub web_capture: Option<WebCaptureSummary>,
    #[serde(skip)]
    current_capture_id: Option<i64>,
}

/// One slide of a post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostMedia {
    /// 0-based order.
    pub position: i64,
    /// `image`, `video`, `file` or `page`.
    pub kind: String,
    /// Remote URL of the slide (may expire).
    pub source_url: Option<String>,
    /// Pixel width, when known.
    pub width: Option<i64>,
    /// Pixel height, when known.
    pub height: Option<i64>,
    /// Video duration, when known.
    pub duration_ms: Option<i64>,
    /// Caption of the slide (file name, page title).
    pub label: Option<String>,
    /// Archived image, or poster of a video.
    pub object: Option<ObjectRef>,
    /// Kept ("offline") video.
    pub video_object: Option<ObjectRef>,
}

/// The current capture of a website, without its page texts.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WebCaptureSummary {
    /// Capture id.
    pub id: i64,
    /// When it was captured.
    pub captured_at: i64,
    /// URL the capture started from.
    pub requested_url: Option<String>,
    /// URL after redirects.
    pub final_url: Option<String>,
    /// Capture status.
    pub status: String,
    /// Whether some pages failed.
    pub partial: bool,
    /// Page title.
    pub title: Option<String>,
    /// Color palette (JSON as stored).
    pub palette: Option<serde_json::Value>,
    /// Fonts (JSON as stored).
    pub fonts: Option<serde_json::Value>,
    /// Detected technologies (JSON as stored).
    pub tech: Option<serde_json::Value>,
    /// Awards (JSON as stored).
    pub awards: Option<serde_json::Value>,
    /// Page metadata (JSON as stored).
    pub meta: Option<serde_json::Value>,
    /// Hero screenshot.
    pub hero: Option<ObjectRef>,
    /// Favicon.
    pub favicon: Option<ObjectRef>,
}

/// Origin of a tag row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TagSource {
    /// Produced by the AI analysis.
    Ai,
    /// Added by the user.
    Manual,
}

/// A tag of a post, after alias resolution.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostTag {
    /// Display form.
    pub tag: String,
    /// Normalized form (matching key).
    pub norm: String,
    /// AI or manual.
    pub source: TagSource,
    /// `general` or `specific` for AI tags with a tier.
    pub tier: Option<String>,
}

/// An AI entity of a post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostEntity {
    /// Display form.
    pub entity: String,
    /// Normalized form.
    pub norm: String,
}

/// Everything about one post.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PostDetail {
    /// The gallery fields.
    #[serde(flatten)]
    pub summary: PostSummary,
    /// Platform-native id.
    pub native_id: String,
    /// When `cover_url` expires, if known.
    pub cover_url_expires_at: Option<i64>,
    /// AI attempts so far.
    pub ai_attempts: i64,
    /// Next AI attempt, when backed off.
    pub ai_next_at: Option<i64>,
    /// Last AI error code.
    pub ai_error: Option<String>,
    /// Provider of the AI analysis.
    pub ai_provider: Option<String>,
    /// Model of the AI analysis.
    pub ai_model: Option<String>,
    /// Version of the AI output schema.
    pub ai_schema_version: Option<i64>,
    /// AI entities as produced.
    pub ai_entities: Vec<String>,
    /// AI keywords.
    pub ai_keywords: Vec<String>,
    /// AI design catalog of a website (JSON as stored).
    pub ai_web: Option<serde_json::Value>,
    /// Tag rows (AI and manual, alias-resolved).
    pub tags: Vec<PostTag>,
    /// Entity rows.
    pub entities: Vec<PostEntity>,
}

/// A slide to insert.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewMedia {
    /// `image`, `video`, `file` or `page`.
    pub kind: String,
    /// Remote URL.
    pub source_url: Option<String>,
    /// When `source_url` expires.
    pub source_url_expires_at: Option<i64>,
    /// Direct video URL kept by the parser.
    pub video_url: Option<String>,
    /// When `video_url` expires.
    pub video_url_expires_at: Option<i64>,
    /// Pixel width.
    pub width: Option<i64>,
    /// Pixel height.
    pub height: Option<i64>,
    /// Video duration.
    pub duration_ms: Option<i64>,
    /// Slide label.
    pub label: Option<String>,
    /// Archived image or poster.
    pub object_id: Option<i64>,
    /// Kept video.
    pub video_object_id: Option<i64>,
}

/// The AI layer of a post, written as a whole.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AiLayer {
    /// Lifecycle status (`done`, …).
    pub status: Option<String>,
    /// Provider id.
    pub provider: Option<String>,
    /// Model id.
    pub model: Option<String>,
    /// Output schema version.
    pub schema_version: Option<i64>,
    /// Description.
    pub description: Option<String>,
    /// Why the post was saved.
    pub save_reason: Option<String>,
    /// Language.
    pub language: Option<String>,
    /// Category.
    pub category: Option<String>,
    /// Content type.
    pub content_type: Option<String>,
    /// Tags (display forms).
    pub tags: Vec<String>,
    /// Tags of the general tier, when the model split them.
    pub general_tags: Option<Vec<String>>,
    /// Tags of the specific tier, when the model split them.
    pub specific_tags: Option<Vec<String>>,
    /// Entities.
    pub entities: Vec<String>,
    /// Keywords.
    pub keywords: Vec<String>,
    /// Design catalog of a website.
    pub web: Option<serde_json::Value>,
    /// When the analysis finished.
    pub analyzed_at: Option<i64>,
}

/// A post to insert. Ingest merge rules (merge, never clobber) are not applied
/// here: this is the plain insert that ingest, import and migration build on.
#[derive(Clone, Debug, PartialEq)]
pub struct NewPost {
    /// Public id (plan §2.8).
    pub key: String,
    /// Source platform.
    pub platform: Platform,
    /// Platform-native id.
    pub native_id: String,
    /// Instagram shortcode.
    pub shortcode: Option<String>,
    /// Link to the original.
    pub post_url: Option<String>,
    /// Link to the author.
    pub profile_url: Option<String>,
    /// Author handle.
    pub author_username: Option<String>,
    /// Author display name.
    pub author_name: Option<String>,
    /// Caption, at most [`CAPTION_MAX_CHARS`].
    pub caption: Option<String>,
    /// Media type.
    pub media_type: String,
    /// Publication time.
    pub posted_at: Option<i64>,
    /// Import time.
    pub imported_at: i64,
    /// Archived cover.
    pub cover_object: Option<i64>,
    /// Remote cover URL.
    pub cover_url: Option<String>,
    /// When `cover_url` expires.
    pub cover_url_expires_at: Option<i64>,
    /// ThumbHash bytes.
    pub thumbhash: Option<Vec<u8>>,
    /// Archive state; `None` means `pending`.
    pub archive_state: Option<String>,
    /// AI layer, if already analyzed.
    pub ai: Option<AiLayer>,
    /// The user's note.
    pub user_note: Option<String>,
    /// The user's tags.
    pub user_tags: Vec<String>,
    /// Website URL.
    pub web_url: Option<String>,
    /// Website domain.
    pub web_domain: Option<String>,
    /// Website URL after redirects.
    pub web_final_url: Option<String>,
    /// Slides, in order (positions are their indexes).
    pub media: Vec<NewMedia>,
}

impl NewPost {
    /// A post with the required fields and everything else empty.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        platform: Platform,
        native_id: impl Into<String>,
        media_type: impl Into<String>,
        imported_at: i64,
    ) -> Self {
        Self {
            key: key.into(),
            platform,
            native_id: native_id.into(),
            shortcode: None,
            post_url: None,
            profile_url: None,
            author_username: None,
            author_name: None,
            caption: None,
            media_type: media_type.into(),
            posted_at: None,
            imported_at,
            cover_object: None,
            cover_url: None,
            cover_url_expires_at: None,
            thumbhash: None,
            archive_state: None,
            ai: None,
            user_note: None,
            user_tags: Vec::new(),
            web_url: None,
            web_domain: None,
            web_final_url: None,
            media: Vec::new(),
        }
    }
}

/// Changes to the user-authored layer; `None` leaves a field untouched.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UserContentPatch {
    /// New note, stored as given; `Some(None)` clears it.
    pub note: Option<Option<String>>,
    /// New manual tags, replacing the old ones; stored as given for display.
    pub tags: Option<Vec<String>>,
}

/// The model a manual AI edit is attributed to (the desktop wrote `manuale`).
pub const MANUAL_AI_MODEL: &str = "manual";

/// Changes to the AI layer, with the semantics of the desktop's
/// `updateAiAnalysis`: `None` leaves a field untouched, `Some(None)` writes
/// `NULL`, and a list is stored as given (`[]` stays an empty JSON array).
/// `provider`, `schema_version` and `error` are web-only columns.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiPatch {
    /// Lifecycle status; `done` also stamps `ai_analyzed_at` unless
    /// `analyzed_at` is given.
    pub status: Option<Option<String>>,
    /// Provider id.
    pub provider: Option<Option<String>>,
    /// Model id.
    pub model: Option<Option<String>>,
    /// Version of the AI output schema the layer follows.
    pub schema_version: Option<Option<i64>>,
    /// Code of the last AI error.
    pub error: Option<Option<String>>,
    /// Description.
    pub description: Option<Option<String>>,
    /// Tags (display forms); their AI tag rows are rebuilt.
    pub tags: Option<Option<Vec<String>>>,
    /// Tags of the general tier: with `tags`, sets the rows' tiers.
    pub general_tags: Option<Vec<String>>,
    /// Tags of the specific tier (it wins over general): with `tags`, sets
    /// the rows' tiers.
    pub specific_tags: Option<Vec<String>>,
    /// Category.
    pub category: Option<Option<String>>,
    /// Content type.
    pub content_type: Option<Option<String>>,
    /// Entities; their rows are rebuilt.
    pub entities: Option<Option<Vec<String>>>,
    /// Keywords.
    pub keywords: Option<Option<Vec<String>>>,
    /// Language.
    pub language: Option<Option<String>>,
    /// Why the post was saved.
    pub save_reason: Option<Option<String>>,
    /// When the analysis finished, as given.
    pub analyzed_at: Option<Option<i64>>,
}

impl AiPatch {
    /// `self` as a manual edit (desktop `analyze:updateManual`): status
    /// `done` and model [`MANUAL_AI_MODEL`], so it also stamps the analysis
    /// time. The layer becomes the user's as a whole, as on the desktop, so
    /// nothing of an earlier analysis stays attributed to it: no provider,
    /// no error, and no output schema version, because the user's text
    /// follows no model's schema.
    #[must_use]
    pub fn manual(self) -> Self {
        Self {
            status: Some(Some("done".to_owned())),
            provider: Some(None),
            model: Some(Some(MANUAL_AI_MODEL.to_owned())),
            schema_version: Some(None),
            error: Some(None),
            ..self
        }
    }
}

/// A saved post found by [`lookup`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LookupHit {
    /// The key as asked for.
    pub key: String,
    /// The post's key (plan §2.8).
    pub post_key: String,
    /// Whether the post is in the trash.
    pub trashed: bool,
}

// ── Reading ──────────────────────────────────────────────────────────────────

/// One page of the gallery.
///
/// # Errors
///
/// [`RepoError::InvalidCursor`] when the cursor belongs to another sort order;
/// database errors otherwise.
pub fn list(
    conn: &Connection,
    filter: &PostFilter,
    page: &PageRequest,
) -> Result<Page<PostSummary>> {
    let limit = page.limit.clamp(1, MAX_PAGE_SIZE);
    let text = TextPlan::new(filter);
    let sort = if page.sort == Sort::Relevance && !text.has_text {
        Sort::Newest
    } else {
        page.sort
    };
    if sort == Sort::Relevance {
        // `ranked_page` attaches the slides and memberships itself.
        let offset = relevance_offset(page.cursor)?;
        if offset >= RELEVANCE_WINDOW {
            return Ok(Page {
                items: Vec::new(),
                next_cursor: None,
            });
        }
        let ranked = rank(conn, filter)?;
        return ranked_page(conn, filter, &ranked, offset, limit);
    }
    let where_sql = WhereSql::new(filter, &text);
    let mut result = list_keyset(conn, where_sql, sort, limit, page.cursor)?;
    attach(conn, &mut result.items)?;
    Ok(result)
}

/// Number of posts matching `filter`.
///
/// # Errors
///
/// Database errors.
pub fn count(conn: &Connection, filter: &PostFilter) -> Result<u64> {
    let (condition, params) = filter_condition(filter);
    let sql = format!("SELECT count(*) FROM posts p WHERE {condition}");
    let n: i64 = conn
        .prepare_cached(&sql)?
        .query_row(params_from_iter(params.iter()), |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Ids of every post matching `filter`, newest first: the resolution of a bulk
/// selection by filter (DATA-17), done server-side.
///
/// # Errors
///
/// Database errors.
pub fn list_ids(conn: &Connection, filter: &PostFilter) -> Result<Vec<i64>> {
    let (condition, params) = filter_condition(filter);
    let sql =
        format!("SELECT p.id FROM posts p WHERE {condition} ORDER BY p.sort_ts DESC, p.id DESC");
    let mut stmt = conn.prepare_cached(&sql)?;
    let ids = stmt
        .query_map(params_from_iter(params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The full post with this key, trashed or not.
///
/// # Errors
///
/// Database errors.
pub fn get(conn: &Connection, key: &str) -> Result<Option<PostDetail>> {
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS}, p.native_id, p.cover_url_expires_at, p.ai_attempts,
                p.ai_next_at, p.ai_error, p.ai_provider, p.ai_model, p.ai_schema_version,
                p.ai_entities_json, p.ai_keywords_json, p.ai_web_json
         FROM {SUMMARY_FROM} WHERE p.key = ?1"
    );
    let found = conn
        .prepare_cached(&sql)?
        .query_row([key], |row| {
            let mut cols = Cols::new(row);
            let summary = summary_from(&mut cols)?;
            Ok(PostDetail {
                summary,
                native_id: cols.next()?,
                cover_url_expires_at: cols.next()?,
                ai_attempts: cols.next()?,
                ai_next_at: cols.next()?,
                ai_error: cols.next()?,
                ai_provider: cols.next()?,
                ai_model: cols.next()?,
                ai_schema_version: cols.next()?,
                ai_entities: json_strings(cols.next::<Option<String>>()?.as_deref()),
                ai_keywords: json_strings(cols.next::<Option<String>>()?.as_deref()),
                ai_web: json_value(cols.next::<Option<String>>()?.as_deref()),
                tags: Vec::new(),
                entities: Vec::new(),
            })
        })
        .optional()?;
    let Some(mut detail) = found else {
        return Ok(None);
    };
    attach(conn, std::slice::from_mut(&mut detail.summary))?;
    let id = detail.summary.id;
    detail.tags = conn
        .prepare_cached(
            "SELECT tag_form, tag_norm, source, tier FROM post_tags WHERE post_id = ?1
             ORDER BY source, tag_norm",
        )?
        .query_map([id], |r| {
            Ok(PostTag {
                tag: r.get(0)?,
                norm: r.get(1)?,
                source: if r.get::<_, String>(2)? == "manual" {
                    TagSource::Manual
                } else {
                    TagSource::Ai
                },
                tier: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    detail.entities = conn
        .prepare_cached(
            "SELECT ent_form, ent_norm FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
        )?
        .query_map([id], |r| {
            Ok(PostEntity {
                entity: r.get(0)?,
                norm: r.get(1)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some(detail))
}

/// The posts with these keys, in the order of `keys`; unknown keys are skipped
/// (desktop `getPostsByIds`).
///
/// # Errors
///
/// Database errors.
pub fn get_many(conn: &Connection, keys: &[String]) -> Result<Vec<PostSummary>> {
    if keys.is_empty() {
        return Ok(Vec::new());
    }
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM}
         WHERE p.key IN (SELECT value FROM json_each(?1))"
    );
    let keys_json = serde_json::to_string(keys).expect("strings serialize");
    let mut found: HashMap<String, PostSummary> = conn
        .prepare_cached(&sql)?
        .query_map([keys_json], |row| summary_from(&mut Cols::new(row)))?
        .map(|r| r.map(|p| (p.key.clone(), p)))
        .collect::<rusqlite::Result<_>>()?;
    let mut items: Vec<PostSummary> = keys.iter().filter_map(|k| found.remove(k)).collect();
    attach(conn, &mut items)?;
    Ok(items)
}

/// Internal id of the post with this key.
///
/// # Errors
///
/// Database errors.
pub fn id_for_key(conn: &Connection, key: &str) -> Result<Option<i64>> {
    Ok(conn
        .prepare_cached("SELECT id FROM posts WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?)
}

/// Keys of the posts with these internal ids, in the order of `ids`; ids
/// without a post are skipped.
///
/// # Errors
///
/// Database errors.
pub fn keys_of(conn: &Connection, ids: &[i64]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut found: HashMap<i64, String> = conn
        .prepare_cached("SELECT id, key FROM posts WHERE id IN (SELECT value FROM json_each(?1))")?
        .query_map([id_list(ids)], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids.iter().filter_map(|id| found.remove(id)).collect())
}

/// The saved posts among `keys`, ids as `platform`'s own pages show them
/// (desktop `savedByKeys`, DATA-05): on Instagram a shortcode, a media pk or
/// a REST id `<pk>_<owner>`; elsewhere the native id (a tweet id, a pin id).
/// Trashed posts are found too, flagged. Hits come in the order of `keys`,
/// once per key; keys without a post are left out.
///
/// An Instagram key matches the post whose pk it decodes to, or whose stored
/// shortcode it is (the desktop matched the id or the shortcode).
///
/// # Errors
///
/// Database errors.
pub fn lookup(conn: &Connection, platform: Platform, keys: &[String]) -> Result<Vec<LookupHit>> {
    let mut wanted: Vec<(&str, Option<String>)> = Vec::new();
    for key in keys {
        if key.is_empty() || wanted.iter().any(|(k, _)| k == key) {
            continue;
        }
        let native = if platform == Platform::Instagram {
            ig::parse_legacy_id(key, None)
                .ok()
                .map(|id| id.pk.as_str().to_owned())
        } else {
            Some(key.clone())
        };
        wanted.push((key, native));
    }
    if wanted.is_empty() {
        return Ok(Vec::new());
    }
    let strings = |items: Vec<&str>| serde_json::to_string(&items).expect("strings serialize");
    let natives = strings(wanted.iter().filter_map(|(_, n)| n.as_deref()).collect());
    let by_native: HashMap<String, (String, bool)> = conn
        .prepare_cached(
            "SELECT native_id, key, deleted_at IS NOT NULL FROM posts
             WHERE platform = ?1 AND native_id IN (SELECT value FROM json_each(?2))",
        )?
        .query_map(params![platform, natives], |r| {
            Ok((r.get(0)?, (r.get(1)?, r.get(2)?)))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let by_shortcode: HashMap<String, (String, bool)> = if platform == Platform::Instagram {
        let codes = strings(wanted.iter().map(|(k, _)| *k).collect());
        conn.prepare_cached(
            "SELECT shortcode, key, deleted_at IS NOT NULL FROM posts
             WHERE platform = 'instagram' AND shortcode IS NOT NULL
               AND shortcode IN (SELECT value FROM json_each(?1))",
        )?
        .query_map([codes], |r| Ok((r.get(0)?, (r.get(1)?, r.get(2)?))))?
        .collect::<rusqlite::Result<_>>()?
    } else {
        HashMap::new()
    };
    Ok(wanted
        .into_iter()
        .filter_map(|(key, native)| {
            let (post_key, trashed) = by_shortcode
                .get(key)
                .or_else(|| native.as_ref().and_then(|n| by_native.get(n)))?;
            Some(LookupHit {
                key: key.to_owned(),
                post_key: post_key.clone(),
                trashed: *trashed,
            })
        })
        .collect())
}

// ── Writing ──────────────────────────────────────────────────────────────────

/// Inserts a post with its slides, AI layer and user layer, and indexes it.
/// Returns its internal id.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a missing key, native id or media type, an
/// over-long caption or an unknown slide kind; [`RepoError::Conflict`] when
/// the key or `(platform, native_id)` already exists.
pub fn insert(conn: &Connection, post: &NewPost, now: i64) -> Result<i64> {
    validate(post)?;
    let sort_ts = post.posted_at.unwrap_or(post.imported_at);
    let media_count = i64::try_from(post.media.len().max(1)).unwrap_or(i64::MAX);
    conn.prepare_cached(
        "INSERT INTO posts (key, platform, native_id, shortcode, post_url, profile_url,
                            author_username, author_name, caption, media_type, media_count,
                            posted_at, imported_at, sort_ts, cover_object, cover_url,
                            cover_url_expires_at, thumbhash, archive_state, user_note,
                            user_tags_json, web_url, web_domain, web_final_url, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                 ?18, COALESCE(?19, 'pending'), ?20, ?21, ?22, ?23, ?24, ?25)",
    )?
    .execute(params![
        post.key,
        post.platform,
        post.native_id,
        post.shortcode,
        post.post_url,
        post.profile_url,
        post.author_username,
        post.author_name,
        post.caption,
        post.media_type,
        media_count,
        post.posted_at,
        post.imported_at,
        sort_ts,
        post.cover_object,
        post.cover_url,
        post.cover_url_expires_at,
        post.thumbhash,
        post.archive_state,
        post.user_note,
        json_array_or_null(&post.user_tags),
        post.web_url,
        post.web_domain,
        post.web_final_url,
        now
    ])
    .map_err(|e| conflict_on_unique(e, "post"))?;
    let id = conn.last_insert_rowid();

    let mut insert_media = conn.prepare_cached(
        "INSERT INTO post_media (post_id, position, kind, source_url, source_url_expires_at,
                                 video_url, video_url_expires_at, width, height, duration_ms,
                                 label, object_id, video_object_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for (position, m) in (0_i64..).zip(&post.media) {
        insert_media.execute(params![
            id,
            position,
            m.kind,
            m.source_url,
            m.source_url_expires_at,
            m.video_url,
            m.video_url_expires_at,
            m.width,
            m.height,
            m.duration_ms,
            m.label,
            m.object_id,
            m.video_object_id
        ])?;
    }
    if let Some(ai) = &post.ai {
        write_ai(conn, id, Some(ai), now)?;
    }
    tags::sync_manual_tags(conn, id, &post.user_tags)?;
    index::reindex_post(conn, id)?;
    Ok(id)
}

/// Replaces the AI layer of a post (columns, AI tag rows, entity rows) and
/// reindexes it. Manual tags are untouched.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown post; database errors otherwise.
pub fn set_ai(conn: &Connection, post_id: i64, ai: &AiLayer, now: i64) -> Result<()> {
    ensure_exists(conn, post_id)?;
    write_ai(conn, post_id, Some(ai), now)?;
    index::reindex_post(conn, post_id)?;
    Ok(())
}

/// Clears the AI layer of a post (the desktop's "clear AI analysis" for one
/// post) and reindexes it. Manual tags are untouched.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown post; database errors otherwise.
pub fn clear_ai(conn: &Connection, post_id: i64, now: i64) -> Result<()> {
    ensure_exists(conn, post_id)?;
    write_ai(conn, post_id, None, now)?;
    index::reindex_post(conn, post_id)?;
    Ok(())
}

/// Updates the user's note and manual tags (DATA-28, DATA-29) and reindexes the
/// post, with the desktop's `updateUserContent` semantics: both are stored as
/// given (the API validates input first), and the manual tag rows are the
/// trimmed, lowercased, alias-resolved and deduped tags. Unlike the desktop, an
/// AI tag of the same name keeps its own row, and values equal to the stored
/// ones write nothing: no row, no `updated_at`, no reindex.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown post; database errors otherwise.
pub fn update_user_content(
    conn: &Connection,
    post_id: i64,
    patch: &UserContentPatch,
    now: i64,
) -> Result<()> {
    ensure_exists(conn, post_id)?;
    let mut changed = false;
    if let Some(note) = &patch.note {
        changed |= conn
            .prepare_cached(
                "UPDATE posts SET user_note = ?2 WHERE id = ?1 AND user_note IS NOT ?2",
            )?
            .execute(params![post_id, note])?
            > 0;
    }
    if let Some(list) = &patch.tags {
        let json = serde_json::to_string(list).expect("strings serialize");
        changed |= conn
            .prepare_cached(
                "UPDATE posts SET user_tags_json = ?2 WHERE id = ?1 AND user_tags_json IS NOT ?2",
            )?
            .execute(params![post_id, json])?
            > 0;
        changed |= tags::sync_manual_tags(conn, post_id, list)?;
    }
    if changed {
        touch(conn, post_id, now)?;
        index::reindex_post(conn, post_id)?;
    }
    Ok(())
}

/// Applies `patch` to the AI layer of a post, with the desktop's
/// `updateAiAnalysis` semantics (`applyAiAnalysis`), and reindexes it:
///
/// - only the fields present are written; `Some(None)` writes `NULL`, a list
///   is stored as given;
/// - `ai_analyzed_at` takes `analyzed_at` when given, else `now` when the
///   status becomes `done`;
/// - tags present (a list or `NULL`) rebuild the AI tag rows: trimmed,
///   lowercased, alias-resolved and deduped, with tiers from `general_tags`
///   and `specific_tags` (specific wins; no tier when neither is given);
///   entities present rebuild the entity rows (no aliases).
///
/// Unlike the desktop, manual tags are a separate layer that this never
/// touches, even for a tag of the same name (plan §1.2 #3), times are in
/// milliseconds, and values equal to the stored ones write nothing: a patch
/// that changes no column and no derived row leaves the post alone, without
/// a new `ai_analyzed_at`, `updated_at` or index row. Returns whether the
/// post changed: an empty patch is a no-op.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown post; database errors otherwise.
pub fn update_ai(conn: &Connection, post_id: i64, patch: &AiPatch, now: i64) -> Result<bool> {
    ensure_exists(conn, post_id)?;
    let text = |v: &Option<String>| v.clone().map_or(Value::Null, Value::Text);
    let list = |v: &Option<Vec<String>>| {
        v.as_ref().map_or(Value::Null, |items| {
            Value::Text(serde_json::to_string(items).expect("strings serialize"))
        })
    };
    let mut columns: Vec<(&str, Value)> = Vec::new();
    let texts = [
        ("ai_description", &patch.description),
        ("ai_status", &patch.status),
        ("ai_provider", &patch.provider),
        ("ai_model", &patch.model),
        ("ai_error", &patch.error),
        ("ai_category", &patch.category),
        ("ai_content_type", &patch.content_type),
        ("ai_language", &patch.language),
        ("ai_save_reason", &patch.save_reason),
    ];
    for (column, value) in texts {
        if let Some(value) = value {
            columns.push((column, text(value)));
        }
    }
    let lists = [
        ("ai_tags_json", &patch.tags),
        ("ai_entities_json", &patch.entities),
        ("ai_keywords_json", &patch.keywords),
    ];
    for (column, value) in lists {
        if let Some(value) = value {
            columns.push((column, list(value)));
        }
    }
    let int = |v: Option<i64>| v.map_or(Value::Null, Value::Integer);
    if let Some(version) = patch.schema_version {
        columns.push(("ai_schema_version", int(version)));
    }
    if let Some(at) = patch.analyzed_at {
        columns.push(("ai_analyzed_at", int(at)));
    }

    let mut changed = false;
    if !columns.is_empty() {
        let sets: Vec<String> = (2..)
            .zip(&columns)
            .map(|(n, (column, _))| format!("{column} = ?{n}"))
            .collect();
        let differs: Vec<String> = (2..)
            .zip(&columns)
            .map(|(n, (column, _))| format!("{column} IS NOT ?{n}"))
            .collect();
        let sql = format!(
            "UPDATE posts SET {} WHERE id = ?1 AND ({})",
            sets.join(", "),
            differs.join(" OR ")
        );
        let values =
            std::iter::once(Value::Integer(post_id)).chain(columns.into_iter().map(|c| c.1));
        changed = conn
            .prepare_cached(&sql)?
            .execute(params_from_iter(values))?
            > 0;
    }
    if let Some(tags) = &patch.tags {
        changed |= tags::sync_ai_tags(
            conn,
            post_id,
            tags.as_deref().unwrap_or_default(),
            patch.general_tags.as_deref(),
            patch.specific_tags.as_deref(),
        )?;
    }
    if let Some(entities) = &patch.entities {
        changed |= tags::sync_entities(conn, post_id, entities.as_deref().unwrap_or_default())?;
    }
    if !changed {
        return Ok(false);
    }
    // A status that becomes `done` stamps the analysis time, unless the patch
    // gives one: the stamp follows a change, it is not a change of its own.
    let done = matches!(&patch.status, Some(Some(status)) if status == "done");
    if done && patch.analyzed_at.is_none() {
        conn.prepare_cached("UPDATE posts SET ai_analyzed_at = ?2 WHERE id = ?1")?
            .execute(params![post_id, now])?;
    }
    touch(conn, post_id, now)?;
    index::reindex_post(conn, post_id)?;
    Ok(true)
}

/// Moves posts to the trash, stamped `now`, and drops their index rows;
/// slides, tags and collection memberships stay for a restore
/// ([`crate::trash::put`]). Returns how many were not already in the trash.
///
/// # Errors
///
/// Database errors.
pub fn trash(conn: &Connection, post_ids: &[i64], now: i64) -> Result<usize> {
    crate::trash::put(conn, &crate::trash::by_ids(post_ids), now).map(|moved| moved.len())
}

/// Restores posts from the trash and indexes them again
/// ([`crate::trash::restore`]). Returns how many were in it.
///
/// # Errors
///
/// Database errors.
pub fn restore(conn: &Connection, post_ids: &[i64], now: i64) -> Result<usize> {
    crate::trash::restore(conn, &crate::trash::by_ids(post_ids), now).map(|back| back.len())
}

/// Deletes posts for good: their index rows, the post rows and everything that
/// cascades from them (slides, memberships, tags, entities, captures). Objects
/// left without references are stamped for the GC ([`crate::trash::purge`]).
/// Returns the posts deleted.
///
/// # Errors
///
/// Database errors.
pub fn purge(conn: &Connection, post_ids: &[i64], now: i64) -> Result<usize> {
    crate::trash::purge(conn, post_ids, now).map(|keys| keys.len())
}

// ── Internals ────────────────────────────────────────────────────────────────

/// Columns of a [`PostSummary`] row, read in this order by [`summary_from`].
const SUMMARY_COLUMNS: &str = "p.id, p.key, p.platform, p.shortcode, p.post_url, p.profile_url,
    p.author_username, p.author_name, p.caption, p.media_type, p.media_count, p.posted_at,
    p.imported_at, p.sort_ts, p.cover_url, p.thumbhash, p.archive_state, p.ai_status,
    p.ai_description, p.ai_category, p.ai_content_type, p.ai_language, p.ai_save_reason,
    p.ai_tags_json, p.ai_analyzed_at, p.user_note, p.user_tags_json, p.web_url, p.web_domain,
    p.web_final_url, p.updated_at, p.deleted_at, p.current_capture_id,
    co.sha256, co.ext, co.mime, co.bytes, co.width, co.height, co.duration_ms, co.variants";

const SUMMARY_FROM: &str = "posts p LEFT JOIN media_objects co ON co.id = p.cover_object";

/// Reads a row's columns in order.
struct Cols<'r, 's> {
    row: &'r Row<'s>,
    next: usize,
}

impl<'r, 's> Cols<'r, 's> {
    fn new(row: &'r Row<'s>) -> Self {
        Self { row, next: 0 }
    }

    fn next<T: FromSql>(&mut self) -> rusqlite::Result<T> {
        let v = self.row.get(self.next)?;
        self.next += 1;
        Ok(v)
    }

    fn object(&mut self) -> rusqlite::Result<Option<ObjectRef>> {
        let v = object_ref_at(self.row, self.next)?;
        self.next += 8;
        Ok(v)
    }
}

fn summary_from(c: &mut Cols<'_, '_>) -> rusqlite::Result<PostSummary> {
    Ok(PostSummary {
        id: c.next()?,
        key: c.next()?,
        platform: c.next()?,
        shortcode: c.next()?,
        post_url: c.next()?,
        profile_url: c.next()?,
        author_username: c.next()?,
        author_name: c.next()?,
        caption: c.next()?,
        media_type: c.next()?,
        media_count: c.next()?,
        posted_at: c.next()?,
        imported_at: c.next()?,
        sort_ts: c.next()?,
        cover_url: c.next()?,
        thumbhash: c.next()?,
        archive_state: c.next()?,
        ai_status: c.next()?,
        ai_description: c.next()?,
        ai_category: c.next()?,
        ai_content_type: c.next()?,
        ai_language: c.next()?,
        ai_save_reason: c.next()?,
        ai_tags: json_strings(c.next::<Option<String>>()?.as_deref()),
        ai_analyzed_at: c.next()?,
        user_note: c.next()?,
        user_tags: json_strings(c.next::<Option<String>>()?.as_deref()),
        web_url: c.next()?,
        web_domain: c.next()?,
        web_final_url: c.next()?,
        updated_at: c.next()?,
        deleted_at: c.next()?,
        current_capture_id: c.next()?,
        cover: c.object()?,
        media: Vec::new(),
        collection_ids: Vec::new(),
        web_capture: None,
    })
}

/// Loads slides, collection ids and captures for a page of posts.
fn attach(conn: &Connection, items: &mut [PostSummary]) -> Result<()> {
    if items.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = items.iter().map(|p| p.id).collect();
    let ids_json = id_list(&ids);

    let mut media_by_post: HashMap<i64, Vec<PostMedia>> = HashMap::new();
    let sql = format!(
        "SELECT pm.post_id, pm.position, pm.kind, pm.source_url, pm.width, pm.height,
                pm.duration_ms, pm.label, {}, {}
         FROM post_media pm
         LEFT JOIN media_objects o ON o.id = pm.object_id
         LEFT JOIN media_objects v ON v.id = pm.video_object_id
         WHERE pm.post_id IN (SELECT value FROM json_each(?1))
         ORDER BY pm.post_id, pm.position",
        object_columns("o"),
        object_columns("v")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query([&ids_json])?;
    while let Some(row) = rows.next()? {
        let mut c = Cols::new(row);
        let post_id: i64 = c.next()?;
        let slide = PostMedia {
            position: c.next()?,
            kind: c.next()?,
            source_url: c.next()?,
            width: c.next()?,
            height: c.next()?,
            duration_ms: c.next()?,
            label: c.next()?,
            object: c.object()?,
            video_object: c.object()?,
        };
        media_by_post.entry(post_id).or_default().push(slide);
    }

    let mut collections_by_post: HashMap<i64, Vec<i64>> = HashMap::new();
    let mut stmt = conn.prepare_cached(
        "SELECT post_id, collection_id FROM post_collections
         WHERE post_id IN (SELECT value FROM json_each(?1))
         ORDER BY post_id, collection_id",
    )?;
    let mut rows = stmt.query([&ids_json])?;
    while let Some(row) = rows.next()? {
        collections_by_post
            .entry(row.get(0)?)
            .or_default()
            .push(row.get(1)?);
    }

    let capture_ids: Vec<i64> = items.iter().filter_map(|p| p.current_capture_id).collect();
    let mut captures: HashMap<i64, WebCaptureSummary> = HashMap::new();
    if !capture_ids.is_empty() {
        let sql = format!(
            "SELECT wc.id, wc.captured_at, wc.requested_url, wc.final_url, wc.status, wc.partial,
                    wc.title, wc.palette_json, wc.fonts_json, wc.tech_json, wc.awards_json,
                    wc.meta_json, {}, {}
             FROM web_captures wc
             LEFT JOIN media_objects h ON h.id = wc.hero_object
             LEFT JOIN media_objects f ON f.id = wc.favicon_object
             WHERE wc.id IN (SELECT value FROM json_each(?1))",
            object_columns("h"),
            object_columns("f")
        );
        let mut stmt = conn.prepare_cached(&sql)?;
        let mut rows = stmt.query([id_list(&capture_ids)])?;
        while let Some(row) = rows.next()? {
            let mut c = Cols::new(row);
            let capture = WebCaptureSummary {
                id: c.next()?,
                captured_at: c.next()?,
                requested_url: c.next()?,
                final_url: c.next()?,
                status: c.next()?,
                partial: c.next::<i64>()? != 0,
                title: c.next()?,
                palette: json_value(c.next::<Option<String>>()?.as_deref()),
                fonts: json_value(c.next::<Option<String>>()?.as_deref()),
                tech: json_value(c.next::<Option<String>>()?.as_deref()),
                awards: json_value(c.next::<Option<String>>()?.as_deref()),
                meta: json_value(c.next::<Option<String>>()?.as_deref()),
                hero: c.object()?,
                favicon: c.object()?,
            };
            captures.insert(capture.id, capture);
        }
    }

    for post in items {
        post.media = media_by_post.remove(&post.id).unwrap_or_default();
        post.collection_ids = collections_by_post.remove(&post.id).unwrap_or_default();
        post.web_capture = post
            .current_capture_id
            .and_then(|id| captures.get(&id).cloned());
    }
    Ok(())
}

fn list_keyset(
    conn: &Connection,
    mut w: WhereSql,
    sort: Sort,
    limit: u32,
    cursor: Option<Cursor>,
) -> Result<Page<PostSummary>> {
    let newest = sort == Sort::Newest;
    let position = match (cursor, newest) {
        (None, _) => None,
        (Some(Cursor::Newest { sort_ts, id }), true)
        | (Some(Cursor::Oldest { sort_ts, id }), false) => Some((sort_ts, id)),
        _ => return Err(RepoError::InvalidCursor),
    };
    let (op, dir) = if newest { ("<", "DESC") } else { (">", "ASC") };
    if let Some((sort_ts, id)) = position {
        // `sort_ts <= x` (not only the OR) lets SQLite seek the index.
        w.push(
            format!("p.sort_ts {op}= ? AND (p.sort_ts {op} ? OR p.id {op} ?)"),
            [
                Value::Integer(sort_ts),
                Value::Integer(sort_ts),
                Value::Integer(id),
            ],
        );
    }
    let sql = format!(
        "SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM} WHERE {}
         ORDER BY p.sort_ts {dir}, p.id {dir} LIMIT ?",
        w.clauses.join(" AND ")
    );
    w.params.push(Value::Integer(i64::from(limit) + 1));
    let mut items: Vec<PostSummary> = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(w.params.iter()), |row| {
            summary_from(&mut Cols::new(row))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let next_cursor = if items.len() > limit as usize {
        items.truncate(limit as usize);
        items.last().map(|p| {
            if newest {
                Cursor::Newest {
                    sort_ts: p.sort_ts,
                    id: p.id,
                }
            } else {
                Cursor::Oldest {
                    sort_ts: p.sort_ts,
                    id: p.id,
                }
            }
        })
    } else {
        None
    };
    Ok(Page { items, next_cursor })
}

/// The relevance order of `filter`: the internal ids of its first
/// [`RELEVANCE_WINDOW`] posts, best first (ties newest first). It is the
/// snapshot that relevance pages are cut from ([`ranked_page`]): the API
/// computes it once per search and library state, and pages through it by
/// offset (plan §2.14). Empty without search text.
///
/// # Errors
///
/// Database errors.
pub fn rank(conn: &Connection, filter: &PostFilter) -> Result<Vec<i64>> {
    let text = TextPlan::new(filter);
    if !text.has_text {
        return Ok(Vec::new());
    }
    let scoring = scoring(conn, &text)?;
    let (sql, params) = rank_query(WhereSql::new(filter, &text), &text, &scoring);
    let ids = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The relevance cursor's offset, if `cursor` belongs to the relevance order.
///
/// # Errors
///
/// [`RepoError::InvalidCursor`] for a cursor of another order.
pub fn relevance_offset(cursor: Option<Cursor>) -> Result<u32> {
    match cursor {
        None => Ok(0),
        Some(Cursor::Relevance { offset }) => Ok(offset),
        Some(_) => Err(RepoError::InvalidCursor),
    }
}

/// One page of a relevance search: the posts at positions `offset..` of
/// `ranked` (a [`rank`] of `filter`), at most `limit` of them, and the cursor
/// of the next page. A post that moved into or out of the trash since the
/// ranking was taken is left out of the page; positions still count it, so
/// pages never overlap.
///
/// # Errors
///
/// Database errors.
pub fn ranked_page(
    conn: &Connection,
    filter: &PostFilter,
    ranked: &[i64],
    offset: u32,
    limit: u32,
) -> Result<Page<PostSummary>> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let start = (offset as usize).min(ranked.len());
    let end = start.saturating_add(limit as usize).min(ranked.len());
    let slice = &ranked[start..end];
    let mut items = Vec::with_capacity(slice.len());
    if !slice.is_empty() {
        let trash = if filter.trash {
            "p.deleted_at IS NOT NULL"
        } else {
            "p.deleted_at IS NULL"
        };
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS} FROM {SUMMARY_FROM}
             WHERE p.id IN (SELECT value FROM json_each(?1)) AND {trash}"
        );
        let mut found: HashMap<i64, PostSummary> = conn
            .prepare_cached(&sql)?
            .query_map([id_list(slice)], |row| summary_from(&mut Cols::new(row)))?
            .map(|r| r.map(|p| (p.id, p)))
            .collect::<rusqlite::Result<_>>()?;
        items.extend(slice.iter().filter_map(|id| found.remove(id)));
        attach(conn, &mut items)?;
    }
    let next = u32::try_from(end).unwrap_or(u32::MAX);
    let next_cursor = (end < ranked.len() && next < RELEVANCE_WINDOW)
        .then_some(Cursor::Relevance { offset: next });
    Ok(Page { items, next_cursor })
}

/// One scored FTS5 match of the relevance query.
#[derive(Clone, Debug, PartialEq)]
struct ScoreArm {
    kind: ArmKind,
    expr: String,
    /// For a token arm, `weight / idf`: it replaces bm25's IDF with the term's
    /// clamped weight. For an infix arm, the score itself (negative: a bonus).
    factor: f64,
}

/// What a [`ScoreArm`] matches against.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ArmKind {
    /// `posts_fts`: the posts it matches add `bm25 × factor` to their score.
    Tokens,
    /// `posts_infix`: the posts it matches add `factor`.
    Infix,
}

/// The inputs of the relevance score that depend on the library's statistics.
#[derive(Clone, Debug, Default, PartialEq)]
struct Scoring {
    arms: Vec<ScoreArm>,
    /// Distinct boost terms that some post has as a tag, with their bonus.
    tag_boosts: Vec<(String, f64)>,
}

/// The scoring of `text` (plan §2.14, [`crate::search::query`]), from document
/// frequencies counted on the caller's snapshot:
///
/// - for every scored term that matches at least one post, its prefix match
///   and, when some post has it as a whole token, its exact match;
/// - for every boost term that is some post's tag, the exact-tag bonus scaled
///   by the tag's weight (the desktop's `6 × idf(tag)`).
fn scoring(conn: &Connection, text: &TextPlan) -> Result<Scoring> {
    let mut out = Scoring::default();
    if text.score_terms.is_empty() && text.boost_terms.is_empty() {
        return Ok(out);
    }
    // The row count FTS5 uses for its IDF: one row per indexed post.
    let rows: i64 = conn
        .prepare_cached("SELECT count(*) FROM posts_fts_docsize")?
        .query_row([], |r| r.get(0))?;
    let rows = u64::try_from(rows).unwrap_or(0);
    let mut count =
        conn.prepare_cached("SELECT count(*) FROM posts_fts WHERE posts_fts MATCH ?1")?;
    let mut df = |expr: &str| -> Result<u64> {
        let n: i64 = count.query_row([expr], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    let mut count_infix =
        conn.prepare_cached("SELECT count(*) FROM posts_infix WHERE posts_infix MATCH ?1")?;
    let mut df_infix = |expr: &str| -> Result<u64> {
        let n: i64 = count_infix.query_row([expr], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    for term in &text.score_terms {
        let Some((prefix, exact)) = query::term_matches(term) else {
            continue;
        };
        let infix = query::infix_unit(term);
        let prefix_df = df(&prefix)?;
        // The weight comes from the token index, as SPIKE-5 tuned it. A term
        // found only inside longer tokens takes it from the infix index; a
        // prefix match is a substring match too, so it needs no count there.
        let weight = if prefix_df > 0 {
            query::term_weight(rows, prefix_df)
        } else {
            match &infix {
                Some(expr) => match df_infix(expr)? {
                    0 => continue,
                    infix_df => query::term_weight(rows, infix_df),
                },
                None => continue,
            }
        };
        if prefix_df > 0 {
            out.arms.push(ScoreArm {
                kind: ArmKind::Tokens,
                expr: prefix,
                factor: weight / query::fts5_idf(rows, prefix_df),
            });
            if let Some(exact) = exact {
                let exact_df = df(&exact)?;
                if exact_df > 0 {
                    out.arms.push(ScoreArm {
                        kind: ArmKind::Tokens,
                        expr: exact,
                        factor: query::EXACT_TOKEN_WEIGHT * weight
                            / query::fts5_idf(rows, exact_df),
                    });
                }
            }
        }
        if let Some(expr) = infix {
            out.arms.push(ScoreArm {
                kind: ArmKind::Infix,
                expr,
                factor: -query::INFIX_WEIGHT * weight,
            });
        }
    }
    let mut tag_df =
        conn.prepare_cached("SELECT count(DISTINCT post_id) FROM post_tags WHERE tag_norm = ?1")?;
    for norm in dedupe(text.boost_terms.iter().cloned()) {
        let n: i64 = tag_df.query_row([&norm], |r| r.get(0))?;
        let n = u64::try_from(n).unwrap_or(0);
        if n > 0 {
            let bonus = query::EXACT_TAG_BOOST * query::term_weight(rows, n);
            out.tag_boosts.push((norm, bonus));
        }
    }
    Ok(out)
}

/// The ranking query (plan §2.14): the ids of the first [`RELEVANCE_WINDOW`]
/// matches by score, the scored matches of every search unit minus the
/// exact-tag and phrase bonuses ([`scoring`]); lower is better, ties newest
/// first.
fn rank_query(w: WhereSql, text: &TextPlan, scoring: &Scoring) -> (String, Vec<Value>) {
    // Parameters bind in the order their `?` appear: WITH, SELECT list, WHERE.
    let mut params: Vec<Value> = Vec::new();
    let mut with = String::new();
    let mut from = "posts p".to_owned();
    let mut score = "0.0".to_owned();
    if !scoring.arms.is_empty() {
        // Scored once and materialized: as a subquery inside the join, SQLite
        // re-runs the FTS queries for every candidate row (seconds at 6k posts).
        // The arms get their own CTE: a lone arm flattened into the aggregate
        // makes bm25() fail ("unable to use function bm25 in the requested
        // context").
        let token_arm = format!(
            "SELECT rowid, {} * ? FROM posts_fts WHERE posts_fts MATCH ?",
            query::bm25_call()
        );
        let infix_arm = "SELECT rowid, ? FROM posts_infix WHERE posts_infix MATCH ?";
        let mut arms = Vec::with_capacity(scoring.arms.len());
        for arm in &scoring.arms {
            params.push(Value::Real(arm.factor));
            params.push(Value::Text(arm.expr.clone()));
            arms.push(match arm.kind {
                ArmKind::Tokens => token_arm.as_str(),
                ArmKind::Infix => infix_arm,
            });
        }
        with = format!(
            "WITH arms (rid, s) AS MATERIALIZED ({}),
                  hits (rid, bm) AS MATERIALIZED (SELECT rid, sum(s) FROM arms GROUP BY rid) ",
            arms.join(" UNION ALL ")
        );
        from.push_str(" LEFT JOIN hits h ON h.rid = p.id");
        score = "COALESCE(h.bm, 0.0)".to_owned();
    }
    if !scoring.tag_boosts.is_empty() {
        score.push_str(
            " - (SELECT total(b.value ->> 1) FROM json_each(?) b WHERE EXISTS
                   (SELECT 1 FROM post_tags pt WHERE pt.post_id = p.id AND pt.tag_norm = b.value ->> 0))",
        );
        params.push(Value::Text(
            serde_json::to_string(&scoring.tag_boosts).expect("tag boosts serialize"),
        ));
    }
    if let Some(phrase) = &text.phrase_expr {
        score.push_str(&format!(
            " - {:.4} * (p.id IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?))",
            query::PHRASE_BONUS
        ));
        params.push(Value::Text(phrase.clone()));
    }
    params.extend(w.params);
    params.push(Value::Integer(i64::from(RELEVANCE_WINDOW)));
    let sql = format!(
        "{with}SELECT p.id, {score} AS score FROM {from} WHERE {}
         ORDER BY score, p.sort_ts DESC, p.id DESC LIMIT ?",
        w.clauses.join(" AND ")
    );
    (sql, params)
}

/// The search side of a filter: FTS blocks, hybrid tags and the score inputs.
struct TextPlan {
    has_text: bool,
    /// Membership clauses of the search blocks, combined by `joiner`.
    blocks: Vec<(String, Vec<Value>)>,
    joiner: &'static str,
    /// Cleaned `tags` (trimmed, lowercased, deduped).
    tags: Vec<String>,
    /// Tags join the search blocks instead of filtering.
    hybrid: bool,
    /// The scored units: the query's content terms, then the concepts.
    score_terms: Vec<String>,
    phrase_expr: Option<String>,
    /// Exact-tag boost candidates.
    boost_terms: Vec<String>,
}

const FTS_BLOCK: &str = "p.id IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?)";
const INFIX_BLOCK: &str = "p.id IN (SELECT rowid FROM posts_infix WHERE posts_infix MATCH ?)";
const TEXT_BLOCK: &str = "p.id IN (SELECT rowid FROM posts_fts WHERE posts_fts MATCH ?
                                   UNION SELECT rowid FROM posts_infix WHERE posts_infix MATCH ?)";
const TAG_EXISTS: &str =
    "EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = p.id AND pt.tag_norm = ?)";

impl TextPlan {
    fn new(filter: &PostFilter) -> Self {
        let tags = clean_norms(&filter.tags);
        let q = non_blank(filter.q.as_deref());
        let concepts = clean_concepts(&filter.concepts);
        let hybrid = !tags.is_empty() && q.is_some();
        let mut blocks = Vec::new();
        let mut score_terms = Vec::new();
        let mut boost_terms = Vec::new();
        let mut phrase_expr = None;

        if let Some(q) = q {
            let parsed = TextQuery::parse(q);
            blocks.push(text_block(
                parsed.match_expr.clone(),
                parsed.infix_expr.clone(),
            ));
            score_terms.extend(parsed.terms.iter().cloned());
            boost_terms.extend(parsed.terms);
            phrase_expr = parsed.phrase_expr;
        }
        for concept in &concepts {
            let unit = query::prefix_unit(concept);
            blocks.push(text_block(
                unit.as_ref()
                    .and_then(|u| query::any_of(std::slice::from_ref(u))),
                query::infix_unit(concept),
            ));
            score_terms.push(concept.clone());
            boost_terms.push(concept.to_lowercase());
        }
        if hybrid {
            for tag in &tags {
                if filter.tag_mode != Mode::And {
                    blocks.push((TAG_EXISTS.to_owned(), vec![Value::Text(tag.clone())]));
                }
                boost_terms.push(tag.clone());
            }
        }
        Self {
            has_text: q.is_some() || !concepts.is_empty(),
            blocks,
            joiner: if filter.concept_mode == Mode::And {
                " AND "
            } else {
                " OR "
            },
            tags,
            hybrid,
            score_terms,
            phrase_expr,
            boost_terms,
        }
    }
}

/// The membership clause of one search block: the posts the token index
/// matches with `tokens`, plus those the infix index matches with `infix`.
fn text_block(tokens: Option<String>, infix: Option<String>) -> (String, Vec<Value>) {
    match (tokens, infix) {
        (Some(tokens), Some(infix)) => (
            TEXT_BLOCK.to_owned(),
            vec![Value::Text(tokens), Value::Text(infix)],
        ),
        (Some(tokens), None) => (FTS_BLOCK.to_owned(), vec![Value::Text(tokens)]),
        (None, Some(infix)) => (INFIX_BLOCK.to_owned(), vec![Value::Text(infix)]),
        // Nothing indexable in the text: the block matches no post.
        (None, None) => ("0".to_owned(), Vec::new()),
    }
}

/// The condition on `posts p` that selects the posts of `filter`, and its
/// parameters in order: what the list, its count and a selection by filter
/// ([`crate::selector`]) share, so they always agree.
pub(crate) fn filter_condition(filter: &PostFilter) -> (String, Vec<Value>) {
    let text = TextPlan::new(filter);
    let w = WhereSql::new(filter, &text);
    (w.clauses.join(" AND "), w.params)
}

/// WHERE clauses (ANDed) and their parameters, in order.
struct WhereSql {
    clauses: Vec<String>,
    params: Vec<Value>,
}

impl WhereSql {
    fn new(filter: &PostFilter, text: &TextPlan) -> Self {
        let mut w = Self {
            // Written literally so the partial indexes on `deleted_at IS NULL` apply.
            clauses: vec![
                if filter.trash {
                    "p.deleted_at IS NOT NULL"
                } else {
                    "p.deleted_at IS NULL"
                }
                .to_owned(),
            ],
            params: Vec::new(),
        };
        if let Some(platform) = filter.platform {
            w.push(
                "p.platform = ?",
                [Value::Text(platform.as_str().to_owned())],
            );
        }
        match filter.source {
            Some(SourceBucket::Web) => w.push("p.platform = 'web'", []),
            Some(SourceBucket::Social) => w.push("p.platform <> 'web'", []),
            None => {}
        }
        if let Some(id) = filter.collection_id {
            w.push(
                "p.id IN (SELECT pc.post_id FROM post_collections pc WHERE pc.collection_id = ?)",
                [Value::Integer(id)],
            );
        }
        let media_types = dedupe(filter.media_types.iter().map(|m| js_trim(m).to_owned()));
        if !media_types.is_empty() {
            w.push(
                format!("p.media_type IN ({})", placeholders(media_types.len())),
                media_types.into_iter().map(Value::Text),
            );
        }
        if let Some(category) = non_blank(filter.category.as_deref()) {
            w.push("p.ai_category = ?", [Value::Text(category.to_owned())]);
        }
        if let Some(content_type) = non_blank(filter.content_type.as_deref()) {
            w.push(
                "p.ai_content_type = ?",
                [Value::Text(content_type.to_owned())],
            );
        }
        if let Some(tag) = non_blank(filter.tag.as_deref()) {
            w.push(TAG_EXISTS, [Value::Text(tag.to_lowercase())]);
        }
        if !text.tags.is_empty() {
            if filter.tag_mode == Mode::And {
                for tag in &text.tags {
                    w.push(TAG_EXISTS, [Value::Text(tag.clone())]);
                }
            } else if !text.hybrid {
                w.push(
                    format!(
                        "EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = p.id
                                 AND pt.tag_norm IN ({}))",
                        placeholders(text.tags.len())
                    ),
                    text.tags.iter().cloned().map(Value::Text),
                );
            }
        }
        if let Some(entity) = non_blank(filter.entity.as_deref()) {
            w.push(
                "EXISTS (SELECT 1 FROM post_entities pe WHERE pe.post_id = p.id AND pe.ent_norm = ?)",
                [Value::Text(entity.to_lowercase())],
            );
        }
        match filter.analyzed {
            Some(true) => w.push("p.ai_status = 'done'", []),
            Some(false) => w.push("(p.ai_status IS NULL OR p.ai_status <> 'done')", []),
            None => {}
        }
        if let Some(status) = non_blank(filter.ai_status.as_deref()) {
            w.push("p.ai_status = ?", [Value::Text(status.to_owned())]);
        }
        let ai_tag =
            "EXISTS (SELECT 1 FROM post_tags pt WHERE pt.post_id = p.id AND pt.source = 'ai')";
        match filter.ai_tagged {
            Some(true) => w.push(ai_tag, []),
            Some(false) => w.push(format!("NOT {ai_tag}"), []),
            None => {}
        }
        let stored = "(p.cover_object IS NOT NULL OR EXISTS (SELECT 1 FROM post_media pm
            WHERE pm.post_id = p.id AND (pm.object_id IS NOT NULL OR pm.video_object_id IS NOT NULL)))";
        match filter.stored {
            Some(true) => w.push(stored, []),
            Some(false) => w.push(format!("NOT {stored}"), []),
            None => {}
        }
        if let Some(from) = filter.date_from {
            w.push("p.sort_ts >= ?", [Value::Integer(from)]);
        }
        if let Some(to) = filter.date_to {
            w.push("p.sort_ts < ?", [Value::Integer(to)]);
        }
        if !text.blocks.is_empty() {
            let clause = text
                .blocks
                .iter()
                .map(|(c, _)| c.as_str())
                .collect::<Vec<_>>()
                .join(text.joiner);
            let params: Vec<Value> = text
                .blocks
                .iter()
                .flat_map(|(_, p)| p.iter().cloned())
                .collect();
            w.push(format!("({clause})"), params);
        }
        w
    }

    fn push(&mut self, clause: impl Into<String>, params: impl IntoIterator<Item = Value>) {
        self.clauses.push(clause.into());
        self.params.extend(params);
    }
}

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(", ")
}

fn non_blank(s: Option<&str>) -> Option<&str> {
    s.map(js_trim).filter(|s| !s.is_empty())
}

fn dedupe(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for item in items {
        if !item.is_empty() && !out.contains(&item) {
            out.push(item);
        }
    }
    out
}

/// Tag filter values: trimmed, lowercased, deduped (desktop `cleanedTags`).
fn clean_norms(items: &[String]) -> Vec<String> {
    dedupe(items.iter().map(|t| js_trim(t).to_lowercase()))
}

/// Concepts: trimmed and deduped, case preserved (desktop `conceptList`).
fn clean_concepts(items: &[String]) -> Vec<String> {
    dedupe(items.iter().map(|c| js_trim(c).to_owned()))
}

fn ensure_exists(conn: &Connection, post_id: i64) -> Result<()> {
    let exists: bool = conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM posts WHERE id = ?1)")?
        .query_row([post_id], |r| r.get(0))?;
    if exists {
        Ok(())
    } else {
        Err(RepoError::NotFound)
    }
}

/// Stamps `updated_at` on a post that an edit changed.
fn touch(conn: &Connection, post_id: i64, now: i64) -> Result<()> {
    conn.prepare_cached("UPDATE posts SET updated_at = ?2 WHERE id = ?1")?
        .execute(params![post_id, now])?;
    Ok(())
}

fn validate(post: &NewPost) -> Result<()> {
    let invalid = |field, reason| Err(RepoError::Invalid { field, reason });
    if js_trim(&post.key).is_empty() || post.key.len() > 200 {
        return invalid("key", "must be 1-200 bytes");
    }
    if js_trim(&post.native_id).is_empty() {
        return invalid("nativeId", "is required");
    }
    if js_trim(&post.media_type).is_empty() {
        return invalid("mediaType", "is required");
    }
    if post
        .caption
        .as_deref()
        .is_some_and(|c| c.chars().count() > CAPTION_MAX_CHARS)
    {
        return invalid("caption", "is longer than 20000 characters");
    }
    if post
        .media
        .iter()
        .any(|m| !matches!(m.kind.as_str(), "image" | "video" | "file" | "page"))
    {
        return invalid("media.kind", "must be image, video, file or page");
    }
    Ok(())
}

/// Writes (or, with `None`, clears) the AI columns and AI-derived rows.
fn write_ai(conn: &Connection, post_id: i64, ai: Option<&AiLayer>, now: i64) -> Result<()> {
    let empty = AiLayer::default();
    let layer = ai.unwrap_or(&empty);
    let web = layer
        .web
        .as_ref()
        .map(|v| serde_json::to_string(v).expect("JSON values serialize"));
    conn.prepare_cached(
        "UPDATE posts SET ai_status = ?2, ai_provider = ?3, ai_model = ?4, ai_schema_version = ?5,
                ai_description = ?6, ai_save_reason = ?7, ai_language = ?8, ai_category = ?9,
                ai_content_type = ?10, ai_tags_json = ?11, ai_entities_json = ?12,
                ai_keywords_json = ?13, ai_web_json = ?14, ai_analyzed_at = ?15, updated_at = ?16
         WHERE id = ?1",
    )?
    .execute(params![
        post_id,
        layer.status,
        layer.provider,
        layer.model,
        layer.schema_version,
        layer.description,
        layer.save_reason,
        layer.language,
        layer.category,
        layer.content_type,
        json_array_or_null(&layer.tags),
        json_array_or_null(&layer.entities),
        json_array_or_null(&layer.keywords),
        web,
        layer.analyzed_at,
        now
    ])?;
    tags::sync_ai_tags(
        conn,
        post_id,
        &layer.tags,
        layer.general_tags.as_deref(),
        layer.specific_tags.as_deref(),
    )?;
    tags::sync_entities(conn, post_id, &layer.entities)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_round_trips() {
        for c in [
            Cursor::Newest {
                sort_ts: 1_700_000_000_000,
                id: 42,
            },
            Cursor::Oldest { sort_ts: -5, id: 1 },
            Cursor::Relevance { offset: 120 },
        ] {
            assert_eq!(Cursor::parse(&c.to_string()).unwrap(), c);
        }
        for bad in ["", "n", "n.1", "n.1.2.3", "x.1.2", "r.-1", "n.a.b", "r.1.2"] {
            assert!(
                matches!(Cursor::parse(bad), Err(RepoError::InvalidCursor)),
                "{bad}"
            );
        }
    }

    #[test]
    fn has_text_ignores_blank_input() {
        let mut f = PostFilter {
            q: Some("  ".into()),
            concepts: vec![" ".into()],
            ..PostFilter::default()
        };
        assert!(!f.has_text());
        f.concepts.push("lamp".into());
        assert!(f.has_text());
    }

    fn memory_library() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        crate::schema::migrate(&mut conn, crate::schema::Kind::Library).unwrap();
        conn
    }

    #[test]
    fn scoring_follows_term_statistics() {
        let conn = memory_library();
        for (i, caption) in ["lampada vetro", "lampadario", "sedia", "tavolo"]
            .into_iter()
            .enumerate()
        {
            let mut post = NewPost::new(
                format!("ig_{i}"),
                Platform::Instagram,
                i.to_string(),
                "image",
                0,
            );
            post.caption = Some(caption.into());
            if i == 2 {
                post.user_tags = vec!["Vetro".into()];
            }
            insert(&conn, &post, 0).unwrap();
        }
        let text = TextPlan::new(&PostFilter {
            q: Some("lampada vetro assente".into()),
            concepts: vec!["!!!".into()],
            ..PostFilter::default()
        });
        let s = scoring(&conn, &text).unwrap();
        let (lampada, lampada_exact) = query::term_matches("lampada").unwrap();
        let (vetro, vetro_exact) = query::term_matches("vetro").unwrap();
        let (lampada_exact, vetro_exact) = (lampada_exact.unwrap(), vetro_exact.unwrap());
        let (lampada_infix, vetro_infix) = (
            query::infix_unit("lampada").unwrap(),
            query::infix_unit("vetro").unwrap(),
        );
        let arms: Vec<(ArmKind, &str)> = s.arms.iter().map(|a| (a.kind, a.expr.as_str())).collect();
        // Terms without a match ("assente") or without an indexable character
        // ("!!!") are not scored; "lampadario" is a prefix hit only.
        assert_eq!(
            arms,
            [
                (ArmKind::Tokens, lampada.as_str()),
                (ArmKind::Tokens, &lampada_exact),
                (ArmKind::Infix, &lampada_infix),
                (ArmKind::Tokens, &vetro),
                (ArmKind::Tokens, &vetro_exact),
                (ArmKind::Infix, &vetro_infix),
            ]
        );
        // 4 rows: "lampada"* is in 2 of them, the token "lampada" in 1; "vetro"
        // is in 2 (a caption and a tag).
        let weight = query::term_weight(4, 2);
        let expected = [
            weight / query::fts5_idf(4, 2),
            query::EXACT_TOKEN_WEIGHT * weight / query::fts5_idf(4, 1),
            -query::INFIX_WEIGHT * weight,
            weight / query::fts5_idf(4, 2),
            query::EXACT_TOKEN_WEIGHT * weight / query::fts5_idf(4, 2),
            -query::INFIX_WEIGHT * weight,
        ];
        for (arm, factor) in s.arms.iter().zip(expected) {
            assert!((arm.factor - factor).abs() < 1e-9, "{arm:?}");
        }
        // Only "vetro" is a tag, of 1 post in 4.
        assert_eq!(
            s.tag_boosts,
            [(
                "vetro".to_owned(),
                query::EXACT_TAG_BOOST * query::term_weight(4, 1)
            )]
        );
        assert_eq!(
            scoring(&conn, &TextPlan::new(&PostFilter::default())).unwrap(),
            Scoring::default()
        );
    }

    #[test]
    fn relevance_runs_with_one_or_no_scored_match() {
        let conn = memory_library();
        let mut a = NewPost::new("ig_1", Platform::Instagram, "1", "image", 0);
        a.caption = Some("posters on the wall".into());
        let mut b = NewPost::new("ig_2", Platform::Instagram, "2", "image", 0);
        b.user_tags = vec!["glass".into()];
        let mut c = NewPost::new("ig_3", Platform::Instagram, "3", "image", 0);
        c.caption = Some("#productdesign".into());
        insert(&conn, &a, 0).unwrap();
        insert(&conn, &b, 0).unwrap();
        insert(&conn, &c, 0).unwrap();
        let page = |filter: PostFilter| -> Vec<String> {
            let req = PageRequest {
                sort: Sort::Relevance,
                ..PageRequest::default()
            };
            list(&conn, &filter, &req)
                .unwrap()
                .items
                .into_iter()
                .map(|p| p.key)
                .collect()
        };
        let kinds = |filter: &PostFilter| -> Vec<ArmKind> {
            scoring(&conn, &TextPlan::new(filter))
                .unwrap()
                .arms
                .iter()
                .map(|a| a.kind)
                .collect()
        };
        // "poster" has a prefix match and no whole-token match: one token arm
        // (and its infix arm).
        let one = PostFilter {
            q: Some("poster".into()),
            ..PostFilter::default()
        };
        assert_eq!(kinds(&one), [ArmKind::Tokens, ArmKind::Infix]);
        assert_eq!(page(one), ["ig_1"]);
        // "design" is only inside a longer token: the infix arm alone.
        let inside = PostFilter {
            q: Some("design".into()),
            ..PostFilter::default()
        };
        assert_eq!(kinds(&inside), [ArmKind::Infix]);
        assert_eq!(page(inside), ["ig_3"]);
        // No term matches: the hybrid tag alone admits and scores the post.
        let none = PostFilter {
            q: Some("zzz".into()),
            tags: vec!["glass".into()],
            ..PostFilter::default()
        };
        assert!(
            scoring(&conn, &TextPlan::new(&none))
                .unwrap()
                .arms
                .is_empty()
        );
        assert_eq!(page(none), ["ig_2"]);
    }

    #[test]
    fn relevance_scores_the_fts_query_once() {
        let conn = memory_library();
        let filter = PostFilter {
            q: Some("lampada vetro".into()),
            tags: vec!["glass".into()],
            ..PostFilter::default()
        };
        let text = TextPlan::new(&filter);
        let mut arms = Vec::new();
        for term in &text.score_terms {
            let (prefix, exact) = query::term_matches(term).unwrap();
            for expr in std::iter::once(prefix).chain(exact) {
                arms.push(ScoreArm {
                    kind: ArmKind::Tokens,
                    expr,
                    factor: 1.0,
                });
            }
            arms.push(ScoreArm {
                kind: ArmKind::Infix,
                expr: query::infix_unit(term).unwrap(),
                factor: -1.0,
            });
        }
        let inputs = Scoring {
            arms,
            tag_boosts: vec![("glass".into(), 3.0)],
        };
        let token_arms = inputs
            .arms
            .iter()
            .filter(|a| a.kind == ArmKind::Tokens)
            .count();
        assert_eq!((token_arms, inputs.arms.len()), (4, 6));
        let (sql, params) = rank_query(WhereSql::new(&filter, &text), &text, &inputs);
        let nodes: Vec<(i64, i64, String)> = conn
            .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map(params_from_iter(params.iter()), |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(3)?))
            })
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        let text: Vec<&str> = nodes.iter().map(|n| n.2.as_str()).collect();
        let plan = text.join("\n");
        assert!(plan.contains("MATERIALIZE arms"), "{plan}");
        assert!(plan.contains("MATERIALIZE hits"), "{plan}");
        // The token index is scanned once per scored match, once for the
        // search block and once for the phrase bonus, the infix index once per
        // infix match and once for the search block; never inside a correlated
        // (per-row) subquery: a per-row bm25 lookup made relevance take seconds
        // at 6k posts.
        let scans = |table: &str| -> Vec<i64> {
            nodes
                .iter()
                .filter(|n| n.2.starts_with(&format!("SCAN {table}")))
                .map(|n| n.0)
                .collect()
        };
        let (fts_scans, infix_scans) = (scans("posts_fts"), scans("posts_infix"));
        assert_eq!(fts_scans.len(), token_arms + 2, "{plan}");
        assert_eq!(
            infix_scans.len(),
            inputs.arms.len() - token_arms + 1,
            "{plan}"
        );
        for id in fts_scans.into_iter().chain(infix_scans) {
            let mut parent = nodes.iter().find(|n| n.0 == id).map_or(0, |n| n.1);
            while parent != 0 {
                let node = nodes.iter().find(|n| n.0 == parent).unwrap();
                assert!(!node.2.starts_with("CORRELATED"), "{plan}");
                parent = node.1;
            }
        }
    }
}
