//! The ingest sanitizer (plan §2.16; P2 contract C5): the server's check of
//! a capture batch before the merge.
//!
//! The browser extension reads its batches from pages the user does not
//! control, so every item is untrusted. [`sanitize_batch`] ports the
//! desktop's `sanitizeInterceptedBatch` (`src/lib/browserSanitize.ts`),
//! tightens it as §2.16 asks, and maps each item it keeps to the
//! [`IncomingPost`] that [`upsert_batch`] merges. Every post it returns
//! passes the merge's validation, so a hostile item can never fail the
//! batch it came in.
//!
//! **The desktop's rules** ([`clean_item`]; the golden set
//! `scripts/golden/sanitize.ts` checks them against the desktop function):
//!
//! - an item is an object whose `id` is a non-empty string of at most
//!   [`MAX_ID_LEN`] UTF-16 code units, or a number;
//! - the batch platform is stamped on it: its own `platform` is ignored;
//! - `shortcode`, `postUrl`, `profileUrl`, `authorUsername`, `authorName` and
//!   `mediaType` are strings of at most [`MAX_STRING_LEN`] UTF-16 code units,
//!   `timestamp` of at most [`MAX_TIMESTAMP_LEN`] and `text` of at most
//!   [`MAX_TEXT_LEN`]; a longer string is cut on a character boundary, and a
//!   value that is not a string is empty;
//! - `thumbnailUrl` and each media `url` must be http(s) URLs of at most
//!   4,096 code units, or they are dropped; a media entry without a valid
//!   `url` is dropped, the others keep their order up to [`MAX_MEDIA`], and
//!   their `type` is `video` or else `image`.
//!
//! **The port's own rules** (unit tests):
//!
//! - a batch of more than [`MAX_BATCH_ITEMS`] items is refused, not cut
//!   ([`SanitizeError::TooManyItems`]);
//! - an item that is not an object is rejected as `bad_item`, one without a
//!   usable id as `bad_id`, where the desktop dropped both silently;
//! - an `id` is a string or an integer: booleans, fractions, arrays and
//!   objects are `bad_id` (the desktop turned them into text such as `true`
//!   or `1,2`). An integer above 2⁵³ keeps every digit; JavaScript rounds it;
//! - URLs must also be on the platform's allowlist, with no credentials, no
//!   port but the scheme's own and no whitespace or control character
//!   ([`hosts::parse_allowed`]). `postUrl` and `profileUrl`, which the
//!   desktop kept as text, follow the same rule;
//! - the id must give a canonical key (§2.8), or the item is `bad_id`:
//!   Instagram `<pk>_<owner>`, the pk or the shortcode (else the item's
//!   `shortcode`); X the tweet id, else the `/status/<id>` of `postUrl`;
//!   Pinterest the pin id, else the `/pin/<id>/` of `postUrl`;
//! - `videoUrl` (P2-05) is kept; the desktop drops it.
//!
//! **The mapping to [`IncomingPost`]:**
//!
//! | Item | Post |
//! |---|---|
//! | `id` (and `shortcode`, `postUrl`) | `key`, `native_id` (§2.8) |
//! | `shortcode` | Instagram only, when it is a valid shortcode |
//! | `postUrl`, `profileUrl` | when allowed; X's author-less `https://x.com//status/<id>` becomes `https://x.com/i/status/<id>`, as the migration repairs it |
//! | `authorUsername`, `authorName` | `None` when blank |
//! | `text` | `caption`, kept even when empty, as the desktop and the migration store it |
//! | `timestamp` (ISO 8601) | `posted_at` in ms; `None` when invalid, before 2000 or more than a day after `now` |
//! | `thumbnailUrl` | `cover_url`, unless it names a video file; its `oe` is `cover_url_expires_at` |
//! | `media[]` | slides, below |
//! | `mediaType` | one of `image`, `images`, `carousel`, `video`, `text`; anything else is derived from the slides: none → `text`, one → its kind, more → `carousel` |
//!
//! An image entry is an image slide: `source_url` is its `url`. A video entry
//! keeps its poster in `source_url` and its direct video in `video_url`:
//! when `url` names a video file (by its extension: Pinterest's parser keeps
//! the MP4 or HLS URL itself), it is not a poster, and the post's cover takes
//! its place; a `url` that names an MP4 is also the slide's `video_url`,
//! unless `videoUrl` gives one. A `videoUrl` that is a streaming manifest
//! (HLS, DASH) is not a direct video and is dropped. Expiries come from the
//! URLs' `oe` ([`hosts::cdn_url_expiry_ms`]).
//!
//! Items with the same key are not merged here: [`upsert_batch`] merges them
//! in order.
//!
//! [`upsert_batch`]: super::merge::upsert_batch

