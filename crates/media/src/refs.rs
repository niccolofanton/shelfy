//! The database side of the store (plan §2.7, §2.13): `media_objects` rows,
//! the columns that reference them, and the reference counting the garbage
//! collector (P4) builds on.
//!
//! Like the core repositories, these are plain functions over a `&Connection`
//! that run inside `UserDb::read` or `UserDb::write` and take `now` (unix ms)
//! as an argument. Row inserts go through
//! [`shelfy_core::repo::media::upsert_object`], the one place that writes new
//! `media_objects` rows.
//!
//! The rules that keep rows and files in step are in the [`crate::store`]
//! documentation; [`publish_and_record`], [`record_rendition`] and
//! [`collect_garbage`] apply them.

use std::sync::LazyLock;

use rusqlite::types::Type;
use rusqlite::{Connection, OptionalExtension as _, Row, params};
use shelfy_core::db::DbError;
use shelfy_core::repo::media::{NewMediaObject, upsert_object};
use shelfy_core::repo::{RepoError, Result};

use crate::digest::Digest;
use crate::kind::MediaKind;
use crate::name::{Rendition, Variants};
use crate::store::{StagedObject, StoredObject, UserMedia};

/// What an object is (`media_objects.role`, plan §2.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// An archived image: a cover or a slide.
    Image,
    /// The still image of a video.
    Poster,
    /// A kept video.
    Video,
    /// An uploaded file that is not an image or a video.
    File,
    /// A client-made preview of an upload.
    Preview,
    /// A website screenshot.
    Screenshot,
    /// A band of a website capture.
    Band,
    /// A section of a website capture.
    Section,
    /// The footer of a website capture.
    Footer,
    /// A filmstrip of a website capture.
    Filmstrip,
    /// A site favicon.
    Favicon,
    /// A site's `og:image`.
    Og,
}

impl Role {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Poster => "poster",
            Self::Video => "video",
            Self::File => "file",
            Self::Preview => "preview",
            Self::Screenshot => "screenshot",
            Self::Band => "band",
            Self::Section => "section",
            Self::Footer => "footer",
            Self::Filmstrip => "filmstrip",
            Self::Favicon => "favicon",
            Self::Og => "og",
        }
    }
}

/// Who produced an object (`media_objects.origin`, plan §2.7).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Origin {
    /// Fetched by the server (archive worker, on-demand video).
    Server,
    /// Uploaded by the browser extension.
    Extension,
    /// Uploaded by the user.
    Upload,
    /// Produced by the capture service.
    Capture,
    /// Moved from a desktop library.
    Migration,
}

impl Origin {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Server => "server",
            Self::Extension => "extension",
            Self::Upload => "upload",
            Self::Capture => "capture",
            Self::Migration => "migration",
        }
    }
}

/// What the caller knows about an object beyond its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectMeta {
    /// What the object is.
    pub role: Role,
    /// Who produced it.
    pub origin: Origin,
    /// Display width (after EXIF orientation), when known.
    pub width: Option<u32>,
    /// Display height (after EXIF orientation), when known.
    pub height: Option<u32>,
    /// Duration of a video, when known.
    pub duration_ms: Option<i64>,
    /// The renditions already stored.
    pub variants: Variants,
}

impl ObjectMeta {
    /// Metadata with only the role and the origin.
    #[must_use]
    pub const fn new(role: Role, origin: Origin) -> Self {
        Self {
            role,
            origin,
            width: None,
            height: None,
            duration_ms: None,
            variants: Variants::NONE,
        }
    }
}

/// A `media_objects` row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ObjectRow {
    /// Row id, referenced by `posts`, `post_media` and the web captures.
    pub id: i64,
    /// The digest of the content.
    pub digest: Digest,
    /// File extension as stored.
    pub ext: String,
    /// MIME type as stored.
    pub mime: String,
    /// Size in bytes.
    pub size: i64,
    /// Display width, when known.
    pub width: Option<i64>,
    /// Display height, when known.
    pub height: Option<i64>,
    /// The renditions stored next to it.
    pub variants: Variants,
    /// When it lost its last reference; `None` while referenced.
    pub unreferenced_since: Option<i64>,
}

