//! Quotas, the media budget and usage accounting (plan §2.13 "Quota and GC",
//! §3.1; P4-07).
//!
//! **Usage** is what a user's library takes on disk: the bytes of its
//! `media_objects` rows plus its database file (§2.13), stored on the user's
//! row of the control database (`users.usage_bytes` = `usage_media_bytes` +
//! `usage_db_bytes`), where `GET /me/usage` reads it. Three things move it:
//!
//! 1. **Commits** of reservations (below): each new object row adds its
//!    bytes as it is recorded, so `GET /me/usage` shows it at once.
//! 2. **Releases** ([`release`]): when object rows are *deleted* (the GC,
//!    P4-12, after 24 h unreferenced; account resets), their bytes come off,
//!    never below 0. Removing a reference (trash, "remove stored copy",
//!    P4-17) changes nothing yet: the object still takes its bytes until
//!    the GC deletes it, and that deletion releases them.
//! 3. **Counts** ([`crate::jobs::usage`], `usage.recompute`): the whole
//!    library counted again, nightly and after installs, purges and resets.
//!    The count is the truth: it corrects any drift of 1 and 2, and is the
//!    only thing that measures the database file.
//!
//! **Limits.** A store is refused when it would pass either limit:
//!
//! | Limit | Refusal | Applies when |
//! |---|---|---|
//! | the user's `users.quota_bytes`; 0 means unlimited (the owner, E4) | 403 `quota_exceeded` | usage + the user's live reservations + the new bytes > the quota |
//! | `SHELFY_MEDIA_BUDGET_GB` (default 30 GiB; 0 turns it off), for every user together | 507 `storage_full` | the last disk sample of the `users` area + what was committed since + every live reservation + the new bytes > the budget |
//!
//! The disk sample comes from the metrics task, every 5 minutes
//! ([`crate::telemetry::metrics::sample_disk`]); before the first one, the
//! first reservation measures the area itself. What no reservation covers
//! (renditions, database growth) and what the GC frees count from the next
//! sample. Refusals are permanent job errors (`JobError::from`). Over quota, the user gets a `quota.exceeded`
//! notification (kind `quota`, params `quotaBytes` and `usedBytes`, target
//! `/settings/storage`) at most once in 24 hours; the library remembers the
//! last one, so a restart sends no second one.
//!
//! **Reservations** live in memory: there is one API process (§2.2). A
//! store reserves before it writes and commits afterwards (P4 lane rule 6):
//!
//! ```ignore
//! // 1. Before fetching or moving any byte: the most it may store.
//! let reservation = quota::reserve(&state, user_id, max_bytes).await?;
//! // 2. Stage the bytes (download, move from the cache, read an upload…).
//! // 3. In the write transaction that records the objects, last of all:
//! db.write(move |tx| {
//!     let added = quota::new_bytes(tx, &[(staged.digest(), staged.size())])?;
//!     let (id, _) = refs::publish_and_record(tx, &media, staged, &renditions, &meta, now)?;
//!     // … link the object to its post …
//!     reservation.commit(added)?; // the bytes the rows added; 0 for a known object
//!     Ok(id)
//! })?;
//! ```
//!
//! - **Commit inside the library transaction, last.** The commit writes the
//!   control database from inside the library's write transaction, the one
//!   lock order of this module: library, then control. A count takes the
//!   library's write lock too, so it sees either both the rows and the
//!   commit or neither, and never double-counts or loses a commit. A failed
//!   commit fails the transaction: nothing is stored or counted.
//! - **Commit what the rows add,** [`new_bytes`] before recording: an object
//!   the library already has (the CAS dedupes by SHA-256) adds nothing.
//!   Commit even more than reserved if that is what was stored; reserve the
//!   most you may store (the cap you enforce) so the check means something.
//! - **Dropping a reservation releases it**: a failed or cancelled store
//!   leaves nothing behind. [`Reservation::commit_part`] commits a chunk and
//!   keeps the rest reserved, for stores made of several transactions.
//! - A reservation is `Send` and `'static`: it can wait in a map across
//!   requests (tus uploads) and move into a blocking closure.
//!
//! **The hooks of the other tasks:**
//!
//! | Writer | Reserves | Commits | On refusal |
//! |---|---|---|---|
//! | P2-10 archive drain (`archive.drain`) | per object, before fetching: the cap it enforces (`IngestLimits::ARCHIVE_IMAGE`, 15 MiB) or the response's `Content-Length` once the headers are in | in the transaction that records and links the object | the item's post becomes `link_only` and the drain goes on; [`is_refused`] tells a refusal from a failure, which stays transient |
//! | P4-08 tus uploads (web purposes) | at creation, `Upload-Length`, kept in memory under the upload id until the upload is consumed or expires (or only [`check`] at creation, and reserve at consumption) | — (the consumer does) | the creation answers the refusal |
//! | P4-18 bookmarks | the files' total before publishing (or the reservations its uploads hold) | in the transaction that publishes the objects and creates the post | the request answers the refusal |
//! | P4-14 capture | 80 MiB before dispatch | with the real bytes in the ingest transaction; [`bump_daily`] `captures` + 1 | the job fails with the code |
//! | P4-16 keep a video | the file's size, before moving it from the cache into the CAS | in the transaction that records and links it; bulk keep checks 85 % of [`Quotas::budget`] first | the request or the job answers the refusal |
//! | P4-19 import v2 | the objects the user lacks, before anything changes | per chunk, [`Reservation::commit_part`] | the job fails, nothing changed |
//! | the migration install (`migrate`) | after a count: the bundle's objects the library has no row for, and its database | per chunk in a merge; a count after a replace | the job fails with the code |
//!
//! Releases: the GC (P4-12) calls [`release`] with the bytes of the rows
//! `refs::collect_garbage` deleted, in the same library transaction, then
//! enqueues a count. Exports do not count (PG8); kept videos and bookmarks
//! do. Daily counters: [`bump_daily`] and [`crate::control::usage_daily`].

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Map, json};
use shelfy_core::db::ControlDb;
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::{self, NewNotification};
use shelfy_media::Digest;

