//! `jobs` and `queue_state` (plan §2.6, §2.12): the durable side of the job
//! system ([`crate::jobs`]).
//!
//! Every change of state is a conditional `UPDATE`, so the database settles
//! races:
//!
//! - a claim moves a job from `queued` to `running` only while it is still
//!   queued, due and its queue is not paused;
//! - the writes of a running attempt (lease renewals, progress, its outcome)
//!   apply only while the row is still that attempt, which a [`Fence`] names:
//!   `state = 'running'` with the `attempts` it was claimed with. A cancel
//!   changes `state`, an expired lease bumps `attempts`, so a stale attempt
//!   can never overwrite a newer one.
//!
//! `attempts` counts the tries that ended without success (a transient error
//! or an expired lease); a job fails for good when it reaches
//! `max_attempts`. Boot recovery and a shutdown that interrupts a job do not
//! count. `jobs_active_dedupe` keeps one active (`queued` or `running`) job
//! per user, kind and dedupe key.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, Row, params, params_from_iter};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use crate::events::model::JobState;

/// One `jobs` row.
#[derive(Clone, Debug, PartialEq)]
pub struct JobRow {
    /// Job id.
    pub id: i64,
    /// The user the job works for.
    pub user_id: String,
    /// Its kind (`archive.drain`, `migrate`, …).
    pub kind: String,
    /// At most one active job per user, kind and key.
    pub dedupe_key: Option<String>,
    /// State.
    pub state: JobState,
    /// Lower runs first, within one user's queue of a kind.
    pub priority: i64,
    /// The worker's input, as JSON.
    pub payload_json: String,
    /// Tries that ended without success.
    pub attempts: u32,
    /// Tries allowed.
    pub max_attempts: u32,
    /// Not before this time, unix ms.
    pub run_at: i64,
    /// While running: the attempt is presumed dead after this time.
    pub lease_until: Option<i64>,
    /// Progress from 0 to 1, when known.
    pub progress: Option<f64>,
    /// Current stage, a code.
    pub stage: Option<String>,
    /// Why the last try failed, a code.
    pub error_code: Option<String>,
    /// Developer-facing detail of that failure; never sent to clients.
    pub error_detail: Option<String>,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// Last change, unix ms.
    pub updated_at: i64,
    /// When the job reached a final state, unix ms.
    pub finished_at: Option<i64>,
}

impl JobRow {
    /// The attempt this row is running, for fenced writes.
    #[must_use]
    pub fn fence(&self) -> Fence {
        Fence {
            id: self.id,
            attempts: self.attempts,
        }
    }
}

/// A running attempt: the job id and the `attempts` it was claimed with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fence {
    /// Job id.
    pub id: i64,
    /// `attempts` when the attempt started.
    pub attempts: u32,
}

/// The stored value of `state`.
#[must_use]
pub const fn state_str(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Running => "running",
        JobState::Succeeded => "succeeded",
        JobState::Failed => "failed",
        JobState::Cancelled => "cancelled",
    }
}

/// The state stored as `value`.
#[must_use]
pub fn parse_state(value: &str) -> Option<JobState> {
    match value {
        "queued" => Some(JobState::Queued),
        "running" => Some(JobState::Running),
        "succeeded" => Some(JobState::Succeeded),
        "failed" => Some(JobState::Failed),
        "cancelled" => Some(JobState::Cancelled),
        _ => None,
    }
}

const COLUMNS: &str = "id, user_id, kind, dedupe_key, state, priority, payload_json, attempts, \
                       max_attempts, run_at, lease_until, progress, stage, error_code, \
                       error_detail, created_at, updated_at, finished_at";

/// The final states, as SQL.
const FINAL: &str = "('succeeded', 'failed', 'cancelled')";

fn from_row(row: &Row<'_>) -> rusqlite::Result<JobRow> {
    let state: String = row.get(4)?;
    Ok(JobRow {
        id: row.get(0)?,
        user_id: row.get(1)?,
        kind: row.get(2)?,
        dedupe_key: row.get(3)?,
        // The CHECK constraint admits only the five states.
        state: parse_state(&state).unwrap_or(JobState::Failed),
        priority: row.get(5)?,
        payload_json: row.get(6)?,
        attempts: row.get(7)?,
        max_attempts: row.get(8)?,
        run_at: row.get(9)?,
        lease_until: row.get(10)?,
        progress: row.get(11)?,
        stage: row.get(12)?,
        error_code: row.get(13)?,
        error_detail: row.get(14)?,
        created_at: row.get(15)?,
        updated_at: row.get(16)?,
        finished_at: row.get(17)?,
    })
}

