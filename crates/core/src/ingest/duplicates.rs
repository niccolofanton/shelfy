//! The duplicate policy of plan §4.2: two full rows of one post become one.
//!
//! A desktop library can hold one post under several ids (an Instagram media
//! id, its pk and its shortcode; a site under `http` and `https`), and a
//! migrated library can bring a post that the web library already holds
//! (`shelfy-migrate run --merge`, §4.1). The rows merge:
//!
//! 1. One row survives whole, platform, media and AI layers included: the row
//!    with archived files, then the one with an AI analysis, then the one with
//!    a user layer, then the one with more archived files; on a tie, the row
//!    listed first ([`survivor`]). The desktop never merged duplicates, so the
//!    policy has no golden file; the migration's dry run uses the same order.
//! 2. The user layers unite: notes are joined with [`NOTE_SEPARATOR`], each
//!    once ([`join_notes`]); manual tags are united, first form first
//!    ([`union_tags`]).
//! 3. Folders unite.
//! 4. The ingest rules of [`super::merge`] fill the gaps of the survivor: the
//!    other row's AI analysis when the survivor is unanalyzed, its date when
//!    the survivor has none. A missing date never replaces a known one, and
//!    the stored post keeps the earliest import time.
//!
//! [`merge_duplicate`] applies this to a stored post and a row read from
//! elsewhere. Merging the same row twice changes nothing the second time.

use std::cmp::Reverse;

use rusqlite::{Connection, OptionalExtension, params};

use crate::repo::posts::{self, AiLayer, CAPTION_MAX_CHARS, NewPost, UserContentPatch};
use crate::repo::{RepoError, Result, media};
use crate::search::terms::js_trim;

/// What joins the notes of merged duplicates.
pub const NOTE_SEPARATOR: &str = "\n\n";

/// What decides which duplicate survives (plan §4.2).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Layers {
    /// Archived files: the cover, slide objects, kept videos, site captures.
    pub archived_files: u64,
    /// An AI analysis: status `done`, a description or AI tags.
    pub ai: bool,
    /// A user layer: a note or manual tags.
    pub user: bool,
}

impl Layers {
    /// Sort key: the smallest survives.
    fn rank(self) -> impl Ord {
        (
            Reverse(self.archived_files > 0),
            Reverse(self.ai),
            Reverse(self.user),
            Reverse(self.archived_files),
        )
    }
}

/// The index of the duplicate that survives, `None` when there is none. On a
/// tie the first one wins, so callers list rows by their own tie-break: the
/// stored post first, the oldest import, the richest id form.
#[must_use]
pub fn survivor(rows: &[Layers]) -> Option<usize> {
    // `min_by_key` keeps the first of equal elements.
    (0..rows.len()).min_by_key(|&i| rows[i].rank())
}

/// Joins the notes of duplicates in order. Blank notes are skipped, and a
/// note already in the result is not added again, so merging the same rows
/// twice gives the same note. `None` when no note is left.
#[must_use]
pub fn join_notes<'a>(notes: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let mut joined: Option<String> = None;
    for note in notes {
        if js_trim(note).is_empty() {
            continue;
        }
        match &mut joined {
            None => joined = Some(note.to_owned()),
            Some(text) if !holds_note(text, note) => {
                text.push_str(NOTE_SEPARATOR);
                text.push_str(note);
            }
            Some(_) => {}
        }
    }
    joined
}

/// Whether `joined` already has `note` as one of its separated parts.
fn holds_note(joined: &str, note: &str) -> bool {
    joined == note
        || joined.starts_with(&format!("{note}{NOTE_SEPARATOR}"))
        || joined.ends_with(&format!("{NOTE_SEPARATOR}{note}"))
        || joined.contains(&format!("{NOTE_SEPARATOR}{note}{NOTE_SEPARATOR}"))
}

