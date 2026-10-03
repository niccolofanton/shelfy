//! Store and link one archived object (plan D4, §2.13): the archive drain's
//! server fetches, and the extension's uploads (P2-14, origin `extension`).
//!
//! 1. [`prepare`] (blocking, on the image pool) turns the staged bytes into
//!    the master: an image as served when it is at most 2,048 px and 1.5 MB,
//!    else a WebP q82 at 2,048 px; a video's poster always a WebP q78 at
//!    1,080 px ([`RenderSpec::MASTER`], [`RenderSpec::POSTER`]). It renders
//!    the `g480` of a cover and of slides 1–3 of a multi-image post, and a
//!    cover's ThumbHash. An image the pipeline cannot decode is kept as
//!    served, without renditions.
//! 2. [`commit`], in one write transaction of the library, last of all
//!    after the fetch: checks the slot still needs the object (an upload
//!    may have filled it meanwhile, or the post may be gone), publishes and
//!    records the object (`refs::publish_and_record`, the CAS dedupes),
//!    links it (`cover_object`; `post_media.object_id`; slide 0 shares the
//!    cover's object), sets the cover's ThumbHash, clears the item's fetch
//!    error, derives the post's archive state again, and commits the quota
//!    reservation (`quota::new_bytes` first). A crash at any point leaves
//!    either everything or no row: at worst an orphan file, which the next
//!    publish of the same bytes reuses and the GC removes.
//!
//! [`link_existing`] links a cover to the object its slide 0 already
//! stores, with the renditions it lacks.

use std::io::Cursor;
use std::sync::Arc;

use rusqlite::{Connection, OptionalExtension as _, Transaction, params};
use shelfy_core::ingest::archive::{ArchivePolicy, Scope, refresh_states};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::refs::{self, ObjectMeta, Origin, Role};
use shelfy_media::render::{self, RenderSpec, Rendered, keeps_original};
use shelfy_media::store::{IngestError, IngestLimits, StagedObject, UserMedia};
use shelfy_media::{Digest, MediaKind, Rendition, Variants};

use super::select::{Item, Slot};
use crate::quota::{self, Reservation};

/// Where an object goes, and what it needs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The post's internal id.
    pub post_id: i64,
    /// Its key.
    pub key: String,
    /// Its platform.
    pub platform: Platform,
    /// What the object fills.
    pub slot: Slot,
    /// A video's poster: always a WebP q78 at ≤ 1,080 px.
    pub poster: bool,
    /// Also the post's cover: gets the ThumbHash.
    pub cover: bool,
    /// Gets a `g480`.
    pub grid: bool,
}

impl From<&Item> for Target {
    fn from(item: &Item) -> Self {
        Self {
            post_id: item.post_id,
            key: item.key.clone(),
            platform: item.platform,
            slot: item.slot,
            poster: item.poster,
            cover: item.cover,
            grid: item.grid,
        }
    }
}

/// The target of an object for `slot` of the post `key`, as the drain's
/// selection makes it: for an extension upload (P2-14). `None` when the
/// post does not exist or the slot is not one the archive fills (a slide
/// that is not an image).
///
/// # Errors
///
/// A query failed.
pub fn target_of(conn: &Connection, key: &str, slot: Slot) -> Result<Option<Target>, RepoError> {
    let Some((post_id, platform, media_type, no_cover)) = conn
        .prepare_cached(
            "SELECT id, platform, media_type, cover_object IS NULL FROM posts WHERE key = ?1",
        )?
        .query_row([key], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, Platform>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
            ))
        })
        .optional()?
    else {
        return Ok(None);
    };
    let kind_at = |position: i64| -> Result<Option<String>, RepoError> {
        Ok(conn
            .prepare_cached("SELECT kind FROM post_media WHERE post_id = ?1 AND position = ?2")?
            .query_row(params![post_id, position], |row| row.get(0))
            .optional()?)
    };
    let target = |slot, poster, cover, grid| Target {
        post_id,
        key: key.to_owned(),
        platform,
        slot,
        poster,
        cover,
        grid,
    };
    Ok(match slot {
        Slot::Cover => {
            let poster = kind_at(0)?.map_or(media_type == "video", |kind| kind == "video");
            Some(target(Slot::Cover, poster, true, true))
        }
        Slot::Slide(position) => {
            if kind_at(position)?.as_deref() != Some("image") {
                return Ok(None);
            }
            let images: i64 = conn
                .prepare_cached(
                    "SELECT count(*) FROM post_media WHERE post_id = ?1 AND kind = 'image'",
                )?
                .query_row([post_id], |row| row.get(0))?;
            let cover = position == 0 && no_cover;
            let grid = cover || (images > 1 && (1..=3).contains(&position));
            Some(target(slot, false, cover, grid))
        }
    })
}

