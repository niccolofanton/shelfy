//! The merge rules of ingest (DATA-04): the desktop's `bulkUpsert`
//! (`electron/db.ts`) on the web schema.
//!
//! An incoming post is matched by its key. A new key inserts the post with
//! everything it carries. A known key merges it into the stored post, and the
//! merge never clobbers what the library already has:
//!
//! - **Platform layer** (shortcode, links, author, caption, cover URL, media
//!   type and count, website URLs): replaced by the incoming values, empty ones
//!   included, until the post has archived media, that is an archived cover or
//!   a kept video (desktop: `thumbnail_path`, `image_path` or `video_path`).
//!   From then on it is frozen.
//! - **Date**: an incoming date replaces the stored one under the same
//!   condition; a missing date never clears it.
//! - **Slides**: an incoming slide at a new position is added; an existing
//!   slide takes the incoming kind and URL until it has an archived object
//!   (desktop: `local_path`). Slides are never removed, and archived objects
//!   are never replaced: files arrive with a post only when it is new.
//! - **AI layer**: the AI fields present in the import are written while the
//!   post is unanalyzed (`ai_status` NULL), or with
//!   [`UpsertOptions::overwrite_ai`] when the import carries an analysis.
//! - **User layer and folders**: untouched.
//!
//! `scripts/golden/merge.ts` runs the desktop function on fixture batches;
//! `crates/core/tests/golden.rs` checks that this port leaves the library in
//! the same state, byte for byte.
//!
//! Deliberate differences from the desktop:
//!
//! - Dates are unix ms, so the caller turns a date that is not ISO 8601 into
//!   `None` (plan §4.2): it is no date, and never replaces a known one. The
//!   desktop stored any non-empty string and let it win.
//! - A new post without a date keeps `posted_at` NULL and sorts by its import
//!   time (`sort_ts`); the desktop wrote the import time as its date.
//! - An empty AI list is stored as NULL, as the repositories do; the desktop
//!   stored `[]`. Both read back as an empty list.
//! - An AI tag and a manual tag with the same name are two rows (§1.2 #3).
//! - A merge that changes nothing writes nothing: `updated_at` and the search
//!   index move only when the stored post changes.
//! - Web-only columns follow the slide they describe: a new URL resets the
//!   slide's fetch state (`fetch_*`), sizes and labels are kept when the import
//!   has none, and `video_url` is refreshed until the video is kept.
//! - Desktop website columns (`web_*_json`) live in `web_captures` and are not
//!   ingest's business; `ai_web_json` was never written by the desktop's
//!   upsert either.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params, params_from_iter};

use crate::repo::posts::{self, AiLayer, CAPTION_MAX_CHARS, NewMedia, NewPost};
use crate::repo::{Platform, RepoError, Result, tags};
use crate::search::index;
use crate::search::terms::js_trim;

/// A post arriving in the library: an item of a capture batch, an imported
/// post or a migrated one. It is the desktop's write-path post (`PostInput`)
/// on the web schema, already validated, with its canonical key (plan §2.8).
#[derive(Clone, Debug, PartialEq)]
pub struct IncomingPost {
    /// Public id: it decides whether the post is new.
    pub key: String,
    /// Source platform; the key must carry its prefix.
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
    /// Caption (desktop `text`), at most [`CAPTION_MAX_CHARS`] characters.
    pub caption: Option<String>,
    /// `image`, `images`, `carousel`, `video`, `text`, …; required.
    pub media_type: String,
    /// Publication time, unix ms; `None` when the source has no date.
    pub posted_at: Option<i64>,
    /// Remote cover URL (desktop `thumbnail_url`).
    pub cover_url: Option<String>,
    /// When `cover_url` expires.
    pub cover_url_expires_at: Option<i64>,
    /// Archived cover. Recorded only when the post is new.
    pub cover_object: Option<i64>,
    /// Website URL.
    pub web_url: Option<String>,
    /// Website domain.
    pub web_domain: Option<String>,
    /// Website URL after redirects.
    pub web_final_url: Option<String>,
    /// Slides as the source listed them; [`derive_media`] turns them into the
    /// stored ones. Archived objects in them count only for new slides.
    pub media: Vec<NewMedia>,
    /// AI fields; absent ones are left alone.
    pub ai: AiFields,
    /// Archive state of a new post; `None` means `pending`.
    pub archive_state: Option<String>,
}