/// Unites manual tag lists in order: each tag once, trimmed, compared without
/// case, in its first form (the desktop's `unionStrings`).
#[must_use]
pub fn union_tags<'a>(lists: impl IntoIterator<Item = &'a [String]>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for list in lists {
        for tag in list {
            let form = js_trim(tag);
            if !form.is_empty() && seen.insert(form.to_lowercase()) {
                out.push(form.to_owned());
            }
        }
    }
    out
}

/// The layers of a post about to be written, given the site captures that
/// come with it.
#[must_use]
pub fn layers_of(post: &NewPost, captures: u64) -> Layers {
    let slide_files = post
        .media
        .iter()
        .map(|m| u64::from(m.object_id.is_some()) + u64::from(m.video_object_id.is_some()))
        .sum::<u64>();
    Layers {
        archived_files: u64::from(post.cover_object.is_some()) + slide_files + captures,
        ai: post.ai.as_ref().is_some_and(has_analysis),
        user: post
            .user_note
            .as_deref()
            .is_some_and(|n| !js_trim(n).is_empty())
            || post.user_tags.iter().any(|t| !js_trim(t).is_empty()),
    }
}

fn has_analysis(ai: &AiLayer) -> bool {
    ai.status.as_deref() == Some("done")
        || ai
            .description
            .as_deref()
            .is_some_and(|d| !js_trim(d).is_empty())
        || ai.tags.iter().any(|t| !js_trim(t).is_empty())
}

/// The other row of a post that the library holds, read from elsewhere (a
/// migration bundle). Its object and folder ids are already ids of this
/// library: the caller records its objects first.
#[derive(Clone, Debug, PartialEq)]
pub struct Duplicate {
    /// The row, with the stored post's key.
    pub post: NewPost,
    /// Its folders.
    pub collections: Vec<i64>,
    /// The site captures that come with it. They count as archived files; the
    /// caller copies them and, when [`DuplicateMerge::replaced`], points
    /// `current_capture_id` at the other row's current one.
    pub captures: u64,
}

/// What [`merge_duplicate`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DuplicateMerge {
    /// The other row survived: its platform, media and AI layers replaced the
    /// stored ones.
    pub replaced: bool,
    /// The stored post survived without an analysis and took the other's.
    pub ai_filled: bool,
    /// The stored post had no date and took the other's.
    pub date_filled: bool,
    /// A note was added to the stored one.
    pub notes_joined: bool,
    /// Manual tags added.
    pub tags_added: usize,
    /// Folders added.
    pub collections_added: usize,
}

impl DuplicateMerge {
    /// Whether the stored post changed.
    #[must_use]
    pub fn changed(&self) -> bool {
        self.replaced
            || self.ai_filled
            || self.date_filled
            || self.notes_joined
            || self.tags_added > 0
            || self.collections_added > 0
    }
}

/// The stored post's side of the comparison.
struct Stored {
    key: String,
    posted_at: Option<i64>,
    imported_at: i64,
    note: Option<String>,
    tags: Vec<String>,
    layers: Layers,
}

