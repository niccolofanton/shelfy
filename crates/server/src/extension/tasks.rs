//! The extension's tasks (plan §2.13, §2.16; P2 contracts C6, C10; P2-14):
//! what the browser extension does for the archive when the server cannot.
//!
//! **Tasks are derived, not stored.** A task is what an `archive_state =
//! 'client'` post still needs from the extension, read from the library at
//! each poll with the core rule's notions ([`Asset::need`],
//! [`needs_hydration`]) and the archive's modes in effect
//! ([`crate::jobs::archive::modes`]):
//!
//! | Kind | One per | When | `url`, `position` |
//! |---|---|---|---|
//! | `upload_media` | wanted asset (the cover, an image slide) | its URL is valid and the server does not fetch the platform (mode `client`, or its CDN breaker is open) | the URL to fetch; the slide's position, `null` for the cover |
//! | `refresh_media` | Instagram post | a wanted asset's URL expired (its column, or the URL's `oe`) | `null`, `null`: re-read the post, send a `refresh` ingest batch |
//! | `hydrate_link` | Instagram post | it has no media at all (a shared link the server found gated, or Instagram handed to the extension) | `null`, `null`: read the post, send a `refresh` batch |
//!
//! The asset types are the user's (`archiveAssetTypes`). The cover is
//! fetched through slide 0 while slide 0's URL is valid, as the archive
//! drain does, and an image post's slide 0 is then not a task of its own.
//!
//! **Tries** live where the drain keeps them (`post_media.fetch_*`, or
//! `posts.cover_fetch_*` for a cover without slides and for a hydration): a
//! task whose item is failed (gone, or [`FETCH_TRIES`] used) or backed off
//! (`fetch_next_at` ahead) is not offered. A `refresh_media` task carries
//! the tries of every expired item of its post that is due.
//!
//! **Ids** name the kind, the post and the slot: `upload_media.ig_123.cover`,
//! `upload_media.ig_123.2`, `refresh_media.ig_123.post`,
//! `hydrate_link.ig_123.post` ([`TaskId`]). They are stable while the work
//! is the same, so a completion finds its task again.
//!
//! **Leases** ([`TaskBoard`], in memory): a poll leases the tasks it
//! returns to its poller (the token) for [`LEASE`]; another poller does not
//! get them until the lease ends or the task completes. A poll by the same
//! poller returns its own leased tasks again, with the lease renewed, so an
//! extension that restarted picks its work back up (it dedupes by id). A
//! restart of the server forgets the leases; the work is idempotent (the
//! CAS dedupes uploads, a refresh merges).
//!
//! **Long poll.** `GET /ingest/tasks?wait=25` answers at once when it has
//! tasks for the poller, else waits up to `wait` seconds for a wake-up
//! ([`wake`]: posts went to `client` through ingest, the archive drain, the
//! breaker handoff or a gated hydration), and ends at shutdown.
//!
//! **Completion** ([`complete`]) is idempotent: an outcome for a task that
//! no longer exists (done, failed, backed off) changes nothing.
//!
//! | Outcome | Kinds | What happens |
//! |---|---|---|
//! | `uploaded` | `upload_media` | the complete `archive-object` upload `uploadId` of the user is claimed, its bytes checked against its SHA-256 and stored and linked like a server fetch (renditions, ThumbHash, the state; origin `extension`), after a quota reservation; a slot filled meanwhile takes nothing |
//! | `refreshed` | `refresh_media`, `hydrate_link` | nothing: the `refresh` batch carried the new data. A task still there (nothing new arrived) counts one try |
//! | `gone` | every kind | the items fail for good (`gone`); a hydration's post becomes `failed` |
//! | `failed`, `skipped` | every kind | one try used, next after the archive's backoff (30 s × 2ⁿ, at most 6 h); the last one fails the items (a hydration's post becomes `failed`) |

use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::str::FromStr;
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension as _, ToSql, params};
use serde::{Deserialize, Serialize};
use shelfy_core::ingest::archive::{
    ArchiveModes, ArchivePolicy, ArchiveState, Asset, FETCH_ERROR_GONE, FETCH_TRIES, PostFacts,
    Scope, SlideFacts, fetch_failed, needs_hydration, refresh_states,
};
use shelfy_core::legacy::convert::cdn_url_expiry_ms;
use shelfy_core::repo::settings::{self, ArchiveAssetTypes};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::pool::ImagePool;
use shelfy_media::refs::Origin;
use shelfy_media::store::{IngestError, IngestLimits, MediaStore};
use tokio::sync::watch;
use tokio::time::Instant;
use utoipa::ToSchema;

use crate::control::uploads::UploadPurpose;
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::jobs::archive::select::{FetchRow, Slot};
use crate::jobs::archive::store::{self, Committed, StoreContext};
use crate::jobs::archive::{self, ITEM_BACKOFF, RESERVE_BYTES};
use crate::library::{self, Change};
use crate::quota;
use crate::routes::uploads::{self, ClaimError, Claimed};
use crate::state::{AppState, blocking};

/// How long a poll leases a task to its poller (P2-14: 5 minutes).
pub const LEASE: Duration = Duration::from_secs(5 * 60);
/// The longest wait of a poll, in seconds (C6: `wait=25`).
pub const MAX_WAIT_SECS: u64 = 25;
/// Tasks per poll when the poller names no limit (C6: `limit=20`).
pub const DEFAULT_LIMIT: usize = 20;
/// The most tasks one poll leases.
pub const MAX_LIMIT: usize = 50;

// ── Tasks ────────────────────────────────────────────────────────────────────

