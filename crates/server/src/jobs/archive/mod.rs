//! `archive.drain` (plan §2.12, §2.13, D4, D14; P2-10): stores the covers
//! and image slides of a user's saved posts from the platform CDNs, before
//! their URLs expire.
//!
//! **A drain.** The work lives in the library ([`select`]): the posts in
//! `pending` or `partial` and their items, with each item's tries in
//! `post_media.fetch_*` (`posts.cover_fetch_*` for a cover without
//! slides). One job per user (dedupe key = the kind) works through them in
//! chunks of [`CHUNK`] items, covers first, then image slides, each by
//! soonest expiry, and only the asset types the user keeps
//! (`archiveAssetTypes`). When only backed-off items are left it re-arms
//! itself at the first one's `fetch_next_at` ([`Outcome::Requeue`]); after
//! [`CHUNKS_PER_RUN`] chunks it yields its slot to other users. The drain
//! sweeper re-arms every 10 minutes the users with pending items and no
//! drain ([`pending`]). Enqueue it with [`enqueue`] after anything leaves
//! posts `pending`: the migration install does, the ingest service (P2-09)
//! does when `refresh_states` counts server work, and the asset-type
//! setting does.
//!
//! **Limits.** [`USER_FETCHES`] fetches per user, [`GLOBAL_FETCHES`]
//! fetches and [`GLOBAL_ENCODES`] encodes over every user, on top of the
//! host groups' rates, concurrency and breakers of the CDN fetcher
//! ([`crate::outbound::cdn`]).
//!
//! **Each item** goes through the fetcher, after a quota reservation of
//! [`RESERVE_BYTES`] (P4-07, L13):
//!
//! | Outcome | The item |
//! |---|---|
//! | `Stored` | stored and linked ([`store`]): master per D4, `g480` and ThumbHash, origin `server` |
//! | `Expired` (no request sent) | its URL's expiry is recorded: the post goes to the extension (`client`, `refresh_media`) |
//! | `Gone` | failed for good (`fetch_error = 'gone'`) |
//! | `Blocked`, `Transient` | a try; next after 30 s × 2ⁿ with jitter (max 6 h) or the `Retry-After`; failed after [`FETCH_TRIES`] |
//! | `Rejected` | failed for good (`fetch_error` says why): the server can never store it |
//! | `BreakerOpen` | no try: the platform goes to the extension while the breaker is open ([`handoff`]); deferred while a half-open breaker's probe is out |
//! | quota refused | the post becomes `link_only` (L13); the drain goes on |
//!
//! After each item the post's state is derived again with the core rule
//! and the archive's current modes ([`modes`]). After each chunk the user's
//! streams get `posts.changed` (reason `archive`, the posts' keys) and
//! `job.updated` with the progress: items done over the items pending when
//! the try started.
//!
//! **Modes** (`SHELFY_ARCHIVE_MODE_<GROUP>`, [`ArchiveArgs`]): `server` for
//! every platform (SPIKE-2 and, for Pinterest, SPIKE-9 and L17). `server`
//! and `auto` both fetch from the server with the breaker handing over to
//! the extension while open; `client` hands every fetch to the extension.
//!
//! **Metrics.** `shelfy_archive_cover_latency_seconds{platform}`: from the
//! post's insert to its cover stored, for posts inserted in the last 7 days
//! (a migrated post keeps its desktop import time); `shelfy_archive_backlog
//! {platform}`: the items left to the server, as the drains last counted
//! them; and the fetcher's `shelfy_media_fetch_total` and
//! `shelfy_breaker_open`.
//!
//! **Seams.** P2-14 stores extension uploads with [`store::prepare`] and
//! [`store::commit`] (origin `extension`). P2-09 derives the states of
//! ingested posts with [`modes`] and calls [`enqueue`]. P4-12's GC and P4's
//! Jobs view (retry: reset `fetch_attempts` and `fetch_error`, then
//! [`enqueue`]) work on the same columns.

mod handoff;
pub mod select;
pub mod store;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use clap::Args;
use futures_util::StreamExt as _;
use futures_util::stream;
use rusqlite::params;
use shelfy_core::ingest::archive::{
    ArchiveMode, ArchiveModes, ArchivePolicy, FETCH_ERROR_GONE, FETCH_TRIES, Scope, refresh_states,
};
use shelfy_core::repo::settings::{self, ArchiveAssetTypes};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_media::pool::ImagePool;
use shelfy_media::refs::Origin;
use shelfy_media::store::{IngestLimits, MediaStore, StagedObject, UserMedia};
use tokio::sync::Semaphore;

pub use handoff::{WATCH_INTERVAL, rederive_all, spawn_watcher, sync_breakers};
use select::{FetchRow, Item, PlatformCounts, Selection, Slot, UrlColumn};
use store::{Committed, StoreContext, Target};