use crate::control::usage_daily::{self, Field};
use crate::control::users;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::Notification;
use crate::jobs::Clock;
use crate::state::{AppState, blocking};
use crate::telemetry::metrics::{USERS_AREA, area_bytes};

/// Bytes in a GiB, the unit of `SHELFY_MEDIA_BUDGET_GB` and of `admin user
/// limits --quota-gb`.
pub const GIB: u64 = 1 << 30;
/// Default of `SHELFY_MEDIA_BUDGET_GB` (§3.1: 30 GB for `users/`).
pub const DEFAULT_MEDIA_BUDGET_GB: u64 = 30;
/// `notifications.kind` of a refusal over quota.
pub const NOTIFICATION_KIND: &str = "quota";
/// `notifications.code` of a refusal over quota.
pub const NOTIFICATION_CODE: &str = "quota.exceeded";
/// Where the notification leads: the Storage section of Settings.
pub const NOTIFICATION_TARGET: &str = "/settings/storage";
/// At most one `quota.exceeded` notification per user in this long.
pub const NOTICE_EVERY_MS: i64 = 24 * 3_600_000;
/// A full media budget is logged at most this often.
const FULL_LOG_EVERY_MS: i64 = 60_000;
/// Reads of a user's usage that may be overtaken by a commit before
/// [`Quotas::try_reserve`] gives up (503): a commit lands in the
/// microseconds between a read and the check about once in a hundred tries.
const MAX_READS: usize = 100;

/// The settings of this module.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QuotaConfig {
    /// `SHELFY_MEDIA_BUDGET_GB` in bytes; 0 means no budget.
    pub media_budget_bytes: u64,
}

impl QuotaConfig {
    /// The settings for a budget of `gib` GiB; `None` when it does not fit
    /// in bytes.
    #[must_use]
    pub fn from_gib(gib: u64) -> Option<Self> {
        gib.checked_mul(GIB)
            .map(|media_budget_bytes| Self { media_budget_bytes })
    }
}

impl Default for QuotaConfig {
    fn default() -> Self {
        Self {
            media_budget_bytes: DEFAULT_MEDIA_BUDGET_GB * GIB,
        }
    }
}