/// What a task asks of the extension (C6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    /// Fetch `url` from the CDN and upload it (`archive-object`).
    UploadMedia,
    /// Re-read an Instagram post whose media URLs expired.
    RefreshMedia,
    /// Read an Instagram post that has no media yet.
    HydrateLink,
}

impl TaskKind {
    /// Its wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UploadMedia => "upload_media",
            Self::RefreshMedia => "refresh_media",
            Self::HydrateLink => "hydrate_link",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        [Self::UploadMedia, Self::RefreshMedia, Self::HydrateLink]
            .into_iter()
            .find(|kind| kind.as_str() == text)
    }
}

/// What a task fills.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum TaskSlot {
    /// The post's cover (`upload_media`).
    Cover,
    /// The image slide at this position (`upload_media`).
    Slide(i64),
    /// The whole post (`refresh_media`, `hydrate_link`).
    Post,
}

/// A task's id: `<kind>.<post key>.<slot>`, the slot `cover`, `post` or a
/// slide's position.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TaskId {
    /// What it asks.
    pub kind: TaskKind,
    /// The post's key.
    pub key: String,
    /// What it fills.
    pub slot: TaskSlot,
}

impl fmt::Display for TaskId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.", self.kind.as_str(), self.key)?;
        match self.slot {
            TaskSlot::Cover => f.write_str("cover"),
            TaskSlot::Slide(position) => write!(f, "{position}"),
            TaskSlot::Post => f.write_str("post"),
        }
    }
}

/// A text that is not a task id.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BadTaskId;

impl FromStr for TaskId {
    type Err = BadTaskId;

    fn from_str(text: &str) -> Result<Self, BadTaskId> {
        if text.len() > 128 {
            return Err(BadTaskId);
        }
        let (kind, rest) = text.split_once('.').ok_or(BadTaskId)?;
        let (key, slot) = rest.rsplit_once('.').ok_or(BadTaskId)?;
        let kind = TaskKind::parse(kind).ok_or(BadTaskId)?;
        let key_ok = !key.is_empty()
            && key
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
        if !key_ok {
            return Err(BadTaskId);
        }
        let slot = match slot {
            "cover" => TaskSlot::Cover,
            "post" => TaskSlot::Post,
            digits if !digits.is_empty() && digits.len() <= 4 => TaskSlot::Slide(
                digits
                    .bytes()
                    .all(|b| b.is_ascii_digit())
                    .then(|| digits.parse().ok())
                    .flatten()
                    .ok_or(BadTaskId)?,
            ),
            _ => return Err(BadTaskId),
        };
        let fits = match kind {
            TaskKind::UploadMedia => slot != TaskSlot::Post,
            TaskKind::RefreshMedia | TaskKind::HydrateLink => slot == TaskSlot::Post,
        };
        if !fits {
            return Err(BadTaskId);
        }
        Ok(Self {
            kind,
            key: key.to_owned(),
            slot,
        })
    }
}

/// One task, as derived from the library.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Task {
    /// Its id.
    pub id: TaskId,
    /// The post's internal id.
    pub post_id: i64,
    /// The post's platform.
    pub platform: Platform,
    /// The post's native id (Instagram's media pk).
    pub native_id: String,
    /// Instagram's code.
    pub shortcode: Option<String>,
    /// The link to the post.
    pub post_url: String,
    /// The URL to fetch (`upload_media`).
    pub url: Option<String>,
    /// When `url` expires; for `refresh_media`, the first expiry it is
    /// about.
    pub expires_at: Option<i64>,
    /// Where its tries are kept, and how many were used.
    rows: Vec<(FetchRow, i64)>,
}

impl Task {
    /// The slide's position (`upload_media` of a slide).
    #[must_use]
    pub fn position(&self) -> Option<i64> {
        match self.id.slot {
            TaskSlot::Slide(position) => Some(position),
            TaskSlot::Cover | TaskSlot::Post => None,
        }
    }
}

/// The tasks of a library, and how many there are per platform.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Derived {
    /// Every task that is due, in the order they are handed out: uploads
    /// by soonest expiry, then refreshes, then hydrations.
    pub tasks: Vec<Task>,
}

/// Tasks per social platform.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, ToSchema)]
pub struct Waiting {
    /// Instagram.
    pub instagram: u64,
    /// X.
    pub twitter: u64,
    /// Pinterest.
    pub pinterest: u64,
}

impl Derived {
    /// How many tasks wait per platform, leased ones included.
    #[must_use]
    pub fn waiting(&self) -> Waiting {
        let mut waiting = Waiting::default();
        for task in &self.tasks {
            match task.platform {
                Platform::Instagram => waiting.instagram += 1,
                Platform::Twitter => waiting.twitter += 1,
                Platform::Pinterest => waiting.pinterest += 1,
                Platform::Web | Platform::Manual => {}
            }
        }
        waiting
    }
}

/// An item's tries, as its row keeps them.
#[derive(Clone, Debug, Default)]
struct Tries {
    attempts: i64,
    next_at: Option<i64>,
    error: Option<String>,
}

impl Tries {
    fn failed(&self) -> bool {
        fetch_failed(self.attempts, self.error.as_deref())
    }

    fn due(&self, now: i64) -> bool {
        self.next_at.is_none_or(|at| at <= now)
    }
}

struct SlideRow {
    position: i64,
    kind: String,
    url: Option<String>,
    expires_column: Option<i64>,
    stored: bool,
    video_stored: bool,
    tries: Tries,
}

struct PostRow {
    id: i64,
    key: String,
    platform: Platform,
    media_type: String,
    native_id: String,
    shortcode: Option<String>,
    post_url: Option<String>,
    cover_url: Option<String>,
    cover_expires_column: Option<i64>,
    cover_stored: bool,
    cover_tries: Tries,
    slides: Vec<SlideRow>,
}