use super::{
    Backoff, Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome,
    SweepContext, codes,
};
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::library;
use crate::outbound::{BreakerState, FetchOutcome, FetchRequest, HostGroup, Outbound, Rejection};
use crate::quota::{self, Reservation};
use crate::state::AppState;
use crate::telemetry::metrics::{ARCHIVE_BACKLOG, ARCHIVE_COVER_LATENCY_SECONDS};

/// The kind's name.
pub const KIND: &str = "archive.drain";
/// Items per chunk (§2.12: 25 slides).
pub const CHUNK: usize = 25;
/// Fetches at once for one user (§2.12).
pub const USER_FETCHES: usize = 2;
/// Fetches at once over every user (§2.12).
pub const GLOBAL_FETCHES: usize = 4;
/// Masters and renditions made at once over every user (§2.12).
pub const GLOBAL_ENCODES: usize = 2;
/// Chunks a try works through before it lets other users' drains run.
pub const CHUNKS_PER_RUN: usize = 8;
/// Bytes reserved against the quota before a fetch: the fetcher's cap.
pub const RESERVE_BYTES: u64 = IngestLimits::ARCHIVE_IMAGE.max_bytes;
/// The delays between an item's tries (§2.12: 30 s × 2ⁿ, at most 6 h).
pub const ITEM_BACKOFF: Backoff = Backoff::DEFAULT;
/// How long a drain waits for a half-open breaker's probe of another drain.
pub const PROBE_WAIT_MS: i64 = 15_000;
/// Covers of posts inserted longer ago than this do not count in the cover
/// latency: a migrated post keeps its desktop import time.
pub const LATENCY_MAX_AGE_MS: i64 = 7 * 86_400_000;

/// The kind, for the registry ([`super::kinds::registry`]): one drain per
/// user, four users at once (their fetches share [`GLOBAL_FETCHES`]), a
/// 5-minute lease renewed by every item.
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(GLOBAL_FETCHES)
            .per_user(1)
            .max_attempts(5)
            .lease(Duration::from_secs(300)),
        run,
    )
    .with_sweep(pending)
}

/// Enqueues `user_id`'s drain; an active one is returned instead (and runs
/// now if it was waiting for a backed-off item).
///
/// # Errors
///
/// The control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str) -> Result<Enqueued, ApiError> {
    jobs.enqueue(NewJob::new(user_id, KIND).dedupe(KIND)).await
}

// ── Configuration ────────────────────────────────────────────────────────────

/// Who fetches each platform's media (`serve`'s flags).
#[derive(Clone, Debug, Args)]
pub struct ArchiveArgs {
    /// Who archives Instagram media: `server` (the server, the breaker
    /// hands over to the extension while open), `auto` (the same) or
    /// `client` (the extension uploads everything).
    #[arg(
        long = "archive-mode-instagram",
        env = "SHELFY_ARCHIVE_MODE_INSTAGRAM",
        value_name = "MODE",
        default_value = "server",
        value_parser = parse_mode
    )]
    pub archive_mode_instagram: ArchiveMode,

    /// Who archives X media: `server`, `auto` or `client`.
    #[arg(
        long = "archive-mode-x",
        env = "SHELFY_ARCHIVE_MODE_X",
        value_name = "MODE",
        default_value = "server",
        value_parser = parse_mode
    )]
    pub archive_mode_x: ArchiveMode,

    /// Who archives Pinterest media: `server` (SPIKE-9, L17), `auto` or
    /// `client`.
    #[arg(
        long = "archive-mode-pinterest",
        env = "SHELFY_ARCHIVE_MODE_PINTEREST",
        value_name = "MODE",
        default_value = "server",
        value_parser = parse_mode
    )]
    pub archive_mode_pinterest: ArchiveMode,
}

/// `SHELFY_ARCHIVE_MODE_<GROUP>`: `server`, `client` or `auto`.
fn parse_mode(text: &str) -> Result<ArchiveMode, String> {
    text.trim()
        .to_ascii_lowercase()
        .parse()
        .map_err(|_| format!("{text:?} is not server, client or auto"))
}

/// The archive's settings.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ArchiveConfig {
    /// The configured mode of each platform.
    pub modes: ArchiveModes,
}

impl ArchiveConfig {
    /// The settings of `args`.
    #[must_use]
    pub fn from_args(args: &ArchiveArgs) -> Self {
        Self {
            modes: ArchiveModes {
                instagram: args.archive_mode_instagram,
                twitter: args.archive_mode_x,
                pinterest: args.archive_mode_pinterest,
            },
        }
    }
}

// ── The process's archive state ──────────────────────────────────────────────

