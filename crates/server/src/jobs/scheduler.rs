//! The scheduler at work (plan §2.12): boot recovery, the dispatcher, the
//! supervisor of each running attempt, the lease watchdog, the nightly
//! schedule and the drain sweeper; and the operations behind the public API
//! of [`super::Jobs`].
//!
//! One dispatcher task runs the loop. It wakes on new or freed work, at the
//! earliest delayed `run_at`, on the watchdog and sweeper timers and at
//! 03:00 UTC; it picks jobs from [`Queues`] in turn order, claims them in one
//! transaction and spawns a supervisor per claimed attempt. The supervisor
//! runs the worker in a task of its own (so a panic is a failed try),
//! renews the lease while the worker shows signs of life, and records the
//! outcome with a fenced write.

use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use anyhow::Context as _;
use shelfy_core::db::ControlDb;
use shelfy_core::repo::RepoError;
use tokio::sync::Notify;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::{Instant, MissedTickBehavior, interval, interval_at, sleep_until};
use tokio_util::sync::CancellationToken;

use super::clock::{Clock, next_daily};
use super::context::{Attempt, JobContext, JobError, JobResult, Outcome, SweepContext, codes};
use super::queues::{KindStats, Queues, Running, Stop};
use super::registry::{Backoff, Kind, Registry};
use super::{
    Enqueued, FINISHED_RETENTION, IDEMPOTENCY_TTL, MAX_DEDUPE_KEY_BYTES, MAX_PAYLOAD_BYTES,
    NIGHTLY_AT, NIGHTLY_DEDUPE, NewJob, SWEEP_INTERVAL, WATCHDOG_INTERVAL, row_post_key,
};
use crate::control::idempotency;
use crate::control::jobs::{self as rows, Fence, Finish, Inserted, JobRow, NewJobRow};
use crate::error::{ApiError, ErrorCode};
use crate::events::EventBus;
use crate::events::model::{JobState, JobUpdatedEvent};
use crate::state::{AppState, blocking};

/// What the job system shares between the API, the dispatcher and the
/// supervisors.
pub(super) struct Shared {
    pub(super) registry: Registry,
    pub(super) clock: Clock,
    control: Arc<ControlDb>,
    pub(super) events: EventBus,
    queues: Mutex<Queues>,
    /// Wakes the dispatcher: work was added, or a slot freed.
    wake: Notify,
    /// Numbers the attempts of this process.
    runs: AtomicU64,
    /// The running scheduler's token.
    stopping: Mutex<Option<CancellationToken>>,
    started: AtomicBool,
    sweeping: AtomicBool,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing panics while holding these locks in a way that breaks the
    // state's invariants: recover.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// The delay before the next try after `failures` failed tries: the kind's
/// backoff with jitter, and at least `floor` (a `Retry-After`).
fn retry_delay(backoff: Backoff, failures: u32, floor: Option<Duration>) -> Duration {
    let random = getrandom::u64().unwrap_or(0);
    backoff
        .jittered(failures, random)
        .max(floor.unwrap_or_default())
}

/// The event that tells clients where `row` stands.
pub(super) fn event_of(row: &JobRow) -> JobUpdatedEvent {
    JobUpdatedEvent {
        id: row.id,
        kind: row.kind.clone(),
        state: row.state,
        progress: row.progress,
        stage: row.stage.clone(),
        post_key: row_post_key(&row.payload_json),
        error_code: row.error_code.clone(),
    }
}

/// A job taken from the queues, whose claim is pending.
struct Picked {
    id: i64,
    run: u64,
    kind: &'static str,
    user: Arc<str>,
    priority: i64,
    run_at: i64,
    lease: Duration,
}

/// The result of one claim.
enum Claim {
    /// The attempt is ours.
    Taken(JobRow),
    /// The job could not be claimed; its row as it is now, if it exists.
    Skipped(Option<JobRow>),
}

/// How an attempt ends in the database (an owned [`Finish`]).
#[derive(Clone, Debug)]
enum Change {
    Succeeded,
    Requeue {
        run_at: i64,
        interrupted: bool,
    },
    Retry {
        run_at: i64,
        code: String,
        detail: Option<String>,
    },
    Fail {
        code: String,
        detail: Option<String>,
    },
}

impl Change {
    fn as_finish(&self) -> Finish<'_> {
        match self {
            Self::Succeeded => Finish::Succeeded,
            Self::Requeue { run_at, .. } => Finish::Requeue { run_at: *run_at },
            Self::Retry {
                run_at,
                code,
                detail,
            } => Finish::Retry {
                run_at: *run_at,
                code,
                detail: detail.as_deref(),
            },
            Self::Fail { code, detail } => Finish::Fail {
                code,
                detail: detail.as_deref(),
            },
        }
    }

