//! What a worker gets and returns: [`JobContext`], [`Outcome`] and
//! [`JobError`]; and what the drain sweeper's check gets, [`SweepContext`].

use std::borrow::Cow;
use std::fmt;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde::de::DeserializeOwned;
use shelfy_core::db::{DbError, UserDb};
use shelfy_core::repo::RepoError;
use tokio_util::sync::CancellationToken;

use super::Jobs;
use super::scheduler::Shared;
use crate::control::jobs::{self as rows, Fence};
use crate::error::{ApiError, ErrorCode};
use crate::events::model::{JobState, JobUpdatedEvent};
use crate::state::AppState;

/// What a try of a job returns.
pub type JobResult = Result<Outcome, JobError>;

/// How a try ended well.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The job is done.
    Succeeded,
    /// Run the job again later, without using a try: a drain that re-arms
    /// itself (`run_at` = its next item's time), or a worker that yields
    /// because its queue was paused (`None`: as soon as it may run).
    Requeue {
        /// When to run again, unix ms; `None` for now.
        run_at: Option<i64>,
    },
}

/// Job error codes that the job system itself records in `errorCode`. A
/// worker's own codes follow the same rules as problem codes (plan §2.9): a
/// stable `snake_case` code, never prose; reuse an [`ErrorCode`] when one
/// fits (`quota_exceeded`, `capture_blocked`, `provider_key_invalid`…).
pub mod codes {
    /// The try stopped showing it was alive for longer than its lease.
    pub const LEASE_EXPIRED: &str = "lease_expired";
    /// The worker panicked, or failed in an unexpected way.
    pub const INTERNAL: &str = "internal";
    /// A dependency (the database, the network) was busy or down.
    pub const UNAVAILABLE: &str = "unavailable";
    /// The job's payload does not fit its kind.
    pub const INVALID_PAYLOAD: &str = "invalid_payload";
    /// The worker stopped because its job was cancelled.
    pub const CANCELLED: &str = "cancelled";
    /// The user's library is locked for maintenance (`admin user lock`, a
    /// restore): the job waits for the unlock without using a try.
    pub const USER_LOCKED: &str = "user_locked";
}

/// Why a try failed (plan §2.12 Errors).
///
/// - **Transient** (network, 5xx, 429, timeouts, a busy database): the job
///   is queued again after a backoff with jitter, until it has used
///   `max_attempts` tries. `retry_after` (a 429's `Retry-After`) is the
///   shortest wait.
/// - **Permanent** (4xx, validation, blocked, quota, a full media budget
///   (`storage_full`), an invalid key): the job fails at once, and the user
///   can retry it.
/// - **The user's library is locked** for maintenance ([`codes::USER_LOCKED`],
///   from a [`DbError`] or an [`ApiError`] that says so): not a failure. The
///   job goes back to the queue without using a try, and the user's jobs
///   wait a minute before the next one starts; a restore that takes an hour
///   costs no job anything. A worker that maps errors itself keeps the code.
///
/// `code` reaches clients (`errorCode`); `detail` is for developers and is
/// stored with the job but never sent or logged.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobError {
    transient: bool,
    code: Cow<'static, str>,
    detail: Option<String>,
    retry_after: Option<Duration>,
}

impl JobError {
    /// A transient failure with `code`.
    #[must_use]
    pub fn transient(code: impl Into<Cow<'static, str>>) -> Self {
        Self {
            transient: true,
            code: code.into(),
            detail: None,
            retry_after: None,
        }
    }

    /// A permanent failure with `code`.
    #[must_use]
    pub fn permanent(code: impl Into<Cow<'static, str>>) -> Self {
        Self {
            transient: false,
            ..Self::transient(code)
        }
    }

    /// What a worker returns when it stops because its token fired (see
    /// [`JobContext::token`]). The scheduler already knows why it fired,
    /// and records that instead.
    #[must_use]
    pub fn cancelled() -> Self {
        Self::permanent(codes::CANCELLED)
    }

    /// Adds a developer-facing detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Waits at least `delay` before the next try.
    #[must_use]
    pub fn with_retry_after(mut self, delay: Duration) -> Self {
        self.retry_after = Some(delay);
        self
    }

