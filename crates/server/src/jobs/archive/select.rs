//! Which items the archive drain fetches next (plan §2.12, §2.13).
//!
//! An *item* is one remote image the server can store for a post that is
//! `pending` or `partial` and not in the trash:
//!
//! - **the cover** (`thumbnail` among the user's `archiveAssetTypes`), when
//!   the post has a `cover_url` and no `cover_object`. It is fetched through
//!   slide 0 (§2.13: an image post's cover is its first slide, a video's
//!   poster the URL slide 0 carries) while slide 0's URL is valid, else from
//!   `cover_url`. Its tries are slide 0's, or `posts.cover_fetch_*` without
//!   slides (the core rule, `shelfy_core::ingest::archive`). When slide 0
//!   already stores an object, the cover only needs linking to it;
//! - **an image slide** (`image` among the asset types) without an object.
//!   Slide 0 is the cover's item while the cover is wanted and missing.
//!
//! An item is *failed* when gone or out of tries (the rule's
//! [`fetch_failed`]), *due* when its `fetch_next_at` has passed. The due
//! items come covers first, then slides, each by soonest expiry (the
//! Instagram URLs that expire first), then by post.

use std::collections::HashSet;

use rusqlite::Connection;
use shelfy_core::ingest::archive::fetch_failed;
use shelfy_core::legacy::convert::cdn_url_expiry_ms;
use shelfy_core::repo::settings::ArchiveAssetTypes;
use shelfy_core::repo::{Platform, RepoError};

/// What an item fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Slot {
    /// The post's cover.
    Cover,
    /// The image slide at this position.
    Slide(i64),
}

/// Where an item's fetch state (`fetch_attempts`, `fetch_next_at`,
/// `fetch_error`) lives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FetchRow {
    /// `post_media` at this position.
    Slide(i64),
    /// `posts.cover_fetch_*`: a cover without slides.
    Post,
}

/// Which URL column an item's URL comes from, for its expiry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UrlColumn {
    /// `post_media.source_url` at this position.
    Slide(i64),
    /// `posts.cover_url`.
    Cover,
}

/// One item of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The post's internal id.
    pub post_id: i64,
    /// Its key, for the events.
    pub key: String,
    /// Its platform.
    pub platform: Platform,
    /// When it was inserted (unix ms): the cover latency's start.
    pub imported_at: i64,
    /// What the bytes fill.
    pub slot: Slot,
    /// The URL to fetch.
    pub url: String,
    /// Where it comes from.
    pub url_column: UrlColumn,
    /// When it expires (the column, else an Instagram URL's `oe`).
    pub expires_at: Option<i64>,
    /// Where the tries are kept.
    pub fetch_row: FetchRow,
    /// Failed tries so far.
    pub attempts: i64,
    /// Not before this time.
    pub next_at: Option<i64>,
    /// A video's poster rather than an image (§2.13: WebP q78, ≤ 1,080 px).
    pub poster: bool,
    /// Also the post's cover: a cover item, or slide 0 of a post without a
    /// cover. Gets a `g480` and the post's ThumbHash.
    pub cover: bool,
    /// Gets a `g480`: a cover, or slides 1–3 of a multi-image post.
    pub grid: bool,
    /// Slide 0 already stores this object: link the cover to it, nothing
    /// to fetch.
    pub existing: Option<i64>,
}

impl Item {
    /// Whether its URL has expired at `now`. Only Instagram URLs expire
    /// (the rule's `Need::Refresh`).
    #[must_use]
    pub fn expired(&self, now: i64) -> bool {
        self.platform == Platform::Instagram && self.expires_at.is_some_and(|at| at <= now)
    }

    /// The item's identity within a library.
    #[must_use]
    pub fn id(&self) -> (i64, Slot) {
        (self.post_id, self.slot)
    }
}

/// Items per platform, the order of [`platform_index`].
pub type PlatformCounts = [u64; 3];

/// The position of a social platform in [`PlatformCounts`].
#[must_use]
pub fn platform_index(platform: Platform) -> Option<usize> {
    match platform {
        Platform::Instagram => Some(0),
        Platform::Twitter => Some(1),
        Platform::Pinterest => Some(2),
        Platform::Web | Platform::Manual => None,
    }
}

/// What [`select`] found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    /// Due items, in fetch order, at most the limit.
    pub due: Vec<Item>,
    /// The earliest `fetch_next_at` of the items not due yet.
    pub next_at: Option<i64>,
    /// Items left to the server, due or waiting, by platform (failed ones
    /// excluded).
    pub pending: PlatformCounts,
    /// Posts with an item whose expiry the library records: their state is
    /// stale when they are still `pending` or `partial` (an Instagram URL
    /// expired since it was derived), so the drain derives it again.
    pub stale: Vec<i64>,
}

