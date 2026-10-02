//! The JSON shapes of the library API (plan §2.9): posts, slides, stored
//! objects, captures, stats and collections.
//!
//! They are the wire contract, kept apart from the core's domain types so the
//! core can change without changing the API:
//!
//! - camelCase fields, timestamps in unix milliseconds, posts addressed by
//!   `key`: the internal row id never leaves the server;
//! - every field is always present, `null` when it has no value and `[]` when a
//!   list is empty (so the OpenAPI document marks them `required`); the one
//!   exception is [`PostPage::total`], sent only when asked for;
//! - binary data travels as standard, padded base64 (RFC 4648 §4): `thumbhash`;
//! - a stored object comes with its `/media/…` URLs (plan §2.9 Media, D5): same
//!   origin, authorized by the session cookie, immutable.
//!
//! Closed sets of values are typed in the document: platform, archive state
//! and slide kind (the schema's `CHECK` constraints) and media type (the
//! desktop's seven types, which ingest and migration keep to). The wire value
//! is the stored text. Open sets (AI status, category) stay strings.

use std::collections::BTreeMap;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use shelfy_core::repo::{self, collections, posts, stats};
use utoipa::ToSchema;

/// Source platform of a post.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Instagram.
    Instagram,
    /// X (named `twitter`, as on the desktop).
    Twitter,
    /// Pinterest.
    Pinterest,
    /// A captured website.
    Web,
    /// A manual bookmark (upload or link).
    Manual,
}

impl From<repo::Platform> for Platform {
    fn from(platform: repo::Platform) -> Self {
        match platform {
            repo::Platform::Instagram => Self::Instagram,
            repo::Platform::Twitter => Self::Twitter,
            repo::Platform::Pinterest => Self::Pinterest,
            repo::Platform::Web => Self::Web,
            repo::Platform::Manual => Self::Manual,
        }
    }
}

impl From<Platform> for repo::Platform {
    fn from(platform: Platform) -> Self {
        match platform {
            Platform::Instagram => Self::Instagram,
            Platform::Twitter => Self::Twitter,
            Platform::Pinterest => Self::Pinterest,
            Platform::Web => Self::Web,
            Platform::Manual => Self::Manual,
        }
    }
}

/// Kind of post (`posts.media_type`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    /// One image.
    Image,
    /// Several images (X).
    Images,
    /// Images and videos (Instagram).
    Carousel,
    /// A video.
    Video,
    /// Text only.
    Text,
    /// A captured website.
    Website,
    /// A manual bookmark of a file that is neither image nor video.
    File,
}

impl MediaType {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Images => "images",
            Self::Carousel => "carousel",
            Self::Video => "video",
            Self::Text => "text",
            Self::Website => "website",
            Self::File => "file",
        }
    }
}

/// Archive progress of a post's media (plan §2.13).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveState {
    /// Not fetched yet.
    Pending,
    /// Some slides are stored.
    Partial,
    /// Every slide that can be stored is.
    Done,
    /// Fetching failed for good.
    Failed,
    /// Waiting for the browser extension to upload the bytes.
    Client,
    /// Kept as a link only (quota or policy).
    LinkOnly,
}

/// Kind of slide (`post_media.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SlideKind {
    /// An image.
    Image,
    /// A video (its `object` is the poster).
    Video,
    /// A file of a manual bookmark.
    File,
    /// A page of a captured website.
    Page,
}

/// A stored object (plan §2.13). Its URLs are same-origin and immutable.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MediaObject {
    /// The stored bytes: `/media/<sha256>.<ext>`.
    pub url: String,
    /// The 480 px WebP rendition, `/media/<sha256>.g480.webp`; `null` when it
    /// does not exist.
    #[schema(required = true)]
    pub g480_url: Option<String>,
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
    /// MIME type.
    pub mime: String,
    /// Size in bytes.
    pub bytes: i64,
    /// Pixel width, when known.
    #[schema(required = true)]
    pub width: Option<i64>,
    /// Pixel height, when known.
    #[schema(required = true)]
    pub height: Option<i64>,
    /// Duration of a video, when known.
    #[schema(required = true)]
    pub duration_ms: Option<i64>,
}