use serde::Serialize;
use serde_json::{Map, Value};

use super::hosts::{self, VideoFile, cdn_url_expiry_ms};
use super::merge::IncomingPost;
use crate::ids::{CanonicalId, ig, pinterest, x};
use crate::legacy::convert::parse_iso8601_ms;
use crate::repo::Platform;
use crate::repo::posts::NewMedia;

/// Most items in one batch (§2.16); a larger batch is refused.
pub const MAX_BATCH_ITEMS: usize = 500;
/// Longest item id, in UTF-16 code units (the desktop's `MAX_ID_LEN`).
pub const MAX_ID_LEN: usize = 256;
/// Longest `text`, in UTF-16 code units (the desktop's `MAX_TEXT_LEN`).
pub const MAX_TEXT_LEN: usize = 20_000;
/// Longest other string, in UTF-16 code units (the desktop's `MAX_URL_LEN`).
pub const MAX_STRING_LEN: usize = 4_096;
/// Longest `timestamp`, in UTF-16 code units.
pub const MAX_TIMESTAMP_LEN: usize = 64;
/// Most media entries kept per item (the desktop's `MAX_MEDIA`).
pub const MAX_MEDIA: usize = 60;
/// Longest key the merge accepts, in bytes.
const MAX_KEY_BYTES: usize = 200;
/// The earliest publication date kept: 2000-01-01T00:00:00Z.
pub const MIN_POSTED_AT_MS: i64 = 946_684_800_000;
/// How far after `now` a publication date may be (clock skew).
pub const MAX_POSTED_AT_AHEAD_MS: i64 = 86_400_000;

/// The media types an item may declare (`posts.media_type` of social posts).
const MEDIA_TYPES: [&str; 5] = ["image", "images", "carousel", "video", "text"];

/// Why an item of a batch was rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RejectCode {
    /// Not a JSON object.
    BadItem,
    /// No usable id, or one that gives no canonical key.
    BadId,
}

impl RejectCode {
    /// The code as the API writes it.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            RejectCode::BadItem => "bad_item",
            RejectCode::BadId => "bad_id",
        }
    }
}

/// An item of a batch that the sanitizer rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rejected {
    /// Its index in the batch.
    pub index: usize,
    /// Why.
    pub code: RejectCode,
}

/// A sanitized batch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SanitizedBatch {
    /// The accepted items, in batch order, ready for [`upsert_batch`].
    ///
    /// [`upsert_batch`]: super::merge::upsert_batch
    pub posts: Vec<IncomingPost>,
    /// The batch index of each post: `posts[n]` is item `indices[n]`.
    pub indices: Vec<usize>,
    /// The rejected items, in batch order.
    pub rejected: Vec<Rejected>,
}

/// Why a whole batch was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SanitizeError {
    /// More than [`MAX_BATCH_ITEMS`] items.
    #[error("a batch holds at most {max} items, not {len}")]
    TooManyItems {
        /// The limit.
        max: usize,
        /// The batch's length.
        len: usize,
    },
    /// Web and manual posts never come in capture batches.
    #[error("{0} posts do not come in capture batches")]
    Platform(Platform),
}

