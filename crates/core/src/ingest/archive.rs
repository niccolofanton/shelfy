//! The archive state of a post (plan §2.12–§2.13; P2 contract C10): what is
//! left to store of its media, and who acts on it next.
//!
//! | `archive_state` | Meaning | Acts on it |
//! |---|---|---|
//! | `pending`, `partial` | The server can fetch something that is left; `partial` once the post stores something. | the archive drain (P2-10) |
//! | `client` | Nothing is left for the server, but the extension can act: upload a still valid URL (the platform's mode is `client`, or its breaker is open), refresh an expired Instagram URL, or hydrate an Instagram post without media that the server cannot (gated, or Instagram handed to the extension). | P2-14, P2-17 |
//! | `failed` | What is left is gone (404, 410) or out of tries. | a new sync (new URLs reset the tries); P4's Jobs view |
//! | `done` | Nothing is left: every wanted asset is stored, or has no URL to fetch. | — |
//! | `link_only` | Metadata only, by policy. | P4 |
//!
//! **What is wanted.** The user's `archiveAssetTypes`: `thumbnail` wants the
//! cover (a video's cover is its poster), `image` wants the image slides.
//! Videos are fetched on demand (D4), and their slides never count. An asset
//! is left while it is not stored and has a URL: nobody can fetch an asset
//! without one.
//!
//! **What an asset needs** ([`Asset::need`]): nothing when it is stored or
//! has no URL; [`Need::Failed`] when it is gone or out of tries
//! ([`FETCH_TRIES`]); [`Need::Refresh`] when its Instagram URL has expired
//! (the extension re-reads the post; the server never sends a request that
//! is bound to fail); else [`Need::Server`], or [`Need::Upload`] when the
//! platform's mode is `client`.
//!
//! **The post's state** ([`state`]), first rule that applies:
//!
//! 1. Web and manual posts: their media come from captures and uploads, never
//!    from the archive. `done` once something is stored, else `link_only`
//!    (a link until P4 captures it, a note without its file).
//! 2. `link_only` stays: it is a policy (stored media removed, over quota)
//!    that only its owner lifts (P4).
//! 3. A post without any media that is not text-only needs hydration
//!    ([`needs_hydration`]), which the link hydration does, not the archive
//!    (P2-11). The server goes first on every platform (L17): `pending`, or
//!    `client` on Instagram while it is handed to the extension (mode
//!    `client`, breaker open). The hydration's verdict stays until the post
//!    has media: `client` (the server found it gated: the extension's
//!    `hydrate_link`) and `failed` (gone).
//! 4. Something the server can fetch: `partial` when the archive stored one
//!    of the post's assets already (a kept video does not count), else
//!    `pending`. The server goes first, while the URLs are valid; what needs
//!    the extension waits for it.
//! 5. Something for the extension: `client`.
//! 6. Something failed: `failed`.
//! 7. `done`.
//!
//! **Fetch state of the cover.** The archive fetches the cover through
//! slide 0 (an image post's cover is its first slide, a video's poster is
//! the URL slide 0 carries, §2.13), so the cover's tries and error are
//! slide 0's (`post_media.fetch_*`). A cover without slides (a text post's
//! avatar) keeps them in `posts.cover_fetch_*` (library v3, P2-10).
//!
//! **Time.** A state depends on `now` through the URLs' expiries: a stored
//! `pending` turns stale once its Instagram URLs expire, and nothing writes
//! the post then. Whoever acts on a post derives its state again first: the
//! archive drain for what it picks (P2-10), ingest for what a sync brought.
//!
//! **The platform modes** (SPIKE-2, SPIKE-9 and L17: `server` on every
//! platform): `server` and `auto` are server-first (D14); `client` hands
//! every fetch to the extension. P2-10's breaker hands a platform over while
//! it is open with [`ArchiveModes::with`] and the `client` mode, then back.
//!
//! The migration's rule (T9, P1-19) was this one without asset types, modes
//! or fetch history. On what it saw, both agree, but for four cases:
//!
//! - an expired Instagram cover with image slides the server can still fetch
//!   is `pending` or `partial` until they are stored, then `client` (it was
//!   `client` at once);
//! - expired Instagram slides alone make a post `client` (they left it
//!   `partial` or `pending` for good);
//! - a web or manual post that stores nothing is `link_only` (it was `done`,
//!   or `pending` with a remote cover);
//! - a social post without media waits for its hydration, as rule 3 says
//!   (it was `done`).

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::repo::settings::{self, ArchiveAssetTypes};
use crate::repo::{Platform, Result};

