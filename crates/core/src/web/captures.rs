//! The versions of a website (plan §2.7 `web_captures`, §2.18; P4-04).
//!
//! A website is a `web` post, and each capture of it is a **version**: a
//! `web_captures` row whose files (screenshots, heroes, bands, sections,
//! footers, filmstrip frames, the og image, the favicon, the scroll video)
//! are CAS objects referenced from `web_capture_assets`. A version lists every
//! file it shows there, so deleting it stamps all of them for the GC and none
//! stays on disk unreferenced (plan §1.2 #5: the desktop leaked the bands and
//! frames beyond the hero, WEB-48). An object that another version or post
//! still uses is never stamped.
//!
//! # The current version
//!
//! The post points at one version, its current one (`current_capture_id`),
//! and mirrors it, as the desktop's `upsertWebReference` did:
//!
//! - the cover is the version's hero (`hero_object`, else the image of its
//!   first page);
//! - the slides are one `page` row per page with an image: the page URL, its
//!   title as the label, and its image (the page's screenshot, else its hero,
//!   else its first band, filmstrip frame or section);
//! - the caption is the title and the meta description (at most
//!   [`CAPTION_MAX_CHARS`]); the author is the domain (`author_username`,
//!   `profile_url`) and the title (`author_name`); `post_url` and
//!   `web_final_url` are the final URL and `web_domain` its domain (a value
//!   the version lacks keeps the post's);
//! - `posted_at`, and so `sort_ts`, is the capture time, as the desktop's
//!   `timestamp`: a site sorts by its last capture;
//! - `archive_state` is `done`;
//! - the search index reads the version's title, meta description and page
//!   text (`web_text`, [`crate::search::index`]).
//!
//! # The AI layer
//!
//! The post's AI columns and its AI tag and entity rows describe the current
//! version. When a version stops being current, [`insert`] freezes that layer
//! into the version's `ai_snapshot_json`; when it becomes current again,
//! [`delete_latest`] restores it into the post. The current version's
//! `ai_snapshot_json` is always `NULL`; so is that of a version frozen while
//! the post had no AI layer, and restoring it leaves the post unanalyzed. A
//! new version leaves the post's AI layer as it is: re-cataloguing it is the
//! web catalog's (P3), and until then the post keeps the analysis of the
//! previous version.
//!
//! The frozen layer is a JSON object with the keys of the migration's
//! (`description`, `tags`, `model`, `status`, `analyzedAt`, `category`,
//! `contentType`, `entities`, `keywords`, `language`, `saveReason`, `web`)
//! plus `provider`, `schemaVersion`, `error`, and the tag tiers
//! `generalTags` and `specificTags` when the layer has them. An analysis
//! that was still queued or running when the layer was frozen (`pending`,
//! `analyzing`) has no result to restore: its status comes back as `NULL`,
//! as the migration resets a stuck `analyzing`.
//!
//! # Placeholders
//!
//! A site without a version is a **placeholder**: no capture, cover, slides,
//! caption or AI layer; it keeps its URL, domain and title and what the user
//! added (note, manual tags, folders). [`placeholder`] builds one for a new
//! site, and [`delete_latest`] leaves one when it deletes the last version.
//!
//! # Changes from the desktop
//!
//! - Deleting stamps objects for the GC (P4-12) instead of unlinking files,
//!   and every file of a version goes with it (§1.2 #5).
//! - "Delete only the report" (`deleteLatestReport`) restores the whole
//!   frozen layer, the web catalog (`ai_web_json`) and the tag tiers
//!   included, and the post mirrors the promoted version again: its caption,
//!   final URL and slides come back too. The desktop's
//!   `promoteSnapshotToPost` kept the deleted version's catalog and text.
//! - A placeholder also loses the web catalog, the AI error and the retry
//!   state, so it reads as unanalyzed (`ai_status IS NULL`).
//! - The caption no longer carries the site text, which the index reads from
//!   the current version (`web_text`); the desktop appended it to `text`.
//! - The cover is a stored object only: `cover_url` stays `NULL`, so the SPA
//!   never loads a site's og image from the site (§1.2 #10).

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use serde_json::{Map, Value};
use url::Url;

use super::AssetRole;
use crate::ids::{self, IdError};
use crate::repo::posts::{self, AiLayer, CAPTION_MAX_CHARS, NewPost};
use crate::repo::{
    ObjectRef, Platform, RepoError, Result, id_list, json_strings, json_value, media,
    object_columns, object_ref_at,
};
use crate::search::index;

/// `web_capture_assets.page_index` of a site-level asset (og image, favicon,
/// scroll video), which belongs to no page. The migration writes the same.
pub const SITE_LEVEL: i64 = -1;

/// `posts.media_type` of every site.
const MEDIA_TYPE: &str = "website";

/// How a capture ended (`web_captures.status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CaptureStatus {
    /// The pages were captured: all of them, or some (`partial`).
    Done,
    /// The site blocked the capture; the version holds what could be kept
    /// without it, such as its og image (P4-14).
    Blocked,
}

impl CaptureStatus {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Done => "done",
            Self::Blocked => "blocked",
        }
    }
}