/// Sanitizes a capture batch of `platform`, read at `now` (unix ms): each
/// item becomes an [`IncomingPost`] or a [`Rejected`] entry.
///
/// # Errors
///
/// [`SanitizeError::TooManyItems`] for a batch of more than
/// [`MAX_BATCH_ITEMS`] items, [`SanitizeError::Platform`] for a web or
/// manual batch.
pub fn sanitize_batch(
    platform: Platform,
    items: &[Value],
    now: i64,
) -> Result<SanitizedBatch, SanitizeError> {
    if matches!(platform, Platform::Web | Platform::Manual) {
        return Err(SanitizeError::Platform(platform));
    }
    if items.len() > MAX_BATCH_ITEMS {
        return Err(SanitizeError::TooManyItems {
            max: MAX_BATCH_ITEMS,
            len: items.len(),
        });
    }
    let mut batch = SanitizedBatch::default();
    for (index, value) in items.iter().enumerate() {
        match clean_item(platform, value).and_then(|item| to_incoming(item, now)) {
            Ok(post) => {
                batch.posts.push(post);
                batch.indices.push(index);
            }
            Err(code) => batch.rejected.push(Rejected { index, code }),
        }
    }
    Ok(batch)
}

/// An item that passed the desktop's rules (and the port's URL and id
/// rules): the desktop's `SanitizedItem`, before keys and dates are derived.
/// It serializes as the desktop's object, field for field.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanItem {
    /// The item's id, as text.
    pub id: String,
    /// The batch platform.
    pub platform: Platform,
    /// Instagram shortcode, as given.
    pub shortcode: String,
    /// Link to the post, as given.
    pub post_url: String,
    /// Link to the author, as given.
    pub profile_url: String,
    /// Author handle.
    pub author_username: String,
    /// Author display name.
    pub author_name: String,
    /// Declared media type.
    pub media_type: String,
    /// Publication date, as given.
    pub timestamp: String,
    /// Caption.
    pub text: String,
    /// Cover URL, valid or empty.
    pub thumbnail_url: String,
    /// Media entries with a valid URL, at most [`MAX_MEDIA`].
    pub media: Vec<CleanMedia>,
}

/// A media entry of a [`CleanItem`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct CleanMedia {
    /// `image` or `video`.
    #[serde(rename = "type")]
    pub kind: MediaKind,
    /// The entry's URL: an image, a video's poster, or (Pinterest) the video.
    pub url: String,
    /// The direct video URL the parser kept (P2-05); not in the desktop's
    /// object.
    #[serde(skip)]
    pub video_url: Option<String>,
}

/// The kind of a media entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaKind {
    /// An image.
    Image,
    /// A video.
    Video,
}

/// The desktop's `sanitizeInterceptedItem` with the port's URL and id rules:
/// the item as a [`CleanItem`] of `platform`, or why it is rejected.
///
/// # Errors
///
/// [`RejectCode::BadItem`] for a value that is not an object,
/// [`RejectCode::BadId`] for an item without a usable id.
pub fn clean_item(platform: Platform, value: &Value) -> Result<CleanItem, RejectCode> {
    let Value::Object(record) = value else {
        return Err(RejectCode::BadItem);
    };
    let id = item_id(record.get("id")).ok_or(RejectCode::BadId)?;
    let text = |key: &str, max: usize| match record.get(key) {
        Some(Value::String(s)) => clamp(s, max).to_owned(),
        _ => String::new(),
    };
    let thumbnail_url = match record.get("thumbnailUrl") {
        Some(Value::String(url)) if hosts::parse_allowed(platform, url).is_some() => url.clone(),
        _ => String::new(),
    };
    Ok(CleanItem {
        id,
        platform,
        shortcode: text("shortcode", MAX_STRING_LEN),
        post_url: text("postUrl", MAX_STRING_LEN),
        profile_url: text("profileUrl", MAX_STRING_LEN),
        author_username: text("authorUsername", MAX_STRING_LEN),
        author_name: text("authorName", MAX_STRING_LEN),
        media_type: text("mediaType", MAX_STRING_LEN),
        timestamp: text("timestamp", MAX_TIMESTAMP_LEN),
        text: text("text", MAX_TEXT_LEN),
        thumbnail_url,
        media: clean_media(platform, record.get("media")),
    })
}