/// The master of an object and its renditions, ready to commit.
#[derive(Debug)]
pub struct Prepared {
    staged: StagedObject,
    g480: Option<Vec<u8>>,
    thumbhash: Option<Vec<u8>>,
    width: Option<u32>,
    height: Option<u32>,
    role: Role,
    /// The served bytes were replaced by a WebP master.
    pub reencoded: bool,
}

impl Prepared {
    /// The master's size, in bytes.
    #[must_use]
    pub fn size(&self) -> u64 {
        self.staged.size()
    }

    /// The master's type.
    #[must_use]
    pub fn kind(&self) -> MediaKind {
        self.staged.kind()
    }

    /// Whether a `g480` was rendered.
    #[must_use]
    pub fn has_g480(&self) -> bool {
        self.g480.is_some()
    }

    /// Whether a ThumbHash was computed.
    #[must_use]
    pub fn has_thumbhash(&self) -> bool {
        self.thumbhash.is_some()
    }
}

/// Makes the master and the renditions of `staged` for `target` (see the
/// module docs). Blocking and CPU-bound: run it on the image pool.
///
/// # Errors
///
/// Writing the WebP master to the store's temporary directory failed.
pub fn prepare(
    media: &UserMedia,
    staged: StagedObject,
    target: &Target,
) -> Result<Prepared, IngestError> {
    let role = if target.poster {
        Role::Poster
    } else {
        Role::Image
    };
    let mut size = (None, None);
    let mut master_webp: Option<Arc<[u8]>> = None;
    let staged = if !staged.kind().is_renderable() {
        staged
    } else {
        let spec = if target.poster {
            Some(RenderSpec::POSTER)
        } else {
            match render::dimensions(staged.path()) {
                Ok((width, height))
                    if keeps_original(staged.kind(), staged.size(), width, height) =>
                {
                    size = (Some(width), Some(height));
                    None
                }
                Ok(_) => Some(RenderSpec::MASTER),
                Err(err) => {
                    tracing::debug!(error = %err, "an archived image cannot be decoded: kept as served");
                    None
                }
            }
        };
        match spec.map(|spec| render::render_file(staged.path(), spec)) {
            Some(Ok(master)) => {
                size = (Some(master.width), Some(master.height));
                let webp: Arc<[u8]> = master.webp.into();
                let reencoded =
                    media.ingest(Cursor::new(&webp[..]), IngestLimits::ARCHIVE_IMAGE)?;
                master_webp = Some(webp);
                reencoded
            }
            Some(Err(err)) => {
                tracing::debug!(error = %err, "an archived image cannot be re-encoded: kept as served");
                staged
            }
            None => staged,
        }
    };
    let reencoded = master_webp.is_some();
    let (g480, thumbhash) = if target.grid && staged.kind().is_renderable() {
        let rendered: Result<Rendered, _> = match &master_webp {
            Some(webp) => render::render_bytes(webp, RenderSpec::G480),
            None => render::render_file(staged.path(), RenderSpec::G480),
        };
        match rendered {
            Ok(grid) => {
                if size.0.is_none() {
                    size = (Some(grid.source_width), Some(grid.source_height));
                }
                (Some(grid.webp), target.cover.then_some(grid.thumbhash))
            }
            Err(err) => {
                tracing::debug!(error = %err, "no g480 for an archived image");
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    Ok(Prepared {
        staged,
        g480,
        thumbhash,
        width: size.0,
        height: size.1,
        role,
        reencoded,
    })
}

/// What [`commit`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Committed {
    /// Stored and linked.
    Stored {
        /// The object's row.
        object_id: i64,
        /// The post's cover was linked.
        cover: bool,
        /// Bytes the object added to the library (0 when it was known).
        added_bytes: u64,
    },
    /// The slot no longer needs it (filled meanwhile, or the post or slide
    /// is gone): nothing was written, the reservation is released.
    Unwanted,
}

/// Where and how [`commit`] stores.
#[derive(Clone, Copy, Debug)]
pub struct StoreContext<'a> {
    /// The user's store.
    pub media: &'a UserMedia,
    /// Who produced the bytes: `server` for a fetch, `extension` for an
    /// upload (P2-14).
    pub origin: Origin,
    /// The policy the post's state is derived with (the archive's current
    /// modes and the user's asset types).
    pub policy: &'a ArchivePolicy,
    /// The time, unix ms.
    pub now: i64,
}

/// Stores `prepared` and links it to `target` in the write transaction
/// `tx`, derives the post's state, and commits `reservation` last (see the
/// module docs).
///
/// # Errors
///
/// The file system or the database failed, or the quota commit failed: the
/// transaction must roll back.
pub fn commit(
    tx: &Transaction<'_>,
    cx: &StoreContext<'_>,
    target: &Target,
    prepared: Prepared,
    reservation: Reservation,
) -> Result<Committed, RepoError> {
    let StoreContext {
        media,
        origin,
        policy,
        now,
    } = *cx;
    if !wanted(tx, target)? {
        return Ok(Committed::Unwanted);
    }
    let Prepared {
        staged,
        g480,
        thumbhash,
        width,
        height,
        role,
        ..
    } = prepared;
    let added = quota::new_bytes(tx, &[(staged.digest(), staged.size())])?;
    let renditions: Vec<(Rendition, &[u8])> = g480
        .as_deref()
        .map(|bytes| (Rendition::G480, bytes))
        .into_iter()
        .collect();
    let meta = ObjectMeta {
        width,
        height,
        ..ObjectMeta::new(role, origin)
    };
    let (object_id, _) = refs::publish_and_record(tx, media, staged, &renditions, &meta, now)?;
    let cover = link(tx, target, object_id)?;
    if cover && let Some(thumbhash) = &thumbhash {
        refs::set_cover_thumbhash(tx, object_id, thumbhash, now)?;
    }
    refresh_states(tx, Scope::Posts(&[target.post_id]), policy, now)?;
    reservation.commit(added)?;
    Ok(Committed::Stored {
        object_id,
        cover,
        added_bytes: added,
    })
}

/// Whether `target`'s slot is still empty.
fn wanted(conn: &Connection, target: &Target) -> Result<bool, RepoError> {
    let found = match target.slot {
        Slot::Cover => conn
            .prepare_cached("SELECT cover_object IS NULL FROM posts WHERE id = ?1")?
            .query_row([target.post_id], |row| row.get::<_, bool>(0))
            .optional()?,
        Slot::Slide(position) => conn
            .prepare_cached(
                "SELECT object_id IS NULL FROM post_media WHERE post_id = ?1 AND position = ?2",
            )?
            .query_row(params![target.post_id, position], |row| {
                row.get::<_, bool>(0)
            })
            .optional()?,
    };
    Ok(found.unwrap_or(false))
}

/// Links `object_id` to `target`'s slot; returns whether the post's cover
/// is now this object.
fn link(conn: &Connection, target: &Target, object_id: i64) -> Result<bool, RepoError> {
    let link_cover = |conn: &Connection| -> Result<usize, RepoError> {
        Ok(conn
            .prepare_cached(
                "UPDATE posts SET cover_object = ?2, cover_fetch_next_at = NULL,
                                  cover_fetch_error = NULL
                 WHERE id = ?1 AND cover_object IS NULL",
            )?
            .execute(params![target.post_id, object_id])?)
    };
    let link_slide = |conn: &Connection, position: i64, kinds: &str| -> Result<usize, RepoError> {
        Ok(conn
            .prepare_cached(&format!(
                "UPDATE post_media SET object_id = ?3, fetch_next_at = NULL, fetch_error = NULL
                 WHERE post_id = ?1 AND position = ?2 AND object_id IS NULL AND kind IN ({kinds})"
            ))?
            .execute(params![target.post_id, position, object_id])?)
    };
    match target.slot {
        Slot::Cover => {
            let linked = link_cover(conn)?;
            // Slide 0 shares the cover's object: its image, or the poster of
            // its video.
            link_slide(conn, 0, "'image', 'video'")?;
            Ok(linked == 1)
        }
        Slot::Slide(position) => {
            link_slide(conn, position, "'image'")?;
            Ok(target.cover && position == 0 && link_cover(conn)? == 1)
        }
    }
}

/// A stored object, as [`link_existing`] reads it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExistingObject {
    /// Its row.
    pub id: i64,
    /// Its content.
    pub digest: Digest,
    /// Its type, when in the allowlist.
    pub kind: Option<MediaKind>,
    /// Its renditions.
    pub variants: Variants,
}