impl From<repo::ObjectRef> for MediaObject {
    fn from(object: repo::ObjectRef) -> Self {
        Self {
            url: format!("/media/{}.{}", object.sha256, object.ext),
            g480_url: object
                .has_g480
                .then(|| format!("/media/{}.g480.webp", object.sha256)),
            sha256: object.sha256,
            mime: object.mime,
            bytes: object.bytes,
            width: object.width,
            height: object.height,
            duration_ms: object.duration_ms,
        }
    }
}

/// One slide of a post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostMedia {
    /// 0-based order.
    pub position: i64,
    /// What the slide is.
    #[schema(value_type = SlideKind)]
    pub kind: String,
    /// Remote URL of the slide; it may have expired.
    #[schema(required = true)]
    pub source_url: Option<String>,
    /// Pixel width, when known.
    #[schema(required = true)]
    pub width: Option<i64>,
    /// Pixel height, when known.
    #[schema(required = true)]
    pub height: Option<i64>,
    /// Video duration, when known.
    #[schema(required = true)]
    pub duration_ms: Option<i64>,
    /// Caption of the slide (file name, page title).
    #[schema(required = true)]
    pub label: Option<String>,
    /// The stored image, or the poster of a video.
    #[schema(required = true)]
    pub object: Option<MediaObject>,
    /// The kept ("offline") video.
    #[schema(required = true)]
    pub video_object: Option<MediaObject>,
}

impl From<posts::PostMedia> for PostMedia {
    fn from(m: posts::PostMedia) -> Self {
        Self {
            position: m.position,
            kind: m.kind,
            source_url: m.source_url,
            width: m.width,
            height: m.height,
            duration_ms: m.duration_ms,
            label: m.label,
            object: m.object.map(Into::into),
            video_object: m.video_object.map(Into::into),
        }
    }
}

/// The current capture of a website, without its page texts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct WebCapture {
    /// Capture id.
    pub id: i64,
    /// When it was captured.
    pub captured_at: i64,
    /// URL the capture started from.
    #[schema(required = true)]
    pub requested_url: Option<String>,
    /// URL after redirects.
    #[schema(required = true)]
    pub final_url: Option<String>,
    /// Capture status.
    pub status: String,
    /// Whether some pages failed.
    pub partial: bool,
    /// Page title.
    #[schema(required = true)]
    pub title: Option<String>,
    /// Color palette, as captured.
    #[schema(required = true)]
    pub palette: Option<serde_json::Value>,
    /// Fonts, as captured.
    #[schema(required = true)]
    pub fonts: Option<serde_json::Value>,
    /// Detected technologies, as captured.
    #[schema(required = true)]
    pub tech: Option<serde_json::Value>,
    /// Awards, as captured.
    #[schema(required = true)]
    pub awards: Option<serde_json::Value>,
    /// Page metadata, as captured.
    #[schema(required = true)]
    pub meta: Option<serde_json::Value>,
    /// Hero screenshot.
    #[schema(required = true)]
    pub hero: Option<MediaObject>,
    /// Favicon.
    #[schema(required = true)]
    pub favicon: Option<MediaObject>,
}

impl From<posts::WebCaptureSummary> for WebCapture {
    fn from(c: posts::WebCaptureSummary) -> Self {
        Self {
            id: c.id,
            captured_at: c.captured_at,
            requested_url: c.requested_url,
            final_url: c.final_url,
            status: c.status,
            partial: c.partial,
            title: c.title,
            palette: c.palette,
            fonts: c.fonts,
            tech: c.tech,
            awards: c.awards,
            meta: c.meta,
            hero: c.hero.map(Into::into),
            favicon: c.favicon.map(Into::into),
        }
    }
}