/// A version to record with [`insert`]. Its objects are recorded first
/// (`repo::media::upsert_object`, through `shelfy_media::refs`). Nothing in
/// it is a file path: files are objects, referenced by id.
#[derive(Clone, Debug, PartialEq)]
pub struct NewCapture {
    /// When the capture ran (unix ms).
    pub captured_at: i64,
    /// The URL the capture started from.
    pub requested_url: Option<String>,
    /// The URL after redirects.
    pub final_url: Option<String>,
    /// How the capture ended.
    pub status: CaptureStatus,
    /// Whether some pages were skipped.
    pub partial: bool,
    /// The browser engine.
    pub engine: Option<String>,
    /// The viewport, `<width>x<height>`.
    pub viewport: Option<String>,
    /// The site's title.
    pub title: Option<String>,
    /// The palette, as the capture produced it.
    pub palette: Option<Value>,
    /// The fonts.
    pub fonts: Option<Value>,
    /// The technologies.
    pub tech: Option<Value>,
    /// The awards.
    pub awards: Option<Value>,
    /// Site metadata: `description` (the caption's second part), `ogImage`,
    /// `lang`, the capture's settings and timeline, …
    pub meta: Option<Value>,
    /// The pages in capture order, one JSON object each: `url`, `title`, and
    /// their text and probes (`description`, `contentText`, `digest`, …).
    pub pages: Vec<Value>,
    /// Site traits (scroll behaviour, …).
    pub traits: Option<Value>,
    /// The hero, which becomes the post's cover; `None` takes the image of
    /// the first page that has one.
    pub hero_object: Option<i64>,
    /// The favicon.
    pub favicon_object: Option<i64>,
}

impl NewCapture {
    /// A finished capture with no pages, metadata or files yet.
    #[must_use]
    pub fn new(captured_at: i64) -> Self {
        Self {
            captured_at,
            requested_url: None,
            final_url: None,
            status: CaptureStatus::Done,
            partial: false,
            engine: None,
            viewport: None,
            title: None,
            palette: None,
            fonts: None,
            tech: None,
            awards: None,
            meta: None,
            pages: Vec::new(),
            traits: None,
            hero_object: None,
            favicon_object: None,
        }
    }
}

/// A file of a version (`web_capture_assets`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NewAsset {
    /// The page (0-based), or [`SITE_LEVEL`] for the og image, the favicon
    /// and the scroll video with its preview and poster.
    pub page_index: i64,
    /// What the file is.
    pub role: AssetRole,
    /// Its place among the page's files of that role (bands from the top).
    pub seq: i64,
    /// The object.
    pub object_id: i64,
    /// The top of a band or section on its page, in CSS pixels.
    pub css_top: Option<i64>,
    /// Its height, in CSS pixels.
    pub css_height: Option<i64>,
}

impl NewAsset {
    /// A file without a position on its page.
    #[must_use]
    pub const fn new(page_index: i64, role: AssetRole, seq: i64, object_id: i64) -> Self {
        Self {
            page_index,
            role,
            seq,
            object_id,
            css_top: None,
            css_height: None,
        }
    }
}

/// A version as the version list shows it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSummary {
    /// Version id.
    pub id: i64,
    /// When it was captured.
    pub captured_at: i64,
    /// The URL the capture started from.
    pub requested_url: Option<String>,
    /// The URL after redirects.
    pub final_url: Option<String>,
    /// `done` or `blocked`.
    pub status: String,
    /// Whether some pages were skipped.
    pub partial: bool,
    /// The site's title.
    pub title: Option<String>,
    /// The hero.
    pub hero: Option<ObjectRef>,
    /// The favicon.
    pub favicon: Option<ObjectRef>,
    /// Number of pages.
    pub page_count: i64,
    /// Number of files.
    pub asset_count: i64,
    /// Whether it is the site's current version.
    pub current: bool,
    /// When the row was written.
    pub created_at: i64,
}

/// One version with its files.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureDetail {
    /// The list fields.
    #[serde(flatten)]
    pub summary: CaptureSummary,
    /// The browser engine.
    pub engine: Option<String>,
    /// The viewport.
    pub viewport: Option<String>,
    /// The palette (JSON as stored).
    pub palette: Option<Value>,
    /// The fonts.
    pub fonts: Option<Value>,
    /// The technologies.
    pub tech: Option<Value>,
    /// The awards.
    pub awards: Option<Value>,
    /// Site metadata.
    pub meta: Option<Value>,
    /// The pages: text and probes, never file paths.
    pub pages: Vec<Value>,
    /// Site traits.
    pub traits: Option<Value>,
    /// The AI layer frozen when the version stopped being current; `None`
    /// for the current version (its layer is the post's) and for a version
    /// that was never analyzed.
    pub ai_snapshot: Option<Value>,
    /// The files, by page ([`SITE_LEVEL`] first), role and sequence.
    pub assets: Vec<CaptureAsset>,
}

/// A file of a version.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureAsset {
    /// The page, or [`SITE_LEVEL`].
    pub page_index: i64,
    /// What the file is ([`AssetRole`] values).
    pub role: String,
    /// Its place among the page's files of that role.
    pub seq: i64,
    /// The top of a band or section on its page, in CSS pixels.
    pub css_top: Option<i64>,
    /// Its height, in CSS pixels.
    pub css_height: Option<i64>,
    /// The object.
    pub object: ObjectRef,
}

/// What [`delete_latest`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LatestDeleted {
    /// The version deleted; `None` when the site had no current version.
    pub deleted: Option<i64>,
    /// The version that became current; `None` when the site is now a
    /// placeholder.
    pub current: Option<i64>,
    /// Objects stamped for the GC.
    pub stamped: usize,
    /// The new cover, when it has no ThumbHash: the ThumbHash stays only
    /// while the cover is the same object. The caller computes it again
    /// (`shelfy_media::refs::set_cover_thumbhash`).
    pub cover_needs_thumbhash: Option<i64>,
}