impl IncomingPost {
    /// A post with the required fields and nothing else.
    #[must_use]
    pub fn new(
        key: impl Into<String>,
        platform: Platform,
        native_id: impl Into<String>,
        media_type: impl Into<String>,
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
            cover_url: None,
            cover_url_expires_at: None,
            cover_object: None,
            web_url: None,
            web_domain: None,
            web_final_url: None,
            media: Vec::new(),
            ai: AiFields::default(),
            archive_state: None,
        }
    }
}

/// The AI fields of an incoming post (the desktop's `extractAiFields`). Each
/// one is `None` when absent, and the column is left alone; `Some(None)` writes
/// NULL; `Some(Some(value))` writes the value.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiFields {
    /// Lifecycle status; `done` also stamps `analyzed_at` when that is absent.
    pub status: Option<Option<String>>,
    /// Provider id (web only).
    pub provider: Option<Option<String>>,
    /// Model id.
    pub model: Option<Option<String>>,
    /// Output schema version (web only).
    pub schema_version: Option<Option<i64>>,
    /// Description.
    pub description: Option<Option<String>>,
    /// Why the post was saved.
    pub save_reason: Option<Option<String>>,
    /// Language.
    pub language: Option<Option<String>>,
    /// Category.
    pub category: Option<Option<String>>,
    /// Content type.
    pub content_type: Option<Option<String>>,
    /// Tags as produced; present, they rebuild the AI tag rows.
    pub tags: Option<Option<Vec<String>>>,
    /// Tags of the general tier: tiers for the rebuilt rows, not stored.
    pub general_tags: Option<Option<Vec<String>>>,
    /// Tags of the specific tier (wins over general).
    pub specific_tags: Option<Option<Vec<String>>>,
    /// Entities; present, they rebuild the entity rows.
    pub entities: Option<Option<Vec<String>>>,
    /// Keywords.
    pub keywords: Option<Option<Vec<String>>>,
    /// When the analysis finished, unix ms.
    pub analyzed_at: Option<Option<i64>>,
}

impl AiFields {
    /// Whether no field is present (the desktop's `extractAiFields` returns
    /// null): the AI layer is not touched and the post does not count as
    /// AI-updated.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Whether the fields carry an analysis, so that
    /// [`UpsertOptions::overwrite_ai`] may replace a stored one (the desktop's
    /// `carriesAnalysis`): tags, tier lists, keywords or entities, even empty,
    /// or a non-empty description. A status or a model alone carries nothing.
    #[must_use]
    pub fn carries_analysis(&self) -> bool {
        let list = |field: &Option<Option<Vec<String>>>| matches!(field, Some(Some(_)));
        list(&self.tags)
            || matches!(&self.description, Some(Some(d)) if !d.is_empty())
            || list(&self.general_tags)
            || list(&self.specific_tags)
            || list(&self.keywords)
            || list(&self.entities)
    }

    /// `analyzed_at` to write: the given one, else `now` when the status
    /// becomes `done`; `None` leaves the column alone.
    fn analyzed_at(&self, now: i64) -> Option<Option<i64>> {
        match (self.analyzed_at, &self.status) {
            (Some(at), _) => Some(at),
            (None, Some(Some(status))) if status == "done" => Some(Some(now)),
            (None, _) => None,
        }
    }