/// A post as the gallery shows it; `PostDetail` adds the heavy fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Post {
    /// Public id (plan §2.8), for example `ig_3141592653589793238`.
    pub key: String,
    /// Source platform.
    pub platform: Platform,
    /// Instagram shortcode.
    #[schema(required = true)]
    pub shortcode: Option<String>,
    /// Link to the original post (for websites: the final URL).
    #[schema(required = true)]
    pub post_url: Option<String>,
    /// Link to the author's profile.
    #[schema(required = true)]
    pub profile_url: Option<String>,
    /// Author handle (for websites: the domain).
    #[schema(required = true)]
    pub author_username: Option<String>,
    /// Author display name (for websites: the title).
    #[schema(required = true)]
    pub author_name: Option<String>,
    /// Caption.
    #[schema(required = true)]
    pub caption: Option<String>,
    /// Kind of post.
    #[schema(value_type = MediaType)]
    pub media_type: String,
    /// Number of slides.
    pub media_count: i64,
    /// Publication time, when known.
    #[schema(required = true)]
    pub posted_at: Option<i64>,
    /// When the post entered the library.
    pub imported_at: i64,
    /// Sort key of the newest/oldest orders: `postedAt`, else `importedAt`.
    pub sort_ts: i64,
    /// The stored cover.
    #[schema(required = true)]
    pub cover: Option<MediaObject>,
    /// Remote cover URL; it may have expired.
    #[schema(required = true)]
    pub cover_url: Option<String>,
    /// ThumbHash placeholder of the cover (≤ 25 bytes), standard base64.
    #[schema(required = true)]
    pub thumbhash: Option<String>,
    /// Archive progress.
    #[schema(value_type = ArchiveState)]
    pub archive_state: String,
    /// AI lifecycle status; `null` when never analyzed.
    #[schema(required = true)]
    pub ai_status: Option<String>,
    /// AI description.
    #[schema(required = true)]
    pub ai_description: Option<String>,
    /// AI category.
    #[schema(required = true)]
    pub ai_category: Option<String>,
    /// AI content type.
    #[schema(required = true)]
    pub ai_content_type: Option<String>,
    /// AI-detected language.
    #[schema(required = true)]
    pub ai_language: Option<String>,
    /// AI guess of why the post was saved.
    #[schema(required = true)]
    pub ai_save_reason: Option<String>,
    /// AI tags, as produced (display forms).
    pub ai_tags: Vec<String>,
    /// When the AI analysis finished.
    #[schema(required = true)]
    pub ai_analyzed_at: Option<i64>,
    /// The user's note.
    #[schema(required = true)]
    pub user_note: Option<String>,
    /// The user's tags (display forms).
    pub user_tags: Vec<String>,
    /// Website URL as saved.
    #[schema(required = true)]
    pub web_url: Option<String>,
    /// Website domain.
    #[schema(required = true)]
    pub web_domain: Option<String>,
    /// Website URL after redirects.
    #[schema(required = true)]
    pub web_final_url: Option<String>,
    /// Last change of the post.
    pub updated_at: i64,
    /// When the post went to the trash; `null` outside the trash.
    #[schema(required = true)]
    pub deleted_at: Option<i64>,
    /// Slides, in order.
    pub media: Vec<PostMedia>,
    /// Collections the post belongs to.
    pub collection_ids: Vec<i64>,
    /// The current capture of a website.
    #[schema(required = true)]
    pub web_capture: Option<WebCapture>,
}