/// An item's id as text: a non-empty string of at most [`MAX_ID_LEN`] UTF-16
/// code units, or an integer.
fn item_id(raw: Option<&Value>) -> Option<String> {
    let id = match raw? {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return None,
    };
    (!id.is_empty() && hosts::utf16_len(&id) <= MAX_ID_LEN).then_some(id)
}

/// The desktop's `clampStr` on a string: at most `max` UTF-16 code units,
/// cut on a character boundary (JavaScript slices code units, then drops a
/// high surrogate left alone at the end).
fn clamp(value: &str, max: usize) -> &str {
    // A UTF-8 string has at least as many bytes as UTF-16 code units.
    if value.len() <= max {
        return value;
    }
    let mut units = 0;
    for (at, c) in value.char_indices() {
        units += c.len_utf16();
        if units > max {
            return &value[..at];
        }
    }
    value
}

/// The media entries with a valid URL, in order, at most [`MAX_MEDIA`].
fn clean_media(platform: Platform, raw: Option<&Value>) -> Vec<CleanMedia> {
    let Some(Value::Array(entries)) = raw else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| clean_media_entry(platform, entry.as_object()?))
        .take(MAX_MEDIA)
        .collect()
}

fn clean_media_entry(platform: Platform, entry: &Map<String, Value>) -> Option<CleanMedia> {
    let url = entry
        .get("url")?
        .as_str()
        .filter(|url| hosts::parse_allowed(platform, url).is_some())?;
    let kind = match entry.get("type").and_then(Value::as_str) {
        Some("video") => MediaKind::Video,
        _ => MediaKind::Image,
    };
    let video_url = entry
        .get("videoUrl")
        .and_then(Value::as_str)
        .filter(|url| hosts::parse_allowed(platform, url).is_some())
        .map(str::to_owned);
    Some(CleanMedia {
        kind,
        url: url.to_owned(),
        video_url,
    })
}

/// A clean item as the post the merge takes, or `bad_id` when its id gives
/// no canonical key.
fn to_incoming(item: CleanItem, now: i64) -> Result<IncomingPost, RejectCode> {
    let platform = item.platform;
    let post_url = hosts::parse_allowed(platform, &item.post_url).map(|_| item.post_url.as_str());
    let id = canonical_id(platform, &item, post_url).ok_or(RejectCode::BadId)?;
    if id.key().len() > MAX_KEY_BYTES {
        return Err(RejectCode::BadId);
    }

    let mut post = IncomingPost::new(id.key(), platform, id.native_id(), "");
    post.shortcode = (platform == Platform::Instagram && is_shortcode(&item.shortcode))
        .then(|| item.shortcode.clone());
    post.post_url = post_url.map(|url| repair_post_url(platform, url, id.native_id()));
    post.profile_url = hosts::parse_allowed(platform, &item.profile_url)
        .is_some()
        .then(|| item.profile_url.clone());
    post.author_username = non_blank(item.author_username);
    post.author_name = non_blank(item.author_name);
    post.caption = Some(item.text);
    post.posted_at = posted_at(&item.timestamp, now);
    post.cover_url = cover_url(platform, &item.thumbnail_url);
    post.cover_url_expires_at = post.cover_url.as_deref().and_then(cdn_url_expiry_ms);
    post.media = slides(platform, &item.media, post.cover_url.as_deref());
    post.media_type = media_type(&item.media_type, &post.media);
    Ok(post)
}

/// The canonical identity of an item (§2.8), from its id, else its
/// shortcode (Instagram) or its allowed post URL (X, Pinterest).
fn canonical_id(
    platform: Platform,
    item: &CleanItem,
    post_url: Option<&str>,
) -> Option<CanonicalId> {
    match platform {
        Platform::Instagram => {
            let shortcode = Some(item.shortcode.as_str()).filter(|s| !s.is_empty());
            let pk = match ig::parse_legacy_id(&item.id, shortcode) {
                Ok(decoded) => decoded.pk,
                Err(_) => ig::MediaPk::from_shortcode(shortcode?).ok()?,
            };
            Some(pk.canonical())
        }
        Platform::Twitter => x::from_legacy(&item.id, post_url).ok(),
        Platform::Pinterest => pinterest::from_legacy(&item.id, post_url).ok(),
        Platform::Web | Platform::Manual => None,
    }
}