/// Why a reservation was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// `used + reserved + bytes > quota`, for this user.
    QuotaExceeded {
        /// The user's quota.
        quota: u64,
        /// The user's usage.
        used: u64,
        /// The user's live reservations.
        reserved: u64,
        /// The bytes asked for.
        bytes: u64,
    },
    /// `used + bytes > budget`, over every user.
    StorageFull {
        /// `SHELFY_MEDIA_BUDGET_GB` in bytes.
        budget: u64,
        /// The `users` area: the last sample, the commits since and every
        /// live reservation.
        used: u64,
        /// The bytes asked for.
        bytes: u64,
    },
}

impl From<Refusal> for ApiError {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::QuotaExceeded {
                quota,
                used,
                reserved,
                bytes,
            } => Self::new(ErrorCode::QuotaExceeded).with_detail(format!(
                "{bytes} bytes do not fit: {used} used and {reserved} reserved of a quota of \
                 {quota} bytes"
            )),
            Refusal::StorageFull {
                budget,
                used,
                bytes,
            } => Self::new(ErrorCode::StorageFull).with_detail(format!(
                "{bytes} bytes do not fit: {used} of the media budget of {budget} bytes are used"
            )),
        }
    }
}

/// Whether `err` is a refusal of [`reserve`] (`quota_exceeded` or
/// `storage_full`), rather than a failure to check: the archive drain leaves
/// a refused item `link_only` and retries a failure.
#[must_use]
pub fn is_refused(err: &ApiError) -> bool {
    matches!(
        err.code(),
        ErrorCode::QuotaExceeded | ErrorCode::StorageFull
    )
}

/// The media budget and what takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// `SHELFY_MEDIA_BUDGET_GB` in bytes; 0 means no budget.
    pub limit_bytes: u64,
    /// The `users` area at the last sample, plus the bytes committed since
    /// and every live reservation.
    pub used_bytes: u64,
}

/// The position of the committed bytes when a disk sample starts (see
/// [`Quotas::sample_mark`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleMark(u64);

/// The reservations and the media budget of the process. Cheap to clone:
/// [`AppState::quota`] holds the server's.
#[derive(Clone)]
pub struct Quotas {
    inner: Arc<Inner>,
}

struct Inner {
    config: QuotaConfig,
    control: Arc<ControlDb>,
    clock: Clock,
    /// The data directory, for the first sample.
    data_root: PathBuf,
    ledger: Mutex<Ledger>,
    /// One first sample at a time.
    first_sample: tokio::sync::Mutex<()>,
}

#[derive(Default)]
struct Ledger {
    users: HashMap<String, UserEntry>,
    /// Every live reservation.
    reserved: u64,
    /// Bytes committed since the process started.
    committed: u64,
    /// The `users` area at the last sample.
    sample: Option<u64>,
    /// `committed` when that sample started.
    sample_mark: u64,
    /// When `storage_full` was last logged.
    full_logged_at: Option<i64>,
}

#[derive(Default)]
struct UserEntry {
    /// The user's live reservations.
    reserved: u64,
    /// Moves on every change of the user's usage row: a reservation that
    /// read the row before a change reads it again.
    seq: u64,
    /// When the user last got (or was found to have) a `quota.exceeded`
    /// notification.
    noticed_at: Option<i64>,
}

/// A reservation to decide: the user's row as read, and what is asked.
struct Check {
    /// [`UserEntry::seq`] when the row was read.
    seq: u64,
    /// The user's quota; 0 means unlimited.
    quota: u64,
    /// The user's usage.
    used: u64,
    /// The bytes asked for.
    bytes: u64,
    /// The media budget; 0 means none.
    budget: u64,
    /// Now, unix ms.
    now: i64,
}

/// What [`Ledger::decide`] did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Decision {
    /// The row changed since it was read: read it again.
    Stale,
    /// Refused; nothing is held.
    Refused(Refusal),
    /// The bytes are held for the user.
    Held,
}

impl Ledger {
    fn user(&mut self, user_id: &str) -> &mut UserEntry {
        self.users.entry(user_id.to_owned()).or_default()
    }

