//! The job system (plan §2.12, D9): an in-process scheduler over the control
//! database's `jobs` table, its workers, and the job-kind registry.
//!
//! **Model.** Two kinds of work, one scheduler:
//!
//! - *task work* (capture a site, fetch one video, export, import, migrate,
//!   purge…) is one `jobs` row per task;
//! - *item work* (thousands of items per user: archive an image, catalog a
//!   post) keeps its state in the user's library, and a per-user *drain*
//!   job (dedupe key = its kind) works through it in chunks. When only
//!   backed-off items remain, the drain returns [`Outcome::Requeue`] with
//!   the next item's time. A sweeper re-arms, every 10 minutes, each user
//!   with pending items and no active drain ([`Sweep`]).
//!
//! **Scheduling.** The queues live in memory, rebuilt from `jobs` at boot,
//! when `running` rows go back to `queued` (their process is gone). Each
//! kind keeps a FIFO per user and serves users round robin, so one user's
//! 6,000-job backlog never starves another user's single job (`queues.rs`).
//! A kind limits its running jobs overall and per user ([`KindSpec`]). A
//! job starts when a conditional `UPDATE` claims it; delayed jobs wait on a
//! timer at the earliest `run_at`.
//!
//! **Leases.** A claim leases the job for [`KindSpec::lease`]. The lease is
//! renewed every third of its length while the worker shows signs of life
//! ([`JobContext::heartbeat`]); a lease that runs out means a hung or lost
//! attempt: the watchdog stops it and queues the job again with
//! `attempts + 1`.
//!
//! **Errors.** A worker classifies failures ([`JobError`]): transient ones
//! retry after a backoff with jitter until `max_attempts`, permanent ones
//! fail at once. Failed and cancelled jobs can be retried by the user.
//!
//! **Controls.** Cancel trips the job's [`CancellationToken`] and records
//! `cancelled` at once; pause and resume act per user and kind
//! (`queue_state`); finished jobs can be cleared, and are pruned after 14
//! days by the nightly schedule (03:00 UTC), which also enqueues the
//! nightly kinds. Each change reaches the user's streams as `job.updated`,
//! at most every 250 ms per job ([`crate::events`]).
//!
//! **Shutdown.** The scheduler runs on a child of the server's shutdown
//! token. When it fires, workers are told to stop and interrupted jobs go
//! back to the queue without using a try; [`Scheduler::stop`] waits for them
//! within the shutdown grace, then aborts what is left, which boot recovery
//! queues again.
//!
//! **Databases.** Workers reach the user's library through
//! [`JobContext::user_db`] only: the handle is taken from the cache per
//! chunk, so a job never writes through a handle the API no longer reads
//! (the *From T11* note of P1-07).
//!
//! | Module | Contents |
//! |---|---|
//! | `scheduler` | the dispatcher, the supervisors, the watchdog, the nightly schedule, the sweeper |
//! | `queues` | the in-memory queues: turn order, limits, pauses |
//! | [`kinds`] | the job-kind registry of the server |
//! | [`usage`] | `usage.recompute`: a user's storage (P1-17) |
//! | [`archive`] | `archive.drain`: the covers and image slides from the platform CDNs (P2-10) |
//! | [`migrate`] | `migrate`: the install of a migrated desktop library (P1-19) |
//! | [`bulk`] | `bulk`: a bulk action over more than 500 posts (P1-11) |
//! | [`purge`] | `purge`: emptying the trash, and the nightly 30-day retention (P1-11) |
//! | [`hydrate`] | `link.hydrate`: a shared link's post from the platform's public endpoints (P2-11) |
//! | [`idempotency`] | the `Idempotency-Key` middleware of job-creating routes |
//! | `registry`, `context`, `clock` | [`Registry`], [`KindSpec`], [`Worker`]; [`JobContext`], [`JobError`], [`AttemptFence`]; [`Clock`] |
//!
//! The control-database queries are in [`crate::control::jobs`] and
//! [`crate::control::idempotency`]; the routes in [`crate::routes::jobs`].