impl ObjectRow {
    /// The type, when the stored extension is in the allowlist.
    #[must_use]
    pub fn kind(&self) -> Option<MediaKind> {
        MediaKind::from_ext(&self.ext)
    }
}

/// Records the row of a stored object and returns its id.
///
/// When the content was recorded before, the existing row is reused: it is
/// marked referenced again, gains `meta.variants`, and gets the dimensions and
/// duration it did not have yet. Role and origin stay as first recorded.
///
/// # Errors
///
/// Database errors.
pub fn record(
    conn: &Connection,
    object: &StoredObject,
    meta: &ObjectMeta,
    now: i64,
) -> Result<i64> {
    let width = meta.width.map(i64::from);
    let height = meta.height.map(i64::from);
    let id = upsert_object(
        conn,
        &NewMediaObject {
            sha256: *object.digest.as_bytes(),
            ext: object.kind.ext().to_owned(),
            mime: object.kind.mime().to_owned(),
            bytes: i64::try_from(object.size).unwrap_or(i64::MAX),
            width,
            height,
            duration_ms: meta.duration_ms,
            role: meta.role.as_str().to_owned(),
            variants: meta.variants.bits(),
            origin: meta.origin.as_str().to_owned(),
        },
        now,
    )?;
    conn.prepare_cached(
        "UPDATE media_objects
         SET variants = variants | ?2, width = coalesce(width, ?3),
             height = coalesce(height, ?4), duration_ms = coalesce(duration_ms, ?5)
         WHERE id = ?1",
    )?
    .execute(params![
        id,
        meta.variants.bits(),
        width,
        height,
        meta.duration_ms
    ])?;
    Ok(id)
}

/// Publishes `staged`, writes its `renditions` and records its row (with
/// those renditions in `variants`), in the caller's write transaction: rule 1
/// of the store's consistency protocol. Returns the row id and the stored
/// object.
///
/// # Errors
///
/// The file system refused ([`DbError::Io`]), or a database error. The
/// caller's transaction then rolls back; files already written are harmless
/// (see [`crate::store`]).
pub fn publish_and_record(
    conn: &Connection,
    media: &UserMedia,
    staged: StagedObject,
    renditions: &[(Rendition, &[u8])],
    meta: &ObjectMeta,
    now: i64,
) -> Result<(i64, StoredObject)> {
    let stored = staged.publish().map_err(io_error)?;
    let mut meta = *meta;
    for &(rendition, bytes) in renditions {
        media
            .store_rendition(&stored.digest, rendition, bytes)
            .map_err(io_error)?;
        meta.variants = meta.variants.with(rendition);
    }
    let id = record(conn, &stored, &meta, now)?;
    Ok((id, stored))
}

/// Writes a rendition of the recorded object `id` (digest `digest`) and marks
/// it in `variants`, in the caller's write transaction (rule 1 again). Returns
/// whether the row exists; the file is written either way.
///
/// # Errors
///
/// The file system refused ([`DbError::Io`]), or a database error.
pub fn record_rendition(
    conn: &Connection,
    media: &UserMedia,
    id: i64,
    digest: &Digest,
    rendition: Rendition,
    bytes: &[u8],
) -> Result<bool> {
    media
        .store_rendition(digest, rendition, bytes)
        .map_err(io_error)?;
    add_variants(conn, id, Variants::NONE.with(rendition))
}

/// The row of `digest`, if recorded.
///
/// # Errors
///
/// Database errors.
pub fn find(conn: &Connection, digest: &Digest) -> Result<Option<ObjectRow>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {OBJECT_COLUMNS} FROM media_objects WHERE sha256 = ?1"
        ))?
        .query_row([&digest.as_bytes()[..]], object_row)
        .optional()?)
}