    /// Holds `check.bytes` for `user_id` if they fit the quota and the
    /// budget, and the row read is still the current one.
    fn decide(&mut self, user_id: &str, check: &Check) -> Decision {
        let entry = self.user(user_id);
        if entry.seq != check.seq {
            return Decision::Stale;
        }
        let reserved = entry.reserved;
        let bytes = check.bytes;
        if check.quota > 0
            && check.used.saturating_add(reserved).saturating_add(bytes) > check.quota
        {
            return Decision::Refused(Refusal::QuotaExceeded {
                quota: check.quota,
                used: check.used,
                reserved,
                bytes,
            });
        }
        let used = self.budget_used();
        if check.budget > 0 && used.saturating_add(bytes) > check.budget {
            if self
                .full_logged_at
                .is_none_or(|at| check.now.saturating_sub(at) >= FULL_LOG_EVERY_MS)
            {
                self.full_logged_at = Some(check.now);
                tracing::warn!(
                    budget_bytes = check.budget,
                    used_bytes = used,
                    "the media budget is full: stores are refused with storage_full"
                );
            }
            return Decision::Refused(Refusal::StorageFull {
                budget: check.budget,
                used,
                bytes,
            });
        }
        let entry = self.user(user_id);
        entry.reserved = entry.reserved.saturating_add(bytes);
        self.reserved = self.reserved.saturating_add(bytes);
        Decision::Held
    }

    fn budget_used(&self) -> u64 {
        self.sample
            .unwrap_or(0)
            .saturating_add(self.committed.saturating_sub(self.sample_mark))
            .saturating_add(self.reserved)
    }

    fn unreserve(&mut self, user_id: &str, bytes: u64) {
        if bytes == 0 {
            return;
        }
        let entry = self.user(user_id);
        entry.reserved = entry.reserved.saturating_sub(bytes);
        self.reserved = self.reserved.saturating_sub(bytes);
    }
}