pub mod ai_drain;
pub mod ai_run;
pub mod archive;
pub mod bulk;
mod clock;
mod context;
pub mod hydrate;
pub mod idempotency;
pub mod kinds;
pub mod migrate;
pub mod purge;
mod queues;
mod registry;
mod scheduler;
pub mod usage;

use std::sync::Arc;
use std::time::Duration;

use shelfy_core::db::ControlDb;
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

pub use clock::Clock;
pub use context::{
    AttemptFence, CancelContext, JobContext, JobError, JobResult, Outcome, SweepContext, codes,
};
pub use queues::KindStats;
pub use registry::{Backoff, BoxFuture, CancelHook, Kind, KindSpec, Registry, Sweep, Worker};

use crate::control::jobs::JobRow;
use crate::error::ApiError;
use crate::events::EventBus;
use crate::state::AppState;
use scheduler::Shared;

/// Finished jobs are deleted after this long (§2.12).
pub const FINISHED_RETENTION: Duration = Duration::from_secs(14 * 86_400);
/// How long an `Idempotency-Key` holds (§2.9).
pub const IDEMPOTENCY_TTL: Duration = Duration::from_secs(86_400);
/// How often expired leases are looked for.
pub const WATCHDOG_INTERVAL: Duration = Duration::from_secs(15);
/// How often the drain sweeper runs (§2.12).
pub const SWEEP_INTERVAL: Duration = Duration::from_secs(600);
/// When the nightly schedule runs, after midnight UTC (§2.12: 03:00 UTC).
pub const NIGHTLY_AT: Duration = Duration::from_secs(3 * 3600);
/// The dedupe key of the jobs the nightly schedule enqueues.
pub const NIGHTLY_DEDUPE: &str = "nightly";
/// The priority of a job unless it says otherwise; lower runs first.
pub const DEFAULT_PRIORITY: i64 = 100;
/// Longest dedupe key, in bytes.
pub const MAX_DEDUPE_KEY_BYTES: usize = 200;
/// Largest payload, as JSON.
pub const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Settings of the job system.
#[derive(Clone, Debug)]
pub struct JobsConfig {
    /// The kinds the scheduler runs: [`kinds::registry`] by default; tests
    /// register their own.
    pub registry: Registry,
    /// Where the time comes from: the wall clock, or tokio's clock in tests.
    pub clock: Clock,
}

impl Default for JobsConfig {
    fn default() -> Self {
        Self {
            registry: kinds::registry(),
            clock: Clock::System,
        }
    }
}

/// A job to enqueue. [`NewJob::new`] fills the defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct NewJob {
    /// The user it works for.
    pub user_id: String,
    /// Its kind; must be registered.
    pub kind: String,
    /// At most one active (queued or running) job per user, kind and key:
    /// enqueueing a duplicate returns the active job instead.
    pub dedupe_key: Option<String>,
    /// Lower runs first, within the user's queue of the kind.
    pub priority: i64,
    /// The worker's input; a `postKey` string names the post it works on.
    pub payload: serde_json::Value,
    /// Not before this time, unix ms; now when `None`.
    pub run_at: Option<i64>,
}

impl NewJob {
    /// A job of `kind` for `user_id`: no dedupe key, priority 100, payload
    /// `{}`, due now.
    #[must_use]
    pub fn new(user_id: impl Into<String>, kind: impl Into<String>) -> Self {
        Self {
            user_id: user_id.into(),
            kind: kind.into(),
            dedupe_key: None,
            priority: DEFAULT_PRIORITY,
            payload: serde_json::Value::Object(serde_json::Map::new()),
            run_at: None,
        }
    }

    /// Sets the payload.
    #[must_use]
    pub fn payload(mut self, payload: serde_json::Value) -> Self {
        self.payload = payload;
        self
    }