/// Tries of one archive item before it fails (plan §2.12: 5 per item).
pub const FETCH_TRIES: i64 = 5;
/// `post_media.fetch_error` of an item that is gone (404, 410).
pub const FETCH_ERROR_GONE: &str = "gone";

/// `posts.archive_state`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveState {
    /// The server fetches; nothing stored yet.
    Pending,
    /// The server fetches the rest of what is partly stored.
    Partial,
    /// Nothing left to store.
    Done,
    /// What is left is gone or out of tries.
    Failed,
    /// The extension acts.
    Client,
    /// Metadata only, by policy.
    LinkOnly,
}

impl ArchiveState {
    /// Every state, in schema order.
    pub const ALL: [ArchiveState; 6] = [
        ArchiveState::Pending,
        ArchiveState::Partial,
        ArchiveState::Done,
        ArchiveState::Failed,
        ArchiveState::Client,
        ArchiveState::LinkOnly,
    ];

    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ArchiveState::Pending => "pending",
            ArchiveState::Partial => "partial",
            ArchiveState::Done => "done",
            ArchiveState::Failed => "failed",
            ArchiveState::Client => "client",
            ArchiveState::LinkOnly => "link_only",
        }
    }
}

impl fmt::Display for ArchiveState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error of parsing an [`ArchiveState`] or an [`ArchiveMode`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown {0}")]
pub struct UnknownValue(&'static str);

impl FromStr for ArchiveState {
    type Err = UnknownValue;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        ArchiveState::ALL
            .into_iter()
            .find(|state| state.as_str() == s)
            .ok_or(UnknownValue("archive state"))
    }
}

impl ToSql for ArchiveState {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for ArchiveState {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

/// Who fetches a platform's media (plan §2.13, D14; set per platform by
/// SPIKE-2 and `SHELFY_ARCHIVE_MODE_<PLATFORM>`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveMode {
    /// The server, with the breaker as a safety net.
    Server,
    /// The extension uploads every asset.
    Client,
    /// Server first, the breaker hands over to the extension (D14): for the
    /// state rule, as [`ArchiveMode::Server`].
    Auto,
}

impl ArchiveMode {
    /// The configured value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            ArchiveMode::Server => "server",
            ArchiveMode::Client => "client",
            ArchiveMode::Auto => "auto",
        }
    }

    /// Whether the server fetches the platform's media.
    #[must_use]
    pub const fn server_fetches(self) -> bool {
        !matches!(self, ArchiveMode::Client)
    }
}

impl FromStr for ArchiveMode {
    type Err = UnknownValue;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        [ArchiveMode::Server, ArchiveMode::Client, ArchiveMode::Auto]
            .into_iter()
            .find(|mode| mode.as_str() == s)
            .ok_or(UnknownValue("archive mode"))
    }
}

/// The archive mode of each platform the archive fetches for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveModes {
    /// Instagram.
    pub instagram: ArchiveMode,
    /// X.
    pub twitter: ArchiveMode,
    /// Pinterest.
    pub pinterest: ArchiveMode,
}

impl Default for ArchiveModes {
    /// `server` on every platform: SPIKE-2 for Instagram and X, SPIKE-9 for
    /// Pinterest (L17; SPIKE-2 had left it `auto`).
    fn default() -> Self {
        Self {
            instagram: ArchiveMode::Server,
            twitter: ArchiveMode::Server,
            pinterest: ArchiveMode::Server,
        }
    }
}

impl ArchiveModes {
    /// The mode of `platform`; `None` for web and manual posts, which the
    /// archive never fetches for.
    #[must_use]
    pub const fn get(&self, platform: Platform) -> Option<ArchiveMode> {
        match platform {
            Platform::Instagram => Some(self.instagram),
            Platform::Twitter => Some(self.twitter),
            Platform::Pinterest => Some(self.pinterest),
            Platform::Web | Platform::Manual => None,
        }
    }

    /// These modes with `platform` set to `mode` (no change for web and
    /// manual posts): how the breaker hands a platform to the extension.
    #[must_use]
    pub const fn with(mut self, platform: Platform, mode: ArchiveMode) -> Self {
        match platform {
            Platform::Instagram => self.instagram = mode,
            Platform::Twitter => self.twitter = mode,
            Platform::Pinterest => self.pinterest = mode,
            Platform::Web | Platform::Manual => {}
        }
        self
    }
}