/// What the archive keeps in the process: its configured modes, its global
/// limits, what the breaker watcher last saw and the backlog gauge. Cheap
/// to clone: [`AppState::archive`] holds the server's.
#[derive(Clone)]
pub struct Archive {
    inner: Arc<Inner>,
}

struct Inner {
    modes: ArchiveModes,
    fetches: Semaphore,
    encodes: Semaphore,
    /// Per CDN group (the order of [`HostGroup::CDN`]): handed over to the
    /// extension at the last check.
    handed_off: Mutex<[bool; 3]>,
    /// Items left to the server per user, as their drains last counted.
    backlog: Mutex<HashMap<String, PlatformCounts>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Archive {
    /// The archive of `config`.
    #[must_use]
    pub fn new(config: ArchiveConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                modes: config.modes,
                fetches: Semaphore::new(GLOBAL_FETCHES),
                encodes: Semaphore::new(GLOBAL_ENCODES),
                handed_off: Mutex::new([false; 3]),
                backlog: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// The configured modes.
    #[must_use]
    pub fn configured_modes(&self) -> ArchiveModes {
        self.inner.modes
    }

    /// The modes in effect: the configured ones, with every platform whose
    /// CDN breaker is open handed to the extension.
    #[must_use]
    pub fn modes(&self, outbound: &Outbound) -> ArchiveModes {
        HostGroup::CDN
            .into_iter()
            .filter(|group| {
                matches!(
                    outbound.cdn().breaker_state(*group),
                    BreakerState::Open { .. }
                )
            })
            .filter_map(platform_of)
            .fold(self.inner.modes, |modes, platform| {
                modes.with(platform, ArchiveMode::Client)
            })
    }

    /// Records which breakers are open now; returns the platforms whose
    /// breaker opened or left the open state since the last call.
    fn observe_breakers(&self, outbound: &Outbound) -> Vec<(Platform, bool)> {
        let mut seen = lock(&self.inner.handed_off);
        let mut changed = Vec::new();
        for (slot, group) in seen.iter_mut().zip(HostGroup::CDN) {
            let open = matches!(
                outbound.cdn().breaker_state(group),
                BreakerState::Open { .. }
            );
            if *slot != open {
                *slot = open;
                changed.extend(platform_of(group).map(|platform| (platform, open)));
            }
        }
        changed
    }

    /// Records `user_id`'s items left and publishes the backlog gauge.
    fn set_backlog(&self, user_id: &str, counts: PlatformCounts) {
        let mut backlog = lock(&self.inner.backlog);
        if counts == [0; 3] {
            backlog.remove(user_id);
        } else {
            backlog.insert(user_id.to_owned(), counts);
        }
        let mut totals = [0_u64; 3];
        for counts in backlog.values() {
            for (total, n) in totals.iter_mut().zip(counts) {
                *total += n;
            }
        }
        drop(backlog);
        for (platform, total) in [Platform::Instagram, Platform::Twitter, Platform::Pinterest]
            .into_iter()
            .zip(totals)
        {
            metrics::gauge!(ARCHIVE_BACKLOG, "platform" => platform.as_str()).set(total as f64);
        }
    }
}

/// The archive modes in effect now (see [`Archive::modes`]): derive every
/// archive state with them (ingest, settings, installs).
#[must_use]
pub fn modes(state: &AppState) -> ArchiveModes {
    state.archive().modes(state.outbound())
}

/// The CDN host group of a platform's media.
#[must_use]
pub const fn group_of(platform: Platform) -> Option<HostGroup> {
    match platform {
        Platform::Instagram => Some(HostGroup::Instagram),
        Platform::Twitter => Some(HostGroup::X),
        Platform::Pinterest => Some(HostGroup::Pinterest),
        Platform::Web | Platform::Manual => None,
    }
}

/// The platform whose media a CDN host group serves.
#[must_use]
pub const fn platform_of(group: HostGroup) -> Option<Platform> {
    match group {
        HostGroup::Instagram => Some(Platform::Instagram),
        HostGroup::X => Some(Platform::Twitter),
        HostGroup::Pinterest => Some(Platform::Pinterest),
        HostGroup::InstagramWeb | HostGroup::XWeb | HostGroup::PinterestWeb => None,
    }
}

// ── The drain ────────────────────────────────────────────────────────────────

/// The drain sweeper's check: when the user's drain has work (now, or the
/// first backed-off item's time). A user without a library has none, and
/// none is created for them.
async fn pending(ctx: SweepContext) -> Result<Option<i64>, JobError> {
    let library = ctx.state().config().data_dir.library_db(ctx.user_id());
    if !tokio::fs::try_exists(&library).await.unwrap_or(false) {
        return Ok(None);
    }
    let now = ctx.now_ms();
    let selection = ctx
        .user_db(move |db| {
            db.read(|conn| -> Result<Selection, RepoError> {
                let assets = settings::read(conn)?.archive_asset_types;
                select::select(conn, assets, now, 1, &HashSet::new())
            })
            .map_err(JobError::from)
        })
        .await?;
    Ok(selection.wake_at(now))
}

async fn run(ctx: JobContext) -> JobResult {
    let drain = Drain::new(ctx.clone())?;
    // Items deferred while a half-open breaker's probe is out, by group;
    // they come back once their group's breaker closed.
    let mut skip: HashMap<(i64, Slot), HostGroup> = HashMap::new();
    // Items that changed nothing (their slot was filled meanwhile): not
    // again in this try.
    let mut stuck: HashSet<(i64, Slot)> = HashSet::new();
    let mut rederived: HashSet<i64> = HashSet::new();
    let mut total: Option<u64> = None;
    let mut done = 0_u64;
    for _ in 0..CHUNKS_PER_RUN {
        if ctx.should_yield() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        let cdn = ctx.state().outbound().cdn();
        skip.retain(|_, group| cdn.breaker_state(*group) != BreakerState::Closed);
        let now = drain.now();
        let excluded: HashSet<(i64, Slot)> = skip.keys().chain(&stuck).copied().collect();
        let selection = drain.select(now, &excluded).await?;
        // Posts whose Instagram URLs expired since their state was derived.
        let stale: Vec<i64> = selection
            .stale
            .iter()
            .copied()
            .filter(|id| rederived.insert(*id))
            .collect();
        if !stale.is_empty() {
            let keys = drain.rederive(stale).await?;
            if !keys.is_empty() {
                library::announce(
                    ctx.state().events(),
                    ctx.user_id(),
                    ChangeReason::Archive,
                    library::event_keys(keys),
                );
            }
        }
        drain
            .archive()
            .set_backlog(ctx.user_id(), selection.pending);
        let total = *total.get_or_insert(selection.pending_total().max(1));
        if selection.due.is_empty() {
            // Deferred items wait for another drain's probe of a half-open
            // breaker.
            let probe = (!skip.is_empty()).then_some(now + PROBE_WAIT_MS);
            return Ok(match selection.next_at.into_iter().chain(probe).min() {
                Some(at) => Outcome::Requeue { run_at: Some(at) },
                None => Outcome::Succeeded,
            });
        }
        let report = drain.chunk(selection.due, &mut skip, &mut stuck).await?;
        done += report.processed;
        if !report.keys.is_empty() {
            library::announce(
                ctx.state().events(),
                ctx.user_id(),
                ChangeReason::Archive,
                library::event_keys(report.keys.into_iter().collect()),
            );
        }
        ctx.progress(Some(done as f64 / total as f64), Some("fetch"))
            .await;
    }
    Ok(Outcome::Requeue { run_at: None })
}

/// What a chunk did.
#[derive(Debug, Default)]
struct ChunkReport {
    /// Items that were dealt with (stored, failed, handed over…).
    processed: u64,
    /// The posts that changed.
    keys: BTreeSet<String>,
}

/// What became of one item.
#[derive(Debug)]
enum Done {
    /// Stored (or its cover linked); the post changed.
    Stored {
        key: String,
        platform: Platform,
        imported_at: i64,
        cover: bool,
    },
    /// Its state changed: a failure, an expiry, `link_only`.
    Changed(String),
    /// Nothing changed: its slot was filled meanwhile.
    Nothing,
    /// Not now: a half-open breaker's probe is out.
    Deferred,
    /// The breaker of its platform is open.
    HandOff(Platform),
}

/// How an item's fetch ended, for its row.
#[derive(Clone, Copy, Debug)]
enum Record {
    /// Its URL expired: the extension refreshes it.
    Expired,
    /// A try failed.
    Failed {
        attempts: i64,
        next_at: Option<i64>,
        error: &'static str,
    },
    /// The quota refused it: metadata only (L13).
    LinkOnly,
}

/// One try of a user's drain.
struct Drain {
    ctx: JobContext,
    media: UserMedia,
}

impl Drain {
    fn new(ctx: JobContext) -> Result<Self, JobError> {
        let media = MediaStore::new(ctx.state().config().data_dir.users_dir())
            .user(ctx.user_id())
            .map_err(|err| {
                JobError::permanent(codes::INVALID_PAYLOAD).with_detail(err.to_string())
            })?;
        Ok(Self { ctx, media })
    }

    fn state(&self) -> &AppState {
        self.ctx.state()
    }

    fn archive(&self) -> &Archive {
        self.state().archive()
    }

    fn now(&self) -> i64 {
        self.ctx.jobs().clock().now_ms()
    }

    async fn select(&self, now: i64, skip: &HashSet<(i64, Slot)>) -> Result<Selection, JobError> {
        let skip = skip.clone();
        self.ctx
            .user_db(move |db| {
                db.read(|conn| -> Result<Selection, RepoError> {
                    let assets: ArchiveAssetTypes = settings::read(conn)?.archive_asset_types;
                    select::select(conn, assets, now, CHUNK, &skip)
                })
                .map_err(JobError::from)
            })
            .await
    }

    async fn chunk(
        &self,
        items: Vec<Item>,
        skip: &mut HashMap<(i64, Slot), HostGroup>,
        stuck: &mut HashSet<(i64, Slot)>,
    ) -> Result<ChunkReport, JobError> {
        let mut report = ChunkReport::default();
        let mut hand_off = BTreeSet::new();
        let mut results = stream::iter(items.into_iter().map(|item| async move {
            let id = (item.id(), group_of(item.platform));
            (id, self.process(item).await)
        }))
        .buffer_unordered(USER_FETCHES);
        loop {
            let next = tokio::select! {
                biased;
                () = self.ctx.token().cancelled() => return Err(JobError::cancelled()),
                next = results.next() => next,
            };
            let Some((id, done)) = next else {
                break;
            };
            match done? {
                Done::Stored {
                    key,
                    platform,
                    imported_at,
                    cover,
                } => {
                    if cover {
                        self.record_latency(platform, imported_at);
                    }
                    report.keys.insert(key);
                    report.processed += 1;
                }
                Done::Changed(key) => {
                    report.keys.insert(key);
                    report.processed += 1;
                }
                Done::Nothing => {
                    stuck.insert(id.0);
                    report.processed += 1;
                }
                Done::Deferred => {
                    skip.extend(id.1.map(|group| (id.0, group)));
                }
                // Its post leaves `pending` with the handoff below.
                Done::HandOff(platform) => {
                    hand_off.insert(platform);
                }
            }
        }
        drop(results);
        if !hand_off.is_empty() {
            let keys = self.hand_off(hand_off.into_iter().collect()).await?;
            report.keys.extend(keys);
        }
        Ok(report)
    }

    /// Fetches and stores one item (see the module docs).
    async fn process(&self, item: Item) -> Result<Done, JobError> {
        self.ctx.heartbeat();
        if self.ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        if let Some(object_id) = item.existing {
            return self.link_existing(item, object_id).await;
        }
        let now = self.now();
        let Some(group) = group_of(item.platform) else {
            return Ok(Done::Nothing);
        };
        if item.expired(now) {
            self.record(&item, Record::Expired).await?;
            return Ok(Done::Changed(item.key));
        }
        let cdn = self.state().outbound().cdn();
        if matches!(cdn.breaker_state(group), BreakerState::Open { .. }) {
            return Ok(Done::HandOff(item.platform));
        }
        let reservation =
            match quota::reserve(self.state(), self.ctx.user_id(), RESERVE_BYTES).await {
                Ok(reservation) => reservation,
                Err(err) if quota::is_refused(&err) => {
                    self.record(&item, Record::LinkOnly).await?;
                    return Ok(Done::Changed(item.key));
                }
                Err(err) => return Err(err.into()),
            };
        let outcome = {
            let _fetch = self
                .archive()
                .inner
                .fetches
                .acquire()
                .await
                .map_err(|_| JobError::transient(codes::INTERNAL))?;
            cdn.fetch(FetchRequest {
                url: &item.url,
                media: &self.media,
                // Only Instagram URLs expire; the fetcher also reads `oe`.
                expires_at_ms: item
                    .expires_at
                    .filter(|_| item.platform == Platform::Instagram),
                now_ms: now,
            })
            .await
        };
        self.ctx.heartbeat();
        let record = match outcome {
            FetchOutcome::Stored(staged) => return self.store(item, staged, reservation).await,
            FetchOutcome::Expired => Record::Expired,
            FetchOutcome::Gone => Record::Failed {
                attempts: item.attempts + 1,
                next_at: None,
                error: FETCH_ERROR_GONE,
            },
            FetchOutcome::Blocked { retry_after, .. } => retry(&item, now, retry_after, "blocked"),
            FetchOutcome::Transient { retry_after } => retry(&item, now, retry_after, "transient"),
            FetchOutcome::Rejected(rejection) => Record::Failed {
                attempts: FETCH_TRIES.max(item.attempts),
                next_at: None,
                error: rejection_code(rejection),
            },
            FetchOutcome::BreakerOpen => {
                return Ok(
                    if matches!(cdn.breaker_state(group), BreakerState::Open { .. }) {
                        Done::HandOff(item.platform)
                    } else {
                        Done::Deferred
                    },
                );
            }
        };
        drop(reservation);
        self.record(&item, record).await?;
        Ok(Done::Changed(item.key))
    }

    /// Makes the master of fetched bytes and stores it.
    async fn store(
        &self,
        item: Item,
        staged: StagedObject,
        reservation: Reservation,
    ) -> Result<Done, JobError> {
        let target = Target::from(&item);
        let prepared = {
            let _encode = self
                .archive()
                .inner
                .encodes
                .acquire()
                .await
                .map_err(|_| JobError::transient(codes::INTERNAL))?;
            let (media, target) = (self.media.clone(), target.clone());
            ImagePool::shared()
                .run(move || store::prepare(&media, staged, &target))
                .await
        };
        self.ctx.heartbeat();
        let prepared = match prepared {
            Ok(Ok(prepared)) => prepared,
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "archive: cannot stage a master");
                drop(reservation);
                let now = self.now();
                self.record(&item, retry(&item, now, None, "transient"))
                    .await?;
                return Ok(Done::Changed(item.key));
            }
            Err(_) => {
                tracing::warn!("archive: an image job panicked");
                drop(reservation);
                self.record(
                    &item,
                    Record::Failed {
                        attempts: FETCH_TRIES.max(item.attempts),
                        next_at: None,
                        error: "undecodable",
                    },
                )
                .await?;
                return Ok(Done::Changed(item.key));
            }
        };
        let (media, now, modes) = (self.media.clone(), self.now(), modes(self.state()));
        let committed = self
            .ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<Committed, RepoError> {
                    let policy = ArchivePolicy::read(tx, modes)?;
                    let cx = StoreContext {
                        media: &media,
                        origin: Origin::Server,
                        policy: &policy,
                        now,
                    };
                    store::commit(tx, &cx, &target, prepared, reservation)
                })
                .map_err(JobError::from)
            })
            .await?;
        Ok(match committed {
            Committed::Stored { cover, .. } => Done::Stored {
                key: item.key,
                platform: item.platform,
                imported_at: item.imported_at,
                cover,
            },
            Committed::Unwanted => Done::Nothing,
        })
    }

    /// Links the cover to the object slide 0 already stores.
    async fn link_existing(&self, item: Item, object_id: i64) -> Result<Done, JobError> {
        let object = self
            .ctx
            .user_db(move |db| {
                db.read(|conn| store::existing_object(conn, object_id))
                    .map_err(JobError::from)
            })
            .await?;
        let Some(object) = object else {
            return Ok(Done::Nothing);
        };
        let rendered = {
            let _encode = self
                .archive()
                .inner
                .encodes
                .acquire()
                .await
                .map_err(|_| JobError::transient(codes::INTERNAL))?;
            let media = self.media.clone();
            ImagePool::shared()
                .run(move || store::render_existing(&media, &object))
                .await
                .ok()
                .flatten()
        };
        let (media, now, modes) = (self.media.clone(), self.now(), modes(self.state()));
        let post_id = item.post_id;
        let linked = self
            .ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<bool, RepoError> {
                    let policy = ArchivePolicy::read(tx, modes)?;
                    store::link_existing(
                        tx,
                        &media,
                        post_id,
                        &object,
                        rendered.as_ref(),
                        &policy,
                        now,
                    )
                })
                .map_err(JobError::from)
            })
            .await?;
        Ok(if linked {
            Done::Stored {
                key: item.key,
                platform: item.platform,
                imported_at: item.imported_at,
                cover: true,
            }
        } else {
            Done::Nothing
        })
    }

    /// Writes how an item's fetch ended, and derives its post's state.
    async fn record(&self, item: &Item, record: Record) -> Result<(), JobError> {
        let (now, modes) = (self.now(), modes(self.state()));
        let (post_id, fetch_row, url_column) = (item.post_id, item.fetch_row, item.url_column);
        let cover = item.slot == Slot::Cover;
        self.ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<(), RepoError> {
                    match record {
                        Record::Expired => match url_column {
                            UrlColumn::Slide(position) if !cover => tx.execute(
                                "UPDATE post_media
                                 SET source_url_expires_at = min(coalesce(source_url_expires_at, ?3), ?3)
                                 WHERE post_id = ?1 AND position = ?2",
                                params![post_id, position, now],
                            )?,
                            // A cover's URLs (slide 0's and the post's) carry
                            // the signature of one capture: both expired, as
                            // the rule must see it, so neither is fetched in
                            // vain.
                            UrlColumn::Slide(_) | UrlColumn::Cover => {
                                tx.execute(
                                    "UPDATE post_media
                                     SET source_url_expires_at =
                                         min(coalesce(source_url_expires_at, ?2), ?2)
                                     WHERE post_id = ?1 AND position = 0
                                       AND source_url IS NOT NULL",
                                    params![post_id, now],
                                )?;
                                tx.execute(
                                    "UPDATE posts
                                     SET cover_url_expires_at = min(coalesce(cover_url_expires_at, ?2), ?2)
                                     WHERE id = ?1",
                                    params![post_id, now],
                                )?
                            }
                        },
                        Record::Failed {
                            attempts,
                            next_at,
                            error,
                        } => match fetch_row {
                            FetchRow::Slide(position) => tx.execute(
                                "UPDATE post_media
                                 SET fetch_attempts = ?3, fetch_next_at = ?4, fetch_error = ?5
                                 WHERE post_id = ?1 AND position = ?2",
                                params![post_id, position, attempts, next_at, error],
                            )?,
                            FetchRow::Post => tx.execute(
                                "UPDATE posts
                                 SET cover_fetch_attempts = ?2, cover_fetch_next_at = ?3,
                                     cover_fetch_error = ?4
                                 WHERE id = ?1",
                                params![post_id, attempts, next_at, error],
                            )?,
                        },
                        Record::LinkOnly => {
                            tx.execute(
                                "UPDATE posts SET archive_state = 'link_only' WHERE id = ?1",
                                [post_id],
                            )?;
                            return Ok(());
                        }
                    };
                    let policy = ArchivePolicy::read(tx, modes)?;
                    refresh_states(tx, Scope::Posts(&[post_id]), &policy, now)?;
                    Ok(())
                })
                .map_err(JobError::from)
            })
            .await
    }

    /// Derives the states of the posts `ids` again; returns the keys of
    /// those that changed.
    async fn rederive(&self, ids: Vec<i64>) -> Result<Vec<String>, JobError> {
        let (now, modes) = (self.now(), modes(self.state()));
        self.ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<Vec<String>, RepoError> {
                    let before = keyed_states(tx, &ids)?;
                    let policy = ArchivePolicy::read(tx, modes)?;
                    refresh_states(tx, Scope::Posts(&ids), &policy, now)?;
                    let after = keyed_states(tx, &ids)?;
                    Ok(after
                        .into_iter()
                        .filter(|(key, state)| before.get(key) != Some(state))
                        .map(|(key, _)| key)
                        .collect())
                })
                .map_err(JobError::from)
            })
            .await
    }

    /// Hands `platforms` over to the extension in this user's library: their
    /// breakers are open. Returns the keys of the posts that changed.
    async fn hand_off(&self, platforms: Vec<Platform>) -> Result<Vec<String>, JobError> {
        let (now, modes) = (self.now(), modes(self.state()));
        self.ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<Vec<String>, RepoError> {
                    let before = states_of(tx, &platforms)?;
                    let policy = ArchivePolicy::read(tx, modes)?;
                    for platform in &platforms {
                        refresh_states(tx, Scope::Platform(*platform), &policy, now)?;
                    }
                    let after = states_of(tx, &platforms)?;
                    Ok(after
                        .into_iter()
                        .filter(|(key, state)| before.get(key) != Some(state))
                        .map(|(key, _)| key)
                        .collect())
                })
                .map_err(JobError::from)
            })
            .await
    }

    fn record_latency(&self, platform: Platform, imported_at: i64) {
        let age_ms = self.now().saturating_sub(imported_at);
        if (0..=LATENCY_MAX_AGE_MS).contains(&age_ms) {
            metrics::histogram!(ARCHIVE_COVER_LATENCY_SECONDS, "platform" => platform.as_str())
                .record(age_ms as f64 / 1000.0);
        }
    }
}