/// A job to insert.
#[derive(Clone, Copy, Debug)]
pub struct NewJobRow<'a> {
    /// The user.
    pub user_id: &'a str,
    /// The kind.
    pub kind: &'a str,
    /// At most one active job per user, kind and key.
    pub dedupe_key: Option<&'a str>,
    /// Lower runs first.
    pub priority: i64,
    /// The worker's input, as JSON.
    pub payload_json: &'a str,
    /// Tries allowed.
    pub max_attempts: u32,
    /// Not before this time, unix ms.
    pub run_at: i64,
}

/// What [`insert`] did.
#[derive(Clone, Debug, PartialEq)]
pub enum Inserted {
    /// A new job.
    Created(JobRow),
    /// An active job with the same dedupe key already existed; when it was
    /// waiting for a later `run_at`, it now runs at the earlier one.
    Existing(JobRow),
}

/// Inserts a queued job, or finds the active job with the same dedupe key.
///
/// # Errors
///
/// The query failed (a missing user is a foreign-key error).
pub fn insert(conn: &Connection, new: &NewJobRow<'_>, now: i64) -> Result<Inserted> {
    let created = conn
        .query_row(
            &format!(
                "INSERT INTO jobs (user_id, kind, dedupe_key, state, priority, payload_json, \
                 max_attempts, run_at, created_at, updated_at) \
                 VALUES (?1, ?2, ?3, 'queued', ?4, ?5, ?6, ?7, ?8, ?8) \
                 ON CONFLICT DO NOTHING RETURNING {COLUMNS}"
            ),
            params![
                new.user_id,
                new.kind,
                new.dedupe_key,
                new.priority,
                new.payload_json,
                new.max_attempts,
                new.run_at,
                now
            ],
            from_row,
        )
        .optional()?;
    if let Some(row) = created {
        return Ok(Inserted::Created(row));
    }
    // Only `jobs_active_dedupe` can conflict: the id is new.
    let existing = conn
        .query_row(
            &format!(
                "SELECT {COLUMNS} FROM jobs WHERE user_id = ?1 AND kind = ?2 AND dedupe_key = ?3 \
                 AND state IN ('queued', 'running')"
            ),
            params![new.user_id, new.kind, new.dedupe_key],
            from_row,
        )
        .optional()?
        .ok_or(RepoError::Conflict("job"))?;
    if existing.state == JobState::Queued && new.run_at < existing.run_at {
        // New work arrived before the waiting job's timer: run it sooner.
        let pulled = conn.query_row(
            &format!(
                "UPDATE jobs SET run_at = ?2, updated_at = ?3 WHERE id = ?1 AND state = 'queued' \
                 RETURNING {COLUMNS}"
            ),
            params![existing.id, new.run_at, now],
            from_row,
        )?;
        return Ok(Inserted::Existing(pulled));
    }
    Ok(Inserted::Existing(existing))
}

/// The job `id` of `user_id`.
///
/// # Errors
///
/// The query failed.
pub fn get(conn: &Connection, user_id: &str, id: i64) -> Result<Option<JobRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1 AND user_id = ?2"),
        params![id, user_id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Which jobs [`list`] returns.
#[derive(Clone, Copy, Debug, Default)]
pub struct ListFilter<'a> {
    /// Only these kinds; every kind when empty.
    pub kinds: &'a [String],
    /// Only these states; every state when empty.
    pub states: &'a [JobState],
    /// Only jobs with a smaller id (the keyset cursor).
    pub before: Option<i64>,
    /// At most this many.
    pub limit: u32,
}

