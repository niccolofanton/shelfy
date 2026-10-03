//! The inputs a social catalog call needs (plan §2.15; P3-13), chosen from a
//! post's row and its stored media, plus the caption and vocabulary helpers.
//!
//! The core picks *which* objects to send and prepares the caption; the server
//! reads their bytes from the content store, transcodes them to JPEG (the
//! owner's node rejects WebP) and extracts video keyframes. So the references
//! here carry the object's digest and type, not its bytes, and the module
//! depends on no media code.
//!
//! The X1 gold-labeler findings shape the selection:
//!
//! - **Carousel video slides** (finding 2): a video slide carries its stored
//!   video (for keyframes), not only image slides.
//! - **The cover is the first slide** (finding 3): objects are de-duplicated
//!   by digest, so a cover that equals the first slide is sent once.
//! - **Hashtag walls** (finding 4): X1's fair poster run improved when the
//!   original caption was preserved. Hashtags remain weak data, as directed by
//!   the shared prompt; its builder strips markers and enforces the 1,200-unit
//!   caption budget. Selection never adds speech or transcript text.
use rusqlite::{Connection, OptionalExtension as _, params};

use super::catalog::CatalogKind;
use crate::repo::Result;

/// Frames sent for one post, over every slide (bounds the vision cost and the
/// node's memory on a shared machine, L19/L21).
pub const MAX_FRAMES: usize = 6;
/// Slides looked at when choosing frames.
pub const MAX_SLIDES: usize = 8;
/// Tags shown to the model as the archive's vocabulary (plan §2.15: top-30).
pub const VOCABULARY_SIZE: usize = 30;

/// A stored object a frame comes from: its digest and type, for the server to
/// resolve to a file in the content store.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrameObject {
    /// The object's SHA-256 (32 bytes), `media_objects.sha256`.
    pub sha256: Vec<u8>,
    /// Its file extension (`jpg`, `webp`, `mp4`…).
    pub ext: String,
    /// Its MIME type.
    pub mime: String,
    /// The renditions it has (`media_objects.variants`; bit 1 = `g480`).
    pub variants: i64,
}

/// One frame source of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Frame {
    /// A still image: a photo slide, or a video's poster or cover still.
    Image(FrameObject),
    /// A video slide: its stored file (for keyframes, finding 2) and its
    /// poster still. Either may be absent.
    Video {
        /// The stored video file (`post_media.video_object_id`), when kept.
        video: Option<FrameObject>,
        /// The poster still (`post_media.object_id`), a video slide's image.
        poster: Option<FrameObject>,
    },
}

/// Everything a catalog call needs about one post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostInputs {
    /// The post's public key.
    pub key: String,
    /// Which catalog it gets (social or web), from its platform and media type.
    pub kind: CatalogKind,
    /// Its media type.
    pub media_type: String,
    /// The original caption, including hashtag evidence; the shared catalog
    /// builder bounds and neutralizes it. `None` for absent/blank captions.
    pub caption: Option<String>,
    /// The frame sources, in order, de-duplicated by object (finding 3).
    pub frames: Vec<Frame>,
}

impl PostInputs {
    /// Whether any frame carries an image (a still, or a video poster): the
    /// catalog prompt's `frames` flag.
    #[must_use]
    pub fn has_frames(&self) -> bool {
        self.frames.iter().any(|frame| match frame {
            Frame::Image(_) => true,
            Frame::Video { poster, .. } => poster.is_some(),
        })
    }

    /// Whether any frame carries a stored video the server can take keyframes
    /// from.
    #[must_use]
    pub fn has_video(&self) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, Frame::Video { video: Some(_), .. }))
    }
}

/// The archive's most-used tags, canonical form, most frequent first (plan
/// §2.15: the top-30 vocabulary hint). The server caches it per generation.
///
/// # Errors
///
/// Database errors.
pub fn vocabulary(conn: &Connection, limit: usize) -> Result<Vec<String>> {
    let mut stmt = conn.prepare_cached(
        "SELECT tag_norm FROM post_tags GROUP BY tag_norm \
         ORDER BY count(*) DESC, tag_norm ASC LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![i64::try_from(limit).unwrap_or(i64::MAX)], |r| {
        r.get::<_, String>(0)
    })?;
    rows.collect::<rusqlite::Result<_>>().map_err(Into::into)
}