/// What the state rule decides with: the platform modes (the server's) and
/// the asset types (the user's).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArchivePolicy {
    /// Who fetches each platform's media.
    pub modes: ArchiveModes,
    /// What the user keeps.
    pub assets: ArchiveAssetTypes,
}

impl ArchivePolicy {
    /// The policy of a library: `modes`, and the asset types of its settings.
    ///
    /// # Errors
    ///
    /// The settings cannot be read.
    pub fn read(conn: &Connection, modes: ArchiveModes) -> Result<Self> {
        Ok(Self {
            modes,
            assets: settings::read(conn)?.archive_asset_types,
        })
    }
}

/// A remote asset the archive may store: a post's cover, or a slide's image
/// or poster.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Asset {
    /// The bytes are stored (`cover_object`, `post_media.object_id`).
    pub stored: bool,
    /// It has a remote URL (`cover_url`, `post_media.source_url`).
    pub has_url: bool,
    /// When that URL expires, unix ms (the `oe` of Instagram and Facebook).
    pub expires_at: Option<i64>,
    /// Fetching it failed for good: gone, or out of tries.
    pub failed: bool,
}

/// What an asset still needs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Need {
    /// Nothing: stored, or no URL to fetch.
    Nothing,
    /// The server fetches it.
    Server,
    /// The extension uploads it: its URL is valid, but the server does not
    /// fetch the platform (`upload_media`).
    Upload,
    /// The extension refreshes its expired Instagram URL (`refresh_media`).
    Refresh,
    /// Gone, or out of tries.
    Failed,
}

impl Asset {
    /// What this asset of a `platform` post needs in `mode` at `now`.
    #[must_use]
    pub fn need(&self, platform: Platform, mode: ArchiveMode, now: i64) -> Need {
        if self.stored || !self.has_url {
            Need::Nothing
        } else if self.failed {
            Need::Failed
        } else if platform == Platform::Instagram && self.expires_at.is_some_and(|at| at <= now) {
            Need::Refresh
        } else if mode.server_fetches() {
            Need::Server
        } else {
            Need::Upload
        }
    }
}

/// A slide as the rule sees it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlideFacts {
    /// An image slide (`kind = 'image'`). Only those are archived: a video
    /// slide's poster is the post's cover, its video is fetched on demand,
    /// and files and pages come from uploads and captures.
    pub image: bool,
    /// Its image, or a video's poster.
    pub asset: Asset,
    /// Its video is kept (`video_object_id`).
    pub video_stored: bool,
}

/// What the rule reads of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PostFacts {
    /// The platform.
    pub platform: Platform,
    /// `posts.media_type`: a `text` post has no media to hydrate.
    pub media_type: String,
    /// The current state. The rule keeps `link_only`, and the hydration's
    /// verdict (`client`, `failed`) on a post without media; it derives the
    /// rest.
    pub state: ArchiveState,
    /// The cover (a video's poster).
    pub cover: Asset,
    /// The slides, in order.
    pub slides: Vec<SlideFacts>,
}

impl PostFacts {
    /// Whether the archive stored any of the post's assets: its cover, an
    /// image or a poster. Kept videos do not count; they are not the
    /// archive's (D4).
    #[must_use]
    pub fn archived_anything(&self) -> bool {
        self.cover.stored || self.slides.iter().any(|s| s.asset.stored)
    }

    /// Whether the post stores any media at all, kept videos included.
    #[must_use]
    pub fn stores_anything(&self) -> bool {
        self.archived_anything() || self.slides.iter().any(|s| s.video_stored)
    }
}

/// Whether a social post must be hydrated before anything can be archived:
/// it has no media at all (no cover, no slide with a URL or a stored object)
/// and is not text-only. The link hydration does it (P2-11): the server
/// first, then, for Instagram, the extension's `hydrate_link`.
#[must_use]
pub fn needs_hydration(post: &PostFacts) -> bool {
    let social = matches!(
        post.platform,
        Platform::Instagram | Platform::Twitter | Platform::Pinterest
    );
    let has_media = post.cover.stored
        || post.cover.has_url
        || post
            .slides
            .iter()
            .any(|s| s.asset.stored || s.asset.has_url || s.video_stored);
    social && !has_media && post.media_type != "text"
}

