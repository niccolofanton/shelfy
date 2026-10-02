//! `media_objects` rows: the database side of the content-addressed store
//! (plan D3, §2.13). The files themselves belong to `shelfy-media`.

use rusqlite::{Connection, OptionalExtension, params};

use super::{RepoError, Result};

/// A media object to record. `sha256` is the raw 32-byte digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewMediaObject {
    /// SHA-256 of the content.
    pub sha256: [u8; 32],
    /// File extension, from the allowlist (`jpg`, `webp`, `mp4`, …).
    pub ext: String,
    /// MIME type.
    pub mime: String,
    /// Size in bytes.
    pub bytes: i64,
    /// Pixel width, when known.
    pub width: Option<i64>,
    /// Pixel height, when known.
    pub height: Option<i64>,
    /// Duration of audio/video, when known.
    pub duration_ms: Option<i64>,
    /// What the object is (`image`, `poster`, `video`, `screenshot`, …).
    pub role: String,
    /// Rendition bitmask (`1` = `g480`).
    pub variants: i64,
    /// Who produced it (`server`, `extension`, `upload`, `capture`, `migration`).
    pub origin: String,
}

/// Records an object and returns its id. Content addressing makes this
/// idempotent: an object with the same digest is reused (and is referenced
/// again, so its `unreferenced_since` is cleared).
///
/// # Errors
///
/// [`RepoError::Invalid`] for an empty extension; database errors otherwise.
pub fn upsert_object(conn: &Connection, object: &NewMediaObject, now: i64) -> Result<i64> {
    if object.ext.is_empty() || !object.ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(RepoError::Invalid {
            field: "ext",
            reason: "must be ASCII letters and digits",
        });
    }
    let existing: Option<i64> = conn
        .prepare_cached("SELECT id FROM media_objects WHERE sha256 = ?1")?
        .query_row([&object.sha256[..]], |r| r.get(0))
        .optional()?;
    if let Some(id) = existing {
        conn.prepare_cached("UPDATE media_objects SET unreferenced_since = NULL WHERE id = ?1")?
            .execute([id])?;
        return Ok(id);
    }
    conn.prepare_cached(
        "INSERT INTO media_objects (sha256, ext, mime, bytes, width, height, duration_ms, role,
                                    variants, origin, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
    )?
    .execute(params![
        &object.sha256[..],
        object.ext,
        object.mime,
        object.bytes,
        object.width,
        object.height,
        object.duration_ms,
        object.role,
        object.variants,
        object.origin,
        now
    ])?;
    Ok(conn.last_insert_rowid())
}

/// Stamps `unreferenced_since = now` on the given objects that no row
/// references any more, so the nightly GC can delete them after the grace
/// period. Returns how many were stamped.
///
/// # Errors
///
/// Database errors.
pub(crate) fn mark_unreferenced(conn: &Connection, object_ids: &[i64], now: i64) -> Result<usize> {
    if object_ids.is_empty() {
        return Ok(0);
    }
    let changed = conn
        .prepare_cached(
            "UPDATE media_objects SET unreferenced_since = ?2
             WHERE id IN (SELECT value FROM json_each(?1))
               AND unreferenced_since IS NULL
               AND NOT EXISTS (SELECT 1 FROM posts WHERE cover_object = media_objects.id)
               AND NOT EXISTS (SELECT 1 FROM post_media WHERE object_id = media_objects.id)
               AND NOT EXISTS (SELECT 1 FROM post_media WHERE video_object_id = media_objects.id)
               AND NOT EXISTS (SELECT 1 FROM web_captures
                               WHERE hero_object = media_objects.id
                                  OR favicon_object = media_objects.id)
               AND NOT EXISTS (SELECT 1 FROM web_capture_assets
                               WHERE object_id = media_objects.id)",
        )?
        .execute(params![super::id_list(object_ids), now])?;
    Ok(changed)
}