/// Reads the inputs of the post `post_id` (platform, media type, caption and
/// the frame objects of its slides). Returns `None` for an unknown post.
///
/// # Errors
///
/// Database errors.
pub fn select(conn: &Connection, post_id: i64) -> Result<Option<PostInputs>> {
    let post = conn
        .prepare_cached(
            "SELECT key, platform, media_type, caption, cover_object FROM posts WHERE id = ?1",
        )?
        .query_row(params![post_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })
        .optional()?;
    let Some((key, platform, media_type, caption, cover_object)) = post else {
        return Ok(None);
    };

    let mut frames = Vec::new();
    let mut seen: Vec<Vec<u8>> = Vec::new();
    let push_image =
        |frames: &mut Vec<Frame>, seen: &mut Vec<Vec<u8>>, obj: Option<FrameObject>| {
            if let Some(obj) = obj
                && !seen.contains(&obj.sha256)
            {
                seen.push(obj.sha256.clone());
                frames.push(Frame::Image(obj));
            }
        };

    // A separate title-card cover is useful evidence. Carousel covers that
    // equal slide one are deduplicated by the same digest below.
    if let Some(id) = cover_object {
        push_image(&mut frames, &mut seen, object_by_id(conn, id)?);
    }

    // The slides, in order: image slides are a still; video slides carry their
    // stored file (keyframes) and their poster still.
    let mut stmt = conn.prepare_cached(
        "SELECT m.kind, io.sha256, io.ext, io.mime, io.variants, \
                vo.sha256, vo.ext, vo.mime, vo.variants \
         FROM post_media m \
         LEFT JOIN media_objects io ON io.id = m.object_id \
         LEFT JOIN media_objects vo ON vo.id = m.video_object_id \
         WHERE m.post_id = ?1 ORDER BY m.position ASC LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        params![post_id, i64::try_from(MAX_SLIDES).unwrap_or(i64::MAX)],
        |r| {
            Ok((
                r.get::<_, String>(0)?,
                frame_object(r, 1)?,
                frame_object(r, 5)?,
            ))
        },
    )?;
    for row in rows {
        if frames.len() >= MAX_FRAMES {
            break;
        }
        let (kind, image, video) = row?;
        match kind.as_str() {
            "video" => {
                let poster = image.clone().filter(|o| !seen.contains(&o.sha256));
                if video.is_some() || poster.is_some() {
                    if let Some(o) = &poster {
                        seen.push(o.sha256.clone());
                    }
                    frames.push(Frame::Video { video, poster });
                }
            }
            "image" => push_image(&mut frames, &mut seen, image),
            _ => {}
        }
    }

    // A post whose slides carry no object but that has a cover (an X text
    // tweet's avatar): send the cover as the one still.
    if frames.is_empty()
        && let Some(cover_id) = cover_object
    {
        push_image(&mut frames, &mut seen, object_by_id(conn, cover_id)?);
    }

    Ok(Some(PostInputs {
        key,
        kind: CatalogKind::of(&platform, &media_type),
        media_type,
        caption: caption.filter(|c| !c.trim().is_empty()),
        frames,
    }))
}

/// Reads a [`FrameObject`] from columns `sha256, ext, mime, variants` starting
/// at `base`; `None` when the join found no object (a NULL `sha256`).
fn frame_object(row: &rusqlite::Row<'_>, base: usize) -> rusqlite::Result<Option<FrameObject>> {
    let Some(sha256) = row.get::<_, Option<Vec<u8>>>(base)? else {
        return Ok(None);
    };
    Ok(Some(FrameObject {
        sha256,
        ext: row.get(base + 1)?,
        mime: row.get(base + 2)?,
        variants: row.get(base + 3)?,
    }))
}

fn object_by_id(conn: &Connection, id: i64) -> Result<Option<FrameObject>> {
    let obj = conn
        .prepare_cached("SELECT sha256, ext, mime, variants FROM media_objects WHERE id = ?1")?
        .query_row(params![id], |r| {
            Ok(FrameObject {
                sha256: r.get(0)?,
                ext: r.get(1)?,
                mime: r.get(2)?,
                variants: r.get(3)?,
            })
        })
        .optional()?;
    Ok(obj)
}