/// The archive state of the posts `ids`, by key.
fn keyed_states(
    conn: &rusqlite::Connection,
    ids: &[i64],
) -> Result<HashMap<String, String>, RepoError> {
    let mut out = HashMap::new();
    let mut statement =
        conn.prepare_cached("SELECT key, archive_state FROM posts WHERE id = ?1")?;
    for id in ids {
        let mut rows = statement.query([id])?;
        while let Some(row) = rows.next()? {
            out.insert(row.get(0)?, row.get(1)?);
        }
    }
    Ok(out)
}

/// The archive state of the posts of `platforms`, by key.
fn states_of(
    conn: &rusqlite::Connection,
    platforms: &[Platform],
) -> Result<HashMap<String, String>, RepoError> {
    let mut out = HashMap::new();
    let mut statement =
        conn.prepare_cached("SELECT key, archive_state FROM posts WHERE platform = ?1")?;
    for platform in platforms {
        let mut rows = statement.query([platform.as_str()])?;
        while let Some(row) = rows.next()? {
            out.insert(row.get(0)?, row.get(1)?);
        }
    }
    Ok(out)
}

/// The record of a failed try of `item` at `now`: the next one after the
/// backoff (or `retry_after`, when longer), or none after [`FETCH_TRIES`].
fn retry(item: &Item, now: i64, retry_after: Option<Duration>, error: &'static str) -> Record {
    let attempts = item.attempts.saturating_add(1);
    if attempts >= FETCH_TRIES {
        return Record::Failed {
            attempts,
            next_at: None,
            error,
        };
    }
    let failures = u32::try_from(attempts).unwrap_or(u32::MAX);
    let slot = match item.slot {
        Slot::Cover => -1,
        Slot::Slide(position) => position,
    };
    let delay = ITEM_BACKOFF.jittered(failures, mix(item.post_id, slot, attempts));
    let delay = retry_after.map_or(delay, |after| delay.max(after));
    let delay_ms = i64::try_from(delay.as_millis()).unwrap_or(i64::MAX);
    Record::Failed {
        attempts,
        next_at: Some(now.saturating_add(delay_ms)),
        error,
    }
}