    /// Whether another try may succeed.
    #[must_use]
    pub fn is_transient(&self) -> bool {
        self.transient
    }

    /// Whether the try stopped because the user's library is locked for
    /// maintenance ([`codes::USER_LOCKED`]).
    #[must_use]
    pub fn is_user_locked(&self) -> bool {
        self.code == codes::USER_LOCKED
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// The developer-facing detail.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    /// The shortest wait before the next try.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }
}

impl fmt::Display for JobError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let class = if self.transient {
            "transient"
        } else {
            "permanent"
        };
        write!(f, "{class} {}", self.code)?;
        if let Some(detail) = &self.detail {
            write!(f, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for JobError {}

impl From<DbError> for JobError {
    fn from(err: DbError) -> Self {
        if err.is_locked() {
            return Self::transient(codes::USER_LOCKED).with_detail(err.to_string());
        }
        // Every database failure may clear up (a busy lock, a full disk);
        // the tries bound how long it is retried.
        let code = if crate::error::is_transient(&err) {
            codes::UNAVAILABLE
        } else {
            codes::INTERNAL
        };
        Self::transient(code).with_detail(err.to_string())
    }
}

impl From<RepoError> for JobError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::NotFound => Self::permanent(ErrorCode::NotFound.as_str()),
            RepoError::Conflict(what) => {
                Self::permanent(ErrorCode::Conflict.as_str()).with_detail(what)
            }
            RepoError::Invalid { field, reason } => {
                Self::permanent(ErrorCode::ValidationFailed.as_str())
                    .with_detail(format!("{field}: {reason}"))
            }
            RepoError::InvalidCursor => Self::permanent(ErrorCode::InvalidCursor.as_str()),
            RepoError::Db(db) => db.into(),
        }
    }
}

impl From<ApiError> for JobError {
    fn from(err: ApiError) -> Self {
        let status = err.status();
        let code = err.code().as_str();
        // A locked library (`user_locked`) is back once the operator's
        // restore is done. A full media budget (`storage_full`, 507) is not:
        // like a quota, it fails now and the user retries later.
        let error = if (status.is_server_error() && err.code() != ErrorCode::StorageFull)
            || status.as_u16() == 429
            || err.code() == ErrorCode::UserLocked
        {
            Self::transient(code)
        } else {
            Self::permanent(code)
        };
        error.with_detail(err.to_string())
    }
}

/// The job a worker runs, and what it may do while it runs.
///
/// **Stopping.** [`JobContext::token`] fires when the user cancels the job,
/// when its lease expires, and when the server shuts down. A worker checks
/// it between steps (or selects on it while it waits) and returns soon
/// after; what it returns then hardly matters, as the scheduler records
/// why it stopped. A shutdown puts an interrupted job back in the queue
/// without using a try, unless the worker finished it.
///
/// **Liveness.** The lease of a running job is renewed every third of its
/// length while the worker shows it is alive: [`JobContext::heartbeat`],
/// [`JobContext::progress`] and every [`JobContext::user_db`] chunk count.
/// A worker that shows nothing for a whole lease (`KindSpec::lease`) is
/// presumed hung: it is stopped, and the job is queued again using a try.
///
/// **Databases.** A worker reaches the user's library only through
/// [`JobContext::user_db`], one chunk at a time.
#[derive(Clone)]
pub struct JobContext {
    inner: Arc<Inner>,
}

struct Inner {
    shared: Arc<Shared>,
    state: AppState,
    id: i64,
    kind: &'static str,
    user_id: Arc<str>,
    payload: serde_json::Value,
    post_key: Option<String>,
    attempts: u32,
    max_attempts: u32,
    fence: Fence,
    token: CancellationToken,
    alive: Arc<AtomicI64>,
    progress: Mutex<Progress>,
}

/// The progress last reported, and when it was last written.
#[derive(Clone, Debug, Default)]
pub(super) struct Progress {
    pub(super) value: Option<f64>,
    pub(super) stage: Option<String>,
    written_at: Option<i64>,
}

/// How often progress is written to the database at most; every report
/// still goes out as an event (throttled to 250 ms by the bus).
const PROGRESS_WRITE_EVERY_MS: i64 = 1_000;