/// The archive state of a post under `policy` at `now` (the module
/// documentation has the rule).
#[must_use]
pub fn state(post: &PostFacts, policy: &ArchivePolicy, now: i64) -> ArchiveState {
    let Some(mode) = policy.modes.get(post.platform) else {
        return if post.stores_anything() {
            ArchiveState::Done
        } else {
            ArchiveState::LinkOnly
        };
    };
    if post.state == ArchiveState::LinkOnly {
        return ArchiveState::LinkOnly;
    }
    if needs_hydration(post) {
        return match post.state {
            // The hydration's verdict: gated (the extension's turn) or gone.
            ArchiveState::Client | ArchiveState::Failed => post.state,
            _ if post.platform == Platform::Instagram && !mode.server_fetches() => {
                ArchiveState::Client
            }
            _ => ArchiveState::Pending,
        };
    }
    let cover = policy.assets.thumbnail.then_some(&post.cover);
    let images = post
        .slides
        .iter()
        .filter(|s| policy.assets.image && s.image)
        .map(|s| &s.asset);
    let (mut server, mut client, mut failed) = (false, false, false);
    for asset in cover.into_iter().chain(images) {
        match asset.need(post.platform, mode, now) {
            Need::Nothing => {}
            Need::Server => server = true,
            Need::Upload | Need::Refresh => client = true,
            Need::Failed => failed = true,
        }
    }
    if server {
        if post.archived_anything() {
            ArchiveState::Partial
        } else {
            ArchiveState::Pending
        }
    } else if client {
        ArchiveState::Client
    } else if failed {
        ArchiveState::Failed
    } else {
        ArchiveState::Done
    }
}

// ── The rule on a library ────────────────────────────────────────────────────

/// Which posts to derive the state of.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope<'a> {
    /// Every post, trashed ones included.
    All,
    /// These posts (internal ids); unknown ids are skipped.
    Posts(&'a [i64]),
    /// The posts of one platform: a breaker opening or closing.
    Platform(Platform),
}

/// Posts by state, as [`refresh_states`] left them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StateCounts {
    /// `pending`.
    pub pending: usize,
    /// `partial`.
    pub partial: usize,
    /// `done`.
    pub done: usize,
    /// `failed`.
    pub failed: usize,
    /// `client`.
    pub client: usize,
    /// `link_only`.
    pub link_only: usize,
}

impl StateCounts {
    /// Counts one post in `state`.
    pub fn add(&mut self, state: ArchiveState) {
        let slot = match state {
            ArchiveState::Pending => &mut self.pending,
            ArchiveState::Partial => &mut self.partial,
            ArchiveState::Done => &mut self.done,
            ArchiveState::Failed => &mut self.failed,
            ArchiveState::Client => &mut self.client,
            ArchiveState::LinkOnly => &mut self.link_only,
        };
        *slot += 1;
    }

    /// The posts in `state`.
    #[must_use]
    pub const fn get(&self, state: ArchiveState) -> usize {
        match state {
            ArchiveState::Pending => self.pending,
            ArchiveState::Partial => self.partial,
            ArchiveState::Done => self.done,
            ArchiveState::Failed => self.failed,
            ArchiveState::Client => self.client,
            ArchiveState::LinkOnly => self.link_only,
        }
    }

    /// The posts the server has something to fetch for (`pending`,
    /// `partial`): the archive drain has work.
    #[must_use]
    pub const fn server_work(&self) -> usize {
        self.pending + self.partial
    }
}

/// What [`refresh_states`] did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Refreshed {
    /// Posts whose stored state changed.
    pub changed: usize,
    /// The posts of the scope, by their state now.
    pub counts: StateCounts,
}