    /// The whole AI layer of a new post: the present fields, NULL elsewhere.
    fn to_layer(&self, now: i64) -> AiLayer {
        let text = |field: &Option<Option<String>>| field.clone().flatten();
        let list = |field: &Option<Option<Vec<String>>>| field.clone().flatten();
        AiLayer {
            status: text(&self.status),
            provider: text(&self.provider),
            model: text(&self.model),
            schema_version: self.schema_version.flatten(),
            description: text(&self.description),
            save_reason: text(&self.save_reason),
            language: text(&self.language),
            category: text(&self.category),
            content_type: text(&self.content_type),
            tags: list(&self.tags).unwrap_or_default(),
            general_tags: list(&self.general_tags),
            specific_tags: list(&self.specific_tags),
            entities: list(&self.entities).unwrap_or_default(),
            keywords: list(&self.keywords).unwrap_or_default(),
            web: None,
            analyzed_at: self.analyzed_at(now).flatten(),
        }
    }
}

/// How a batch merges.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UpsertOptions {
    /// Let an import that carries an analysis replace a stored one (the
    /// desktop's JSON import). Captures leave it off: a sync never clobbers
    /// an analysis.
    pub overwrite_ai: bool,
}

/// What happened to one incoming post.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UpsertedPost {
    /// Internal id of the stored post.
    pub id: i64,
    /// The key was new.
    pub inserted: bool,
    /// The stored post changed (always true for a new one).
    pub changed: bool,
    /// The AI fields were applied (the desktop's `aiUpdated`): the post was
    /// new or unanalyzed, or overwritten. They may still have changed nothing.
    pub ai_applied: bool,
}

/// What happened to a batch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpsertSummary {
    /// New keys.
    pub inserted: usize,
    /// Known keys (the desktop's `skipped`), changed or not.
    pub merged: usize,
    /// Known keys whose stored post changed.
    pub changed: usize,
    /// Posts whose AI fields were applied (the desktop's `aiUpdated`).
    pub ai_updated: usize,
    /// One entry per incoming post, in order.
    pub posts: Vec<UpsertedPost>,
}

/// Merges a batch into the library, in order: a key that appears twice is
/// inserted, then merged. Stops at the first invalid post; run it in the
/// transaction of [`UserDb::write`] so the batch applies whole or not at all.
///
/// # Errors
///
/// [`RepoError::Invalid`] for an invalid post (see [`upsert_post`]); database
/// errors otherwise.
///
/// [`UserDb::write`]: crate::db::UserDb::write
pub fn upsert_batch(
    conn: &Connection,
    batch: &[IncomingPost],
    options: UpsertOptions,
    now: i64,
) -> Result<UpsertSummary> {
    let mut summary = UpsertSummary::default();
    for post in batch {
        let done = upsert_post(conn, post, options, now)?;
        if done.inserted {
            summary.inserted += 1;
        } else {
            summary.merged += 1;
            summary.changed += usize::from(done.changed);
        }
        summary.ai_updated += usize::from(done.ai_applied);
        summary.posts.push(done);
    }
    Ok(summary)
}