const POSTS: &str = "SELECT id, key, platform, media_type, native_id, shortcode, post_url,
           cover_url, cover_url_expires_at, cover_object IS NOT NULL,
           cover_fetch_attempts, cover_fetch_next_at, cover_fetch_error
    FROM posts
    WHERE archive_state = 'client' AND deleted_at IS NULL
      AND platform IN ('instagram', 'twitter', 'pinterest')";

const SLIDES: &str = "SELECT m.post_id, m.position, m.kind, m.source_url, m.source_url_expires_at,
           m.object_id IS NOT NULL, m.video_object_id IS NOT NULL,
           m.fetch_attempts, m.fetch_next_at, m.fetch_error
    FROM post_media m JOIN posts p ON p.id = m.post_id
    WHERE p.archive_state = 'client' AND p.deleted_at IS NULL
      AND p.platform IN ('instagram', 'twitter', 'pinterest')";

fn load(conn: &Connection, key: Option<&str>) -> Result<Vec<PostRow>, RepoError> {
    let args: Vec<&dyn ToSql> = key.iter().map(|k| k as &dyn ToSql).collect();
    let (post_filter, slide_filter) = if key.is_some() {
        (" AND key = ?1", " AND p.key = ?1")
    } else {
        ("", "")
    };
    let mut posts: Vec<PostRow> = Vec::new();
    let mut statement = conn.prepare_cached(&format!("{POSTS}{post_filter} ORDER BY id"))?;
    let mut rows = statement.query(args.as_slice())?;
    while let Some(row) = rows.next()? {
        posts.push(PostRow {
            id: row.get(0)?,
            key: row.get(1)?,
            platform: row.get(2)?,
            media_type: row.get(3)?,
            native_id: row.get(4)?,
            shortcode: row.get(5)?,
            post_url: row.get(6)?,
            cover_url: row.get(7)?,
            cover_expires_column: row.get(8)?,
            cover_stored: row.get(9)?,
            cover_tries: Tries {
                attempts: row.get(10)?,
                next_at: row.get(11)?,
                error: row.get(12)?,
            },
            slides: Vec::new(),
        });
    }
    let mut statement = conn.prepare_cached(&format!(
        "{SLIDES}{slide_filter} ORDER BY m.post_id, m.position"
    ))?;
    let mut rows = statement.query(args.as_slice())?;
    // Both lists are in id order: walk the posts along the slides.
    let mut at = 0;
    while let Some(row) = rows.next()? {
        let post_id: i64 = row.get(0)?;
        while posts.get(at).is_some_and(|post| post.id < post_id) {
            at += 1;
        }
        let Some(post) = posts.get_mut(at).filter(|post| post.id == post_id) else {
            continue;
        };
        post.slides.push(SlideRow {
            position: row.get(1)?,
            kind: row.get(2)?,
            url: row.get(3)?,
            expires_column: row.get(4)?,
            stored: row.get(5)?,
            video_stored: row.get(6)?,
            tries: Tries {
                attempts: row.get(7)?,
                next_at: row.get(8)?,
                error: row.get(9)?,
            },
        });
    }
    Ok(posts)
}