/// A valid Instagram shortcode (it decodes to a pk).
fn is_shortcode(value: &str) -> bool {
    !value.is_empty() && ig::MediaPk::from_shortcode(value).is_ok()
}

/// X's parser writes `https://x.com//status/<id>` when the author is unknown;
/// the migration repairs it to `https://x.com/i/status/<id>`, and so does
/// ingest.
fn repair_post_url(platform: Platform, url: &str, native_id: &str) -> String {
    if platform == Platform::Twitter && url.starts_with("https://x.com//status/") {
        format!("https://x.com/i/status/{native_id}")
    } else {
        url.to_owned()
    }
}

fn non_blank(value: String) -> Option<String> {
    (!value.trim().is_empty()).then_some(value)
}

/// The publication date of an ISO 8601 timestamp, when it is plausible.
fn posted_at(timestamp: &str, now: i64) -> Option<i64> {
    let ms = parse_iso8601_ms(timestamp.trim())?;
    (MIN_POSTED_AT_MS..=now.saturating_add(MAX_POSTED_AT_AHEAD_MS))
        .contains(&ms)
        .then_some(ms)
}

/// The cover URL: the item's valid `thumbnailUrl`, unless it names a video
/// file (Pinterest gives a video pin without an image its MP4 as cover).
fn cover_url(platform: Platform, thumbnail_url: &str) -> Option<String> {
    let url = hosts::parse_allowed(platform, thumbnail_url)?;
    hosts::video_file(&url)
        .is_none()
        .then(|| thumbnail_url.to_owned())
}

/// The slides of the clean media entries (see the module documentation).
fn slides(platform: Platform, media: &[CleanMedia], cover: Option<&str>) -> Vec<NewMedia> {
    media
        .iter()
        .map(|m| match m.kind {
            MediaKind::Image => NewMedia {
                kind: "image".to_owned(),
                source_url: Some(m.url.clone()),
                source_url_expires_at: cdn_url_expiry_ms(&m.url),
                ..NewMedia::default()
            },
            MediaKind::Video => {
                let file = hosts::parse_allowed(platform, &m.url)
                    .as_ref()
                    .and_then(hosts::video_file);
                let poster = match file {
                    Some(_) => cover.map(str::to_owned),
                    None => Some(m.url.clone()),
                };
                let video_url = m
                    .video_url
                    .clone()
                    .filter(|url| {
                        hosts::parse_allowed(platform, url).is_some_and(|url| !is_manifest(&url))
                    })
                    .or_else(|| (file == Some(VideoFile::Mp4)).then(|| m.url.clone()));
                NewMedia {
                    kind: "video".to_owned(),
                    source_url_expires_at: poster.as_deref().and_then(cdn_url_expiry_ms),
                    source_url: poster,
                    video_url_expires_at: video_url.as_deref().and_then(cdn_url_expiry_ms),
                    video_url,
                    ..NewMedia::default()
                }
            }
        })
        .collect()
}

/// A streaming manifest (HLS, DASH), not a direct video file.
fn is_manifest(url: &url::Url) -> bool {
    let path = url.path().to_ascii_lowercase();
    path.ends_with(".m3u8") || path.ends_with(".mpd")
}

/// The declared media type when it is a known one, else one derived from
/// the slides.
fn media_type(declared: &str, slides: &[NewMedia]) -> String {
    if MEDIA_TYPES.contains(&declared) {
        return declared.to_owned();
    }
    match slides {
        [] => "text",
        [one] if one.kind == "video" => "video",
        [_] => "image",
        _ => "carousel",
    }
    .to_owned()
}

#[cfg(test)]
mod tests;