/// Merges `other` into the stored post `post_id` with the §4.2 policy (see
/// the module documentation). On a tie the stored post survives. Objects that
/// lose their last reference are stamped for the GC.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown post; [`RepoError::Invalid`] when
/// `other` has another key, an over-long caption or an unknown slide kind;
/// database errors otherwise (an unknown object or folder id, …).
pub fn merge_duplicate(
    conn: &Connection,
    post_id: i64,
    other: &Duplicate,
    now: i64,
) -> Result<DuplicateMerge> {
    let stored = read_stored(conn, post_id)?.ok_or(RepoError::NotFound)?;
    validate(&stored, &other.post)?;
    let incoming = layers_of(&other.post, other.captures);
    let replaced = survivor(&[stored.layers, incoming]) == Some(1);
    let mut done = DuplicateMerge {
        replaced,
        ..DuplicateMerge::default()
    };

    if replaced {
        replace_layers(conn, post_id, &stored, &other.post, now)?;
    } else {
        if !stored.layers.ai
            && let Some(ai) = other.post.ai.as_ref().filter(|ai| has_analysis(ai))
        {
            posts::set_ai(conn, post_id, ai, now)?;
            done.ai_filled = true;
        }
        if stored.posted_at.is_none()
            && let Some(posted_at) = other.post.posted_at
        {
            conn.prepare_cached(
                "UPDATE posts SET posted_at = ?2, sort_ts = ?2, updated_at = ?3 WHERE id = ?1",
            )?
            .execute(params![post_id, posted_at, now])?;
            done.date_filled = true;
        }
        let objects: Vec<i64> = row_objects(&other.post).collect();
        media::mark_unreferenced(conn, &objects, now)?;
    }

    // The user layers, survivor first.
    let (first, second) = if replaced {
        (
            (
                other.post.user_note.as_deref(),
                other.post.user_tags.as_slice(),
            ),
            (stored.note.as_deref(), stored.tags.as_slice()),
        )
    } else {
        (
            (stored.note.as_deref(), stored.tags.as_slice()),
            (
                other.post.user_note.as_deref(),
                other.post.user_tags.as_slice(),
            ),
        )
    };
    // Compared with the stored layer as these functions would write it, so a
    // blank note or a repeated tag alone is no reason to rewrite it.
    let mut patch = UserContentPatch::default();
    let note = join_notes([first.0, second.0].into_iter().flatten());
    if note != join_notes(stored.note.as_deref()) {
        done.notes_joined = true;
        patch.note = Some(note);
    }
    let tags = union_tags([first.1, second.1]);
    let stored_tags = union_tags([stored.tags.as_slice()]);
    if tags != stored_tags {
        done.tags_added = tags.len().saturating_sub(stored_tags.len());
        patch.tags = Some(tags);
    }
    if patch.note.is_some() || patch.tags.is_some() {
        posts::update_user_content(conn, post_id, &patch, now)?;
    }

    let mut add = conn.prepare_cached(
        "INSERT INTO post_collections (post_id, collection_id, added_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (post_id, collection_id) DO NOTHING",
    )?;
    for &collection in &other.collections {
        done.collections_added += add.execute(params![post_id, collection, now])?;
    }
    if done.collections_added > 0 {
        conn.prepare_cached("UPDATE posts SET updated_at = ?2 WHERE id = ?1")?
            .execute(params![post_id, now])?;
    }
    Ok(done)
}

fn read_stored(conn: &Connection, post_id: i64) -> Result<Option<Stored>> {
    let found = conn
        .prepare_cached(
            "SELECT p.key, p.posted_at, p.imported_at, p.user_note, p.user_tags_json,
                    p.ai_status, p.ai_description, p.ai_tags_json,
                    (p.cover_object IS NOT NULL)
                      + (SELECT count(m.object_id) + count(m.video_object_id)
                         FROM post_media m WHERE m.post_id = p.id)
                      + (SELECT count(*) FROM web_captures c WHERE c.post_id = p.id)
             FROM posts p WHERE p.id = ?1",
        )?
        .query_row([post_id], |r| {
            let tags = strings(r.get::<_, Option<String>>(4)?.as_deref());
            let ai = AiLayer {
                status: r.get(5)?,
                description: r.get(6)?,
                tags: strings(r.get::<_, Option<String>>(7)?.as_deref()),
                ..AiLayer::default()
            };
            let note: Option<String> = r.get(3)?;
            let files: i64 = r.get(8)?;
            Ok(Stored {
                key: r.get(0)?,
                posted_at: r.get(1)?,
                imported_at: r.get(2)?,
                layers: Layers {
                    archived_files: u64::try_from(files).unwrap_or(0),
                    ai: has_analysis(&ai),
                    user: note.as_deref().is_some_and(|n| !js_trim(n).is_empty())
                        || tags.iter().any(|t| !js_trim(t).is_empty()),
                },
                note,
                tags,
            })
        })
        .optional()?;
    Ok(found)
}