    /// After `failures` failed tries of a job allowed `max_attempts`: a
    /// retry after the backoff when `error` is transient and tries are
    /// left, a failure otherwise.
    fn after_error(
        error: &JobError,
        failures: u32,
        max_attempts: u32,
        backoff: Backoff,
        now: i64,
    ) -> Self {
        let code = error.code().to_owned();
        let detail = error.detail().map(str::to_owned);
        if error.is_transient() && failures < max_attempts {
            let delay = retry_delay(backoff, failures, error.retry_after());
            Self::Retry {
                run_at: now.saturating_add(millis(delay)),
                code,
                detail,
            }
        } else {
            Self::Fail { code, detail }
        }
    }
}

/// Aborts a task when dropped: the worker of an aborted supervisor stops too.
struct AbortOnDrop<T>(JoinHandle<T>);

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Clears a flag when dropped, however the task ends.
struct ClearOnDrop<'a>(&'a AtomicBool);

impl Drop for ClearOnDrop<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl Shared {
    pub(super) fn new(
        registry: Registry,
        clock: Clock,
        control: Arc<ControlDb>,
        events: EventBus,
    ) -> Self {
        Self {
            queues: Mutex::new(Queues::new(&registry)),
            registry,
            clock,
            control,
            events,
            wake: Notify::new(),
            runs: AtomicU64::new(1),
            stopping: Mutex::new(None),
            started: AtomicBool::new(false),
            sweeping: AtomicBool::new(false),
        }
    }