impl Quotas {
    /// The ledger of a process whose control database is `control`, with
    /// the job system's `clock` and the data directory `data_root`.
    #[must_use]
    pub fn new(
        config: QuotaConfig,
        control: Arc<ControlDb>,
        clock: Clock,
        data_root: PathBuf,
    ) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                control,
                clock,
                data_root,
                ledger: Mutex::new(Ledger::default()),
                first_sample: tokio::sync::Mutex::new(()),
            }),
        }
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> QuotaConfig {
        self.inner.config
    }

    fn lock(&self) -> MutexGuard<'_, Ledger> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn now_ms(&self) -> i64 {
        self.inner.clock.now_ms()
    }

    /// Reserves `bytes` for `user_id`, or says why not. Unlike [`reserve`],
    /// sends no notification.
    ///
    /// # Errors
    ///
    /// 404 `not_found` when there is no such user; 503 `unavailable` when
    /// the usage kept changing under the check; database errors. A refusal
    /// is not an error here: it is the inner `Err`.
    pub async fn try_reserve(
        &self,
        user_id: &str,
        bytes: u64,
    ) -> Result<Result<Reservation, Refusal>, ApiError> {
        self.ensure_sample().await;
        let budget = self.inner.config.media_budget_bytes;
        for _ in 0..MAX_READS {
            let seq = self.lock().user(user_id).seq;
            let control = Arc::clone(&self.inner.control);
            let id = user_id.to_owned();
            let usage = blocking(move || control.read(|conn| users::usage(conn, &id)))
                .await?
                .ok_or_else(|| ApiError::not_found().with_detail("no such user"))?;
            let check = Check {
                seq,
                quota: u64::try_from(usage.quota_bytes).unwrap_or(0),
                used: u64::try_from(usage.used_bytes).unwrap_or(0),
                bytes,
                budget,
                now: self.now_ms(),
            };
            match self.lock().decide(user_id, &check) {
                // A commit, release or count changed the row since it was
                // read: read it again.
                Decision::Stale => {}
                Decision::Refused(refusal) => return Ok(Err(refusal)),
                Decision::Held => {
                    return Ok(Ok(Reservation {
                        quotas: self.clone(),
                        user_id: user_id.to_owned(),
                        remaining: bytes,
                    }));
                }
            }
        }
        Err(ApiError::new(ErrorCode::Unavailable)
            .with_retry_after(1)
            .with_detail("the usage kept changing during the quota check"))
    }

    /// Takes `bytes` of deleted objects off `user_id`'s usage, never below
    /// 0. Call it in the library write transaction that deleted their rows
    /// (the lock order of the module docs). Blocking.
    ///
    /// # Errors
    ///
    /// The control database failed.
    pub fn release(&self, user_id: &str, bytes: u64) -> Result<(), RepoError> {
        if bytes == 0 {
            return Ok(());
        }
        let bytes = i64::try_from(bytes).unwrap_or(i64::MAX);
        self.inner
            .control
            .write(|tx| users::remove_media_usage(tx, user_id, bytes))?;
        self.lock().user(user_id).seq += 1;
        Ok(())
    }

    /// Records a count of `user_id`'s library (`usage.recompute`): `media`
    /// bytes of objects and a `db`-byte database file. Call it in a library
    /// write transaction that holds the count (see [`crate::jobs::usage`]).
    /// Returns the drift of the media bytes: the count minus the usage the
    /// commits and releases kept. Blocking.
    ///
    /// # Errors
    ///
    /// The control database failed.
    pub fn record_count(&self, user_id: &str, media: i64, db: i64) -> Result<i64, RepoError> {
        let now = self.now_ms();
        let drift = self.inner.control.write(|tx| -> Result<i64, RepoError> {
            let kept = users::usage(tx, user_id)?.map_or(0, |u| u.media_bytes);
            users::set_usage(tx, user_id, media, db, now)?;
            Ok(media.saturating_sub(kept))
        })?;
        self.lock().user(user_id).seq += 1;
        Ok(drift)
    }

    /// The live reservations of `user_id`, in bytes.
    #[must_use]
    pub fn reserved(&self, user_id: &str) -> u64 {
        self.lock().users.get(user_id).map_or(0, |e| e.reserved)
    }

    /// Every live reservation, in bytes.
    #[must_use]
    pub fn reserved_total(&self) -> u64 {
        self.lock().reserved
    }

    /// The media budget and what takes it now (P4-16's bulk keep refuses
    /// past 85 % of it). Measures the `users` area first if it was never
    /// sampled.
    pub async fn budget(&self) -> Budget {
        self.ensure_sample().await;
        Budget {
            limit_bytes: self.inner.config.media_budget_bytes,
            used_bytes: self.lock().budget_used(),
        }
    }

    /// Where the committed bytes stand: take it before a disk sample starts
    /// and give it to [`Quotas::record_users_sample`], so what is committed
    /// during the walk is counted until the next sample.
    #[must_use]
    pub fn sample_mark(&self) -> SampleMark {
        SampleMark(self.lock().committed)
    }

    /// Keeps a disk sample of the `users` area (`bytes`), started at `mark`;
    /// a sample that started before the kept one is dropped.
    pub fn record_users_sample(&self, bytes: u64, mark: SampleMark) {
        let mut ledger = self.lock();
        if ledger.sample.is_some() && mark.0 < ledger.sample_mark {
            return;
        }
        ledger.sample = Some(bytes);
        ledger.sample_mark = mark.0;
    }

    /// Measures the `users` area once when the budget is on and no sample
    /// was taken yet (the maintenance task takes the first one at start).
    async fn ensure_sample(&self) {
        if self.inner.config.media_budget_bytes == 0 || self.lock().sample.is_some() {
            return;
        }
        let _first = self.inner.first_sample.lock().await;
        if self.lock().sample.is_some() {
            return;
        }
        let mark = self.sample_mark();
        let root = self.inner.data_root.clone();
        match tokio::task::spawn_blocking(move || area_bytes(&root, USERS_AREA)).await {
            Ok(bytes) => self.record_users_sample(bytes, mark),
            Err(err) => tracing::warn!(error = %err, "measuring the users area failed"),
        }
    }

    /// Commits `actual` stored bytes for `user_id` and ends `part` bytes of
    /// its reservation.
    fn commit_bytes(&self, user_id: &str, part: u64, actual: u64) -> Result<(), RepoError> {
        if actual > 0 {
            let bytes = i64::try_from(actual).map_err(|_| RepoError::Invalid {
                field: "bytes",
                reason: "too large",
            })?;
            let now = self.now_ms();
            self.inner.control.write(|tx| -> Result<(), RepoError> {
                users::add_media_usage(tx, user_id, bytes)?;
                usage_daily::bump(tx, user_id, Field::BytesIn, bytes, now)
            })?;
        }
        let mut ledger = self.lock();
        ledger.unreserve(user_id, part);
        ledger.committed = ledger.committed.saturating_add(actual);
        ledger.user(user_id).seq += 1;
        Ok(())
    }

    /// Whether `user_id` may get a `quota.exceeded` notification at `now`.
    fn may_notice(&self, user_id: &str, now: i64) -> bool {
        self.lock()
            .user(user_id)
            .noticed_at
            .is_none_or(|at| now.saturating_sub(at) >= NOTICE_EVERY_MS)
    }

    fn noticed(&self, user_id: &str, at: i64) {
        self.lock().user(user_id).noticed_at = Some(at);
    }
}