/// The tasks of the library behind `conn` (of the post `key` only, when
/// given) at `now`, for the asset types `assets` and the archive modes in
/// effect `modes` (see the module docs).
///
/// # Errors
///
/// A query failed.
pub fn derive(
    conn: &Connection,
    key: Option<&str>,
    assets: ArchiveAssetTypes,
    modes: ArchiveModes,
    now: i64,
) -> Result<Derived, RepoError> {
    let mut uploads = Vec::new();
    let mut refreshes = Vec::new();
    let mut hydrations = Vec::new();
    for post in load(conn, key)? {
        let Some(mode) = modes.get(post.platform) else {
            continue;
        };
        let instagram = post.platform == Platform::Instagram;
        let task = |kind: TaskKind, slot: TaskSlot| Task {
            id: TaskId {
                kind,
                key: post.key.clone(),
                slot,
            },
            post_id: post.id,
            platform: post.platform,
            native_id: post.native_id.clone(),
            shortcode: post.shortcode.clone(),
            post_url: post.post_url.clone().unwrap_or_else(|| {
                crate::jobs::hydrate::canonical_url(post.platform, &post.native_id)
            }),
            url: None,
            expires_at: None,
            rows: Vec::new(),
        };

        if needs_hydration(&facts(&post)) {
            if instagram && !post.cover_tries.failed() && post.cover_tries.due(now) {
                let mut hydrate = task(TaskKind::HydrateLink, TaskSlot::Post);
                hydrate
                    .rows
                    .push((FetchRow::Post, post.cover_tries.attempts));
                hydrations.push(hydrate);
            }
            continue;
        }

        // Only Instagram URLs expire: the column, else the URL's `oe`.
        let expiry = |url: &str, column: Option<i64>| {
            column.or_else(|| instagram.then(|| cdn_url_expiry_ms(url)).flatten())
        };
        let expired = |at: Option<i64>| instagram && at.is_some_and(|at| at <= now);
        let uploads_here = !mode.server_fetches();
        let mut refresh = task(TaskKind::RefreshMedia, TaskSlot::Post);
        let mut upload = |slot: TaskSlot, url: String, at: Option<i64>, row, tries: &Tries| {
            let mut item = task(TaskKind::UploadMedia, slot);
            item.url = Some(url);
            item.expires_at = at;
            item.rows.push((row, tries.attempts));
            uploads.push(item);
        };
        let needs_refresh = |refresh: &mut Task, at: Option<i64>, row, tries: &Tries| {
            refresh.rows.push((row, tries.attempts));
            refresh.expires_at = Some(
                refresh
                    .expires_at
                    .map_or(at.unwrap_or(now), |first| first.min(at.unwrap_or(now))),
            );
        };

        let slide0 = post.slides.first().filter(|slide| slide.position == 0);
        // The cover, through slide 0 while its URL is valid (§2.13); an
        // image post's slide 0 is then the cover's item.
        let mut slide0_is_cover = false;
        if assets.thumbnail
            && !post.cover_stored
            && let Some(cover_url) = &post.cover_url
        {
            slide0_is_cover = slide0.is_some_and(|slide| slide.kind == "image");
            let (row, tries) = match slide0 {
                Some(slide) => (FetchRow::Slide(0), &slide.tries),
                None => (FetchRow::Post, &post.cover_tries),
            };
            if !tries.failed() && tries.due(now) {
                let through_slide = slide0
                    .filter(|slide| matches!(slide.kind.as_str(), "image" | "video"))
                    .and_then(|slide| {
                        let url = slide.url.as_ref()?;
                        Some((url.clone(), expiry(url, slide.expires_column)))
                    })
                    .filter(|(_, at)| !expired(*at));
                let (url, at) = through_slide.unwrap_or_else(|| {
                    (
                        cover_url.clone(),
                        expiry(cover_url, post.cover_expires_column),
                    )
                });
                if expired(at) {
                    needs_refresh(&mut refresh, at, row, tries);
                } else if uploads_here {
                    upload(TaskSlot::Cover, url, at, row, tries);
                }
            }
        }
        if assets.image {
            for slide in &post.slides {
                let Some(url) = &slide.url else { continue };
                if slide.kind != "image"
                    || slide.stored
                    || (slide.position == 0 && slide0_is_cover)
                    || slide.tries.failed()
                    || !slide.tries.due(now)
                {
                    continue;
                }
                let row = FetchRow::Slide(slide.position);
                let at = expiry(url, slide.expires_column);
                if expired(at) {
                    needs_refresh(&mut refresh, at, row, &slide.tries);
                } else if uploads_here {
                    upload(
                        TaskSlot::Slide(slide.position),
                        url.clone(),
                        at,
                        row,
                        &slide.tries,
                    );
                }
            }
        }
        if !refresh.rows.is_empty() {
            refreshes.push(refresh);
        }
    }
    let by_expiry = |a: &Task, b: &Task| {
        (a.expires_at.is_none(), a.expires_at, a.post_id, a.id.slot).cmp(&(
            b.expires_at.is_none(),
            b.expires_at,
            b.post_id,
            b.id.slot,
        ))
    };
    uploads.sort_by(by_expiry);
    refreshes.sort_by(by_expiry);
    let mut tasks = uploads;
    tasks.extend(refreshes);
    tasks.extend(hydrations);
    Ok(Derived { tasks })
}

/// The facts the core rule's [`needs_hydration`] reads.
fn facts(post: &PostRow) -> PostFacts {
    PostFacts {
        platform: post.platform,
        media_type: post.media_type.clone(),
        state: ArchiveState::Client,
        cover: Asset {
            stored: post.cover_stored,
            has_url: post.cover_url.is_some(),
            ..Asset::default()
        },
        slides: post
            .slides
            .iter()
            .map(|slide| SlideFacts {
                image: slide.kind == "image",
                asset: Asset {
                    stored: slide.stored,
                    has_url: slide.url.is_some(),
                    ..Asset::default()
                },
                video_stored: slide.video_stored,
            })
            .collect(),
    }
}

// ── Leases and wake-ups ──────────────────────────────────────────────────────

#[derive(Clone, Debug)]
struct Lease {
    holder: String,
    until: i64,
}

/// The process's leases of tasks and the wake-ups of waiting polls (see the
/// module docs). [`crate::extension::ExtensionState::tasks`] holds the
/// server's.
#[derive(Default)]
pub struct TaskBoard {
    /// By user, then task id.
    leases: Mutex<HashMap<String, HashMap<String, Lease>>>,
    /// By user: bumped by [`TaskBoard::wake`].
    wakers: Mutex<HashMap<String, watch::Sender<u64>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl TaskBoard {
    /// Leases to `holder` up to `limit` of `user_id`'s `tasks` at `now`:
    /// those nobody holds, whose lease ended, or that `holder` holds
    /// already (renewed). Returns them with the end of their lease, in the
    /// order of `tasks`.
    pub fn lease<'a>(
        &self,
        user_id: &str,
        holder: &str,
        tasks: &'a [Task],
        limit: usize,
        now: i64,
    ) -> Vec<(&'a Task, i64)> {
        let until = now.saturating_add(crate::auth::millis(LEASE));
        let mut leases = lock(&self.leases);
        let user = leases.entry(user_id.to_owned()).or_default();
        user.retain(|_, lease| lease.until > now);
        let mut leased = Vec::new();
        for task in tasks {
            if leased.len() == limit {
                break;
            }
            let id = task.id.to_string();
            let free = user.get(&id).is_none_or(|lease| lease.holder == holder);
            if free {
                user.insert(
                    id,
                    Lease {
                        holder: holder.to_owned(),
                        until,
                    },
                );
                leased.push((task, until));
            }
        }
        if user.is_empty() {
            leases.remove(user_id);
        }
        leased
    }