impl Selection {
    /// Items left to the server.
    #[must_use]
    pub fn pending_total(&self) -> u64 {
        self.pending.iter().sum()
    }

    /// When the drain has work: now when something is due or a state is
    /// stale, else the next item's time; `None` when nothing is left.
    #[must_use]
    pub fn wake_at(&self, now: i64) -> Option<i64> {
        if self.due.is_empty() && self.stale.is_empty() {
            self.next_at
        } else {
            Some(now)
        }
    }
}

const COVERS: &str = "
    SELECT p.id, p.key, p.platform, p.media_type, p.imported_at,
           p.cover_url, p.cover_url_expires_at,
           p.cover_fetch_attempts, p.cover_fetch_next_at, p.cover_fetch_error,
           m.kind, m.source_url, m.source_url_expires_at, m.object_id,
           m.fetch_attempts, m.fetch_next_at, m.fetch_error
    FROM posts p LEFT JOIN post_media m ON m.post_id = p.id AND m.position = 0
    WHERE p.archive_state IN ('pending', 'partial') AND p.deleted_at IS NULL
      AND p.platform IN ('instagram', 'twitter', 'pinterest')
      AND p.cover_object IS NULL AND p.cover_url IS NOT NULL";

const SLIDES: &str = "
    SELECT m.post_id, p.key, p.platform, p.imported_at, p.cover_object IS NULL,
           p.cover_url IS NOT NULL, m.position, m.source_url, m.source_url_expires_at,
           m.fetch_attempts, m.fetch_next_at, m.fetch_error,
           (SELECT count(*) FROM post_media s WHERE s.post_id = m.post_id AND s.kind = 'image')
    FROM post_media m JOIN posts p ON p.id = m.post_id
    WHERE p.archive_state IN ('pending', 'partial') AND p.deleted_at IS NULL
      AND p.platform IN ('instagram', 'twitter', 'pinterest')
      AND m.kind = 'image' AND m.object_id IS NULL AND m.source_url IS NOT NULL";

/// The expiry of `url`: its column, else an Instagram URL's `oe`.
fn expiry(platform: Platform, url: &str, column: Option<i64>) -> Option<i64> {
    column.or_else(|| {
        (platform == Platform::Instagram)
            .then(|| cdn_url_expiry_ms(url))
            .flatten()
    })
}

/// Whether the library records that a URL expired: the rule's `Refresh`.
fn recorded_expired(platform: Platform, column: Option<i64>, now: i64) -> bool {
    platform == Platform::Instagram && column.is_some_and(|at| at <= now)
}