/// The facts of the posts in `scope`, by internal id, in id order.
///
/// # Errors
///
/// A query failed.
pub fn load(conn: &Connection, scope: Scope<'_>) -> Result<Vec<(i64, PostFacts)>> {
    const POSTS: &str = "SELECT id, platform, media_type, archive_state, cover_object IS NOT NULL,
                                cover_url IS NOT NULL, cover_url_expires_at,
                                cover_fetch_attempts, cover_fetch_error
                         FROM posts";
    const SLIDES: &str = "SELECT post_id, position, kind, object_id IS NOT NULL,
                                 source_url IS NOT NULL, source_url_expires_at, fetch_attempts,
                                 fetch_error, video_object_id IS NOT NULL
                          FROM post_media";
    let (post_filter, slide_filter, arg) = match scope {
        Scope::All => ("", "", None),
        Scope::Posts(ids) => (
            "WHERE id IN (SELECT value FROM json_each(?1))",
            "WHERE post_id IN (SELECT value FROM json_each(?1))",
            Some(serde_json::to_string(ids).expect("ids serialize")),
        ),
        Scope::Platform(platform) => (
            "WHERE platform = ?1",
            "WHERE post_id IN (SELECT id FROM posts WHERE platform = ?1)",
            Some(platform.as_str().to_owned()),
        ),
    };
    let params: Vec<&dyn ToSql> = arg.iter().map(|a| a as &dyn ToSql).collect();

    let mut posts: Vec<(i64, PostFacts)> = Vec::new();
    let mut stmt = conn.prepare_cached(&format!("{POSTS} {post_filter} ORDER BY id"))?;
    let mut rows = stmt.query(params.as_slice())?;
    while let Some(row) = rows.next()? {
        // The cover's own fetch state, for a post without slides; slide 0's
        // replaces it below.
        let attempts: i64 = row.get(7)?;
        let error: Option<String> = row.get(8)?;
        posts.push((
            row.get(0)?,
            PostFacts {
                platform: row.get(1)?,
                media_type: row.get(2)?,
                state: row.get(3)?,
                cover: Asset {
                    stored: row.get(4)?,
                    has_url: row.get(5)?,
                    expires_at: row.get(6)?,
                    failed: fetch_failed(attempts, error.as_deref()),
                },
                slides: Vec::new(),
            },
        ));
    }

    let mut stmt = conn.prepare_cached(&format!(
        "{SLIDES} {slide_filter} ORDER BY post_id, position"
    ))?;
    let mut rows = stmt.query(params.as_slice())?;
    // Both lists are in id order: walk the posts along the slides.
    let mut at = 0;
    while let Some(row) = rows.next()? {
        let post_id: i64 = row.get(0)?;
        while posts.get(at).is_some_and(|(id, _)| *id < post_id) {
            at += 1;
        }
        let Some((_, post)) = posts.get_mut(at).filter(|(id, _)| *id == post_id) else {
            continue;
        };
        let position: i64 = row.get(1)?;
        let kind: String = row.get(2)?;
        let attempts: i64 = row.get(6)?;
        let error: Option<String> = row.get(7)?;
        let failed = fetch_failed(attempts, error.as_deref());
        let slide = SlideFacts {
            image: kind == "image",
            asset: Asset {
                stored: row.get(3)?,
                has_url: row.get(4)?,
                expires_at: row.get(5)?,
                failed,
            },
            video_stored: row.get(8)?,
        };
        if position == 0 {
            post.cover.failed = failed;
        }
        post.slides.push(slide);
    }
    Ok(posts)
}

/// Whether an item's fetch failed for good: gone, or out of tries.
#[must_use]
pub fn fetch_failed(attempts: i64, error: Option<&str>) -> bool {
    error == Some(FETCH_ERROR_GONE) || attempts >= FETCH_TRIES
}

/// Derives the state of the posts in `scope` under `policy` at `now` and
/// stores the ones that changed. `updated_at` does not move: the state is
/// derived data, like the search index.
///
/// Run it in the transaction of the write that changed the posts (ingest,
/// an archive fetch, an install), with the user's asset types
/// ([`ArchivePolicy::read`]) and the server's modes.
///
/// # Errors
///
/// A query failed.
pub fn refresh_states(
    conn: &Connection,
    scope: Scope<'_>,
    policy: &ArchivePolicy,
    now: i64,
) -> Result<Refreshed> {
    let mut refreshed = Refreshed::default();
    let mut update = conn.prepare_cached(
        "UPDATE posts SET archive_state = ?2 WHERE id = ?1 AND archive_state IS NOT ?2",
    )?;
    for (id, post) in load(conn, scope)? {
        let next = state(&post, policy, now);
        refreshed.counts.add(next);
        if next != post.state {
            refreshed.changed += update.execute(params![id, next])?;
        }
    }
    Ok(refreshed)
}

#[cfg(test)]
mod tests;