    /// Ends the lease of `user_id`'s task `id`, whoever holds it.
    pub fn release(&self, user_id: &str, id: &str) {
        let mut leases = lock(&self.leases);
        if let Some(user) = leases.get_mut(user_id) {
            user.remove(id);
            if user.is_empty() {
                leases.remove(user_id);
            }
        }
    }

    /// A receiver that changes at `user_id`'s next [`TaskBoard::wake`].
    pub fn subscribe(&self, user_id: &str) -> watch::Receiver<u64> {
        lock(&self.wakers)
            .entry(user_id.to_owned())
            .or_insert_with(|| watch::channel(0).0)
            .subscribe()
    }

    /// Wakes `user_id`'s waiting polls: they derive their tasks again.
    pub fn wake(&self, user_id: &str) {
        let mut wakers = lock(&self.wakers);
        if let Some(sender) = wakers.get(user_id) {
            if sender.receiver_count() == 0 {
                wakers.remove(user_id);
            } else {
                sender.send_modify(|n| *n = n.wrapping_add(1));
            }
        }
    }

    /// Forgets ended leases and wake-ups nobody waits for (maintenance).
    pub fn sweep(&self, now: i64) {
        let mut leases = lock(&self.leases);
        leases.retain(|_, user| {
            user.retain(|_, lease| lease.until > now);
            !user.is_empty()
        });
        drop(leases);
        lock(&self.wakers).retain(|_, sender| sender.receiver_count() > 0);
    }

    /// How many leases are held (tests, metrics).
    #[must_use]
    pub fn leased(&self, user_id: &str, now: i64) -> usize {
        lock(&self.leases)
            .get(user_id)
            .map_or(0, |user| user.values().filter(|l| l.until > now).count())
    }
}

/// Wakes `user_id`'s waiting task polls: posts went to `client`.
pub fn wake(state: &AppState, user_id: &str) {
    state.extension().tasks().wake(user_id);
}

// ── Polls ────────────────────────────────────────────────────────────────────

/// What a poll got.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Polled {
    /// The leased tasks, each with the end of its lease.
    pub tasks: Vec<(Task, i64)>,
    /// Every task that waits, per platform.
    pub waiting: Waiting,
}

/// The time tasks are derived and leased with: the job system's clock, as
/// the archive drain's.
fn now(state: &AppState) -> i64 {
    state.jobs().clock().now_ms()
}

async fn derive_for(state: &AppState, user_id: &str) -> Result<Derived, ApiError> {
    let (modes, now) = (archive::modes(state), now(state));
    let db = state.user_db(user_id).await?;
    blocking(move || {
        db.read(|conn| -> Result<Derived, RepoError> {
            let assets = settings::read(conn)?.archive_asset_types;
            derive(conn, None, assets, modes, now)
        })
    })
    .await
}

/// `GET /ingest/tasks` for `holder` (the poller's token) of `user_id`:
/// leases up to `limit` tasks; without any, waits up to `wait` for a
/// wake-up, or until the server shuts down (see the module docs).
///
/// # Errors
///
/// The library cannot be read.
pub async fn poll(
    state: &AppState,
    user_id: &str,
    holder: &str,
    wait: Duration,
    limit: usize,
) -> Result<Polled, ApiError> {
    let board = state.extension().tasks();
    let deadline = Instant::now() + wait;
    let shutdown = state.shutdown_token().clone();
    loop {
        // Subscribe first: a wake-up during the derivation is not lost.
        let mut woken = board.subscribe(user_id);
        let derived = derive_for(state, user_id).await?;
        let leased = board.lease(user_id, holder, &derived.tasks, limit, now(state));
        let polled = Polled {
            tasks: leased
                .into_iter()
                .map(|(task, until)| (task.clone(), until))
                .collect(),
            waiting: derived.waiting(),
        };
        if !polled.tasks.is_empty() || Instant::now() >= deadline {
            return Ok(polled);
        }
        tokio::select! {
            () = shutdown.cancelled() => return Ok(polled),
            () = tokio::time::sleep_until(deadline) => return Ok(polled),
            _ = woken.changed() => {}
        }
    }
}

// ── Completion ───────────────────────────────────────────────────────────────

/// How the extension ended a task (C6).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum TaskOutcome {
    /// `upload_media`: the bytes are in the upload `uploadId`.
    Uploaded,
    /// `refresh_media`, `hydrate_link`: a `refresh` batch carried the post.
    Refreshed,
    /// The post or its media are gone (404, deleted).
    Gone,
    /// It did not work; `errorCode` says why.
    Failed,
    /// The extension could not try now (no Instagram tab, a pacing cap).
    Skipped,
}

impl TaskOutcome {
    /// Its wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Uploaded => "uploaded",
            Self::Refreshed => "refreshed",
            Self::Gone => "gone",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }

    /// Whether a task of `kind` can end this way.
    #[must_use]
    pub const fn fits(self, kind: TaskKind) -> bool {
        match self {
            Self::Uploaded => matches!(kind, TaskKind::UploadMedia),
            Self::Refreshed => matches!(kind, TaskKind::RefreshMedia | TaskKind::HydrateLink),
            Self::Gone | Self::Failed | Self::Skipped => true,
        }
    }
}

/// `fetch_error` of an item the extension failed: `ext_<code>`, the code
/// cut to lowercase ASCII letters, digits and `_`.
fn error_code(outcome: TaskOutcome, raw: Option<&str>) -> String {
    let code: String = raw
        .unwrap_or("")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
        .map(|c| c.to_ascii_lowercase())
        .take(40)
        .collect();
    let code = if code.is_empty() {
        outcome.as_str()
    } else {
        &code
    };
    format!("ext_{code}")
}