/// The jobs of `user_id`, newest first.
///
/// # Errors
///
/// The query failed.
pub fn list(conn: &Connection, user_id: &str, filter: &ListFilter<'_>) -> Result<Vec<JobRow>> {
    let mut sql = format!("SELECT {COLUMNS} FROM jobs WHERE user_id = ?");
    let mut values = vec![Value::from(user_id.to_owned())];
    if !filter.kinds.is_empty() {
        sql.push_str(&format!(" AND kind IN ({})", marks(filter.kinds.len())));
        values.extend(filter.kinds.iter().cloned().map(Value::from));
    }
    if !filter.states.is_empty() {
        sql.push_str(&format!(" AND state IN ({})", marks(filter.states.len())));
        values.extend(
            filter
                .states
                .iter()
                .map(|s| Value::from(state_str(*s).to_owned())),
        );
    }
    if let Some(before) = filter.before {
        sql.push_str(" AND id < ?");
        values.push(Value::from(before));
    }
    sql.push_str(" ORDER BY id DESC LIMIT ?");
    values.push(Value::from(i64::from(filter.limit)));
    let mut statement = conn.prepare(&sql)?;
    let rows = statement
        .query_map(params_from_iter(values), from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn marks(n: usize) -> String {
    vec!["?"; n].join(", ")
}

/// Jobs of one kind of a user, by state.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct KindCounts {
    /// The kind.
    pub kind: String,
    /// Waiting to run.
    pub queued: u64,
    /// Running.
    pub running: u64,
    /// Finished.
    pub succeeded: u64,
    /// Failed for good.
    pub failed: u64,
    /// Cancelled.
    pub cancelled: u64,
}

impl KindCounts {
    fn add(&mut self, state: JobState, count: u64) {
        let slot = match state {
            JobState::Queued => &mut self.queued,
            JobState::Running => &mut self.running,
            JobState::Succeeded => &mut self.succeeded,
            JobState::Failed => &mut self.failed,
            JobState::Cancelled => &mut self.cancelled,
        };
        *slot += count;
    }
}

/// The jobs of `user_id` counted by kind and state, ordered by kind; only
/// kinds with jobs, or only `kind` when given.
///
/// # Errors
///
/// The query failed.
pub fn counts(conn: &Connection, user_id: &str, kind: Option<&str>) -> Result<Vec<KindCounts>> {
    let mut statement = conn.prepare_cached(
        "SELECT kind, state, COUNT(*) FROM jobs WHERE user_id = ?1 AND (?2 IS NULL OR kind = ?2) \
         GROUP BY kind, state ORDER BY kind",
    )?;
    let mut rows = statement.query(params![user_id, kind])?;
    let mut out: Vec<KindCounts> = Vec::new();
    while let Some(row) = rows.next()? {
        let kind: String = row.get(0)?;
        let state: String = row.get(1)?;
        let count = u64::try_from(row.get::<_, i64>(2)?).unwrap_or(0);
        if out.last().is_none_or(|last| last.kind != kind) {
            out.push(KindCounts {
                kind,
                ..KindCounts::default()
            });
        }
        if let (Some(last), Some(state)) = (out.last_mut(), parse_state(&state)) {
            last.add(state, count);
        }
    }
    Ok(out)
}

/// Claims the queued job `id` for an attempt: `running`, leased until
/// `lease_until`. `None` when it is no longer queued, not due at `now`, or
/// its queue is paused.
///
/// # Errors
///
/// The query failed.
pub fn claim(conn: &Connection, id: i64, lease_until: i64, now: i64) -> Result<Option<JobRow>> {
    conn.query_row(
        &format!(
            "UPDATE jobs SET state = 'running', lease_until = ?2, updated_at = ?3 \
             WHERE id = ?1 AND state = 'queued' AND run_at <= ?3 \
             AND NOT EXISTS (SELECT 1 FROM queue_state q WHERE q.user_id = jobs.user_id \
                             AND q.kind = jobs.kind AND q.paused = 1) \
             RETURNING {COLUMNS}"
        ),
        params![id, lease_until, now],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// The job `id` of any user, for the scheduler.
///
/// # Errors
///
/// The query failed.
pub fn find(conn: &Connection, id: i64) -> Result<Option<JobRow>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM jobs WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Extends the lease of the attempt `fence`. False when the row is no longer
/// that attempt.
///
/// # Errors
///
/// The query failed.
pub fn renew(conn: &Connection, fence: Fence, lease_until: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE jobs SET lease_until = ?3 WHERE id = ?1 AND state = 'running' AND attempts = ?2",
        params![fence.id, fence.attempts, lease_until],
    )?;
    Ok(changed == 1)
}

/// Records the progress of the attempt `fence`. False when the row is no
/// longer that attempt.
///
/// # Errors
///
/// The query failed.
pub fn set_progress(
    conn: &Connection,
    fence: Fence,
    progress: Option<f64>,
    stage: Option<&str>,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE jobs SET progress = ?3, stage = ?4, updated_at = ?5 \
         WHERE id = ?1 AND state = 'running' AND attempts = ?2",
        params![fence.id, fence.attempts, progress, stage, now],
    )?;
    Ok(changed == 1)
}

/// How an attempt ended.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Finish<'a> {
    /// Done: `succeeded`, progress 1, the error of an earlier try cleared.
    Succeeded,
    /// Back to `queued` at `run_at`, without using a try (a drain re-arming
    /// itself, a yield at a pause, an interruption by a shutdown).
    Requeue {
        /// When to run again.
        run_at: i64,
    },
    /// The try failed and tries are left: back to `queued` at `run_at`.
    Retry {
        /// When to run again (the backoff).
        run_at: i64,
        /// Why it failed.
        code: &'a str,
        /// Developer-facing detail.
        detail: Option<&'a str>,
    },
    /// Failed for good.
    Fail {
        /// Why.
        code: &'a str,
        /// Developer-facing detail.
        detail: Option<&'a str>,
    },
}