/// A well-spread number from an item's identity, for its jitter.
fn mix(post_id: i64, slot: i64, attempts: i64) -> u64 {
    let mut x = (post_id as u64)
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add((slot as u64).wrapping_mul(0xBF58_476D_1CE4_E5B9))
        .wrapping_add(attempts as u64);
    x ^= x >> 30;
    x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^ (x >> 31)
}

/// `post_media.fetch_error` of an item the server can never store.
const fn rejection_code(rejection: Rejection) -> &'static str {
    match rejection {
        Rejection::Url => "unsupported_url",
        Rejection::Refused => "refused",
        Rejection::TooLarge => "too_large",
        Rejection::NotImage => "not_image",
        Rejection::Redirects => "redirects",
        Rejection::Status(_) => "status",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::select::platform_index;
    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        archive: ArchiveArgs,
    }

    #[test]
    fn modes_default_to_the_server_and_parse() {
        let config = ArchiveConfig::from_args(&Cli::parse_from(["shelfy"]).archive);
        assert_eq!(config.modes, ArchiveModes::default());
        assert_eq!(config.modes.pinterest, ArchiveMode::Server, "L17");
        let cli = Cli::parse_from([
            "shelfy",
            "--archive-mode-instagram",
            "client",
            "--archive-mode-x",
            " Auto ",
        ]);
        let config = ArchiveConfig::from_args(&cli.archive);
        assert_eq!(config.modes.instagram, ArchiveMode::Client);
        assert_eq!(config.modes.twitter, ArchiveMode::Auto);
        assert!(Cli::try_parse_from(["shelfy", "--archive-mode-x", "browser"]).is_err());
    }

    #[test]
    fn platforms_map_to_their_cdn_groups() {
        for platform in [Platform::Instagram, Platform::Twitter, Platform::Pinterest] {
            let group = group_of(platform).unwrap();
            assert!(group.is_cdn());
            assert_eq!(platform_of(group), Some(platform));
            assert!(platform_index(platform).is_some());
        }
        assert_eq!(group_of(Platform::Web), None);
        assert_eq!(platform_of(HostGroup::XWeb), None);
    }

    fn item(attempts: i64) -> Item {
        Item {
            post_id: 7,
            key: "x_7".into(),
            platform: Platform::Twitter,
            imported_at: 0,
            slot: Slot::Slide(1),
            url: "https://pbs.twimg.com/media/a.jpg".into(),
            url_column: UrlColumn::Slide(1),
            expires_at: None,
            fetch_row: FetchRow::Slide(1),
            attempts,
            next_at: None,
            poster: false,
            cover: false,
            grid: true,
            existing: None,
        }
    }

    #[test]
    fn tries_back_off_then_fail() {
        const NOW: i64 = 1_000_000;
        for (attempts, base_s) in [(0_i64, 30_i64), (1, 60), (2, 120), (3, 240)] {
            let Record::Failed {
                attempts: next,
                next_at: Some(at),
                error,
            } = retry(&item(attempts), NOW, None, "transient")
            else {
                panic!("a try is left after {attempts}");
            };
            assert_eq!((next, error), (attempts + 1, "transient"));
            let delay = at - NOW;
            assert!(
                (base_s * 500..=base_s * 1000).contains(&delay),
                "{attempts}: {delay}"
            );
        }
        assert!(matches!(
            retry(&item(FETCH_TRIES - 1), NOW, None, "blocked"),
            Record::Failed {
                attempts: FETCH_TRIES,
                next_at: None,
                error: "blocked"
            }
        ));
        // A longer Retry-After wins.
        let Record::Failed {
            next_at: Some(at), ..
        } = retry(&item(0), NOW, Some(Duration::from_secs(600)), "blocked")
        else {
            panic!("a try is left");
        };
        assert_eq!(at, NOW + 600_000);
    }
}