/// Longest stage code, in bytes.
const MAX_STAGE_BYTES: usize = 64;

/// What [`JobContext::new`] needs to know about the attempt.
pub(super) struct Attempt {
    pub(super) row: rows::JobRow,
    pub(super) kind: &'static str,
    pub(super) token: CancellationToken,
    pub(super) alive: Arc<AtomicI64>,
}

impl JobContext {
    pub(super) fn new(shared: Arc<Shared>, state: AppState, attempt: Attempt) -> Self {
        let Attempt {
            row,
            kind,
            token,
            alive,
        } = attempt;
        let payload: serde_json::Value =
            serde_json::from_str(&row.payload_json).unwrap_or(serde_json::Value::Null);
        let post_key = super::post_key(&payload);
        Self {
            inner: Arc::new(Inner {
                shared,
                state,
                id: row.id,
                kind,
                user_id: row.user_id.as_str().into(),
                post_key,
                attempts: row.attempts,
                max_attempts: row.max_attempts,
                fence: row.fence(),
                token,
                alive,
                progress: Mutex::new(Progress {
                    value: row.progress,
                    stage: row.stage,
                    written_at: None,
                }),
                payload,
            }),
        }
    }

    /// The job id.
    #[must_use]
    pub fn id(&self) -> i64 {
        self.inner.id
    }