/// Ends the attempt `fence` as `finish` says, keeping its last `progress`
/// and `stage`. `None` when the row is no longer that attempt (cancelled, or
/// taken back by the lease watchdog), which leaves it unchanged.
///
/// # Errors
///
/// The query failed.
pub fn finish(
    conn: &Connection,
    fence: Fence,
    finish: &Finish<'_>,
    progress: Option<f64>,
    stage: Option<&str>,
    now: i64,
) -> Result<Option<JobRow>> {
    let fenced =
        format!("WHERE id = ?1 AND state = 'running' AND attempts = ?2 RETURNING {COLUMNS}");
    let row = match *finish {
        Finish::Succeeded => conn.query_row(
            &format!(
                "UPDATE jobs SET state = 'succeeded', lease_until = NULL, progress = 1.0, \
                 stage = ?3, error_code = NULL, error_detail = NULL, finished_at = ?4, \
                 updated_at = ?4 {fenced}"
            ),
            params![fence.id, fence.attempts, stage, now],
            from_row,
        ),
        Finish::Requeue { run_at } => conn.query_row(
            &format!(
                "UPDATE jobs SET state = 'queued', lease_until = NULL, run_at = ?3, \
                 progress = ?4, stage = ?5, updated_at = ?6 {fenced}"
            ),
            params![fence.id, fence.attempts, run_at, progress, stage, now],
            from_row,
        ),
        Finish::Retry {
            run_at,
            code,
            detail,
        } => conn.query_row(
            &format!(
                "UPDATE jobs SET state = 'queued', lease_until = NULL, run_at = ?3, \
                 attempts = attempts + 1, error_code = ?4, error_detail = ?5, progress = ?6, \
                 stage = ?7, updated_at = ?8 {fenced}"
            ),
            params![
                fence.id,
                fence.attempts,
                run_at,
                code,
                detail,
                progress,
                stage,
                now
            ],
            from_row,
        ),
        Finish::Fail { code, detail } => conn.query_row(
            &format!(
                "UPDATE jobs SET state = 'failed', lease_until = NULL, attempts = attempts + 1, \
                 error_code = ?3, error_detail = ?4, progress = ?5, stage = ?6, \
                 finished_at = ?7, updated_at = ?7 {fenced}"
            ),
            params![fence.id, fence.attempts, code, detail, progress, stage, now],
            from_row,
        ),
    };
    row.optional().map_err(RepoError::from)
}