/// A new site that has no version yet, saved from `url`: the post that the
/// capture of `POST /sites` (P4-14) or an import of a website (P4-10) starts
/// from. Its key is the scheme-insensitive web identity of `url` (plan §2.8);
/// its author is the URL's domain, as on the desktop.
///
/// # Errors
///
/// [`IdError::Empty`] for a blank URL.
pub fn placeholder(url: &str, now: i64) -> Result<NewPost, IdError> {
    let id = ids::web::from_url(url)?;
    let domain = domain_of(url);
    let mut post = NewPost::new(id.key(), Platform::Web, id.native_id(), MEDIA_TYPE, now);
    post.post_url = Some(url.to_owned());
    post.web_url = Some(url.to_owned());
    post.web_final_url = Some(url.to_owned());
    post.profile_url = domain.as_ref().map(|d| format!("https://{d}"));
    post.author_username.clone_from(&domain);
    post.author_name.clone_from(&domain);
    post.web_domain = domain;
    Ok(post)
}

/// The domain of a site URL: its host, lowercased, without a leading `www.`
/// (the desktop's `webHostname`). `None` when the URL has no host.
#[must_use]
pub fn domain_of(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    let host = parsed.host_str()?.to_lowercase();
    let domain = host.strip_prefix("www.").unwrap_or(&host);
    (!domain.is_empty()).then(|| domain.to_owned())
}

// ── Writing ──────────────────────────────────────────────────────────────────

/// Records a new version of the site `post_id` and makes it the current one:
///
/// - writes the `web_captures` row and its `web_capture_assets`;
/// - freezes the post's AI layer into the outgoing current version's
///   `ai_snapshot_json` (`NULL` when the post has no AI layer); the post's
///   layer itself is left as it is;
/// - points `current_capture_id` at the new version and mirrors it into the
///   post (see the module docs): site fields, caption, slides, cover,
///   `archive_state = 'done'`;
/// - stamps the objects the post's old cover and slides leave without a
///   reference, and reindexes the post.
///
/// The post's ThumbHash stays only when the cover is the same object as
/// before; the caller sets the new one (`shelfy_media::refs::set_cover_thumbhash`).
/// Everything is checked before anything is written. Returns the version id.
///
/// # Errors
///
/// [`RepoError::NotFound`] when `post_id` is not a website;
/// [`RepoError::Invalid`] for a page that is not a JSON object, an asset on
/// a page that does not exist (site-level roles must use [`SITE_LEVEL`],
/// page roles a page), a negative sequence, two assets with the same page,
/// role and sequence, or an object id that is not stored; database errors
/// otherwise.
pub fn insert(
    conn: &Connection,
    post_id: i64,
    capture: &NewCapture,
    assets: &[NewAsset],
    now: i64,
) -> Result<i64> {
    let site = site(conn, post_id)?;
    validate(conn, capture, assets)?;

    if let Some(outgoing) = site.current {
        let frozen = freeze_ai(conn, post_id)?;
        conn.prepare_cached("UPDATE web_captures SET ai_snapshot_json = ?2 WHERE id = ?1")?
            .execute(params![outgoing, frozen])?;
    }

    let files: Vec<Asset> = assets
        .iter()
        .map(|a| Asset {
            page_index: a.page_index,
            role: a.role.as_str().to_owned(),
            seq: a.seq,
            object_id: a.object_id,
        })
        .collect();
    let hero = capture
        .hero_object
        .or_else(|| first_image(&files, capture.pages.len()));
    let json = |value: &Option<Value>| {
        value
            .as_ref()
            .filter(|v| !v.is_null())
            .map(Value::to_string)
    };
    let pages = serde_json::to_string(&capture.pages).expect("JSON values serialize");
    conn.prepare_cached(
        "INSERT INTO web_captures (post_id, captured_at, requested_url, final_url, status, partial,
                                   engine, viewport, title, palette_json, fonts_json, tech_json,
                                   awards_json, meta_json, pages_json, traits_json, hero_object,
                                   favicon_object, ai_snapshot_json, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18,
                 NULL, ?19)",
    )?
    .execute(params![
        post_id,
        capture.captured_at,
        capture.requested_url,
        capture.final_url,
        capture.status.as_str(),
        capture.partial,
        capture.engine,
        capture.viewport,
        capture.title,
        json(&capture.palette),
        json(&capture.fonts),
        json(&capture.tech),
        json(&capture.awards),
        json(&capture.meta),
        pages,
        json(&capture.traits),
        hero,
        capture.favicon_object,
        now
    ])?;
    let capture_id = conn.last_insert_rowid();
    let mut insert_asset = conn.prepare_cached(
        "INSERT INTO web_capture_assets (capture_id, page_index, role, seq, object_id, css_top,
                                         css_height)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for a in assets {
        insert_asset.execute(params![
            capture_id,
            a.page_index,
            a.role.as_str(),
            a.seq,
            a.object_id,
            a.css_top,
            a.css_height
        ])?;
    }

    let outgoing = mirror_objects(conn, post_id)?;
    mirror(conn, post_id, capture_id, now)?;
    media::mark_unreferenced(conn, &outgoing, now)?;
    index::reindex_post(conn, post_id)?;
    Ok(capture_id)
}

/// Deletes the version `capture_id` of the site `post_id`, which must not be
/// the current one (desktop `deleteWebSnapshot`, WEB-47): the row and its
/// files, then stamps the objects left without a reference. The post does not
/// change. Returns how many objects were stamped.
///
/// # Errors
///
/// [`RepoError::NotFound`] when `post_id` is not a website or the version is
/// not one of its; [`RepoError::Conflict`] for the current version (delete
/// it with [`delete_latest`]); database errors otherwise.
pub fn delete_version(conn: &Connection, post_id: i64, capture_id: i64, now: i64) -> Result<usize> {
    let site = site(conn, post_id)?;
    let belongs: bool = conn
        .prepare_cached(
            "SELECT EXISTS (SELECT 1 FROM web_captures WHERE id = ?1 AND post_id = ?2)",
        )?
        .query_row(params![capture_id, post_id], |r| r.get(0))?;
    if !belongs {
        return Err(RepoError::NotFound);
    }
    if site.current == Some(capture_id) {
        return Err(RepoError::Conflict("current version"));
    }
    let objects = version_objects(conn, capture_id)?;
    drop_version(conn, capture_id)?;
    media::mark_unreferenced(conn, &objects, now)
}