/// `POST /ingest/tasks/{id}/complete`: ends `user_id`'s task `id` with
/// `outcome` (see the module docs).
///
/// # Errors
///
/// 404 for a text that is not a task id; 422 for an outcome the task's kind
/// cannot have, or `uploaded` without a usable `uploadId`; 409
/// `upload_consumed` for an upload used before while the slot is still
/// empty; the quota's refusals; the databases failed.
pub async fn complete(
    state: &AppState,
    user_id: &str,
    id: &str,
    outcome: TaskOutcome,
    upload_id: Option<&str>,
    error: Option<&str>,
) -> Result<(), ApiError> {
    let task: TaskId = id
        .parse()
        .map_err(|BadTaskId| ApiError::not_found().with_detail("not a task id"))?;
    if !outcome.fits(task.kind) {
        return Err(ApiError::invalid_field(
            "outcome",
            format!(
                "a {} task does not end {}",
                task.kind.as_str(),
                outcome.as_str()
            ),
        ));
    }
    state.extension().tasks().release(user_id, id);
    if outcome == TaskOutcome::Uploaded {
        let upload_id = upload_id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| ApiError::invalid_field("uploadId", "is required when uploaded"))?;
        return complete_upload(state, user_id, &task, upload_id).await;
    }
    let (modes, now) = (archive::modes(state), now(state));
    let error = error_code(outcome, error);
    library::write(state, user_id, ChangeReason::Archive, move |tx| {
        let assets = settings::read(tx)?.archive_asset_types;
        let found = derive(tx, Some(&task.key), assets, modes, now)?
            .tasks
            .into_iter()
            .find(|t| t.id == task);
        let keys = Some(vec![task.key.clone()]);
        let Some(found) = found else {
            if outcome == TaskOutcome::Refreshed && task.kind == TaskKind::HydrateLink {
                // Hydrated: the post's cover keeps no hydration tries.
                tx.execute(
                    "UPDATE posts SET cover_fetch_attempts = 0, cover_fetch_next_at = NULL,
                                      cover_fetch_error = NULL
                     WHERE key = ?1 AND cover_fetch_error LIKE 'ext\\_%' ESCAPE '\\'",
                    [&task.key],
                )?;
            }
            return Ok(Change { value: (), keys });
        };
        let hydration = task.kind == TaskKind::HydrateLink;
        let fail_post = |tx: &Connection| -> Result<(), RepoError> {
            tx.execute(
                "UPDATE posts SET archive_state = 'failed' WHERE id = ?1",
                [found.post_id],
            )?;
            Ok(())
        };
        match outcome {
            TaskOutcome::Gone => {
                for (row, attempts) in &found.rows {
                    set_tries(tx, found.post_id, *row, *attempts, None, FETCH_ERROR_GONE)?;
                }
                if hydration {
                    fail_post(tx)?;
                }
            }
            TaskOutcome::Failed | TaskOutcome::Skipped | TaskOutcome::Refreshed => {
                let mut out_of_tries = false;
                for (row, attempts) in &found.rows {
                    let attempts = attempts.saturating_add(1);
                    let next_at = (attempts < FETCH_TRIES).then(|| {
                        let failures = u32::try_from(attempts).unwrap_or(u32::MAX);
                        let delay = ITEM_BACKOFF.jittered(failures, seed(found.post_id, *row));
                        now.saturating_add(i64::try_from(delay.as_millis()).unwrap_or(i64::MAX))
                    });
                    out_of_tries |= next_at.is_none();
                    set_tries(tx, found.post_id, *row, attempts, next_at, &error)?;
                }
                if hydration && out_of_tries {
                    fail_post(tx)?;
                }
            }
            TaskOutcome::Uploaded => unreachable!("handled above"),
        }
        let policy = ArchivePolicy::read(tx, modes)?;
        refresh_states(tx, Scope::Posts(&[found.post_id]), &policy, now)?;
        Ok(Change { value: (), keys })
    })
    .await?;
    Ok(())
}

/// Writes an item's tries.
fn set_tries(
    conn: &Connection,
    post_id: i64,
    row: FetchRow,
    attempts: i64,
    next_at: Option<i64>,
    error: &str,
) -> Result<(), RepoError> {
    match row {
        FetchRow::Slide(position) => conn.execute(
            "UPDATE post_media SET fetch_attempts = ?3, fetch_next_at = ?4, fetch_error = ?5
             WHERE post_id = ?1 AND position = ?2",
            params![post_id, position, attempts, next_at, error],
        )?,
        FetchRow::Post => conn.execute(
            "UPDATE posts SET cover_fetch_attempts = ?2, cover_fetch_next_at = ?3,
                              cover_fetch_error = ?4
             WHERE id = ?1",
            params![post_id, attempts, next_at, error],
        )?,
    };
    Ok(())
}