/// Cancels the job `id` of `user_id` if it is queued or running.
///
/// # Errors
///
/// The query failed.
pub fn cancel(conn: &Connection, user_id: &str, id: i64, now: i64) -> Result<Option<JobRow>> {
    conn.query_row(
        &format!(
            "UPDATE jobs SET state = 'cancelled', lease_until = NULL, finished_at = ?3, \
             updated_at = ?3 WHERE id = ?1 AND user_id = ?2 AND state IN ('queued', 'running') \
             RETURNING {COLUMNS}"
        ),
        params![id, user_id, now],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Cancels every queued or running job of one kind of `user_id`.
///
/// # Errors
///
/// The query failed.
pub fn cancel_kind(conn: &Connection, user_id: &str, kind: &str, now: i64) -> Result<Vec<JobRow>> {
    let mut statement = conn.prepare(&format!(
        "UPDATE jobs SET state = 'cancelled', lease_until = NULL, finished_at = ?3, \
         updated_at = ?3 WHERE user_id = ?1 AND kind = ?2 AND state IN ('queued', 'running') \
         RETURNING {COLUMNS}"
    ))?;
    let rows = statement
        .query_map(params![user_id, kind, now], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Queues the failed or cancelled job `id` of `user_id` again, with every try
/// available and its error cleared. `None` when the job is in another state.
///
/// # Errors
///
/// [`RepoError::Conflict`] when an active job with the same dedupe key
/// exists; query failures.
pub fn retry(conn: &Connection, user_id: &str, id: i64, now: i64) -> Result<Option<JobRow>> {
    conn.query_row(
        &format!(
            "UPDATE jobs SET state = 'queued', attempts = 0, run_at = ?3, lease_until = NULL, \
             progress = NULL, stage = NULL, error_code = NULL, error_detail = NULL, \
             finished_at = NULL, updated_at = ?3 \
             WHERE id = ?1 AND user_id = ?2 AND state IN ('failed', 'cancelled') \
             RETURNING {COLUMNS}"
        ),
        params![id, user_id, now],
        from_row,
    )
    .optional()
    .map_err(|e| conflict_on_unique(e, "job"))
}

/// Deletes the finished jobs of one kind of `user_id`; returns how many.
///
/// # Errors
///
/// The query failed.
pub fn clear_finished(conn: &Connection, user_id: &str, kind: &str) -> Result<u64> {
    let deleted = conn.execute(
        &format!("DELETE FROM jobs WHERE user_id = ?1 AND kind = ?2 AND state IN {FINAL}"),
        params![user_id, kind],
    )?;
    Ok(deleted as u64)
}

/// Deletes the jobs that finished before `before`; returns how many.
///
/// # Errors
///
/// The query failed.
pub fn prune_finished(conn: &Connection, before: i64) -> Result<u64> {
    let deleted = conn.execute(
        &format!("DELETE FROM jobs WHERE state IN {FINAL} AND finished_at < ?1"),
        [before],
    )?;
    Ok(deleted as u64)
}

/// Boot recovery: every `running` job goes back to `queued` (its process is
/// gone). The try is not counted. Returns how many.
///
/// # Errors
///
/// The query failed.
pub fn recover_running(conn: &Connection, now: i64) -> Result<u64> {
    let recovered = conn.execute(
        "UPDATE jobs SET state = 'queued', lease_until = NULL, updated_at = ?1 \
         WHERE state = 'running'",
        [now],
    )?;
    Ok(recovered as u64)
}

/// A queued job, as the scheduler keeps it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedJob {
    /// Job id.
    pub id: i64,
    /// The user.
    pub user_id: String,
    /// The kind.
    pub kind: String,
    /// Lower runs first.
    pub priority: i64,
    /// Not before this time.
    pub run_at: i64,
}

/// For each kind of `kinds`, the `run_at` of its oldest queued job that is
/// due at `now` and whose queue is not paused (`None` when there is none):
/// how long work has waited for a slot, for the metric
/// `shelfy_job_oldest_queued_seconds`. A paused queue waits on purpose, and
/// a job waiting for its `run_at` (a backoff, a drain's next item) is not
/// due. One `jobs_ready` index probe per kind.
///
/// # Errors
///
/// The query failed.
pub fn oldest_due<'k>(
    conn: &Connection,
    kinds: &[&'k str],
    now: i64,
) -> Result<Vec<(&'k str, Option<i64>)>> {
    let mut statement = conn.prepare_cached(
        "SELECT run_at FROM jobs j WHERE kind = ?1 AND state = 'queued' AND run_at <= ?2 \
         AND NOT EXISTS (SELECT 1 FROM queue_state q WHERE q.user_id = j.user_id \
                         AND q.kind = j.kind AND q.paused = 1) \
         ORDER BY run_at LIMIT 1",
    )?;
    kinds
        .iter()
        .map(|&kind| {
            let run_at = statement
                .query_row(params![kind, now], |row| row.get(0))
                .optional()?;
            Ok((kind, run_at))
        })
        .collect()
}

/// Every queued job, oldest first, for the scheduler at boot.
///
/// # Errors
///
/// The query failed.
pub fn queued(conn: &Connection) -> Result<Vec<QueuedJob>> {
    let mut statement = conn.prepare(
        "SELECT id, user_id, kind, priority, run_at FROM jobs WHERE state = 'queued' ORDER BY id",
    )?;
    let rows = statement
        .query_map([], |row| {
            Ok(QueuedJob {
                id: row.get(0)?,
                user_id: row.get(1)?,
                kind: row.get(2)?,
                priority: row.get(3)?,
                run_at: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Running jobs whose lease ended before `now`.
///
/// # Errors
///
/// The query failed.
pub fn expired(conn: &Connection, now: i64) -> Result<Vec<JobRow>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM jobs WHERE state = 'running' AND lease_until < ?1 ORDER BY id"
    ))?;
    let rows = statement
        .query_map([now], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Whether `user_id` has a queued or running job of `kind`.
///
/// # Errors
///
/// The query failed.
pub fn has_active(conn: &Connection, user_id: &str, kind: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE user_id = ?1 AND kind = ?2 \
         AND state IN ('queued', 'running'))",
        params![user_id, kind],
        |row| row.get(0),
    )
    .map_err(RepoError::from)
}

/// Whether `user_id` has a queued or running job of `kind` created before
/// the job `before`: a smaller id, since a new job's id is above every id in
/// the table when it is created.
///
/// # Errors
///
/// The query failed.
pub fn has_active_before(
    conn: &Connection,
    user_id: &str,
    kind: &str,
    before: i64,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM jobs WHERE user_id = ?1 AND kind = ?2 \
         AND state IN ('queued', 'running') AND id < ?3)",
        params![user_id, kind, before],
        |row| row.get(0),
    )
    .map_err(RepoError::from)
}

/// The active users, for the nightly schedule and the drain sweeper.
///
/// # Errors
///
/// The query failed.
pub fn active_users(conn: &Connection) -> Result<Vec<String>> {
    let mut statement = conn.prepare("SELECT id FROM users WHERE status = 'active' ORDER BY id")?;
    let ids = statement
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(ids)
}

/// Pauses (or resumes) one kind of `user_id`'s jobs; returns whether the flag
/// changed.
///
/// # Errors
///
/// The query failed.
pub fn set_paused(conn: &Connection, user_id: &str, kind: &str, paused: bool) -> Result<bool> {
    let changed = if paused {
        conn.execute(
            "INSERT INTO queue_state (user_id, kind, paused) VALUES (?1, ?2, 1) \
             ON CONFLICT (user_id, kind) DO UPDATE SET paused = 1 WHERE paused = 0",
            params![user_id, kind],
        )?
    } else {
        // A resumed queue keeps no row.
        conn.execute(
            "DELETE FROM queue_state WHERE user_id = ?1 AND kind = ?2",
            params![user_id, kind],
        )?
    };
    Ok(changed > 0)
}

/// The kinds `user_id` paused.
///
/// # Errors
///
/// The query failed.
pub fn paused_kinds(conn: &Connection, user_id: &str) -> Result<Vec<String>> {
    let mut statement = conn
        .prepare("SELECT kind FROM queue_state WHERE user_id = ?1 AND paused = 1 ORDER BY kind")?;
    let kinds = statement
        .query_map([user_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?;
    Ok(kinds)
}

/// Every paused queue, as `(user, kind)`, for the scheduler at boot.
///
/// # Errors
///
/// The query failed.
pub fn paused_all(conn: &Connection) -> Result<Vec<(String, String)>> {
    let mut statement = conn.prepare("SELECT user_id, kind FROM queue_state WHERE paused = 1")?;
    let pairs = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(pairs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    fn new_job<'a>(user: &'a str, kind: &'a str, dedupe: Option<&'a str>) -> NewJobRow<'a> {
        NewJobRow {
            user_id: user,
            kind,
            dedupe_key: dedupe,
            priority: 100,
            payload_json: "{}",
            max_attempts: 3,
            run_at: NOW,
        }
    }

    fn created(inserted: Inserted) -> JobRow {
        match inserted {
            Inserted::Created(row) => row,
            Inserted::Existing(row) => panic!("expected a new job, found {}", row.id),
        }
    }

    #[test]
    fn one_active_job_per_dedupe_key() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            let first = created(insert(tx, &new_job(&owner, "test.drain", Some("d")), NOW)?);
            assert_eq!(first.state, JobState::Queued);
            assert_eq!((first.attempts, first.max_attempts), (0, 3));
            assert_eq!(first.payload_json, "{}");

            // Same key: the active job comes back; an earlier run_at pulls it in.
            let later = NewJobRow {
                run_at: NOW + 60_000,
                ..new_job(&owner, "test.drain", Some("d"))
            };
            assert_eq!(insert(tx, &later, NOW)?, Inserted::Existing(first.clone()));
            let sooner = NewJobRow {
                run_at: NOW - 1,
                ..new_job(&owner, "test.drain", Some("d"))
            };
            let Inserted::Existing(pulled) = insert(tx, &sooner, NOW + 5)? else {
                panic!("expected the existing job")
            };
            assert_eq!((pulled.id, pulled.run_at), (first.id, NOW - 1));

            // Another user, another kind, or no key: new jobs.
            created(insert(tx, &new_job(&member, "test.drain", Some("d")), NOW)?);
            created(insert(tx, &new_job(&owner, "test.other", Some("d")), NOW)?);
            created(insert(tx, &new_job(&owner, "test.drain", None), NOW)?);
            created(insert(tx, &new_job(&owner, "test.drain", None), NOW)?);

            // Once the first one is over, the key is free again.
            cancel(tx, &owner, first.id, NOW)?.expect("cancelled");
            let again = created(insert(tx, &new_job(&owner, "test.drain", Some("d")), NOW)?);
            assert_ne!(again.id, first.id);
            // A retry of the old one would clash with the new one.
            assert!(matches!(
                retry(tx, &owner, first.id, NOW),
                Err(RepoError::Conflict("job"))
            ));
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn claims_are_conditional_and_attempts_are_fenced() {
        let (db, owner, _) = control_with_users();
        db.write(|tx| {
            let job = created(insert(tx, &new_job(&owner, "test.k", None), NOW)?);
            let future = created(insert(
                tx,
                &NewJobRow {
                    run_at: NOW + 1_000,
                    ..new_job(&owner, "test.k", None)
                },
                NOW,
            )?);
            assert_eq!(claim(tx, future.id, NOW + 30_000, NOW)?, None, "not due");

            set_paused(tx, &owner, "test.k", true)?;
            assert_eq!(claim(tx, job.id, NOW + 30_000, NOW)?, None, "paused");
            assert!(set_paused(tx, &owner, "test.k", false)?);
            assert!(!set_paused(tx, &owner, "test.k", false)?, "already resumed");

            let running = claim(tx, job.id, NOW + 30_000, NOW)?.expect("claimed");
            assert_eq!(running.state, JobState::Running);
            assert_eq!(running.lease_until, Some(NOW + 30_000));
            assert_eq!(claim(tx, job.id, NOW + 30_000, NOW)?, None, "claimed once");

            let fence = running.fence();
            assert!(renew(tx, fence, NOW + 60_000)?);
            assert!(set_progress(tx, fence, Some(0.5), Some("fetch"), NOW)?);
            let retried = finish(
                tx,
                fence,
                &Finish::Retry {
                    run_at: NOW + 1_000,
                    code: "unavailable",
                    detail: Some("busy"),
                },
                Some(0.5),
                Some("fetch"),
                NOW,
            )?
            .expect("finished");
            assert_eq!(retried.state, JobState::Queued);
            assert_eq!(retried.attempts, 1);
            assert_eq!(retried.error_code.as_deref(), Some("unavailable"));
            assert_eq!(retried.lease_until, None);

            // The old attempt can no longer write anything.
            assert!(!renew(tx, fence, NOW + 90_000)?);
            assert!(!set_progress(tx, fence, Some(0.9), None, NOW)?);
            assert_eq!(
                finish(tx, fence, &Finish::Succeeded, None, None, NOW)?,
                None
            );

            let second = claim(tx, job.id, NOW + 31_000, NOW + 1_000)?.expect("claimed");
            assert_eq!(second.attempts, 1);
            let done = finish(
                tx,
                second.fence(),
                &Finish::Succeeded,
                Some(0.7),
                None,
                NOW + 2_000,
            )?
            .expect("finished");
            assert_eq!(done.state, JobState::Succeeded);
            assert_eq!(done.progress, Some(1.0));
            assert_eq!(done.finished_at, Some(NOW + 2_000));
            assert_eq!((done.attempts, done.error_code), (1, None));
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn a_cancel_beats_the_running_attempt() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            let job = created(insert(tx, &new_job(&owner, "test.k", None), NOW)?);
            let running = claim(tx, job.id, NOW + 30_000, NOW)?.expect("claimed");
            assert_eq!(
                cancel(tx, &member, job.id, NOW)?,
                None,
                "another user's job"
            );
            let cancelled = cancel(tx, &owner, job.id, NOW)?.expect("cancelled");
            assert_eq!(cancelled.state, JobState::Cancelled);
            assert_eq!(cancelled.finished_at, Some(NOW));
            assert_eq!(
                finish(
                    tx,
                    running.fence(),
                    &Finish::Fail {
                        code: "x",
                        detail: None
                    },
                    None,
                    None,
                    NOW
                )?,
                None
            );
            assert_eq!(cancel(tx, &owner, job.id, NOW)?, None, "already over");

            let again = retry(tx, &owner, job.id, NOW + 5)?.expect("queued again");
            assert_eq!(again.state, JobState::Queued);
            assert_eq!((again.attempts, again.finished_at), (0, None));
            assert_eq!(
                retry(tx, &owner, job.id, NOW)?,
                None,
                "only failed or cancelled"
            );
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn boot_recovery_expiry_and_pruning() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            let a = created(insert(tx, &new_job(&owner, "test.k", None), NOW)?);
            let b = created(insert(tx, &new_job(&member, "test.k", None), NOW)?);
            claim(tx, a.id, NOW + 10, NOW)?.expect("claimed");
            claim(tx, b.id, NOW + 50, NOW)?.expect("claimed");
            assert_eq!(
                expired(tx, NOW + 20)?
                    .iter()
                    .map(|r| r.id)
                    .collect::<Vec<_>>(),
                [a.id]
            );
            assert!(has_active(tx, &owner, "test.k")?);
            assert_eq!(recover_running(tx, NOW + 100)?, 2);
            let queued = queued(tx)?;
            assert_eq!(queued.len(), 2);
            assert_eq!(queued[0].id, a.id);
            assert_eq!(get(tx, &owner, a.id)?.unwrap().attempts, 0, "not counted");

            cancel(tx, &owner, a.id, NOW)?;
            cancel(tx, &member, b.id, NOW + 1_000)?;
            assert_eq!(prune_finished(tx, NOW + 1)?, 1);
            assert_eq!(get(tx, &owner, a.id)?, None);
            assert_eq!(clear_finished(tx, &member, "test.other")?, 0);
            assert_eq!(clear_finished(tx, &member, "test.k")?, 1);
            assert!(!has_active(tx, &owner, "test.k")?);
            assert_eq!(active_users(tx)?, [member.clone(), owner.clone()]);
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn lists_and_counts_are_per_user() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            let mut ids = Vec::new();
            for kind in ["a.one", "b.two", "a.one"] {
                ids.push(created(insert(tx, &new_job(&owner, kind, None), NOW)?).id);
            }
            created(insert(tx, &new_job(&member, "a.one", None), NOW)?);
            cancel(tx, &owner, ids[0], NOW)?;
            set_paused(tx, &owner, "b.two", true)?;

            let all = list(
                tx,
                &owner,
                &ListFilter {
                    limit: 10,
                    ..ListFilter::default()
                },
            )?;
            assert_eq!(
                all.iter().map(|r| r.id).collect::<Vec<_>>(),
                [ids[2], ids[1], ids[0]],
                "newest first, own jobs only"
            );
            let kinds = ["a.one".to_owned()];
            let states = [JobState::Queued];
            let filtered = list(
                tx,
                &owner,
                &ListFilter {
                    kinds: &kinds,
                    states: &states,
                    before: None,
                    limit: 10,
                },
            )?;
            assert_eq!(filtered.iter().map(|r| r.id).collect::<Vec<_>>(), [ids[2]]);
            let page = list(
                tx,
                &owner,
                &ListFilter {
                    before: Some(ids[1]),
                    limit: 10,
                    ..ListFilter::default()
                },
            )?;
            assert_eq!(page.iter().map(|r| r.id).collect::<Vec<_>>(), [ids[0]]);

            let counts = counts(tx, &owner, None)?;
            assert_eq!(
                counts,
                [
                    KindCounts {
                        kind: "a.one".into(),
                        queued: 1,
                        cancelled: 1,
                        ..KindCounts::default()
                    },
                    KindCounts {
                        kind: "b.two".into(),
                        queued: 1,
                        ..KindCounts::default()
                    },
                ]
            );
            assert_eq!(super::counts(tx, &owner, Some("b.two"))?.len(), 1);
            assert_eq!(paused_kinds(tx, &owner)?, ["b.two"]);
            assert!(paused_kinds(tx, &member)?.is_empty());
            assert_eq!(paused_all(tx)?, [(owner.clone(), "b.two".to_owned())]);
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn states_round_trip() {
        for state in [
            JobState::Queued,
            JobState::Running,
            JobState::Succeeded,
            JobState::Failed,
            JobState::Cancelled,
        ] {
            assert_eq!(parse_state(state_str(state)), Some(state));
            assert_eq!(
                serde_json::to_value(state).unwrap(),
                state_str(state),
                "the stored and the wire form agree"
            );
        }
        assert_eq!(parse_state("done"), None);
    }
}