impl From<posts::PostSummary> for Post {
    fn from(p: posts::PostSummary) -> Self {
        Self {
            key: p.key,
            platform: p.platform.into(),
            shortcode: p.shortcode,
            post_url: p.post_url,
            profile_url: p.profile_url,
            author_username: p.author_username,
            author_name: p.author_name,
            caption: p.caption,
            media_type: p.media_type,
            media_count: p.media_count,
            posted_at: p.posted_at,
            imported_at: p.imported_at,
            sort_ts: p.sort_ts,
            cover: p.cover.map(Into::into),
            cover_url: p.cover_url,
            thumbhash: p.thumbhash.map(|bytes| STANDARD.encode(bytes)),
            archive_state: p.archive_state,
            ai_status: p.ai_status,
            ai_description: p.ai_description,
            ai_category: p.ai_category,
            ai_content_type: p.ai_content_type,
            ai_language: p.ai_language,
            ai_save_reason: p.ai_save_reason,
            ai_tags: p.ai_tags,
            ai_analyzed_at: p.ai_analyzed_at,
            user_note: p.user_note,
            user_tags: p.user_tags,
            web_url: p.web_url,
            web_domain: p.web_domain,
            web_final_url: p.web_final_url,
            updated_at: p.updated_at,
            deleted_at: p.deleted_at,
            media: p.media.into_iter().map(Into::into).collect(),
            collection_ids: p.collection_ids,
            web_capture: p.web_capture.map(Into::into),
        }
    }
}

/// Origin of a tag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TagSource {
    /// Produced by the AI analysis.
    Ai,
    /// Added by the user.
    Manual,
}

/// A tag of a post, after alias resolution. An AI tag and a manual tag with
/// the same name are two entries.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostTag {
    /// Display form.
    pub tag: String,
    /// Normalized form, the matching key of the `tag` and `tags` filters.
    pub norm: String,
    /// AI or manual.
    pub source: TagSource,
    /// `general` or `specific`, for AI tags with a tier.
    #[schema(required = true)]
    pub tier: Option<String>,
}

impl From<posts::PostTag> for PostTag {
    fn from(t: posts::PostTag) -> Self {
        Self {
            tag: t.tag,
            norm: t.norm,
            source: match t.source {
                posts::TagSource::Ai => TagSource::Ai,
                posts::TagSource::Manual => TagSource::Manual,
            },
            tier: t.tier,
        }
    }
}

/// An AI entity of a post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostEntity {
    /// Display form.
    pub entity: String,
    /// Normalized form, the matching key of the `entity` filter.
    pub norm: String,
}

/// Everything about one post: the gallery fields plus the AI bookkeeping and
/// the tag and entity rows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostDetail {
    /// The gallery fields.
    #[serde(flatten)]
    pub post: Post,
    /// Platform-native id.
    pub native_id: String,
    /// When `coverUrl` expires, if known.
    #[schema(required = true)]
    pub cover_url_expires_at: Option<i64>,
    /// AI attempts so far.
    pub ai_attempts: i64,
    /// Next AI attempt, when backed off.
    #[schema(required = true)]
    pub ai_next_at: Option<i64>,
    /// Code of the last AI error.
    #[schema(required = true)]
    pub ai_error: Option<String>,
    /// Provider of the AI analysis.
    #[schema(required = true)]
    pub ai_provider: Option<String>,
    /// Model of the AI analysis.
    #[schema(required = true)]
    pub ai_model: Option<String>,
    /// Version of the AI output schema.
    #[schema(required = true)]
    pub ai_schema_version: Option<i64>,
    /// AI entities, as produced.
    pub ai_entities: Vec<String>,
    /// AI keywords.
    pub ai_keywords: Vec<String>,
    /// AI design catalog of a website, as produced.
    #[schema(required = true)]
    pub ai_web: Option<serde_json::Value>,
    /// Tag rows, AI and manual.
    pub tags: Vec<PostTag>,
    /// Entity rows.
    pub entities: Vec<PostEntity>,
}

impl From<posts::PostDetail> for PostDetail {
    fn from(d: posts::PostDetail) -> Self {
        Self {
            post: d.summary.into(),
            native_id: d.native_id,
            cover_url_expires_at: d.cover_url_expires_at,
            ai_attempts: d.ai_attempts,
            ai_next_at: d.ai_next_at,
            ai_error: d.ai_error,
            ai_provider: d.ai_provider,
            ai_model: d.ai_model,
            ai_schema_version: d.ai_schema_version,
            ai_entities: d.ai_entities,
            ai_keywords: d.ai_keywords,
            ai_web: d.ai_web,
            tags: d.tags.into_iter().map(Into::into).collect(),
            entities: d
                .entities
                .into_iter()
                .map(|e| PostEntity {
                    entity: e.entity,
                    norm: e.norm,
                })
                .collect(),
        }
    }
}