/// A well-spread number from an item's identity and the time, for its
/// jitter.
fn seed(post_id: i64, row: FetchRow) -> u64 {
    let slot = match row {
        FetchRow::Slide(position) => position,
        FetchRow::Post => -1,
    };
    let mut x = (post_id as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((slot as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(crate::ids::now_ms() as u64);
    x ^= x >> 31;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 29)
}

/// Whether the slot of `task` is filled already, or its post is gone.
fn settled(conn: &Connection, key: &str, slot: TaskSlot) -> Result<bool, RepoError> {
    let filled: Option<bool> = match slot {
        TaskSlot::Cover | TaskSlot::Post => conn
            .prepare_cached("SELECT cover_object IS NOT NULL FROM posts WHERE key = ?1")?
            .query_row([key], |row| row.get(0))
            .optional()?,
        TaskSlot::Slide(position) => conn
            .prepare_cached(
                "SELECT m.object_id IS NOT NULL FROM post_media m JOIN posts p ON p.id = m.post_id
                 WHERE p.key = ?1 AND m.position = ?2",
            )?
            .query_row(params![key, position], |row| row.get(0))
            .optional()?,
    };
    Ok(filled.unwrap_or(true))
}

/// Why storing a claimed upload failed: whether the upload is spent.
enum StoreError {
    /// The upload cannot serve (bad bytes, quota): it is discarded.
    Spent(ApiError),
    /// Try again: the upload is given back.
    Retry(ApiError),
}

/// `uploaded`: claims the upload, stores it into the task's slot, then
/// discards the upload's bytes.
async fn complete_upload(
    state: &AppState,
    user_id: &str,
    task: &TaskId,
    upload_id: &str,
) -> Result<(), ApiError> {
    let slot = match task.slot {
        TaskSlot::Cover => Slot::Cover,
        TaskSlot::Slide(position) => Slot::Slide(position),
        TaskSlot::Post => return Err(ApiError::not_found().with_detail("not a task id")),
    };
    let ids = [upload_id.to_owned()];
    let claimed = match uploads::claim(state, user_id, UploadPurpose::ARCHIVE_OBJECT, &ids).await {
        Ok(mut claimed) => claimed.remove(0),
        Err(ClaimError::Consumed { id }) => {
            // A repeat after the upload was stored: nothing to do.
            let db = state.user_db(user_id).await?;
            let (key, task_slot) = (task.key.clone(), task.slot);
            let done = blocking(move || db.read(|conn| settled(conn, &key, task_slot))).await?;
            return if done {
                Ok(())
            } else {
                Err(ClaimError::Consumed { id }.into())
            };
        }
        Err(err) => return Err(err.into()),
    };
    match store_claimed(state, user_id, task, slot, &claimed).await {
        Ok(()) => {
            uploads::discard(state, user_id, &ids).await?;
            Ok(())
        }
        Err(StoreError::Spent(err)) => {
            uploads::discard(state, user_id, &ids).await?;
            Err(err)
        }
        Err(StoreError::Retry(err)) => {
            uploads::release(state, user_id, &ids).await?;
            Err(err)
        }
    }
}

async fn store_claimed(
    state: &AppState,
    user_id: &str,
    task: &TaskId,
    slot: Slot,
    claimed: &Claimed,
) -> Result<(), StoreError> {
    let retry = StoreError::Retry;
    let db = state.user_db(user_id).await.map_err(retry)?;
    let key = task.key.clone();
    let target = blocking(move || db.read(|conn| store::target_of(conn, &key, slot)))
        .await
        .map_err(retry)?;
    // The post is gone, or the slot is not one the archive fills.
    let Some(target) = target else {
        return Ok(());
    };
    let media = MediaStore::new(state.config().data_dir.users_dir())
        .user(user_id)
        .map_err(|err| retry(ApiError::internal(anyhow::anyhow!("{err}"))))?;

    // The bytes, into the store's staging area: what the purpose checked
    // when the upload completed, checked again against its SHA-256.
    let (path, staging) = (claimed.path.clone(), media.clone());
    let staged = blocking(move || -> Result<_, ApiError> {
        let file = File::open(&path).map_err(ApiError::internal)?;
        Ok(staging.ingest(file, IngestLimits::ARCHIVE_IMAGE))
    })
    .await
    .map_err(retry)?;
    let staged = match staged {
        Ok(staged) if staged.digest() == claimed.sha256 => staged,
        Ok(_) => return Err(StoreError::Spent(unusable("its bytes changed"))),
        Err(IngestError::Io(err)) => return Err(retry(ApiError::internal(err))),
        Err(err) => return Err(StoreError::Spent(unusable(&err.to_string()))),
    };

    let reservation = match quota::reserve(state, user_id, RESERVE_BYTES).await {
        Ok(reservation) => reservation,
        Err(err) if quota::is_refused(&err) => {
            // As for a server fetch (L13): metadata only.
            let post_id = target.post_id;
            library::write(state, user_id, ChangeReason::Archive, move |tx| {
                tx.execute(
                    "UPDATE posts SET archive_state = 'link_only' WHERE id = ?1",
                    [post_id],
                )?;
                Ok::<_, RepoError>(Change {
                    value: (),
                    keys: Some(vec![task_key(tx, post_id)?]),
                })
            })
            .await
            .map_err(retry)?;
            return Err(StoreError::Spent(err));
        }
        Err(err) => return Err(retry(err)),
    };

    let (pool_media, pool_target) = (media.clone(), target.clone());
    let prepared = match ImagePool::shared()
        .run(move || store::prepare(&pool_media, staged, &pool_target))
        .await
    {
        Ok(Ok(prepared)) => prepared,
        Ok(Err(err)) => return Err(retry(ApiError::internal(err))),
        Err(_) => return Err(StoreError::Spent(unusable("its image cannot be read"))),
    };

    let (modes, now) = (archive::modes(state), now(state));
    let key = target.key.clone();
    library::write(state, user_id, ChangeReason::Archive, move |tx| {
        let policy = ArchivePolicy::read(tx, modes)?;
        let cx = StoreContext {
            media: &media,
            origin: Origin::Extension,
            policy: &policy,
            now,
        };
        let committed = store::commit(tx, &cx, &target, prepared, reservation)?;
        Ok::<_, RepoError>(Change {
            keys: match committed {
                Committed::Stored { .. } => Some(vec![key]),
                Committed::Unwanted => Some(Vec::new()),
            },
            value: committed,
        })
    })
    .await
    .map_err(retry)?;
    Ok(())
}

fn task_key(conn: &Connection, post_id: i64) -> Result<String, RepoError> {
    Ok(
        conn.query_row("SELECT key FROM posts WHERE id = ?1", [post_id], |row| {
            row.get(0)
        })?,
    )
}

/// 422 on `uploadId`: the upload cannot be stored.
fn unusable(why: &str) -> ApiError {
    ApiError::invalid_field("uploadId", format!("the upload cannot be stored: {why}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(text: &str) -> Option<TaskId> {
        text.parse().ok()
    }

    #[test]
    fn ids_name_the_kind_the_post_and_the_slot() {
        for text in [
            "upload_media.ig_3191575067010950169.cover",
            "upload_media.x_1.0",
            "upload_media.pin_77.12",
            "refresh_media.ig_1.post",
            "hydrate_link.ig_1.post",
        ] {
            assert_eq!(id(text).unwrap().to_string(), text);
        }
        let parsed = id("upload_media.ig_5.3").unwrap();
        assert_eq!(
            (parsed.kind, parsed.key.as_str(), parsed.slot),
            (TaskKind::UploadMedia, "ig_5", TaskSlot::Slide(3))
        );
        for bad in [
            "",
            "upload_media",
            "upload_media.ig_1",
            "upload_media..cover",
            "upload_media.ig_1.post",
            "refresh_media.ig_1.cover",
            "hydrate_link.ig_1.0",
            "download.ig_1.cover",
            "upload_media.ig/1.cover",
            "upload_media.ig_1.-1",
            "upload_media.ig_1.+1",
            "upload_media.ig_1.99999",
            "upload_media.ig.1.2",
        ] {
            assert_eq!(id(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn outcomes_fit_their_kinds() {
        use TaskKind::{HydrateLink, RefreshMedia, UploadMedia};
        assert!(TaskOutcome::Uploaded.fits(UploadMedia));
        assert!(!TaskOutcome::Uploaded.fits(RefreshMedia));
        assert!(!TaskOutcome::Refreshed.fits(UploadMedia));
        assert!(TaskOutcome::Refreshed.fits(HydrateLink));
        for kind in [UploadMedia, RefreshMedia, HydrateLink] {
            for outcome in [TaskOutcome::Gone, TaskOutcome::Failed, TaskOutcome::Skipped] {
                assert!(outcome.fits(kind));
            }
        }
    }

    #[test]
    fn error_codes_are_cleaned_and_marked() {
        assert_eq!(
            error_code(TaskOutcome::Failed, Some("HTTP_403")),
            "ext_http_403"
        );
        assert_eq!(error_code(TaskOutcome::Skipped, None), "ext_skipped");
        assert_eq!(
            error_code(TaskOutcome::Failed, Some("gone")),
            "ext_gone",
            "never the archive's own `gone`"
        );
        assert_eq!(error_code(TaskOutcome::Failed, Some("<>!")), "ext_failed");
        assert_eq!(
            error_code(TaskOutcome::Failed, Some(&"a".repeat(99))).len(),
            44
        );
    }

    fn task(n: i64) -> Task {
        Task {
            id: TaskId {
                kind: TaskKind::UploadMedia,
                key: format!("x_{n}"),
                slot: TaskSlot::Cover,
            },
            post_id: n,
            platform: Platform::Twitter,
            native_id: n.to_string(),
            shortcode: None,
            post_url: String::new(),
            url: None,
            expires_at: None,
            rows: Vec::new(),
        }
    }

    #[test]
    fn a_lease_is_one_pollers_until_it_ends() {
        let board = TaskBoard::default();
        let tasks: Vec<Task> = (1..=3).map(task).collect();
        let lease = crate::auth::millis(LEASE);
        let ids = |leased: Vec<(&Task, i64)>| -> Vec<i64> {
            leased.into_iter().map(|(t, _)| t.post_id).collect()
        };
        assert_eq!(ids(board.lease("U", "A", &tasks, 2, 0)), [1, 2]);
        assert_eq!(ids(board.lease("U", "B", &tasks, 20, 1)), [3], "exclusive");
        assert_eq!(ids(board.lease("U", "A", &tasks, 20, 2)), [1, 2], "renewed");
        assert_eq!(
            ids(board.lease("U", "B", &tasks, 20, 3)),
            [3],
            "its own task again"
        );
        assert_eq!(
            ids(board.lease("V", "B", &tasks, 20, 3)),
            [1, 2, 3],
            "per user"
        );
        board.release("U", "upload_media.x_1.cover");
        assert_eq!(ids(board.lease("U", "B", &tasks, 20, 4)), [1, 3]);
        // A's lease of task 2 ends a lease after its renewal at 2.
        assert!(ids(board.lease("U", "C", &tasks, 20, lease + 1)).is_empty());
        assert_eq!(ids(board.lease("U", "C", &tasks, 20, lease + 2)), [2]);
        assert_eq!(board.leased("U", lease + 2), 3);
        board.sweep(10 * lease);
        assert_eq!(board.leased("U", 0), 0);
    }

    #[tokio::test]
    async fn a_wake_reaches_the_users_waiting_polls_only() {
        let board = TaskBoard::default();
        let mut alice = board.subscribe("A");
        let mut bob = board.subscribe("B");
        board.wake("A");
        assert!(alice.has_changed().unwrap());
        assert!(!bob.has_changed().unwrap());
        alice.mark_unchanged();
        bob.mark_unchanged();
        drop(alice);
        board.wake("A");
        board.sweep(0);
        assert_eq!(lock(&board.wakers).len(), 1, "only Bob still waits");
    }
}