    fn queues(&self) -> MutexGuard<'_, Queues> {
        lock(&self.queues)
    }

    /// Runs `f` on the control database, off the async workers.
    pub(super) async fn control<T, F>(&self, f: F) -> anyhow::Result<T>
    where
        F: FnOnce(&ControlDb) -> Result<T, RepoError> + Send + 'static,
        T: Send + 'static,
    {
        let control = Arc::clone(&self.control);
        let result = tokio::task::spawn_blocking(move || f(&control))
            .await
            .context("a control database task failed")?;
        Ok(result?)
    }

    /// Tells the user's open streams where `row` stands (`job.updated`).
    pub(super) fn publish(&self, row: &JobRow) {
        self.events.job_updated(&row.user_id, event_of(row));
    }

    pub(super) fn is_paused(&self, user: &str, kind: &str) -> bool {
        self.queues().is_paused(user, kind)
    }

    pub(super) fn stats(&self) -> Vec<KindStats> {
        self.queues().stats()
    }

    fn shutting_down(&self) -> bool {
        lock(&self.stopping)
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    /// The registered kind called `name`, for the queue routes.
    fn kind_named(&self, name: &str) -> Result<&Kind, ApiError> {
        self.registry
            .get(name)
            .ok_or_else(|| ApiError::not_found().with_detail("no such kind of job"))
    }

    /// Marks the scheduler as started; false when it already runs.
    pub(super) fn mark_started(&self, token: &CancellationToken) -> bool {
        if self.started.swap(true, Ordering::AcqRel) {
            return false;
        }
        *lock(&self.stopping) = Some(token.clone());
        true
    }

    // ---- Operations of the public API ----------------------------------

    pub(super) async fn enqueue(&self, new: NewJob) -> Result<Enqueued, ApiError> {
        let kind = self
            .registry
            .get(&new.kind)
            .ok_or_else(|| ApiError::invalid_field("kind", "is not a job kind of this server"))?;
        if new
            .dedupe_key
            .as_ref()
            .is_some_and(|key| key.is_empty() || key.len() > MAX_DEDUPE_KEY_BYTES)
        {
            return Err(ApiError::invalid_field(
                "dedupeKey",
                "must be 1 to 200 bytes",
            ));
        }
        let payload = serde_json::to_string(&new.payload).map_err(ApiError::internal)?;
        if payload.len() > MAX_PAYLOAD_BYTES {
            return Err(ApiError::invalid_field("payload", "is larger than 64 KiB"));
        }
        let max_attempts = kind.spec().max_attempts;
        let now = self.clock.now_ms();
        let run_at = new.run_at.unwrap_or(now);
        let NewJob {
            user_id,
            kind,
            dedupe_key,
            priority,
            ..
        } = new;
        let control = Arc::clone(&self.control);
        let inserted = blocking(move || {
            control.write(|tx| {
                let row = NewJobRow {
                    user_id: &user_id,
                    kind: &kind,
                    dedupe_key: dedupe_key.as_deref(),
                    priority,
                    payload_json: &payload,
                    max_attempts,
                    run_at,
                };
                rows::insert(tx, &row, now)
            })
        })
        .await?;
        let (job, created) = match inserted {
            Inserted::Created(job) => (job, true),
            Inserted::Existing(job) => (job, false),
        };
        if job.state == JobState::Queued {
            self.queues().add(
                job.id,
                &job.kind,
                &job.user_id,
                job.priority,
                job.run_at,
                now,
            );
            self.wake.notify_one();
        }
        if created {
            self.publish(&job);
        }
        Ok(Enqueued { job, created })
    }

    pub(super) async fn get(&self, user_id: &str, id: i64) -> Result<Option<JobRow>, ApiError> {
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        blocking(move || control.read(|conn| rows::get(conn, &user, id))).await
    }

    /// Forgets cancelled jobs: out of the queues, and their running attempts
    /// told to stop.
    fn forget(&self, ids: impl IntoIterator<Item = i64>) {
        let mut queues = self.queues();
        for id in ids {
            queues.remove(id);
            if let Some(running) = queues.running.get_mut(&id) {
                running.stop = Some(Stop::Cancelled);
                running.token.cancel();
            }
        }
    }

    pub(super) async fn cancel(&self, user_id: &str, id: i64) -> Result<JobRow, ApiError> {
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        let now = self.clock.now_ms();
        let (cancelled, current) = blocking(move || {
            control.write(|tx| match rows::cancel(tx, &user, id, now)? {
                Some(row) => Ok::<_, RepoError>((Some(row), None)),
                None => Ok((None, rows::get(tx, &user, id)?)),
            })
        })
        .await?;
        if let Some(row) = cancelled {
            self.forget([row.id]);
            self.publish(&row);
            return Ok(row);
        }
        match current {
            None => Err(ApiError::not_found()),
            Some(row) if row.state == JobState::Cancelled => Ok(row),
            Some(_) => {
                Err(ApiError::new(ErrorCode::Conflict).with_detail("the job already finished"))
            }
        }
    }

    pub(super) async fn retry(&self, user_id: &str, id: i64) -> Result<JobRow, ApiError> {
        let Some(current) = self.get(user_id, id).await? else {
            return Err(ApiError::not_found());
        };
        if !matches!(current.state, JobState::Failed | JobState::Cancelled) {
            return Err(ApiError::new(ErrorCode::Conflict)
                .with_detail("only a failed or cancelled job can be retried"));
        }
        if self.registry.get(&current.kind).is_none() {
            return Err(ApiError::new(ErrorCode::Conflict)
                .with_detail("this kind of job no longer runs on this server"));
        }
        if self.queues().running.contains_key(&id) {
            return Err(ApiError::new(ErrorCode::Conflict)
                .with_detail("the cancelled try is still stopping; retry in a moment"));
        }
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        let now = self.clock.now_ms();
        let retried = blocking(move || control.write(|tx| rows::retry(tx, &user, id, now)))
            .await
            .map_err(|err| {
                if err.code() == ErrorCode::Conflict {
                    err.with_detail("an active job of this kind already does the same work")
                } else {
                    err
                }
            })?;
        let Some(row) = retried else {
            return Err(ApiError::new(ErrorCode::Conflict)
                .with_detail("only a failed or cancelled job can be retried"));
        };
        self.queues().add(
            row.id,
            &row.kind,
            &row.user_id,
            row.priority,
            row.run_at,
            now,
        );
        self.wake.notify_one();
        self.publish(&row);
        Ok(row)
    }

    pub(super) async fn set_paused(
        &self,
        user_id: &str,
        kind: &str,
        paused: bool,
    ) -> Result<bool, ApiError> {
        let kind = self.kind_named(kind)?.name();
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        let changed =
            blocking(move || control.write(|tx| rows::set_paused(tx, &user, kind, paused))).await?;
        self.queues().set_paused(user_id, kind, paused);
        if !paused {
            self.wake.notify_one();
        }
        Ok(changed)
    }

    pub(super) async fn cancel_all(&self, user_id: &str, kind: &str) -> Result<u64, ApiError> {
        let kind = self.kind_named(kind)?.name();
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        let now = self.clock.now_ms();
        let cancelled =
            blocking(move || control.write(|tx| rows::cancel_kind(tx, &user, kind, now))).await?;
        self.forget(cancelled.iter().map(|row| row.id));
        for row in &cancelled {
            self.publish(row);
        }
        Ok(cancelled.len() as u64)
    }

    pub(super) async fn clear_finished(&self, user_id: &str, kind: &str) -> Result<u64, ApiError> {
        let kind = self.kind_named(kind)?.name();
        let control = Arc::clone(&self.control);
        let user = user_id.to_owned();
        blocking(move || control.write(|tx| rows::clear_finished(tx, &user, kind))).await
    }

    // ---- The scheduler -------------------------------------------------

    /// Boot: `running` rows go back to `queued` (their process is gone),
    /// then every queued job and paused queue is loaded.
    async fn boot(&self) {
        let now = self.clock.now_ms();
        let loaded = self
            .control(move |control| {
                control.write(|tx| {
                    let recovered = rows::recover_running(tx, now)?;
                    let queued = rows::queued(tx)?;
                    let paused = rows::paused_all(tx)?;
                    Ok((recovered, queued, paused))
                })
            })
            .await;
        match loaded {
            Ok((recovered, queued, paused)) => {
                let mut loaded = 0_usize;
                {
                    let mut queues = self.queues();
                    for (user, kind) in &paused {
                        if let Some(kind) = self.registry.get(kind) {
                            queues.set_paused(user, kind.name(), true);
                        }
                    }
                    for job in &queued {
                        if queues.add(
                            job.id,
                            &job.kind,
                            &job.user_id,
                            job.priority,
                            job.run_at,
                            now,
                        ) {
                            loaded += 1;
                        }
                    }
                }
                if recovered > 0 {
                    tracing::info!(
                        recovered,
                        "jobs that were running at the last stop are queued again"
                    );
                }
                let unknown = queued.len() - loaded;
                if unknown > 0 {
                    tracing::warn!(
                        unknown,
                        "queued jobs of kinds this build does not run stay queued"
                    );
                }
                tracing::info!(queued = loaded, kinds = ?self.registry, "job scheduler started");
            }
            Err(err) => tracing::error!(
                error = %format!("{err:#}"),
                "loading the job queues failed; queued jobs wait for the next start"
            ),
        }
        self.prune().await;
    }

    /// Deletes jobs finished over 14 days ago and idempotency keys over 24 h
    /// old.
    async fn prune(&self) {
        let now = self.clock.now_ms();
        let finished_before = now.saturating_sub(millis(FINISHED_RETENTION));
        let keys_before = now.saturating_sub(millis(IDEMPOTENCY_TTL));
        let pruned = self
            .control(move |control| {
                control.write(|tx| {
                    let jobs = rows::prune_finished(tx, finished_before)?;
                    let keys = idempotency::prune(tx, keys_before)?;
                    Ok((jobs, keys))
                })
            })
            .await;
        match pruned {
            Ok((0, 0)) => {}
            Ok((jobs, keys)) => {
                tracing::info!(jobs, idempotency_keys = keys, "pruned old jobs and keys")
            }
            Err(err) => tracing::warn!(error = %format!("{err:#}"), "pruning old jobs failed"),
        }
    }

    /// Starts every job that may start now.
    async fn dispatch(
        self: &Arc<Self>,
        state: &AppState,
        token: &CancellationToken,
        tasks: &mut JoinSet<()>,
    ) {
        while !token.is_cancelled() {
            let now = self.clock.now_ms();
            let picked = self.pick_all(token, now);
            if picked.is_empty() {
                return;
            }
            let leases: Vec<(i64, i64)> = picked
                .iter()
                .map(|p| (p.id, now.saturating_add(millis(p.lease))))
                .collect();
            let claims = self
                .control(move |control| {
                    control.write(|tx| {
                        leases
                            .iter()
                            .map(|&(id, lease_until)| {
                                Ok(match rows::claim(tx, id, lease_until, now)? {
                                    Some(row) => Claim::Taken(row),
                                    None => Claim::Skipped(rows::find(tx, id)?),
                                })
                            })
                            .collect::<Result<Vec<_>, RepoError>>()
                    })
                })
                .await;
            match claims {
                Ok(claims) => {
                    for (picked, claim) in picked.into_iter().zip(claims) {
                        match claim {
                            Claim::Taken(row) => self.launch(state, picked, row, tasks),
                            Claim::Skipped(row) => self.unpick(&picked, row.as_ref(), now),
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(error = %format!("{err:#}"), "claiming jobs failed; trying again soon");
                    let mut queues = self.queues();
                    for picked in &picked {
                        queues.end_run(picked.id, picked.run);
                        queues.add(
                            picked.id,
                            picked.kind,
                            &picked.user,
                            picked.priority,
                            picked.run_at,
                            now,
                        );
                    }
                    // The watchdog tick wakes the loop again.
                    return;
                }
            }
        }
    }

    /// Takes every job that may start, holding their slots with a pending
    /// attempt each.
    fn pick_all(&self, token: &CancellationToken, now: i64) -> Vec<Picked> {
        let mut queues = self.queues();
        queues.promote(now);
        let mut picked = Vec::new();
        for kind in self.registry.kinds() {
            while let Some((id, job)) = queues.pick(kind.name()) {
                let run = self.runs.fetch_add(1, Ordering::Relaxed);
                queues.running.insert(
                    id,
                    Running {
                        run,
                        kind: job.kind,
                        user: Arc::clone(&job.user),
                        token: token.child_token(),
                        stop: None,
                        alive: Arc::new(AtomicI64::new(now)),
                        worker: None,
                    },
                );
                picked.push(Picked {
                    id,
                    run,
                    kind: job.kind,
                    user: job.user,
                    priority: job.priority,
                    run_at: job.run_at,
                    lease: kind.spec().lease,
                });
            }
        }
        picked
    }

    /// A job that could not be claimed: its slot goes back, and it waits
    /// again if it is still queued (paused or rescheduled meanwhile).
    fn unpick(&self, picked: &Picked, row: Option<&JobRow>, now: i64) {
        let mut queues = self.queues();
        queues.end_run(picked.id, picked.run);
        if let Some(row) = row.filter(|row| row.state == JobState::Queued) {
            queues.add(
                row.id,
                &row.kind,
                &row.user_id,
                row.priority,
                row.run_at,
                now,
            );
        }
    }

    /// Spawns the supervisor of a claimed attempt.
    fn launch(
        self: &Arc<Self>,
        state: &AppState,
        picked: Picked,
        row: JobRow,
        tasks: &mut JoinSet<()>,
    ) {
        let Some(kind) = self.registry.get(picked.kind).cloned() else {
            return;
        };
        let attempt = {
            let mut queues = self.queues();
            let Some(running) = queues
                .running
                .get(&picked.id)
                .filter(|running| running.run == picked.run)
            else {
                return;
            };
            if running.stop == Some(Stop::Cancelled) {
                // Cancelled between the pick and the claim: the database
                // already says so.
                queues.end_run(picked.id, picked.run);
                return;
            }
            Attempt {
                kind: kind.name(),
                token: running.token.clone(),
                alive: Arc::clone(&running.alive),
                row,
            }
        };
        tasks.spawn(supervise(
            Arc::clone(self),
            state.clone(),
            kind,
            attempt,
            picked.run,
        ));
    }

    /// Records the worker task of an attempt, so an expired lease can stop it.
    fn set_worker(&self, id: i64, run: u64, worker: tokio::task::AbortHandle) {
        if let Some(running) = self
            .queues()
            .running
            .get_mut(&id)
            .filter(|running| running.run == run)
        {
            running.worker = Some(worker);
        }
    }

    /// Extends the lease of a running attempt to its last sign of life plus
    /// the lease; an attempt that showed none since the last renewal is left
    /// to expire.
    async fn renew(&self, fence: Fence, alive: &AtomicI64, lease: Duration, written: &mut i64) {
        let until = alive.load(Ordering::Acquire).saturating_add(millis(lease));
        if until <= *written {
            return;
        }
        match self
            .control(move |control| control.write(|tx| rows::renew(tx, fence, until)))
            .await
        {
            Ok(true) => *written = until,
            // No longer this attempt: a cancel or the watchdog took it, and
            // stopped the worker.
            Ok(false) => {}
            Err(err) => tracing::warn!(
                job_id = fence.id,
                error = %format!("{err:#}"),
                "renewing a job lease failed; trying again at the next renewal"
            ),
        }
    }

    /// Records how an attempt ended, gives its slot back and wakes the
    /// dispatcher.
    async fn finish(&self, ended: Ended) {
        let Ended {
            fence,
            run,
            kind,
            max_attempts,
            result,
            progress,
            stage,
            started,
        } = ended;
        let stop = {
            let queues = self.queues();
            match queues.running.get(&fence.id) {
                Some(running) if running.run == run => running.stop,
                // The watchdog ended this attempt.
                _ => Some(Stop::LeaseLost),
            }
        };
        let now = self.clock.now_ms();
        let change = match (stop, &result) {
            // The database already says what happened.
            (Some(_), _) => None,
            (None, Ok(Outcome::Succeeded)) => Some(Change::Succeeded),
            (None, Ok(Outcome::Requeue { run_at })) => Some(Change::Requeue {
                run_at: run_at.unwrap_or(now),
                interrupted: false,
            }),
            // Stopped by the shutdown: back to the queue, the try not counted.
            (None, Err(_)) if self.shutting_down() => Some(Change::Requeue {
                run_at: now,
                interrupted: true,
            }),
            (None, Err(error)) => Some(Change::after_error(
                error,
                fence.attempts + 1,
                max_attempts,
                kind.spec().backoff,
                now,
            )),
        };
        let mut row = None;
        if let Some(change) = &change {
            let change = change.clone();
            let written = self
                .control(move |control| {
                    control.write(|tx| {
                        rows::finish(
                            tx,
                            fence,
                            &change.as_finish(),
                            progress,
                            stage.as_deref(),
                            now,
                        )
                    })
                })
                .await;
            match written {
                Ok(written) => row = written,
                Err(err) => tracing::error!(
                    job_id = fence.id,
                    kind = kind.name(),
                    error = %format!("{err:#}"),
                    "recording a job's outcome failed; its lease will bring it back"
                ),
            }
        }
        {
            let mut queues = self.queues();
            queues.end_run(fence.id, run);
            if let Some(row) = row.as_ref().filter(|row| row.state == JobState::Queued) {
                queues.add(
                    row.id,
                    &row.kind,
                    &row.user_id,
                    row.priority,
                    row.run_at,
                    now,
                );
            }
        }
        self.wake.notify_one();
        if let (Some(row), Some(change)) = (&row, &change) {
            self.publish(row);
            log_outcome(row, change, started.elapsed());
        } else if stop == Some(Stop::Cancelled) {
            // A progress report that raced the cancel may have gone out after
            // it: the stored state has the last word.
            let id = fence.id;
            match self
                .control(move |control| control.read(|conn| rows::find(conn, id)))
                .await
            {
                Ok(Some(row)) => self.publish(&row),
                Ok(None) => {}
                Err(err) => {
                    tracing::debug!(job_id = id, error = %format!("{err:#}"), "reading a cancelled job failed");
                }
            }
        }
    }

    /// The lease watchdog: a running attempt whose lease ran out showed no
    /// sign of life for a whole lease (or its process is gone). The job is
    /// queued again using a try (or fails when none is left), and the
    /// attempt, if it runs here, is stopped and its slot freed.
    async fn expire_leases(&self) {
        let now = self.clock.now_ms();
        let expired = match self
            .control(move |control| control.read(|conn| rows::expired(conn, now)))
            .await
        {
            Ok(expired) => expired,
            Err(err) => {
                tracing::warn!(error = %format!("{err:#}"), "checking job leases failed");
                return;
            }
        };
        for row in expired {
            let backoff = self
                .registry
                .get(&row.kind)
                .map_or(Backoff::DEFAULT, |kind| kind.spec().backoff);
            let error = JobError::transient(codes::LEASE_EXPIRED)
                .with_detail("the attempt showed no sign of life for a whole lease");
            let change =
                Change::after_error(&error, row.attempts + 1, row.max_attempts, backoff, now);
            let (fence, progress, stage) = (row.fence(), row.progress, row.stage.clone());
            let written = self
                .control(move |control| {
                    control.write(|tx| {
                        rows::finish(
                            tx,
                            fence,
                            &change.as_finish(),
                            progress,
                            stage.as_deref(),
                            now,
                        )
                    })
                })
                .await;
            let updated = match written {
                Ok(Some(updated)) => updated,
                // It ended meanwhile.
                Ok(None) => continue,
                Err(err) => {
                    tracing::warn!(job_id = row.id, error = %format!("{err:#}"), "expiring a job lease failed");
                    continue;
                }
            };
            {
                let mut queues = self.queues();
                if let Some(running) = queues.running.get_mut(&row.id) {
                    running.stop = Some(Stop::LeaseLost);
                    running.token.cancel();
                    if let Some(worker) = &running.worker {
                        worker.abort();
                    }
                    let run = running.run;
                    queues.end_run(row.id, run);
                }
                if updated.state == JobState::Queued {
                    queues.add(
                        updated.id,
                        &updated.kind,
                        &updated.user_id,
                        updated.priority,
                        updated.run_at,
                        now,
                    );
                }
            }
            tracing::warn!(
                job_id = updated.id,
                kind = %updated.kind,
                attempts = updated.attempts,
                state = rows::state_str(updated.state),
                "a job's lease expired: its attempt showed no sign of life"
            );
            self.publish(&updated);
            self.wake.notify_one();
        }
    }
}

/// How an attempt ended, for [`Shared::finish`].
struct Ended {
    fence: Fence,
    run: u64,
    kind: Kind,
    max_attempts: u32,
    result: JobResult,
    progress: Option<f64>,
    stage: Option<String>,
    started: Instant,
}

fn log_outcome(row: &JobRow, change: &Change, elapsed: Duration) {
    let duration_ms = u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX);
    match change {
        Change::Succeeded => {
            tracing::debug!(job_id = row.id, kind = %row.kind, duration_ms, "job succeeded");
        }
        Change::Requeue {
            interrupted: true, ..
        } => tracing::info!(
            job_id = row.id,
            kind = %row.kind,
            "job interrupted by the shutdown; it runs again at the next start"
        ),
        Change::Requeue { .. } => {
            tracing::debug!(job_id = row.id, kind = %row.kind, run_at = row.run_at, "job queued again");
        }
        Change::Retry { code, .. } => tracing::info!(
            job_id = row.id,
            kind = %row.kind,
            attempts = row.attempts,
            error_code = %code,
            run_at = row.run_at,
            "job try failed; retrying after a backoff"
        ),
        Change::Fail { code, .. } => tracing::warn!(
            job_id = row.id,
            kind = %row.kind,
            attempts = row.attempts,
            error_code = %code,
            duration_ms,
            "job failed"
        ),
    }
}

/// Runs one attempt: the worker in a task of its own, the lease renewals
/// beside it, then the outcome.
async fn supervise(shared: Arc<Shared>, state: AppState, kind: Kind, attempt: Attempt, run: u64) {
    let started = Instant::now();
    let fence = attempt.row.fence();
    let max_attempts = attempt.row.max_attempts;
    let mut written = attempt.row.lease_until.unwrap_or(0);
    let alive = Arc::clone(&attempt.alive);
    shared.publish(&attempt.row);
    let ctx = JobContext::new(Arc::clone(&shared), state, attempt);
    let worker = tokio::spawn(kind.worker().run(ctx.clone()));
    shared.set_worker(fence.id, run, worker.abort_handle());
    let mut worker = AbortOnDrop(worker);

    let lease = kind.spec().lease;
    let every = (lease / 3).max(Duration::from_millis(1));
    let mut renewals = interval_at(Instant::now() + every, every);
    renewals.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let joined = loop {
        tokio::select! {
            joined = &mut worker.0 => break joined,
            _ = renewals.tick() => shared.renew(fence, &alive, lease, &mut written).await,
        }
    };
    let result = match joined {
        Ok(result) => result,
        Err(err) if err.is_panic() => {
            tracing::error!(
                job_id = fence.id,
                kind = kind.name(),
                "a job worker panicked"
            );
            Err(JobError::transient(codes::INTERNAL).with_detail("the worker panicked"))
        }
        // Aborted after its lease expired; the watchdog recorded it.
        Err(_) => Err(JobError::transient(codes::INTERNAL).with_detail("the worker was stopped")),
    };
    let (progress, stage) = ctx.last_progress();
    drop(ctx);
    shared
        .finish(Ended {
            fence,
            run,
            kind,
            max_attempts,
            result,
            progress,
            stage,
            started,
        })
        .await;
}

/// The dispatcher: runs until `token` fires, then waits for the
/// supervisors, which stop their workers and re-queue what they interrupt.
pub(super) async fn run(shared: Arc<Shared>, state: AppState, token: CancellationToken) {
    let _running = ClearOnDrop(&shared.started);
    shared.boot().await;
    let mut tasks: JoinSet<()> = JoinSet::new();
    let mut watchdog = interval(WATCHDOG_INTERVAL);
    watchdog.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut sweeper = interval_at(Instant::now() + SWEEP_INTERVAL, SWEEP_INTERVAL);
    sweeper.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut nightly_ms = next_daily(shared.clock.now_ms(), NIGHTLY_AT);
    loop {
        shared.dispatch(&state, &token, &mut tasks).await;
        let delayed = shared.queues().next_delayed();
        let nightly_at = shared.clock.instant_at(nightly_ms);
        tokio::select! {
            biased;
            () = token.cancelled() => break,
            Some(joined) = tasks.join_next(), if !tasks.is_empty() => {
                if let Err(err) = joined
                    && err.is_panic()
                {
                    tracing::error!("a job scheduler task panicked");
                }
            }
            () = shared.wake.notified() => {}
            () = sleep_until(delayed.map_or(nightly_at, |at| shared.clock.instant_at(at))), if delayed.is_some() => {}
            _ = watchdog.tick() => shared.expire_leases().await,
            _ = sweeper.tick() => {
                if !shared.sweeping.swap(true, Ordering::AcqRel) {
                    tasks.spawn(sweep(Arc::clone(&shared), state.clone()));
                }
            }
            () = sleep_until(nightly_at) => {
                let now = shared.clock.now_ms();
                if now >= nightly_ms {
                    nightly_ms = next_daily(now, NIGHTLY_AT);
                    tasks.spawn(nightly(Arc::clone(&shared)));
                }
            }
        }
    }
    while tasks.join_next().await.is_some() {}
}

/// The nightly schedule (03:00 UTC): pruning, then the nightly kinds for
/// every active user.
async fn nightly(shared: Arc<Shared>) {
    shared.prune().await;
    let kinds: Vec<&'static str> = shared
        .registry
        .kinds()
        .filter(|kind| kind.spec().nightly)
        .map(Kind::name)
        .collect();
    if kinds.is_empty() {
        return;
    }
    let users = match shared
        .control(|control| control.read(rows::active_users))
        .await
    {
        Ok(users) => users,
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "listing users for the nightly jobs failed");
            return;
        }
    };
    for user in &users {
        if shared.shutting_down() {
            return;
        }
        for &kind in &kinds {
            let job = NewJob::new(user.as_str(), kind).dedupe(NIGHTLY_DEDUPE);
            if let Err(err) = shared.enqueue(job).await {
                tracing::warn!(kind, error = %err, "enqueuing a nightly job failed");
            }
        }
    }
}