/// One page of `GET /api/v1/posts`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PostPage {
    /// The posts of this page.
    pub items: Vec<Post>,
    /// Pass it as `cursor` to get the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Posts matching the filters, over all pages. Sent only with
    /// `includeTotal=true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub total: Option<u64>,
}

/// One page of `GET /api/v1/search`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SearchPage {
    /// The results of this page, best match first.
    pub items: Vec<Post>,
    /// Pass it as `cursor` to get the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
    /// Posts matching the search, over all pages (not capped by the 1,000
    /// results that relevance paging reaches).
    pub total: u64,
}

/// Library counters. Trashed posts count only in `trashed`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Stats {
    /// Posts in the library.
    pub total: u64,
    /// Posts per platform.
    pub by_platform: PlatformCounts,
    /// Posts per media type; types without posts are absent.
    pub by_media_type: BTreeMap<String, u64>,
    /// Posts with at least one stored object (the `stored=yes` filter).
    pub stored: u64,
    /// Posts per kind of stored object.
    pub stored_by_kind: StoredByKind,
    /// Posts in the trash.
    pub trashed: u64,
}

/// Posts per platform, every platform present.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCounts {
    /// Instagram.
    pub instagram: u64,
    /// X.
    pub twitter: u64,
    /// Pinterest.
    pub pinterest: u64,
    /// Websites.
    pub web: u64,
    /// Manual bookmarks.
    pub manual: u64,
}

/// Posts with a stored object of each kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct StoredByKind {
    /// A stored cover.
    pub covers: u64,
    /// At least one stored image slide.
    pub images: u64,
    /// At least one kept video.
    pub videos: u64,
}

impl From<stats::Stats> for Stats {
    fn from(s: stats::Stats) -> Self {
        let count = |p| s.by_platform.get(&p).copied().unwrap_or(0);
        Self {
            total: s.total,
            by_platform: PlatformCounts {
                instagram: count(repo::Platform::Instagram),
                twitter: count(repo::Platform::Twitter),
                pinterest: count(repo::Platform::Pinterest),
                web: count(repo::Platform::Web),
                manual: count(repo::Platform::Manual),
            },
            by_media_type: s.by_media_type,
            stored: s.stored,
            stored_by_kind: StoredByKind {
                covers: s.stored_by_kind.covers,
                images: s.stored_by_kind.images,
                videos: s.stored_by_kind.videos,
            },
            trashed: s.trashed,
        }
    }
}

/// A collection ("source" in the UI): a manual one, or a saved folder or board
/// linked to a platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    /// Id, the value of the `collection` filter.
    pub id: i64,
    /// Name.
    pub name: String,
    /// Hex color, `#rrggbb` or `#rgb`.
    pub color: String,
    /// Platform of a linked folder or board; `null` for a manual collection.
    #[schema(required = true)]
    pub platform: Option<Platform>,
    /// Folder or board id on that platform.
    #[schema(required = true)]
    pub external_id: Option<String>,
    /// Name of the folder or board when it was linked.
    #[schema(required = true)]
    pub source_name: Option<String>,
    /// Manual order, when set.
    #[schema(required = true)]
    pub position: Option<i64>,
    /// Creation time.
    pub created_at: i64,
    /// Posts in the collection, trash excluded.
    pub count: u64,
}

impl From<collections::Collection> for Collection {
    fn from(c: collections::Collection) -> Self {
        Self {
            id: c.id,
            name: c.name,
            color: c.color,
            platform: c.platform.map(Into::into),
            external_id: c.external_id,
            source_name: c.source_name,
            position: c.position,
            created_at: c.created_at,
            count: c.count,
        }
    }
}

/// Every collection, in manual order, then creation order.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionList {
    /// The collections.
    pub items: Vec<Collection>,
}