    /// Sets the dedupe key.
    #[must_use]
    pub fn dedupe(mut self, key: impl Into<String>) -> Self {
        self.dedupe_key = Some(key.into());
        self
    }

    /// Runs it not before `at_ms`.
    #[must_use]
    pub fn run_at(mut self, at_ms: i64) -> Self {
        self.run_at = Some(at_ms);
        self
    }

    /// Sets the priority.
    #[must_use]
    pub fn priority(mut self, priority: i64) -> Self {
        self.priority = priority;
        self
    }
}

/// What [`Jobs::enqueue`] did.
#[derive(Clone, Debug, PartialEq)]
pub struct Enqueued {
    /// The job: the new one, or the active duplicate.
    pub job: JobRow,
    /// False when an active job with the same dedupe key was returned. If
    /// it was waiting for a later time, it now runs at the new job's time.
    pub created: bool,
}

/// The job system: enqueue jobs, control them, run the scheduler. Cheap to
/// clone; [`AppState::jobs`] holds the server's.
#[derive(Clone)]
pub struct Jobs {
    shared: Arc<Shared>,
}

impl Jobs {
    /// The job system of `config` on the control database. The scheduler
    /// does not run until [`Jobs::start`]; jobs enqueued before wait.
    #[must_use]
    pub fn new(config: &JobsConfig, control: Arc<ControlDb>, events: EventBus) -> Self {
        Self {
            shared: Arc::new(Shared::new(
                config.registry.clone(),
                config.clock,
                control,
                events,
            )),
        }
    }

    /// The registered kinds.
    #[must_use]
    pub fn registry(&self) -> &Registry {
        &self.shared.registry
    }

    /// The clock of the job system.
    #[must_use]
    pub fn clock(&self) -> &Clock {
        &self.shared.clock
    }

    /// Starts the scheduler on `state` until `token` fires (the server
    /// passes a child of its shutdown token). It recovers the jobs of the
    /// last run first.
    ///
    /// # Panics
    ///
    /// When the scheduler already runs.
    #[must_use]
    pub fn start(&self, state: AppState, token: CancellationToken) -> Scheduler {
        assert!(
            self.shared.mark_started(&token),
            "the job scheduler is already running"
        );
        let task = tokio::spawn(scheduler::run(
            Arc::clone(&self.shared),
            state,
            token.clone(),
        ));
        Scheduler {
            task: Some(task),
            token,
        }
    }

    /// Enqueues a job, or returns the active job with the same dedupe key.
    ///
    /// # Errors
    ///
    /// 422 `validation_failed` for an unknown kind, a dedupe key or a payload
    /// that is too long; database errors.
    pub async fn enqueue(&self, job: NewJob) -> Result<Enqueued, ApiError> {
        self.shared.enqueue(job).await
    }

    /// Admits a job inserted atomically with feature metadata in the control DB.
    /// The caller must pass a committed row; boot recovery covers a crash before admission.
    pub(crate) fn admit_committed(&self, row: &JobRow) {
        self.shared.admit_committed(row);
    }

    /// The job `id` of `user_id`.
    ///
    /// # Errors
    ///
    /// Database errors.
    pub async fn get(&self, user_id: &str, id: i64) -> Result<Option<JobRow>, ApiError> {
        self.shared.get(user_id, id).await
    }

    /// Cancels a queued or running job; cancelling a cancelled job changes
    /// nothing. A running worker is told to stop.
    ///
    /// # Errors
    ///
    /// 404 when `user_id` has no such job; 409 when it already finished.
    pub async fn cancel(&self, user_id: &str, id: i64) -> Result<JobRow, ApiError> {
        self.shared.cancel(user_id, id).await.map(|(row, _)| row)
    }

    /// Cancels a job and reports whether this call changed it. A repeated
    /// cancel must not rerun a kind hook against newly enqueued work.
    pub(crate) async fn cancel_changed(
        &self,
        user_id: &str,
        id: i64,
    ) -> Result<(JobRow, bool), ApiError> {
        self.shared.cancel(user_id, id).await
    }