/// The items of the library at `now` for the asset types `assets`: the
/// first `limit` due ones not in `skip`, the time of the next one, and the
/// count of what is left.
///
/// # Errors
///
/// A query failed.
pub fn select(
    conn: &Connection,
    assets: ArchiveAssetTypes,
    now: i64,
    limit: usize,
    skip: &HashSet<(i64, Slot)>,
) -> Result<Selection, RepoError> {
    let mut covers = Vec::new();
    let mut slides = Vec::new();
    let mut stale = Vec::new();
    // Posts whose slide 0 is the cover's item.
    let mut cover_slide0 = HashSet::new();
    if assets.thumbnail {
        let mut statement = conn.prepare_cached(COVERS)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let post_id: i64 = row.get(0)?;
            let platform: Platform = row.get(2)?;
            let media_type: String = row.get(3)?;
            let cover_url: String = row.get(5)?;
            let cover_expiry = expiry(platform, &cover_url, row.get(6)?);
            let slide_kind: Option<String> = row.get(10)?;
            let slide_url: Option<String> = row.get(11)?;
            let slide_expires: Option<i64> = row.get(12)?;
            let slide_object: Option<i64> = row.get(13)?;
            let has_slide0 = slide_kind.is_some();
            let poster = match slide_kind.as_deref() {
                Some(kind) => kind == "video",
                None => media_type == "video",
            };
            let (attempts, next_at, error): (i64, Option<i64>, Option<String>) = if has_slide0 {
                (row.get(14)?, row.get(15)?, row.get(16)?)
            } else {
                (row.get(7)?, row.get(8)?, row.get(9)?)
            };
            // Slide 0's URL while it is valid (§2.13), else the cover's.
            let slide_choice = slide_url
                .filter(|_| matches!(slide_kind.as_deref(), Some("image" | "video")))
                .map(|url| {
                    let at = expiry(platform, &url, slide_expires);
                    (url, at)
                })
                .filter(|(_, at)| platform != Platform::Instagram || at.is_none_or(|at| at > now));
            let (url, url_column, expires_at) = match slide_choice {
                Some((url, at)) => (url, UrlColumn::Slide(0), at),
                None => (cover_url, UrlColumn::Cover, cover_expiry),
            };
            if has_slide0 && slide_kind.as_deref() == Some("image") {
                cover_slide0.insert(post_id);
            }
            // An expiry the library records is the extension's (refresh),
            // as the rule says; one only the URL's `oe` tells is recorded
            // by the drain first, without a request. A cover that only
            // needs linking to slide 0's object fetches nothing.
            if slide_object.is_none()
                && url_column == UrlColumn::Cover
                && recorded_expired(platform, row.get(6)?, now)
            {
                stale.push(post_id);
                continue;
            }
            covers.push((
                Item {
                    post_id,
                    key: row.get(1)?,
                    platform,
                    imported_at: row.get(4)?,
                    slot: Slot::Cover,
                    url,
                    url_column,
                    expires_at,
                    fetch_row: if has_slide0 {
                        FetchRow::Slide(0)
                    } else {
                        FetchRow::Post
                    },
                    attempts,
                    next_at,
                    poster,
                    cover: true,
                    grid: true,
                    existing: slide_object,
                },
                error,
            ));
        }
    }
    if assets.image {
        let mut statement = conn.prepare_cached(SLIDES)?;
        let mut rows = statement.query([])?;
        while let Some(row) = rows.next()? {
            let post_id: i64 = row.get(0)?;
            let position: i64 = row.get(6)?;
            if position == 0 && cover_slide0.contains(&post_id) {
                continue;
            }
            let platform: Platform = row.get(2)?;
            let no_cover: bool = row.get(4)?;
            let images: i64 = row.get(12)?;
            let url: String = row.get(7)?;
            let column: Option<i64> = row.get(8)?;
            if recorded_expired(platform, column, now) {
                stale.push(post_id);
                continue;
            }
            let expires_at = expiry(platform, &url, column);
            // Slide 0 of a post without a stored cover becomes its cover too
            // (§2.13: they share the object).
            let cover = position == 0 && no_cover;
            slides.push((
                Item {
                    post_id,
                    key: row.get(1)?,
                    platform,
                    imported_at: row.get(3)?,
                    slot: Slot::Slide(position),
                    url,
                    url_column: UrlColumn::Slide(position),
                    expires_at,
                    fetch_row: FetchRow::Slide(position),
                    attempts: row.get(9)?,
                    next_at: row.get(10)?,
                    poster: false,
                    cover,
                    grid: cover || (images > 1 && (1..=3).contains(&position)),
                    existing: None,
                },
                row.get::<_, Option<String>>(11)?,
            ));
        }
    }

    stale.sort_unstable();
    stale.dedup();
    let mut selection = Selection {
        stale,
        ..Selection::default()
    };
    let mut due_covers = Vec::new();
    let mut due_slides = Vec::new();
    for (is_cover, (item, error)) in covers
        .into_iter()
        .map(|c| (true, c))
        .chain(slides.into_iter().map(|s| (false, s)))
    {
        // A cover that only needs linking is never failed: nothing is
        // fetched.
        if item.existing.is_none() && fetch_failed(item.attempts, error.as_deref()) {
            continue;
        }
        if let Some(index) = platform_index(item.platform) {
            selection.pending[index] += 1;
        }
        let due = item.existing.is_some() || item.next_at.is_none_or(|at| at <= now);
        if !due {
            let at = item.next_at.unwrap_or(now);
            selection.next_at = Some(selection.next_at.map_or(at, |next| next.min(at)));
            continue;
        }
        if skip.contains(&item.id()) {
            continue;
        }
        if is_cover {
            due_covers.push(item);
        } else {
            due_slides.push(item);
        }
    }
    let order = |a: &Item, b: &Item| {
        (
            a.expires_at.is_none(),
            a.expires_at,
            a.post_id,
            a.slot_order(),
        )
            .cmp(&(
                b.expires_at.is_none(),
                b.expires_at,
                b.post_id,
                b.slot_order(),
            ))
    };
    due_covers.sort_by(order);
    due_slides.sort_by(order);
    selection.due = due_covers
        .into_iter()
        .chain(due_slides)
        .take(limit)
        .collect();
    Ok(selection)
}

impl Item {
    fn slot_order(&self) -> i64 {
        match self.slot {
            Slot::Cover => -1,
            Slot::Slide(position) => position,
        }
    }
}