/// "Delete only the report" (desktop `deleteLatestReport`, WEB-48): deletes
/// the current version of the site `post_id` and its files.
///
/// - When an older version is left, the newest of them becomes current: the
///   post mirrors it again and its frozen AI layer is restored into the post
///   (the AI tag and entity rows are rebuilt; the retry state is reset).
/// - Otherwise the post becomes a placeholder: no capture, cover, slides,
///   caption or AI layer. The URL, domain and title stay, and so do the
///   user's note, manual tags and folders.
///
/// In both cases the objects left without a reference (the deleted
/// version's files, the old cover and slides) are stamped for the GC, and
/// the post is reindexed. A placeholder has no report to delete: it is left
/// as it is. A site whose current version is missing while older ones exist
/// gets the newest of them as current.
///
/// # Errors
///
/// [`RepoError::NotFound`] when `post_id` is not a website; database errors
/// otherwise.
pub fn delete_latest(conn: &Connection, post_id: i64, now: i64) -> Result<LatestDeleted> {
    let site = site(conn, post_id)?;
    let next: Option<(i64, Option<String>)> = conn
        .prepare_cached(
            "SELECT id, ai_snapshot_json FROM web_captures WHERE post_id = ?1 AND id IS NOT ?2
             ORDER BY captured_at DESC, id DESC LIMIT 1",
        )?
        .query_row(params![post_id, site.current], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?;
    if site.current.is_none() && next.is_none() {
        return Ok(LatestDeleted {
            deleted: None,
            current: None,
            stamped: 0,
            cover_needs_thumbhash: None,
        });
    }
    let mut objects = mirror_objects(conn, post_id)?;
    if let Some(current) = site.current {
        objects.extend(version_objects(conn, current)?);
        conn.prepare_cached("UPDATE posts SET current_capture_id = NULL WHERE id = ?1")?
            .execute([post_id])?;
        drop_version(conn, current)?;
    }
    match &next {
        Some((version, frozen)) => {
            mirror(conn, post_id, *version, now)?;
            restore_ai(conn, post_id, frozen.as_deref(), now)?;
            conn.prepare_cached("UPDATE web_captures SET ai_snapshot_json = NULL WHERE id = ?1")?
                .execute([version])?;
        }
        None => make_placeholder(conn, post_id, now)?,
    }
    let stamped = media::mark_unreferenced(conn, &objects, now)?;
    index::reindex_post(conn, post_id)?;
    let cover_needs_thumbhash = conn
        .prepare_cached("SELECT cover_object FROM posts WHERE id = ?1 AND thumbhash IS NULL")?
        .query_row([post_id], |r| r.get::<_, Option<i64>>(0))
        .optional()?
        .flatten();
    Ok(LatestDeleted {
        deleted: site.current,
        current: next.map(|(id, _)| id),
        stamped,
        cover_needs_thumbhash,
    })
}

// ── Reading ──────────────────────────────────────────────────────────────────

/// The versions of the site `post_id`, newest capture first (then the
/// larger id), with their page and file counts. Empty for a placeholder or
/// a post that is not a website.
///
/// # Errors
///
/// Database errors.
pub fn list(conn: &Connection, post_id: i64) -> Result<Vec<CaptureSummary>> {
    let sql = format!(
        "SELECT {} FROM {SUMMARY_FROM} WHERE c.post_id = ?1 ORDER BY c.captured_at DESC, c.id DESC",
        summary_columns()
    );
    let versions = conn
        .prepare_cached(&sql)?
        .query_map([post_id], |r| summary_at(r, 0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(versions)
}

/// The version `capture_id` of the site `post_id` with its files; `None`
/// when the site has no such version.
///
/// # Errors
///
/// Database errors.
pub fn get(conn: &Connection, post_id: i64, capture_id: i64) -> Result<Option<CaptureDetail>> {
    let sql = format!(
        "SELECT {}, c.engine, c.viewport, c.palette_json, c.fonts_json, c.tech_json,
                c.awards_json, c.meta_json, c.pages_json, c.traits_json, c.ai_snapshot_json
         FROM {SUMMARY_FROM} WHERE c.post_id = ?1 AND c.id = ?2",
        summary_columns()
    );
    let found = conn
        .prepare_cached(&sql)?
        .query_row(params![post_id, capture_id], |r| {
            let summary = summary_at(r, 0)?;
            let json = |i: usize| -> rusqlite::Result<Option<Value>> {
                Ok(json_value(
                    r.get::<_, Option<String>>(SUMMARY_WIDTH + i)?.as_deref(),
                ))
            };
            Ok(CaptureDetail {
                summary,
                engine: r.get(SUMMARY_WIDTH)?,
                viewport: r.get(SUMMARY_WIDTH + 1)?,
                palette: json(2)?,
                fonts: json(3)?,
                tech: json(4)?,
                awards: json(5)?,
                meta: json(6)?,
                pages: page_list(r.get::<_, Option<String>>(SUMMARY_WIDTH + 7)?.as_deref()),
                traits: json(8)?,
                ai_snapshot: json(9)?,
                assets: Vec::new(),
            })
        })
        .optional()?;
    let Some(mut detail) = found else {
        return Ok(None);
    };
    let sql = format!(
        "SELECT a.page_index, a.role, a.seq, a.css_top, a.css_height, {}
         FROM web_capture_assets a JOIN media_objects o ON o.id = a.object_id
         WHERE a.capture_id = ?1 ORDER BY a.page_index, a.role, a.seq",
        object_columns("o")
    );
    let mut stmt = conn.prepare_cached(&sql)?;
    let mut rows = stmt.query([capture_id])?;
    while let Some(r) = rows.next()? {
        if let Some(object) = object_ref_at(r, 5)? {
            detail.assets.push(CaptureAsset {
                page_index: r.get(0)?,
                role: r.get(1)?,
                seq: r.get(2)?,
                css_top: r.get(3)?,
                css_height: r.get(4)?,
                object,
            });
        }
    }
    Ok(Some(detail))
}

// ── Internals ────────────────────────────────────────────────────────────────

const SUMMARY_FROM: &str = "web_captures c JOIN posts p ON p.id = c.post_id
    LEFT JOIN media_objects h ON h.id = c.hero_object
    LEFT JOIN media_objects f ON f.id = c.favicon_object";

/// Columns of [`summary_columns`], before the two objects.
const SUMMARY_SCALARS: usize = 11;
/// All columns of [`summary_columns`]: the scalars and two objects of eight.
const SUMMARY_WIDTH: usize = SUMMARY_SCALARS + 16;

/// The columns [`summary_at`] reads.
fn summary_columns() -> String {
    format!(
        "c.id, c.captured_at, c.requested_url, c.final_url, c.status, c.partial, c.title,
         c.created_at, c.id IS p.current_capture_id,
         CASE WHEN json_valid(c.pages_json) THEN json_array_length(c.pages_json) ELSE 0 END,
         (SELECT count(*) FROM web_capture_assets a WHERE a.capture_id = c.id), {}, {}",
        object_columns("h"),
        object_columns("f")
    )
}

fn summary_at(r: &rusqlite::Row<'_>, start: usize) -> rusqlite::Result<CaptureSummary> {
    Ok(CaptureSummary {
        id: r.get(start)?,
        captured_at: r.get(start + 1)?,
        requested_url: r.get(start + 2)?,
        final_url: r.get(start + 3)?,
        status: r.get(start + 4)?,
        partial: r.get::<_, i64>(start + 5)? != 0,
        title: r.get(start + 6)?,
        created_at: r.get(start + 7)?,
        current: r.get(start + 8)?,
        page_count: r.get(start + 9)?,
        asset_count: r.get(start + 10)?,
        hero: object_ref_at(r, start + SUMMARY_SCALARS)?,
        favicon: object_ref_at(r, start + SUMMARY_SCALARS + 8)?,
    })
}

/// What the writes read of a site's post.
struct Site {
    current: Option<i64>,
}

/// The post `post_id`, which must be a website.
fn site(conn: &Connection, post_id: i64) -> Result<Site> {
    let row: Option<(String, Option<i64>)> = conn
        .prepare_cached("SELECT platform, current_capture_id FROM posts WHERE id = ?1")?
        .query_row([post_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    match row {
        Some((platform, current)) if platform == Platform::Web.as_str() => Ok(Site { current }),
        _ => Err(RepoError::NotFound),
    }
}

fn validate(conn: &Connection, capture: &NewCapture, assets: &[NewAsset]) -> Result<()> {
    let invalid = |field, reason| Err(RepoError::Invalid { field, reason });
    if capture.pages.iter().any(|page| !page.is_object()) {
        return invalid("pages", "must be JSON objects");
    }
    let pages = i64::try_from(capture.pages.len()).unwrap_or(i64::MAX);
    let mut keys = HashSet::new();
    for a in assets {
        if is_site_level(a.role) {
            if a.page_index != SITE_LEVEL {
                return invalid("assets.pageIndex", "must be -1 for a site-level file");
            }
        } else if !(0..pages).contains(&a.page_index) {
            return invalid("assets.pageIndex", "must be the index of a page");
        }
        if a.seq < 0 {
            return invalid("assets.seq", "must not be negative");
        }
        if !keys.insert((a.page_index, a.role, a.seq)) {
            return invalid("assets", "must not repeat a page, role and sequence");
        }
    }
    let mut objects: Vec<i64> = assets
        .iter()
        .map(|a| a.object_id)
        .chain(capture.hero_object)
        .chain(capture.favicon_object)
        .collect();
    objects.sort_unstable();
    objects.dedup();
    if !objects.is_empty() {
        let stored: i64 = conn
            .prepare_cached(
                "SELECT count(*) FROM media_objects WHERE id IN (SELECT value FROM json_each(?1))",
            )?
            .query_row([id_list(&objects)], |r| r.get(0))?;
        if usize::try_from(stored).ok() != Some(objects.len()) {
            return invalid("objectId", "must be a stored object");
        }
    }
    Ok(())
}

/// Roles of the files that belong to the site rather than to a page.
const fn is_site_level(role: AssetRole) -> bool {
    matches!(
        role,
        AssetRole::Og
            | AssetRole::Favicon
            | AssetRole::Video
            | AssetRole::VideoPreview
            | AssetRole::VideoPoster
    )
}

/// A file of a version, as the mirror reads it.
#[derive(Clone, Debug)]
struct Asset {
    page_index: i64,
    role: String,
    seq: i64,
    object_id: i64,
}

/// The roles a page's image is taken from, in order of preference: the
/// desktop's single page image (`screenshotPath`), the hero, then the first
/// band, frame or section.
const PAGE_IMAGE_ROLES: [AssetRole; 5] = [
    AssetRole::Screenshot,
    AssetRole::Hero,
    AssetRole::Band,
    AssetRole::Filmstrip,
    AssetRole::Section,
];

/// The image of page `index`, if it has one.
fn page_image(files: &[Asset], index: usize) -> Option<i64> {
    let index = i64::try_from(index).ok()?;
    PAGE_IMAGE_ROLES.iter().find_map(|role| {
        files
            .iter()
            .filter(|a| a.page_index == index && a.role == role.as_str())
            .min_by_key(|a| a.seq)
            .map(|a| a.object_id)
    })
}

/// The image of the first of `pages` pages that has one.
fn first_image(files: &[Asset], pages: usize) -> Option<i64> {
    (0..pages).find_map(|index| page_image(files, index))
}

/// The files of a stored version.
fn version_files(conn: &Connection, capture_id: i64) -> Result<Vec<Asset>> {
    let files = conn
        .prepare_cached(
            "SELECT page_index, role, seq, object_id FROM web_capture_assets
             WHERE capture_id = ?1 ORDER BY page_index, role, seq",
        )?
        .query_map([capture_id], |r| {
            Ok(Asset {
                page_index: r.get(0)?,
                role: r.get(1)?,
                seq: r.get(2)?,
                object_id: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(files)
}

/// The pages of a `pages_json` value; anything but an array reads as none.
fn page_list(pages_json: Option<&str>) -> Vec<Value> {
    match json_value(pages_json) {
        Some(Value::Array(pages)) => pages,
        _ => Vec::new(),
    }
}

/// A non-blank string field of a JSON object.
fn text_field<'v>(value: &'v Value, key: &str) -> Option<&'v str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

/// The caption of a site: its title and meta description, joined by a blank
/// line, cut to [`CAPTION_MAX_CHARS`]; `None` when both are blank.
fn caption(title: Option<&str>, description: Option<&str>) -> Option<String> {
    let parts: Vec<&str> = [title, description]
        .into_iter()
        .flatten()
        .filter(|s| !s.trim().is_empty())
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(parts.join("\n\n").chars().take(CAPTION_MAX_CHARS).collect())
}

/// Makes `capture_id` the current version of the site `post_id` and writes
/// the post's mirror of it (see the module docs). The caller stamps what the
/// old mirror referenced and reindexes.
fn mirror(conn: &Connection, post_id: i64, capture_id: i64, now: i64) -> Result<()> {
    struct Version {
        captured_at: i64,
        requested_url: Option<String>,
        final_url: Option<String>,
        title: Option<String>,
        meta_json: Option<String>,
        pages_json: Option<String>,
        hero_object: Option<i64>,
    }
    let v = conn
        .prepare_cached(
            "SELECT captured_at, requested_url, final_url, title, meta_json, pages_json,
                    hero_object
             FROM web_captures WHERE id = ?1",
        )?
        .query_row([capture_id], |r| {
            Ok(Version {
                captured_at: r.get(0)?,
                requested_url: r.get(1)?,
                final_url: r.get(2)?,
                title: r.get(3)?,
                meta_json: r.get(4)?,
                pages_json: r.get(5)?,
                hero_object: r.get(6)?,
            })
        })?;
    let files = version_files(conn, capture_id)?;
    let pages = page_list(v.pages_json.as_deref());

    conn.prepare_cached("DELETE FROM post_media WHERE post_id = ?1")?
        .execute([post_id])?;
    let mut slide = conn.prepare_cached(
        "INSERT INTO post_media (post_id, position, kind, source_url, label, object_id, width,
                                 height)
         SELECT ?1, ?2, 'page', ?3, ?4, id, width, height FROM media_objects WHERE id = ?5",
    )?;
    let mut slides = 0_i64;
    let mut first = None;
    for (index, page) in pages.iter().enumerate() {
        let Some(object) = page_image(&files, index) else {
            continue;
        };
        first.get_or_insert(object);
        slide.execute(params![
            post_id,
            slides,
            text_field(page, "url"),
            text_field(page, "title"),
            object
        ])?;
        slides += 1;
    }

    let cover = v.hero_object.or(first);
    let domain = v
        .final_url
        .as_deref()
        .and_then(domain_of)
        .or_else(|| v.requested_url.as_deref().and_then(domain_of));
    let title = v.title.as_deref().filter(|t| !t.trim().is_empty());
    let meta = json_value(v.meta_json.as_deref());
    let description = meta
        .as_ref()
        .and_then(|m| text_field(m, "description").or_else(|| text_field(m, "ogDescription")));
    let author_name = title.map(str::to_owned).or_else(|| domain.clone());
    let profile_url = domain.as_ref().map(|d| format!("https://{d}"));
    let post_url = v.final_url.as_deref().or(v.requested_url.as_deref());
    // SET expressions read the row as it was: the ThumbHash stays only when
    // the cover is the same object.
    conn.prepare_cached(
        "UPDATE posts SET current_capture_id = ?2,
                thumbhash = CASE WHEN cover_object IS ?3 THEN thumbhash END,
                cover_object = ?3, cover_url = NULL, cover_url_expires_at = NULL,
                media_type = ?4, media_count = ?5, caption = ?6,
                post_url = COALESCE(?7, post_url), profile_url = COALESCE(?8, profile_url),
                author_username = COALESCE(?9, author_username),
                author_name = COALESCE(?10, author_name),
                web_url = COALESCE(web_url, ?11, ?12), web_domain = COALESCE(?9, web_domain),
                web_final_url = COALESCE(?12, web_final_url),
                posted_at = ?13, sort_ts = ?13, archive_state = 'done', updated_at = ?14
         WHERE id = ?1",
    )?
    .execute(params![
        post_id,
        capture_id,
        cover,
        MEDIA_TYPE,
        slides.max(1),
        caption(title, description),
        post_url,
        profile_url,
        domain,
        author_name,
        v.requested_url,
        v.final_url,
        v.captured_at,
        now
    ])?;
    Ok(())
}

/// Turns the site `post_id` into a placeholder (see the module docs). The
/// caller stamps what the old mirror referenced and reindexes.
fn make_placeholder(conn: &Connection, post_id: i64, now: i64) -> Result<()> {
    conn.prepare_cached("DELETE FROM post_media WHERE post_id = ?1")?
        .execute([post_id])?;
    conn.prepare_cached(
        "UPDATE posts SET current_capture_id = NULL, cover_object = NULL, cover_url = NULL,
                cover_url_expires_at = NULL, thumbhash = NULL, caption = NULL, media_type = ?2,
                media_count = 1, archive_state = 'pending', updated_at = ?3
         WHERE id = ?1",
    )?
    .execute(params![post_id, MEDIA_TYPE, now])?;
    restore_ai(conn, post_id, None, now)
}

/// The objects the post's mirror references: its cover and its slides.
fn mirror_objects(conn: &Connection, post_id: i64) -> Result<Vec<i64>> {
    let ids = conn
        .prepare_cached(
            "SELECT cover_object FROM posts WHERE id = ?1 AND cover_object IS NOT NULL
             UNION SELECT object_id FROM post_media WHERE post_id = ?1 AND object_id IS NOT NULL
             UNION SELECT video_object_id FROM post_media
                   WHERE post_id = ?1 AND video_object_id IS NOT NULL",
        )?
        .query_map([post_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The objects a version references: its hero, its favicon and its files.
fn version_objects(conn: &Connection, capture_id: i64) -> Result<Vec<i64>> {
    let ids = conn
        .prepare_cached(
            "SELECT hero_object FROM web_captures WHERE id = ?1 AND hero_object IS NOT NULL
             UNION SELECT favicon_object FROM web_captures
                   WHERE id = ?1 AND favicon_object IS NOT NULL
             UNION SELECT object_id FROM web_capture_assets WHERE capture_id = ?1",
        )?
        .query_map([capture_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Deletes a version and its files. The files are deleted explicitly, so
/// none is left behind on a connection without foreign keys.
fn drop_version(conn: &Connection, capture_id: i64) -> Result<()> {
    conn.prepare_cached("DELETE FROM web_capture_assets WHERE capture_id = ?1")?
        .execute([capture_id])?;
    conn.prepare_cached("DELETE FROM web_captures WHERE id = ?1")?
        .execute([capture_id])?;
    Ok(())
}

/// The post's AI layer as a frozen version keeps it (see the module docs);
/// `None` when the post has none.
fn freeze_ai(conn: &Connection, post_id: i64) -> Result<Option<String>> {
    const TEXT: [(&str, &str); 9] = [
        ("status", "ai_status"),
        ("provider", "ai_provider"),
        ("model", "ai_model"),
        ("error", "ai_error"),
        ("description", "ai_description"),
        ("saveReason", "ai_save_reason"),
        ("language", "ai_language"),
        ("category", "ai_category"),
        ("contentType", "ai_content_type"),
    ];
    let row = conn
        .prepare_cached(
            "SELECT ai_status, ai_provider, ai_model, ai_error, ai_description, ai_save_reason,
                    ai_language, ai_category, ai_content_type, ai_schema_version,
                    ai_analyzed_at, ai_tags_json, ai_entities_json, ai_keywords_json, ai_web_json
             FROM posts WHERE id = ?1",
        )?
        .query_row([post_id], |r| {
            let mut texts = Vec::with_capacity(TEXT.len());
            for i in 0..TEXT.len() {
                texts.push(r.get::<_, Option<String>>(i)?);
            }
            let ints: [Option<i64>; 2] = [r.get(9)?, r.get(10)?];
            let lists: [Option<String>; 4] = [r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?];
            Ok((texts, ints, lists))
        })?;
    let (texts, [schema_version, analyzed_at], [tags, entities, keywords, web]) = row;
    if texts.iter().all(Option::is_none)
        && schema_version.is_none()
        && analyzed_at.is_none()
        && [&tags, &entities, &keywords, &web]
            .iter()
            .all(|v| v.is_none())
    {
        return Ok(None);
    }
    let mut layer = Map::new();
    for ((key, _), value) in TEXT.iter().zip(texts) {
        layer.insert((*key).to_owned(), value.map_or(Value::Null, Value::String));
    }
    let number = |v: Option<i64>| v.map_or(Value::Null, Value::from);
    layer.insert("schemaVersion".into(), number(schema_version));
    layer.insert("analyzedAt".into(), number(analyzed_at));
    let strings = |raw: &Option<String>| Value::from(json_strings(raw.as_deref()));
    layer.insert("tags".into(), strings(&tags));
    layer.insert("entities".into(), strings(&entities));
    layer.insert("keywords".into(), strings(&keywords));
    layer.insert(
        "web".into(),
        json_value(web.as_deref()).unwrap_or(Value::Null),
    );
    // The tiers of the AI tag rows, so a restore rebuilds the same rows.
    let tiers: Vec<(String, String)> = conn
        .prepare_cached(
            "SELECT tag_form, tier FROM post_tags
             WHERE post_id = ?1 AND source = 'ai' AND tier IS NOT NULL ORDER BY tag_norm",
        )?
        .query_map([post_id], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<_>>()?;
    if !tiers.is_empty() {
        for (key, tier) in [("generalTags", "general"), ("specificTags", "specific")] {
            let forms: Vec<&str> = tiers
                .iter()
                .filter(|(_, t)| t == tier)
                .map(|(form, _)| form.as_str())
                .collect();
            layer.insert(key.into(), Value::from(forms));
        }
    }
    Ok(Some(Value::Object(layer).to_string()))
}

/// Writes the frozen layer `frozen` into the post `post_id`, rebuilding its
/// AI tag and entity rows; `None`, or JSON that is not an object, clears the
/// layer. The error is the frozen one and the retry state starts over. The
/// caller reindexes the post.
fn restore_ai(conn: &Connection, post_id: i64, frozen: Option<&str>, now: i64) -> Result<()> {
    let layer = frozen
        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
        .filter(Value::is_object);
    let error = match &layer {
        Some(layer) => {
            posts::set_ai(conn, post_id, &thaw(layer), now)?;
            layer
                .get("error")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }
        None => {
            posts::clear_ai(conn, post_id, now)?;
            None
        }
    };
    conn.prepare_cached(
        "UPDATE posts SET ai_error = ?2, ai_attempts = 0, ai_next_at = NULL WHERE id = ?1",
    )?
    .execute(params![post_id, error])?;
    Ok(())
}

/// AI statuses of an analysis that is queued or running (the desktop's
/// `AiStatus`): a frozen layer restores them as unanalyzed.
const IN_FLIGHT: [&str; 2] = ["pending", "analyzing"];

/// The AI layer of a frozen JSON object, read leniently: a field of the
/// wrong type reads as absent, a list keeps only its strings.
fn thaw(frozen: &Value) -> AiLayer {
    let text = |key: &str| frozen.get(key).and_then(Value::as_str).map(str::to_owned);
    let int = |key: &str| frozen.get(key).and_then(Value::as_i64);
    let list = |key: &str| -> Option<Vec<String>> {
        frozen.get(key).and_then(Value::as_array).map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
    };
    AiLayer {
        status: text("status").filter(|s| !IN_FLIGHT.contains(&s.as_str())),
        provider: text("provider"),
        model: text("model"),
        schema_version: int("schemaVersion"),
        description: text("description"),
        save_reason: text("saveReason"),
        language: text("language"),
        category: text("category"),
        content_type: text("contentType"),
        tags: list("tags").unwrap_or_default(),
        general_tags: list("generalTags"),
        specific_tags: list("specificTags"),
        entities: list("entities").unwrap_or_default(),
        keywords: list("keywords").unwrap_or_default(),
        web: frozen.get("web").filter(|w| !w.is_null()).cloned(),
        analyzed_at: int("analyzedAt"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn domains_follow_the_desktop() {
        assert_eq!(
            domain_of("https://WWW.Studio.Example.test/work?x=1").as_deref(),
            Some("studio.example.test")
        );
        assert_eq!(
            domain_of("http://www.example.test").as_deref(),
            Some("example.test")
        );
        assert_eq!(domain_of("not a url"), None);
        assert_eq!(domain_of("mailto:someone@example.test"), None);
    }

    #[test]
    fn the_caption_is_title_and_description() {
        assert_eq!(
            caption(Some("Studio"), Some("Product design")).as_deref(),
            Some("Studio\n\nProduct design")
        );
        assert_eq!(caption(Some(" "), Some("Only")).as_deref(), Some("Only"));
        assert_eq!(caption(None, Some("  ")), None);
        let long = "é".repeat(CAPTION_MAX_CHARS);
        let cut = caption(Some("Title"), Some(&long)).unwrap();
        assert_eq!(cut.chars().count(), CAPTION_MAX_CHARS);
        assert!(cut.starts_with("Title\n\né"));
    }

    #[test]
    fn a_page_image_prefers_the_screenshot_then_the_hero() {
        let file = |page_index, role: AssetRole, seq, object_id| Asset {
            page_index,
            role: role.as_str().to_owned(),
            seq,
            object_id,
        };
        let files = [
            file(0, AssetRole::Band, 1, 11),
            file(0, AssetRole::Band, 0, 10),
            file(0, AssetRole::Hero, 0, 20),
            file(1, AssetRole::Band, 0, 30),
            file(1, AssetRole::Footer, 0, 40),
            file(2, AssetRole::Footer, 0, 50),
            file(SITE_LEVEL, AssetRole::Og, 0, 60),
        ];
        assert_eq!(page_image(&files, 0), Some(20));
        assert_eq!(page_image(&files, 1), Some(30));
        assert_eq!(page_image(&files, 2), None);
        let mut with_screenshot = files.to_vec();
        with_screenshot.push(file(0, AssetRole::Screenshot, 0, 70));
        assert_eq!(page_image(&with_screenshot, 0), Some(70));
        assert_eq!(first_image(&files[3..], 3), Some(30));
        assert_eq!(first_image(&files[5..], 3), None);
    }

    #[test]
    fn a_frozen_layer_thaws_leniently() {
        let frozen: Value = serde_json::from_str(
            r#"{"status":"done","model":"m","schemaVersion":"2","tags":["a",1,"b"],
                "generalTags":["a"],"entities":null,"web":{"facets":{"style":["minimal"]}},
                "analyzedAt":1700000000000}"#,
        )
        .unwrap();
        let layer = thaw(&frozen);
        assert_eq!(layer.status.as_deref(), Some("done"));
        assert_eq!(layer.schema_version, None, "a string is not a version");
        assert_eq!(layer.tags, ["a", "b"]);
        assert_eq!(layer.general_tags.as_deref(), Some(&["a".to_owned()][..]));
        assert_eq!(layer.specific_tags, None);
        assert!(layer.entities.is_empty());
        assert_eq!(layer.analyzed_at, Some(1_700_000_000_000));
        assert!(layer.web.is_some());

        for (status, restored) in [
            ("pending", None),
            ("analyzing", None),
            ("error", Some("error")),
        ] {
            let frozen = serde_json::json!({"status": status, "description": "kept"});
            let layer = thaw(&frozen);
            assert_eq!(layer.status.as_deref(), restored, "{status}");
            assert_eq!(layer.description.as_deref(), Some("kept"));
        }
    }
}