/// Adds renditions to the row `id`. Returns whether the row exists.
///
/// # Errors
///
/// Database errors.
pub fn add_variants(conn: &Connection, id: i64, variants: Variants) -> Result<bool> {
    let changed = conn
        .prepare_cached("UPDATE media_objects SET variants = variants | ?2 WHERE id = ?1")?
        .execute(params![id, variants.bits()])?;
    Ok(changed == 1)
}

/// Sets the ThumbHash of every post whose cover is the object `object_id`.
/// Returns how many posts changed.
///
/// # Errors
///
/// Database errors.
pub fn set_cover_thumbhash(
    conn: &Connection,
    object_id: i64,
    thumbhash: &[u8],
    now: i64,
) -> Result<usize> {
    Ok(conn
        .prepare_cached(
            "UPDATE posts SET thumbhash = ?2, updated_at = ?3
             WHERE cover_object = ?1 AND thumbhash IS NOT ?2",
        )?
        .execute(params![object_id, thumbhash, now])?)
}

// ── Reference counting ───────────────────────────────────────────────────────

/// Every `(table, column)` that references a `media_objects` row. A test
/// checks it against the foreign keys of the schema, so a migration that adds
/// a reference cannot forget it here.
pub const REFERENCES: [(&str, &str); 6] = [
    ("posts", "cover_object"),
    ("post_media", "object_id"),
    ("post_media", "video_object_id"),
    ("web_captures", "hero_object"),
    ("web_captures", "favicon_object"),
    ("web_capture_assets", "object_id"),
];

/// SQL condition: the `media_objects` row in scope has no reference.
static UNREFERENCED: LazyLock<String> = LazyLock::new(|| {
    REFERENCES
        .iter()
        .map(|(table, column)| {
            format!("NOT EXISTS (SELECT 1 FROM {table} WHERE {column} = media_objects.id)")
        })
        .collect::<Vec<_>>()
        .join(" AND ")
});

/// How many columns reference the object `id`.
///
/// # Errors
///
/// Database errors.
pub fn reference_count(conn: &Connection, id: i64) -> Result<u64> {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        let terms: Vec<String> = REFERENCES
            .iter()
            .map(|(table, column)| format!("(SELECT count(*) FROM {table} WHERE {column} = ?1)"))
            .collect();
        format!("SELECT {}", terms.join(" + "))
    });
    let count: i64 = conn.prepare_cached(&SQL)?.query_row([id], |r| r.get(0))?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Stamps `unreferenced_since = now` on those of `ids` that no column
/// references any more and that are not stamped yet. Call it after removing
/// references; returns how many were stamped.
///
/// # Errors
///
/// Database errors.
pub fn stamp_unreferenced(conn: &Connection, ids: &[i64], now: i64) -> Result<usize> {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "UPDATE media_objects SET unreferenced_since = ?2
             WHERE id IN (SELECT value FROM json_each(?1))
               AND unreferenced_since IS NULL AND {}",
            *UNREFERENCED
        )
    });
    if ids.is_empty() {
        return Ok(0);
    }
    Ok(conn
        .prepare_cached(&SQL)?
        .execute(params![id_list(ids), now])?)
}

/// Recounts every object: stamps the unreferenced ones that are not stamped
/// and clears the stamp of those referenced again. The nightly safety net for
/// a write path that forgot [`stamp_unreferenced`]. Returns
/// `(stamped, cleared)`.
///
/// # Errors
///
/// Database errors.
pub fn restamp(conn: &Connection, now: i64) -> Result<(usize, usize)> {
    static STAMP: LazyLock<String> = LazyLock::new(|| {
        format!(
            "UPDATE media_objects SET unreferenced_since = ?1
             WHERE unreferenced_since IS NULL AND {}",
            *UNREFERENCED
        )
    });
    static CLEAR: LazyLock<String> = LazyLock::new(|| {
        format!(
            "UPDATE media_objects SET unreferenced_since = NULL
             WHERE unreferenced_since IS NOT NULL AND NOT ({})",
            *UNREFERENCED
        )
    });
    let stamped = conn.prepare_cached(&STAMP)?.execute([now])?;
    let cleared = conn.prepare_cached(&CLEAR)?.execute([])?;
    Ok((stamped, cleared))
}