/// Bytes held for a store until it commits them; dropping it releases them.
/// See the module docs.
#[must_use = "a reservation is released when dropped: commit it after storing"]
pub struct Reservation {
    quotas: Quotas,
    user_id: String,
    remaining: u64,
}

impl Reservation {
    /// The user it holds bytes for.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The bytes it still holds.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.remaining
    }

    /// Adds `actual` stored bytes to the user's usage (and to today's
    /// `bytes_in`), and holds that much less, down to 0. Call it last in the
    /// library write transaction that recorded the objects, with what
    /// [`new_bytes`] found. Blocking.
    ///
    /// # Errors
    ///
    /// The control database failed: return the error, so the library
    /// transaction rolls back too. The reservation is unchanged.
    pub fn commit_part(&mut self, actual: u64) -> Result<(), RepoError> {
        let part = actual.min(self.remaining);
        if actual > self.remaining {
            tracing::debug!(
                reserved = self.remaining,
                actual,
                "a store committed more than it reserved"
            );
        }
        self.quotas.commit_bytes(&self.user_id, part, actual)?;
        self.remaining -= part;
        Ok(())
    }

    /// [`Reservation::commit_part`], then releases what is left.
    ///
    /// # Errors
    ///
    /// Like [`Reservation::commit_part`]; the whole reservation is released.
    pub fn commit(mut self, actual: u64) -> Result<(), RepoError> {
        self.commit_part(actual)
    }

    /// Releases the reservation without storing anything; the same as
    /// dropping it.
    pub fn release(self) {}
}

impl Drop for Reservation {
    fn drop(&mut self) {
        let remaining = std::mem::take(&mut self.remaining);
        self.quotas.lock().unreserve(&self.user_id, remaining);
    }
}

impl fmt::Debug for Reservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Reservation")
            .field("user_id", &self.user_id)
            .field("bytes", &self.remaining)
            .finish_non_exhaustive()
    }
}

/// Reserves `bytes` for a store of `user_id` (see the module docs). Over
/// quota, the user gets a `quota.exceeded` notification, at most once in 24
/// hours.
///
/// # Errors
///
/// 403 `quota_exceeded` or 507 `storage_full` ([`is_refused`]); 404 for an
/// unknown user; 503 or database errors when the check could not run.
pub async fn reserve(state: &AppState, user_id: &str, bytes: u64) -> Result<Reservation, ApiError> {
    match state.quota().try_reserve(user_id, bytes).await? {
        Ok(reservation) => Ok(reservation),
        Err(refusal) => {
            if let Refusal::QuotaExceeded { quota, used, .. } = refusal {
                notify_over_quota(state, user_id, used, quota).await;
            }
            Err(refusal.into())
        }
    }
}

/// Whether `bytes` would fit now, without holding them: a store that
/// reserves later (P4-08 may check at an upload's creation).
///
/// # Errors
///
/// Like [`reserve`].
pub async fn check(state: &AppState, user_id: &str, bytes: u64) -> Result<(), ApiError> {
    reserve(state, user_id, bytes)
        .await
        .map(Reservation::release)
}

/// Takes `bytes` of deleted objects off `user_id`'s usage, never below 0;
/// see [`Quotas::release`]. Blocking: call it in the library write
/// transaction that deleted the rows.
///
/// # Errors
///
/// The control database failed.
pub fn release(state: &AppState, user_id: &str, bytes: u64) -> Result<(), RepoError> {
    state.quota().release(user_id, bytes)
}