/// The string items of a JSON array column.
fn strings(raw: Option<&str>) -> Vec<String> {
    match raw.map(serde_json::from_str::<serde_json::Value>) {
        Some(Ok(serde_json::Value::Array(items))) => items
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn validate(stored: &Stored, post: &NewPost) -> Result<()> {
    let invalid = |field, reason| Err(RepoError::Invalid { field, reason });
    if post.key != stored.key {
        return invalid("key", "differs from the stored post");
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

/// The objects a row references directly.
fn row_objects(post: &NewPost) -> impl Iterator<Item = i64> + '_ {
    post.cover_object.into_iter().chain(
        post.media
            .iter()
            .flat_map(|m| m.object_id.into_iter().chain(m.video_object_id)),
    )
}

/// The other row survived: its platform, media and AI layers replace the
/// stored ones. The stored post keeps its id, key and user layer, the earliest
/// import time and, when the other row has none, its date.
fn replace_layers(
    conn: &Connection,
    post_id: i64,
    stored: &Stored,
    post: &NewPost,
    now: i64,
) -> Result<()> {
    let old_objects: Vec<i64> = conn
        .prepare_cached(
            "SELECT cover_object FROM posts WHERE id = ?1 AND cover_object IS NOT NULL
             UNION SELECT object_id FROM post_media WHERE post_id = ?1 AND object_id IS NOT NULL
             UNION SELECT video_object_id FROM post_media
                   WHERE post_id = ?1 AND video_object_id IS NOT NULL",
        )?
        .query_map([post_id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let posted_at = post.posted_at.or(stored.posted_at);
    let imported_at = post.imported_at.min(stored.imported_at);
    let media_count = i64::try_from(post.media.len().max(1)).unwrap_or(i64::MAX);
    conn.prepare_cached(
        "UPDATE posts SET shortcode = ?2, post_url = ?3, profile_url = ?4, author_username = ?5,
           author_name = ?6, caption = ?7, media_type = ?8, media_count = ?9, posted_at = ?10,
           imported_at = ?11, sort_ts = ?12, cover_object = ?13, cover_url = ?14,
           cover_url_expires_at = ?15, thumbhash = ?16, archive_state = COALESCE(?17, 'pending'),
           web_url = ?18, web_domain = ?19, web_final_url = ?20, updated_at = ?21
         WHERE id = ?1",
    )?
    .execute(params![
        post_id,
        post.shortcode,
        post.post_url,
        post.profile_url,
        post.author_username,
        post.author_name,
        post.caption,
        post.media_type,
        media_count,
        posted_at,
        imported_at,
        posted_at.unwrap_or(imported_at),
        post.cover_object,
        post.cover_url,
        post.cover_url_expires_at,
        post.thumbhash,
        post.archive_state,
        post.web_url,
        post.web_domain,
        post.web_final_url,
        now
    ])?;
    conn.prepare_cached("DELETE FROM post_media WHERE post_id = ?1")?
        .execute([post_id])?;
    let mut insert = conn.prepare_cached(
        "INSERT INTO post_media (post_id, position, kind, source_url, source_url_expires_at,
                                 video_url, video_url_expires_at, width, height, duration_ms,
                                 label, object_id, video_object_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    )?;
    for (position, m) in (0_i64..).zip(&post.media) {
        insert.execute(params![
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
    }
    // The other row's analysis, or the stored one when it has none.
    match post.ai.as_ref().filter(|ai| has_analysis(ai)) {
        Some(ai) => posts::set_ai(conn, post_id, ai, now)?,
        None => crate::search::index::reindex_post(conn, post_id)?,
    }
    media::mark_unreferenced(conn, &old_objects, now)?;
    Ok(())
}