/// Counts objects eligible for collection without changing stamps, rows or files.
/// Uses the same reference predicate as collection, including trashed posts.
///
/// # Errors
///
/// Database errors.
pub fn garbage_totals(conn: &Connection, cutoff: i64) -> Result<(u64, u64)> {
    let sql = format!(
        "SELECT count(*), coalesce(sum(bytes), 0) FROM media_objects
         WHERE unreferenced_since IS NOT NULL AND unreferenced_since <= ?1 AND {}",
        *UNREFERENCED
    );
    let (objects, bytes) = conn.query_row(&sql, [cutoff], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
    })?;
    Ok((
        u64::try_from(objects).unwrap_or(0),
        u64::try_from(bytes).unwrap_or(0),
    ))
}

/// An object deleted by [`collect_garbage`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Garbage {
    /// The deleted row id.
    pub id: i64,
    /// The digest of the content.
    pub digest: Digest,
    /// The type, when the stored extension is in the allowlist.
    pub kind: Option<MediaKind>,
    /// Size in bytes.
    pub size: i64,
}

/// Deletes up to `limit` objects unreferenced since `cutoff` or earlier (the
/// plan keeps them 24 h) and their files, in the caller's write transaction:
/// rule 2 of the store's consistency protocol. References are checked again,
/// so an object that gained one since it was stamped stays. Returns what was
/// deleted.
///
/// # Errors
///
/// Database errors, or the file system refused ([`DbError::Io`]); the
/// caller's transaction then rolls back.
pub fn collect_garbage(
    conn: &Connection,
    media: &UserMedia,
    cutoff: i64,
    limit: u32,
) -> Result<Vec<Garbage>> {
    static SQL: LazyLock<String> = LazyLock::new(|| {
        format!(
            "DELETE FROM media_objects WHERE id IN (
               SELECT id FROM media_objects
               WHERE unreferenced_since IS NOT NULL AND unreferenced_since <= ?1 AND {}
               ORDER BY unreferenced_since, id LIMIT ?2)
             RETURNING id, sha256, ext, bytes",
            *UNREFERENCED
        )
    });
    let garbage: Vec<Garbage> = conn
        .prepare_cached(&SQL)?
        .query_map(params![cutoff, limit], |row| {
            Ok(Garbage {
                id: row.get(0)?,
                digest: digest_at(row, 1)?,
                kind: MediaKind::from_ext(&row.get::<_, String>(2)?),
                size: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    for item in &garbage {
        if let Some(kind) = item.kind {
            media.remove(&item.digest, kind).map_err(io_error)?;
        }
    }
    Ok(garbage)
}

// ── Internals ────────────────────────────────────────────────────────────────

const OBJECT_COLUMNS: &str =
    "id, sha256, ext, mime, bytes, width, height, variants, unreferenced_since";

fn object_row(row: &Row<'_>) -> rusqlite::Result<ObjectRow> {
    Ok(ObjectRow {
        id: row.get(0)?,
        digest: digest_at(row, 1)?,
        ext: row.get(2)?,
        mime: row.get(3)?,
        size: row.get(4)?,
        width: row.get(5)?,
        height: row.get(6)?,
        variants: Variants::from_bits(row.get(7)?),
        unreferenced_since: row.get(8)?,
    })
}

fn digest_at(row: &Row<'_>, index: usize) -> rusqlite::Result<Digest> {
    let blob: Vec<u8> = row.get(index)?;
    Digest::from_slice(&blob).ok_or_else(|| {
        rusqlite::Error::FromSqlConversionFailure(
            index,
            Type::Blob,
            "media_objects.sha256 is not 32 bytes".into(),
        )
    })
}

fn io_error(err: std::io::Error) -> RepoError {
    RepoError::Db(DbError::Io(err))
}

/// A JSON array of ids, the parameter of `IN (SELECT value FROM json_each(?))`.
fn id_list(ids: &[i64]) -> String {
    let items: Vec<String> = ids.iter().map(i64::to_string).collect();
    format!("[{}]", items.join(","))
}