/// Inserts one incoming post, or merges it into the stored post with its key.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a blank key, native id or media type, a key
/// without the platform's prefix, an over-long caption or an unknown slide
/// kind; database errors otherwise (an object id that does not exist, …).
pub fn upsert_post(
    conn: &Connection,
    post: &IncomingPost,
    options: UpsertOptions,
    now: i64,
) -> Result<UpsertedPost> {
    let slides = derive_media(post);
    validate(post, &slides)?;
    let stored: Option<(i64, Option<String>)> = conn
        .prepare_cached("SELECT id, ai_status FROM posts WHERE key = ?1")?
        .query_row([&post.key], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((id, ai_status)) = stored else {
        return insert(conn, post, slides, now);
    };

    let mut changed = refresh_platform_layer(conn, id, post, slides.len())?;
    changed |= merge_slides(conn, id, &slides)?;
    let ai_applied = !post.ai.is_empty()
        && (ai_status.is_none() || (options.overwrite_ai && post.ai.carries_analysis()));
    if ai_applied {
        changed |= apply_ai(conn, id, &post.ai, now)?;
    }
    if changed {
        conn.prepare_cached("UPDATE posts SET updated_at = ?2 WHERE id = ?1")?
            .execute(params![id, now])?;
        index::reindex_post(conn, id)?;
    }
    Ok(UpsertedPost {
        id,
        inserted: false,
        changed,
        ai_applied,
    })
}

/// The slides of an incoming post (the desktop's `deriveMedia`): the listed
/// slides that have a URL, in order; when the source listed none, one slide
/// from the cover URL, unless the post is text-only.
#[must_use]
pub fn derive_media(post: &IncomingPost) -> Vec<NewMedia> {
    if !post.media.is_empty() {
        return post
            .media
            .iter()
            .filter(|m| m.source_url.as_deref().is_some_and(|url| !url.is_empty()))
            .cloned()
            .collect();
    }
    match post.cover_url.as_deref() {
        Some(url) if !url.is_empty() && post.media_type != "text" => {
            let kind = if post.media_type == "video" {
                "video"
            } else {
                "image"
            };
            vec![NewMedia {
                kind: kind.to_owned(),
                source_url: Some(url.to_owned()),
                source_url_expires_at: post.cover_url_expires_at,
                ..NewMedia::default()
            }]
        }
        _ => Vec::new(),
    }
}

/// The prefix of a platform's keys (plan §2.8).
fn key_prefix(platform: Platform) -> &'static str {
    match platform {
        Platform::Instagram => "ig_",
        Platform::Twitter => "x_",
        Platform::Pinterest => "pin_",
        Platform::Web => "web_",
        Platform::Manual => "m_",
    }
}