    /// The kind.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.inner.kind
    }

    /// The user the job works for.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.inner.user_id
    }

    /// The payload given at enqueue.
    #[must_use]
    pub fn payload(&self) -> &serde_json::Value {
        &self.inner.payload
    }

    /// The payload as `T`.
    ///
    /// # Errors
    ///
    /// A permanent [`codes::INVALID_PAYLOAD`] when it does not fit `T`.
    pub fn payload_as<T: DeserializeOwned>(&self) -> Result<T, JobError> {
        T::deserialize(&self.inner.payload)
            .map_err(|err| JobError::permanent(codes::INVALID_PAYLOAD).with_detail(err.to_string()))
    }

    /// The post the job works on: the payload's `postKey`, if any.
    #[must_use]
    pub fn post_key(&self) -> Option<&str> {
        self.inner.post_key.as_deref()
    }

    /// Which try this is, from 1.
    #[must_use]
    pub fn attempt(&self) -> u32 {
        self.inner.attempts + 1
    }

    /// Tries allowed.
    #[must_use]
    pub fn max_attempts(&self) -> u32 {
        self.inner.max_attempts
    }

    /// The application state: configuration, databases, the event bus.
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.inner.state
    }

    /// The job system, to enqueue follow-up jobs.
    #[must_use]
    pub fn jobs(&self) -> &Jobs {
        self.inner.state.jobs()
    }

    /// Fires when the job must stop: cancelled, lease lost, or shutdown.
    #[must_use]
    pub fn token(&self) -> &CancellationToken {
        &self.inner.token
    }

    /// Whether [`JobContext::token`] fired.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.token.is_cancelled()
    }

    /// Whether the user paused this kind of job. A long worker (a drain)
    /// checks it between chunks and returns [`Outcome::Requeue`]: the job
    /// waits in the queue until the user resumes it.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.inner
            .shared
            .is_paused(&self.inner.user_id, self.inner.kind)
    }

    /// Whether to stop at the next step: cancelled or paused.
    #[must_use]
    pub fn should_yield(&self) -> bool {
        self.is_cancelled() || self.is_paused()
    }

    /// Shows the scheduler that the worker is alive (see the type's docs).
    /// Cheap and callable from blocking code.
    pub fn heartbeat(&self) {
        let now = self.inner.shared.clock.now_ms();
        self.inner.alive.fetch_max(now, Ordering::AcqRel);
    }

    /// Reports how far the job got: `progress` from 0 to 1 and the current
    /// `stage` (a code such as `fetch`); `None` keeps a value as it was. The
    /// report goes out as a `job.updated` event and is written to the job at
    /// most once a second (at once when the stage changes). Counts as a
    /// heartbeat. Once [`JobContext::token`] fired, reports are dropped.
    pub async fn progress(&self, progress: Option<f64>, stage: Option<&str>) {
        self.heartbeat();
        if self.is_cancelled() {
            // The job is over or stopping: its state is someone else's now.
            return;
        }
        let shared = &self.inner.shared;
        let now = shared.clock.now_ms();
        let (value, stage, write) = {
            let mut current = lock(&self.inner.progress);
            if let Some(p) = progress.filter(|p| p.is_finite()) {
                current.value = Some(p.clamp(0.0, 1.0));
            }
            let stage = stage.map(|s| truncate(s, MAX_STAGE_BYTES));
            let stage_changed = stage.is_some() && stage != current.stage.as_deref();
            if let Some(stage) = stage {
                current.stage = Some(stage.to_owned());
            }
            let write = stage_changed
                || current
                    .written_at
                    .is_none_or(|at| now.saturating_sub(at) >= PROGRESS_WRITE_EVERY_MS);
            if write {
                current.written_at = Some(now);
            }
            (current.value, current.stage.clone(), write)
        };
        shared.events.job_updated(
            &self.inner.user_id,
            JobUpdatedEvent {
                id: self.inner.id,
                kind: self.inner.kind.to_owned(),
                state: JobState::Running,
                progress: value,
                stage: stage.clone(),
                post_key: self.inner.post_key.clone(),
                error_code: None,
            },
        );
        if write {
            let fence = self.inner.fence;
            let written = shared
                .control(move |control| {
                    control.write(|tx| rows::set_progress(tx, fence, value, stage.as_deref(), now))
                })
                .await;
            if let Err(err) = written {
                tracing::debug!(job_id = fence.id, error = %format!("{err:#}"), "writing progress failed");
            }
        }
    }

    /// Runs `f`, one chunk of work, on the user's library, off the async
    /// workers. Counts as a heartbeat, before and after.
    ///
    /// The handle comes from the cache for each chunk and is dropped after
    /// it, so a job never keeps a handle while the cache evicts it: its
    /// writes always move the generation that the API's ETags read (plan
    /// §2.9, the *From T11* note of P1-07). Should the cache evict the
    /// handle during the chunk and serve a new one, the new one is evicted
    /// too after the chunk wrote, so no ETag taken from it can match again.
    /// Since P1-03 the handles of a library share its generation across
    /// evictions for idleness or capacity ([`shelfy_core::generation`]); an
    /// explicit eviction retires the generation, which this check covers.
    ///
    /// # Errors
    ///
    /// `f`'s error, or the library cannot be opened.
    pub async fn user_db<T, F>(&self, f: F) -> Result<T, JobError>
    where
        F: FnOnce(&UserDb) -> Result<T, JobError> + Send + 'static,
        T: Send + 'static,
    {
        self.heartbeat();
        let out = with_user_db(&self.inner.state, &self.inner.user_id, f).await;
        self.heartbeat();
        out
    }

    /// The last reported progress and stage.
    pub(super) fn last_progress(&self) -> (Option<f64>, Option<String>) {
        let current = lock(&self.inner.progress);
        (current.value, current.stage.clone())
    }
}

impl fmt::Debug for JobContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobContext")
            .field("id", &self.inner.id)
            .field("kind", &self.inner.kind)
            .field("attempt", &self.attempt())
            .finish_non_exhaustive()
    }
}

/// What the drain sweeper's check ([`super::Sweep`]) gets: one user.
#[derive(Clone)]
pub struct SweepContext {
    state: AppState,
    user_id: Arc<str>,
    kind: &'static str,
    now_ms: i64,
}

impl SweepContext {
    pub(super) fn new(state: AppState, user_id: &str, kind: &'static str, now_ms: i64) -> Self {
        Self {
            state,
            user_id: user_id.into(),
            kind,
            now_ms,
        }
    }

    /// The user to check.
    #[must_use]
    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    /// The drain's kind.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The time of the sweep, unix ms (the job system's clock).
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        self.now_ms
    }

    /// The application state.
    #[must_use]
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// Runs `f` on the user's library, like [`JobContext::user_db`].
    ///
    /// # Errors
    ///
    /// `f`'s error, or the library cannot be opened.
    pub async fn user_db<T, F>(&self, f: F) -> Result<T, JobError>
    where
        F: FnOnce(&UserDb) -> Result<T, JobError> + Send + 'static,
        T: Send + 'static,
    {
        with_user_db(&self.state, &self.user_id, f).await
    }
}