/// The drain sweeper (every 10 minutes): for each drain kind and each active
/// user without an active job of it, its check says whether work is
/// pending; if so the drain is enqueued (dedupe key = the kind).
async fn sweep(shared: Arc<Shared>, state: AppState) {
    let _sweeping = ClearOnDrop(&shared.sweeping);
    let drains: Vec<Kind> = shared
        .registry
        .kinds()
        .filter(|kind| kind.sweep().is_some())
        .cloned()
        .collect();
    if drains.is_empty() {
        return;
    }
    let users = match shared
        .control(|control| control.read(rows::active_users))
        .await
    {
        Ok(users) => users,
        Err(err) => {
            tracing::warn!(error = %format!("{err:#}"), "listing users for the drain sweep failed");
            return;
        }
    };
    for user in &users {
        if shared.shutting_down() {
            return;
        }
        for kind in &drains {
            let (owner, name) = (user.clone(), kind.name());
            match shared
                .control(move |control| control.read(|conn| rows::has_active(conn, &owner, name)))
                .await
            {
                Ok(false) => {}
                Ok(true) => continue,
                Err(err) => {
                    tracing::warn!(kind = name, error = %format!("{err:#}"), "the drain sweep failed");
                    continue;
                }
            }
            let Some(check) = kind.sweep() else {
                continue;
            };
            let ctx = SweepContext::new(state.clone(), user, name, shared.clock.now_ms());
            match check.pending(ctx).await {
                Ok(Some(run_at)) => {
                    let job = NewJob::new(user.as_str(), name).dedupe(name).run_at(run_at);
                    if let Err(err) = shared.enqueue(job).await {
                        tracing::warn!(kind = name, error = %err, "enqueuing a drain failed");
                    }
                }
                Ok(None) => {}
                Err(err) => {
                    tracing::warn!(kind = name, error_code = err.code(), "a drain check failed");
                }
            }
        }
    }
}