fn validate(post: &IncomingPost, slides: &[NewMedia]) -> Result<()> {
    let invalid = |field, reason| Err(RepoError::Invalid { field, reason });
    if js_trim(&post.key).is_empty() || post.key.len() > 200 {
        return invalid("key", "must be 1-200 bytes");
    }
    if !post.key.starts_with(key_prefix(post.platform)) {
        return invalid("key", "does not match the platform");
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
    if slides
        .iter()
        .any(|m| !matches!(m.kind.as_str(), "image" | "video" | "file" | "page"))
    {
        return invalid("media.kind", "must be image, video, file or page");
    }
    Ok(())
}

/// A new key: the post as it comes, with its slides, objects and AI fields.
fn insert(
    conn: &Connection,
    post: &IncomingPost,
    slides: Vec<NewMedia>,
    now: i64,
) -> Result<UpsertedPost> {
    let ai_applied = !post.ai.is_empty();
    let mut new = NewPost::new(
        post.key.clone(),
        post.platform,
        post.native_id.clone(),
        post.media_type.clone(),
        now,
    );
    new.shortcode.clone_from(&post.shortcode);
    new.post_url.clone_from(&post.post_url);
    new.profile_url.clone_from(&post.profile_url);
    new.author_username.clone_from(&post.author_username);
    new.author_name.clone_from(&post.author_name);
    new.caption.clone_from(&post.caption);
    new.posted_at = post.posted_at;
    new.cover_object = post.cover_object;
    new.cover_url.clone_from(&post.cover_url);
    new.cover_url_expires_at = post.cover_url_expires_at;
    new.archive_state.clone_from(&post.archive_state);
    new.web_url.clone_from(&post.web_url);
    new.web_domain.clone_from(&post.web_domain);
    new.web_final_url.clone_from(&post.web_final_url);
    new.media = slides;
    new.ai = ai_applied.then(|| post.ai.to_layer(now));
    let id = posts::insert(conn, &new, now)?;
    Ok(UpsertedPost {
        id,
        inserted: true,
        changed: true,
        ai_applied,
    })
}

/// Replaces the platform layer and the date while the post has no archived
/// cover and no kept video (the desktop's `updateMeta`). Returns whether the
/// post changed.
fn refresh_platform_layer(
    conn: &Connection,
    id: i64,
    post: &IncomingPost,
    slides: usize,
) -> Result<bool> {
    let media_count = i64::try_from(slides.max(1)).unwrap_or(i64::MAX);
    let changed = conn
        .prepare_cached(
            "UPDATE posts SET
               shortcode = ?2, post_url = ?3, profile_url = ?4, author_username = ?5,
               author_name = ?6, caption = ?7, cover_url = ?8, cover_url_expires_at = ?9,
               media_type = ?10, media_count = ?11, web_url = ?12, web_domain = ?13,
               web_final_url = ?14, posted_at = COALESCE(?15, posted_at),
               sort_ts = COALESCE(?15, posted_at, imported_at)
             WHERE id = ?1
               AND cover_object IS NULL
               AND NOT EXISTS (SELECT 1 FROM post_media
                               WHERE post_id = ?1 AND video_object_id IS NOT NULL)
               AND (shortcode IS NOT ?2 OR post_url IS NOT ?3 OR profile_url IS NOT ?4
                    OR author_username IS NOT ?5 OR author_name IS NOT ?6 OR caption IS NOT ?7
                    OR cover_url IS NOT ?8 OR cover_url_expires_at IS NOT ?9
                    OR media_type IS NOT ?10 OR media_count IS NOT ?11 OR web_url IS NOT ?12
                    OR web_domain IS NOT ?13 OR web_final_url IS NOT ?14
                    OR posted_at IS NOT COALESCE(?15, posted_at))",
        )?
        .execute(params![
            id,
            post.shortcode,
            post.post_url,
            post.profile_url,
            post.author_username,
            post.author_name,
            post.caption,
            post.cover_url,
            post.cover_url_expires_at,
            post.media_type,
            media_count,
            post.web_url,
            post.web_domain,
            post.web_final_url,
            post.posted_at,
        ])?;
    Ok(changed > 0)
}

/// Adds the slides at new positions and refreshes the others while they have
/// no archived object (the desktop's `mergePostMedia`). Returns whether a
/// slide changed.
fn merge_slides(conn: &Connection, post_id: i64, slides: &[NewMedia]) -> Result<bool> {
    let mut insert = conn.prepare_cached(
        "INSERT INTO post_media (post_id, position, kind, source_url, source_url_expires_at,
                                 video_url, video_url_expires_at, width, height, duration_ms,
                                 label, object_id, video_object_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
         ON CONFLICT (post_id, position) DO NOTHING",
    )?;
    // A new URL is a new chance for the archive worker: its fetch state resets.
    let mut refresh = conn.prepare_cached(
        "UPDATE post_media SET
           fetch_attempts = CASE WHEN source_url IS NOT ?4 THEN 0 ELSE fetch_attempts END,
           fetch_next_at = CASE WHEN source_url IS NOT ?4 THEN NULL ELSE fetch_next_at END,
           fetch_error = CASE WHEN source_url IS NOT ?4 THEN NULL ELSE fetch_error END,
           kind = ?3, source_url = ?4, source_url_expires_at = ?5,
           width = COALESCE(?6, width), height = COALESCE(?7, height),
           duration_ms = COALESCE(?8, duration_ms), label = COALESCE(?9, label)
         WHERE post_id = ?1 AND position = ?2
           AND object_id IS NULL AND video_object_id IS NULL
           AND (kind IS NOT ?3 OR source_url IS NOT ?4 OR source_url_expires_at IS NOT ?5
                OR width IS NOT COALESCE(?6, width) OR height IS NOT COALESCE(?7, height)
                OR duration_ms IS NOT COALESCE(?8, duration_ms)
                OR label IS NOT COALESCE(?9, label))",
    )?;
    let mut refresh_video = conn.prepare_cached(
        "UPDATE post_media SET video_url = ?3, video_url_expires_at = ?4
         WHERE post_id = ?1 AND position = ?2 AND video_object_id IS NULL AND ?3 IS NOT NULL
           AND (video_url IS NOT ?3 OR video_url_expires_at IS NOT ?4)",
    )?;
    let mut changed = false;
    for (position, m) in (0_i64..).zip(slides) {
        let inserted = insert.execute(params![
            post_id,
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
        if inserted > 0 {
            changed = true;
            continue;
        }
        changed |= refresh.execute(params![
            post_id,
            position,
            m.kind,
            m.source_url,
            m.source_url_expires_at,
            m.width,
            m.height,
            m.duration_ms,
            m.label
        ])? > 0;
        changed |= refresh_video.execute(params![
            post_id,
            position,
            m.video_url,
            m.video_url_expires_at
        ])? > 0;
    }
    Ok(changed)
}

/// Writes the present AI fields of a stored post and rebuilds the tag or
/// entity rows when tags or entities are present (the desktop's
/// `applyAiAnalysis`). Returns whether the post changed.
fn apply_ai(conn: &Connection, post_id: i64, fields: &AiFields, now: i64) -> Result<bool> {
    let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::Text);
    let list = |value: &Option<Vec<String>>| match value.as_deref() {
        Some(items) if !items.is_empty() => {
            Value::Text(serde_json::to_string(items).expect("strings serialize"))
        }
        _ => Value::Null,
    };
    let int = |value: Option<i64>| value.map_or(Value::Null, Value::Integer);
    let mut columns: Vec<(&str, Value)> = Vec::new();
    let texts = [
        ("ai_status", &fields.status),
        ("ai_provider", &fields.provider),
        ("ai_model", &fields.model),
        ("ai_description", &fields.description),
        ("ai_save_reason", &fields.save_reason),
        ("ai_language", &fields.language),
        ("ai_category", &fields.category),
        ("ai_content_type", &fields.content_type),
    ];
    for (column, field) in texts {
        if let Some(value) = field {
            columns.push((column, text(value)));
        }
    }
    let lists = [
        ("ai_tags_json", &fields.tags),
        ("ai_entities_json", &fields.entities),
        ("ai_keywords_json", &fields.keywords),
    ];
    for (column, field) in lists {
        if let Some(value) = field {
            columns.push((column, list(value)));
        }
    }
    if let Some(value) = fields.schema_version {
        columns.push(("ai_schema_version", int(value)));
    }
    if let Some(value) = fields.analyzed_at(now) {
        columns.push(("ai_analyzed_at", int(value)));
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
        changed = conn.prepare(&sql)?.execute(params_from_iter(values))? > 0;
    }
    if let Some(tags) = &fields.tags {
        let before = ai_tag_rows(conn, post_id)?;
        tags::sync_ai_tags(
            conn,
            post_id,
            tags.as_deref().unwrap_or_default(),
            fields.general_tags.as_ref().and_then(Option::as_deref),
            fields.specific_tags.as_ref().and_then(Option::as_deref),
        )?;
        changed |= ai_tag_rows(conn, post_id)? != before;
    }
    if let Some(entities) = &fields.entities {
        let before = entity_rows(conn, post_id)?;
        tags::sync_entities(conn, post_id, entities.as_deref().unwrap_or_default())?;
        changed |= entity_rows(conn, post_id)? != before;
    }
    Ok(changed)
}

type TagRow = (String, String, Option<String>);

fn ai_tag_rows(conn: &Connection, post_id: i64) -> Result<Vec<TagRow>> {
    let rows = conn
        .prepare_cached(
            "SELECT tag_norm, tag_form, tier FROM post_tags
             WHERE post_id = ?1 AND source = 'ai' ORDER BY tag_norm",
        )?
        .query_map([post_id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

fn entity_rows(conn: &Connection, post_id: i64) -> Result<Vec<(String, String)>> {
    let rows = conn
        .prepare_cached(
            "SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
        )?
        .query_map([post_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