/// The object `id`, if recorded.
///
/// # Errors
///
/// A query failed.
pub fn existing_object(conn: &Connection, id: i64) -> Result<Option<ExistingObject>, RepoError> {
    Ok(conn
        .prepare_cached("SELECT sha256, ext, variants FROM media_objects WHERE id = ?1")?
        .query_row([id], |row| {
            let sha: Vec<u8> = row.get(0)?;
            let ext: String = row.get(1)?;
            Ok((sha, ext, row.get::<_, i64>(2)?))
        })
        .optional()?
        .and_then(|(sha, ext, variants)| {
            Some(ExistingObject {
                id,
                digest: Digest::from_slice(&sha)?,
                kind: MediaKind::from_ext(&ext),
                variants: Variants::from_bits(variants),
            })
        }))
}

/// The `g480` and ThumbHash of a stored object, rendered from its file
/// (blocking: run it on the image pool). `None` when it is not an image
/// the pipeline decodes.
#[must_use]
pub fn render_existing(media: &UserMedia, object: &ExistingObject) -> Option<Rendered> {
    let kind = object.kind.filter(|kind| kind.is_renderable())?;
    let path = media.object_path(&object.digest, kind);
    render::render_file(&path, RenderSpec::G480)
        .map_err(|err| tracing::debug!(error = %err, "no g480 for a stored cover"))
        .ok()
}