/// The bytes that recording `objects` (digest and size) adds to the
/// library's usage: those whose content has no `media_objects` row yet, each
/// digest once. Call it in the write transaction that records them, before
/// recording them.
///
/// # Errors
///
/// The query failed.
pub fn new_bytes(conn: &Connection, objects: &[(Digest, u64)]) -> Result<u64, RepoError> {
    let mut seen = HashSet::with_capacity(objects.len());
    let mut known = conn.prepare_cached("SELECT 1 FROM media_objects WHERE sha256 = ?1")?;
    let mut total = 0_u64;
    for (digest, size) in objects {
        if seen.insert(*digest) && !known.exists([&digest.as_bytes()[..]])? {
            total = total.saturating_add(*size);
        }
    }
    Ok(total)
}

/// Adds `n` to today's `field` of `user_id` (UTC, the job system's clock);
/// see [`crate::control::usage_daily`].
///
/// # Errors
///
/// A negative `n`; the control database failed.
pub async fn bump_daily(
    state: &AppState,
    user_id: &str,
    field: Field,
    n: i64,
) -> Result<(), ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user_id.to_owned();
    let now = state.jobs().clock().now_ms();
    blocking(move || control.write(|tx| usage_daily::bump(tx, &user_id, field, n, now))).await
}

/// Sends `user_id` a `quota.exceeded` notification unless one went out in
/// the last 24 hours. Best effort: a failure is logged and the next refusal
/// tries again.
async fn notify_over_quota(state: &AppState, user_id: &str, used: u64, quota: u64) {
    let quotas = state.quota();
    let now = quotas.now_ms();
    if !quotas.may_notice(user_id, now) {
        return;
    }
    let db = match state.user_db(user_id).await {
        Ok(db) => db,
        Err(err) => {
            tracing::debug!(error = %err, "cannot notify a refusal over quota");
            return;
        }
    };
    let mut params = Map::new();
    params.insert("quotaBytes".into(), json!(quota));
    params.insert("usedBytes".into(), json!(used));
    let new = NewNotification {
        kind: NOTIFICATION_KIND.to_owned(),
        code: NOTIFICATION_CODE.to_owned(),
        params,
        target: Some(NOTIFICATION_TARGET.to_owned()),
    };
    let written = blocking(move || {
        db.write(|tx| -> Result<_, RepoError> {
            let last: Option<i64> = tx
                .query_row(
                    "SELECT max(created_at) FROM notifications WHERE code = ?1",
                    params![NOTIFICATION_CODE],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            match last {
                // The library remembers one from before a restart.
                Some(at) if now.saturating_sub(at) < NOTICE_EVERY_MS => Ok((at, None)),
                _ => Ok((now, Some(notifications::create(tx, &new, now)?))),
            }
        })
    })
    .await;
    match written {
        Ok((at, created)) => {
            quotas.noticed(user_id, at);
            if let Some(created) = created {
                state
                    .events()
                    .notification(user_id, &Notification::from(created));
            }
        }
        Err(err) => tracing::warn!(error = %err, "cannot store the over-quota notification"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budgets_are_whole_gib() {
        assert_eq!(QuotaConfig::default().media_budget_bytes, 30 * GIB);
        assert_eq!(QuotaConfig::from_gib(0).unwrap().media_budget_bytes, 0);
        assert_eq!(
            QuotaConfig::from_gib(5).unwrap().media_budget_bytes,
            5 << 30
        );
        assert_eq!(QuotaConfig::from_gib(u64::MAX), None);
    }

    #[test]
    fn refusals_carry_their_codes_and_numbers() {
        let over = ApiError::from(Refusal::QuotaExceeded {
            quota: 1_000,
            used: 900,
            reserved: 50,
            bytes: 100,
        });
        assert_eq!(over.code(), ErrorCode::QuotaExceeded);
        assert!(is_refused(&over));
        assert_eq!(
            over.to_string(),
            "quota_exceeded: 100 bytes do not fit: 900 used and 50 reserved of a quota of 1000 \
             bytes"
        );
        let full = ApiError::from(Refusal::StorageFull {
            budget: 10,
            used: 9,
            bytes: 2,
        });
        assert_eq!(full.code(), ErrorCode::StorageFull);
        assert_eq!(full.status().as_u16(), 507);
        assert!(is_refused(&full));
        assert!(!is_refused(&ApiError::not_found()));
    }

    #[test]
    fn the_budget_counts_the_sample_the_commits_since_and_the_reservations() {
        let mut ledger = Ledger::default();
        assert_eq!(ledger.budget_used(), 0, "no sample yet");
        ledger.committed = 300;
        ledger.sample = Some(10_000);
        ledger.sample_mark = 100;
        ledger.reserved = 50;
        assert_eq!(ledger.budget_used(), 10_000 + 200 + 50);
        ledger.user("a").reserved = 50;
        ledger.unreserve("a", 80);
        assert_eq!(ledger.user("a").reserved, 0, "never below 0");
        assert_eq!(ledger.reserved, 0);
    }

    #[tokio::test]
    async fn the_newest_sample_wins() {
        let dir = tempfile::tempdir().unwrap();
        let control = ControlDb::open(
            dir.path().join("control.sqlite"),
            &shelfy_core::db::ControlDbConfig::default(),
        )
        .unwrap();
        let quotas = Quotas::new(
            QuotaConfig::default(),
            Arc::new(control),
            Clock::System,
            dir.path().to_path_buf(),
        );
        let early = quotas.sample_mark();
        quotas.lock().committed = 500;
        let late = quotas.sample_mark();
        quotas.record_users_sample(2_000, late);
        quotas.record_users_sample(1_000, early);
        assert_eq!(
            quotas.budget().await.used_bytes,
            2_000,
            "the older walk is dropped"
        );
        quotas.lock().committed = 800;
        assert_eq!(
            quotas.budget().await.used_bytes,
            2_300,
            "commits since the sample"
        );
    }

    fn check(seq: u64, quota: u64, used: u64, bytes: u64, budget: u64) -> Check {
        Check {
            seq,
            quota,
            used,
            bytes,
            budget,
            now: 0,
        }
    }

    #[test]
    fn a_check_on_a_row_that_changed_since_it_was_read_is_made_again() {
        let mut ledger = Ledger::default();
        let read_at = ledger.user("a").seq;
        // A commit lands between the read and the check: the row read is
        // stale, whatever it says.
        ledger.user("a").seq += 1;
        assert_eq!(
            ledger.decide("a", &check(read_at, 1_000, 0, 100, 0)),
            Decision::Stale
        );
        assert_eq!(ledger.user("a").reserved, 0, "nothing held");
        assert_eq!(
            ledger.decide("a", &check(read_at + 1, 1_000, 900, 100, 0)),
            Decision::Held
        );
        assert_eq!((ledger.user("a").reserved, ledger.reserved), (100, 100));
    }

    #[test]
    fn decisions_count_every_live_reservation() {
        let mut ledger = Ledger::default();
        // The quota: used + this user's reservations + the bytes.
        assert_eq!(
            ledger.decide("a", &check(0, 1_000, 500, 300, 0)),
            Decision::Held
        );
        assert_eq!(
            ledger.decide("a", &check(0, 1_000, 500, 201, 0)),
            Decision::Refused(Refusal::QuotaExceeded {
                quota: 1_000,
                used: 500,
                reserved: 300,
                bytes: 201,
            })
        );
        assert_eq!(
            ledger.decide("a", &check(0, 1_000, 500, 200, 0)),
            Decision::Held,
            "exactly the quota fits"
        );
        // Unlimited: only the budget, over every user.
        ledger.sample = Some(1_000);
        assert_eq!(
            ledger.decide("b", &check(0, 0, 0, 8_501, 10_000)),
            Decision::Refused(Refusal::StorageFull {
                budget: 10_000,
                used: 1_500,
                bytes: 8_501,
            }),
            "the sample and a's 500 reserved"
        );
        assert_eq!(
            ledger.decide("b", &check(0, 0, 0, 8_500, 10_000)),
            Decision::Held,
            "exactly the budget fits"
        );
        assert_eq!(
            ledger.decide("b", &check(0, 0, 0, 1, 10_000)),
            Decision::Refused(Refusal::StorageFull {
                budget: 10_000,
                used: 10_000,
                bytes: 1,
            })
        );
        assert_eq!(
            ledger.decide("b", &check(0, 0, 0, 7_500, 0)),
            Decision::Held,
            "no budget"
        );
    }
}