    /// Queues a failed or cancelled job again, with every try available.
    ///
    /// # Errors
    ///
    /// 404 when `user_id` has no such job; 409 when it is not failed or
    /// cancelled, its cancelled try is still stopping, an active job with
    /// its dedupe key exists, or its kind no longer runs here.
    pub async fn retry(&self, user_id: &str, id: i64) -> Result<JobRow, ApiError> {
        self.shared.retry(user_id, id).await
    }

    /// Pauses `user_id`'s jobs of `kind`: queued ones wait, running ones go
    /// on (a drain yields at its next chunk). Returns whether it changed.
    ///
    /// # Errors
    ///
    /// 404 for an unknown kind; database errors.
    pub async fn pause(&self, user_id: &str, kind: &str) -> Result<bool, ApiError> {
        self.shared.set_paused(user_id, kind, true).await
    }

    /// Resumes `user_id`'s jobs of `kind`. Returns whether it changed.
    ///
    /// # Errors
    ///
    /// 404 for an unknown kind; database errors.
    pub async fn resume(&self, user_id: &str, kind: &str) -> Result<bool, ApiError> {
        self.shared.set_paused(user_id, kind, false).await
    }

    /// Whether `user_id` paused `kind`.
    #[must_use]
    pub fn is_paused(&self, user_id: &str, kind: &str) -> bool {
        self.shared.is_paused(user_id, kind)
    }

    /// Cancels every queued and running job of `kind` of `user_id`; returns
    /// how many.
    ///
    /// # Errors
    ///
    /// 404 for an unknown kind; database errors.
    pub async fn cancel_all(&self, user_id: &str, kind: &str) -> Result<u64, ApiError> {
        self.shared.cancel_all(user_id, kind).await
    }

    /// Deletes the finished jobs of `kind` of `user_id`; returns how many.
    ///
    /// # Errors
    ///
    /// 404 for an unknown kind; database errors.
    pub async fn clear_finished(&self, user_id: &str, kind: &str) -> Result<u64, ApiError> {
        self.shared.clear_finished(user_id, kind).await
    }

    /// What the scheduler holds, per kind: the seam for the job metrics
    /// (P1-15).
    #[must_use]
    pub fn stats(&self) -> Vec<KindStats> {
        self.shared.stats()
    }
}

/// The running scheduler. Dropping it aborts the scheduler.
pub struct Scheduler {
    task: Option<JoinHandle<()>>,
    token: CancellationToken,
}

impl Scheduler {
    /// Stops the scheduler: fires its token, then waits until `deadline` for
    /// the workers to stop (interrupted jobs go back to the queue). Workers
    /// still running at the deadline are aborted; their jobs stay `running`
    /// and are queued again at the next start. Returns whether everything
    /// stopped in time.
    pub async fn stop(mut self, deadline: Instant) -> bool {
        self.token.cancel();
        let Some(mut task) = self.task.take() else {
            return true;
        };
        if tokio::time::timeout_at(deadline, &mut task).await.is_ok() {
            return true;
        }
        task.abort();
        let _ = task.await;
        false
    }

    /// Aborts the scheduler at once, as a crash would: nothing more is
    /// recorded, and the jobs that were running are queued again at the next
    /// start. For tests.
    pub async fn abort(mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for Scheduler {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

/// The `postKey` of a payload, if it has one.
fn post_key(payload: &serde_json::Value) -> Option<String> {
    payload
        .get("postKey")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

/// The `postKey` of a stored payload.
pub(crate) fn row_post_key(payload_json: &str) -> Option<String> {
    if !payload_json.contains("postKey") {
        return None;
    }
    serde_json::from_str::<serde_json::Value>(payload_json)
        .ok()
        .as_ref()
        .and_then(post_key)
}

pub mod export;