/// Links the post `post_id`'s cover to `object`, which its slide 0 already
/// stores, with its `g480` (when it lacked one) and the post's ThumbHash
/// from `rendered`; then derives the post's state. Returns whether the
/// cover was linked.
///
/// # Errors
///
/// The file system or the database failed.
pub fn link_existing(
    tx: &Transaction<'_>,
    media: &UserMedia,
    post_id: i64,
    object: &ExistingObject,
    rendered: Option<&Rendered>,
    policy: &ArchivePolicy,
    now: i64,
) -> Result<bool, RepoError> {
    let linked = tx
        .prepare_cached(
            "UPDATE posts SET cover_object = ?2, cover_fetch_next_at = NULL,
                              cover_fetch_error = NULL
             WHERE id = ?1 AND cover_object IS NULL",
        )?
        .execute(params![post_id, object.id])?
        == 1;
    if linked && let Some(rendered) = rendered {
        if !object.variants.contains(Rendition::G480) {
            refs::record_rendition(
                tx,
                media,
                object.id,
                &object.digest,
                Rendition::G480,
                &rendered.webp,
            )?;
        }
        refs::set_cover_thumbhash(tx, object.id, &rendered.thumbhash, now)?;
    }
    refresh_states(tx, Scope::Posts(&[post_id]), policy, now)?;
    Ok(linked)
}