/// One chunk on `user_id`'s library: the handle is taken from the cache and
/// dropped afterwards (see [`JobContext::user_db`]).
async fn with_user_db<T, F>(state: &AppState, user_id: &str, f: F) -> Result<T, JobError>
where
    F: FnOnce(&UserDb) -> Result<T, JobError> + Send + 'static,
    T: Send + 'static,
{
    let cache = Arc::clone(state.user_dbs());
    let user = user_id.to_owned();
    let chunk = tokio::task::spawn_blocking(move || {
        let db = cache.get(&user)?;
        let before = db.generation();
        let out = f(&db);
        if db.generation() != before
            && let Some(current) = cache.get_if_present(&user)
            && !Arc::ptr_eq(&current, &db)
        {
            // Evicted mid-chunk and reopened by someone else: that handle's
            // generation missed this write, so retire it.
            cache.evict(&user);
        }
        out
    });
    match chunk.await {
        Ok(out) => out,
        Err(join) => Err(JobError::transient(codes::INTERNAL)
            .with_detail(format!("a database chunk failed: {join}"))),
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `text` cut to at most `max` bytes, on a character boundary.
fn truncate(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests {
    use axum::http::StatusCode;

    use super::*;

    #[test]
    fn errors_map_to_transient_or_permanent_codes() {
        let busy = JobError::from(DbError::ReaderTimeout);
        assert!(busy.is_transient());
        assert_eq!(busy.code(), codes::UNAVAILABLE);
        let broken = JobError::from(DbError::InvalidUserId);
        assert!(broken.is_transient());
        assert_eq!(broken.code(), codes::INTERNAL);

        let missing = JobError::from(RepoError::NotFound);
        assert!(!missing.is_transient());
        assert_eq!(missing.code(), "not_found");
        let invalid = JobError::from(RepoError::Invalid {
            field: "name",
            reason: "is required",
        });
        assert_eq!(invalid.code(), "validation_failed");
        assert_eq!(invalid.detail(), Some("name: is required"));

        let limited = JobError::from(ApiError::from_status(StatusCode::TOO_MANY_REQUESTS));
        assert!(limited.is_transient());
        assert_eq!(limited.code(), "rate_limited");
        let quota = JobError::from(ApiError::new(ErrorCode::QuotaExceeded));
        assert!(!quota.is_transient());
        assert_eq!(quota.code(), "quota_exceeded");
        let full = JobError::from(ApiError::new(ErrorCode::StorageFull));
        assert!(!full.is_transient(), "a full media budget is permanent");
        assert_eq!(full.code(), "storage_full");

        // A library locked for a restore waits for the unlock, however it
        // surfaces.
        for locked in [
            JobError::from(DbError::Locked),
            JobError::from(DbError::Open(std::sync::Arc::new(DbError::Locked))),
            JobError::from(RepoError::Db(DbError::Locked)),
            JobError::from(ApiError::user_locked()),
        ] {
            assert!(locked.is_transient());
            assert!(locked.is_user_locked(), "{locked}");
            assert_eq!(locked.code(), codes::USER_LOCKED);
            assert_eq!(codes::USER_LOCKED, ErrorCode::UserLocked.as_str());
        }
        assert!(!busy.is_user_locked());

        let shown = JobError::transient("unavailable")
            .with_detail("busy")
            .with_retry_after(Duration::from_secs(3));
        assert_eq!(shown.to_string(), "transient unavailable: busy");
        assert_eq!(shown.retry_after(), Some(Duration::from_secs(3)));
        assert_eq!(JobError::cancelled().code(), codes::CANCELLED);
    }

    #[test]
    fn stages_are_cut_on_a_character_boundary() {
        assert_eq!(truncate("fetch", 64), "fetch");
        assert_eq!(truncate("abcdef", 3), "abc");
        assert_eq!(truncate("aé", 2), "a");
    }
}
