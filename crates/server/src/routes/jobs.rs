//! `/api/v1/jobs` and `/api/v1/queues`: the signed-in user's background jobs
//! and their queues (plan §2.9 Jobs, §2.12). The machinery is in
//! [`crate::jobs`].
//!
//! | Route | Answer |
//! |---|---|
//! | `GET /jobs?kind&state&limit&cursor` | the user's jobs, newest first |
//! | `GET /jobs/summary` | per kind: jobs by state, and whether the queue is paused |
//! | `POST /jobs/{id}/cancel` | the job, cancelled; a running worker is told to stop |
//! | `POST /jobs/{id}/retry` | the failed or cancelled job, queued again (`Idempotency-Key`) |
//! | `POST /queues/{kind}/pause`, `resume` | the queue: its jobs wait, or run again |
//! | `POST /queues/{kind}/cancel-all`, `clear-finished` | the queue, with how many jobs were cancelled or deleted |
//!
//! Every route needs a session; another user's job is a 404, like a missing
//! one. Changes also arrive as `job.updated` events.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::auth::no_store;
use super::listing::{MAX_VALUES, page_size};
use crate::control::jobs::{self as rows, JobRow, KindCounts, ListFilter};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::events::model::JobState;
use crate::extract::{Json, Path, Query};
use crate::jobs::idempotency::IdempotencyHeader;
use crate::jobs::row_post_key;
use crate::state::{AppState, blocking};

/// Prefix of the decoded cursor text, so a cursor of another route never
/// parses here.
const CURSOR_TAG: &str = "jobs.";
/// Longest cursor accepted, in characters.
const MAX_CURSOR_CHARS: usize = 64;
/// Longest kind accepted in a filter, in bytes.
const MAX_KIND_BYTES: usize = 64;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_jobs))
        .routes(routes!(jobs_summary))
        .routes(routes!(cancel_job))
        .routes(routes!(retry_job))
        .routes(routes!(pause_queue))
        .routes(routes!(resume_queue))
        .routes(routes!(cancel_queue))
        .routes(routes!(clear_queue))
}

/// A background job.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Job {
    /// Job id.
    pub id: i64,
    /// Kind (`archive.drain`, `migrate`, …).
    pub kind: String,
    /// State.
    pub state: JobState,
    /// Progress from 0 to 1, when known.
    #[schema(required = true)]
    pub progress: Option<f64>,
    /// Current stage, a code, when the job has stages.
    #[schema(required = true)]
    pub stage: Option<String>,
    /// The post the job works on, if one.
    #[schema(required = true)]
    pub post_key: Option<String>,
    /// Why the job failed (`failed`), or why its last try failed before a
    /// retry. A stable code (`lease_expired`, `unavailable`, …), never prose.
    #[schema(required = true)]
    pub error_code: Option<String>,
    /// Tries that ended without success.
    pub attempts: u32,
    /// Tries allowed before the job fails for good.
    pub max_attempts: u32,
    /// Not before this time (a delayed job, or the backoff before a retry).
    pub run_at: i64,
    /// Creation time.
    pub created_at: i64,
    /// Last change.
    pub updated_at: i64,
    /// When the job finished (succeeded, failed or cancelled).
    #[schema(required = true)]
    pub finished_at: Option<i64>,
}

impl From<JobRow> for Job {
    fn from(row: JobRow) -> Self {
        Self {
            post_key: row_post_key(&row.payload_json),
            id: row.id,
            kind: row.kind,
            state: row.state,
            progress: row.progress,
            stage: row.stage,
            error_code: row.error_code,
            attempts: row.attempts,
            max_attempts: row.max_attempts,
            run_at: row.run_at,
            created_at: row.created_at,
            updated_at: row.updated_at,
            finished_at: row.finished_at,
        }
    }
}

/// Filters and paging of `GET /api/v1/jobs`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, IntoParams)]
#[serde(default, rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct JobsQuery {
    /// Only jobs of these kinds. Repeatable: any of them.
    #[param(style = Form, explode)]
    pub kind: Vec<String>,
    /// Only jobs in these states. Repeatable: any of them.
    #[param(style = Form, explode)]
    pub state: Vec<JobState>,
    /// Page size, 1–200 (larger values are clamped). Default 60.
    #[param(minimum = 1, maximum = 200)]
    pub limit: Option<u32>,
    /// `nextCursor` of the previous page.
    pub cursor: Option<String>,
}

/// One page of jobs, newest first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobPage {
    /// The jobs of this page.
    pub items: Vec<Job>,
    /// Pass it as `cursor` to get the next page; `null` on the last page.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// One queue: a kind of job of the user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueSummary {
    /// The kind.
    pub kind: String,
    /// Whether the user paused it: its queued jobs wait.
    pub paused: bool,
    /// Jobs waiting to run.
    pub queued: u64,
    /// Jobs running.
    pub running: u64,
    /// Jobs finished.
    pub succeeded: u64,
    /// Jobs failed for good.
    pub failed: u64,
    /// Jobs cancelled.
    pub cancelled: u64,
}

/// Every queue of the user: each kind this server runs, and any other kind
/// the user has jobs of.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct JobsSummary {
    /// The queues, by kind.
    pub queues: Vec<QueueSummary>,
}

/// The outcome of an action on a queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueResult {
    /// The queue after the action.
    pub queue: QueueSummary,
    /// What the action changed: 1 when pause or resume changed the queue
    /// (0 when it already was so), the jobs cancelled, or the jobs deleted.
    pub affected: u64,
}

/// The user's jobs, newest first.
#[utoipa::path(
    get,
    path = "/api/v1/jobs",
    tag = "jobs",
    operation_id = "listJobs",
    params(JobsQuery),
    responses(
        (status = OK, description = "One page of jobs.", body = JobPage),
    )
)]
pub async fn list_jobs(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<JobsQuery>,
) -> Result<Response, ApiError> {
    if query.kind.len() > MAX_VALUES {
        return Err(ApiError::invalid_field(
            "kind",
            format!("more than {MAX_VALUES} values"),
        ));
    }
    if query.kind.iter().any(|kind| kind.len() > MAX_KIND_BYTES) {
        return Err(ApiError::invalid_field("kind", "longer than 64 bytes"));
    }
    let limit = page_size(query.limit);
    let before = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let mut rows = blocking(move || {
        control.read(|conn| {
            let filter = ListFilter {
                kinds: &query.kind,
                states: &query.state,
                before,
                limit: limit + 1,
            };
            rows::list(conn, &user_id, &filter)
        })
    })
    .await?;
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next_cursor = more
        .then(|| rows.last().map(|row| encode_cursor(row.id)))
        .flatten();
    let page = JobPage {
        items: rows.into_iter().map(Job::from).collect(),
        next_cursor,
    };
    Ok(no_store(Json(page).into_response()))
}

/// Every queue of the user, with its jobs counted by state.
#[utoipa::path(
    get,
    path = "/api/v1/jobs/summary",
    tag = "jobs",
    operation_id = "getJobsSummary",
    responses(
        (status = OK, description = "The queues.", body = JobsSummary),
    )
)]
pub async fn jobs_summary(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let queues = summaries(&state, user.id(), None).await?;
    Ok(no_store(Json(JobsSummary { queues }).into_response()))
}

/// The queues of `user_id` (only `kind` when given): the registered kinds
/// and the kinds the user has jobs of, by name.
async fn summaries(
    state: &AppState,
    user_id: &str,
    kind: Option<&'static str>,
) -> Result<Vec<QueueSummary>, ApiError> {
    let control = Arc::clone(state.control());
    let user = user_id.to_owned();
    let (counts, paused) = blocking(move || {
        control.read(|conn| {
            let counts = rows::counts(conn, &user, kind)?;
            let paused = rows::paused_kinds(conn, &user)?;
            Ok::<_, shelfy_core::repo::RepoError>((counts, paused))
        })
    })
    .await?;
    let mut queues: BTreeMap<String, KindCounts> = state
        .jobs()
        .registry()
        .kinds()
        .map(|registered| registered.name())
        .filter(|name| kind.is_none_or(|kind| kind == *name))
        .map(|name| {
            let counts = KindCounts {
                kind: name.to_owned(),
                ..KindCounts::default()
            };
            (name.to_owned(), counts)
        })
        .collect();
    for counts in counts {
        queues.insert(counts.kind.clone(), counts);
    }
    Ok(queues
        .into_values()
        .map(|counts| QueueSummary {
            paused: paused.contains(&counts.kind),
            kind: counts.kind,
            queued: counts.queued,
            running: counts.running,
            succeeded: counts.succeeded,
            failed: counts.failed,
            cancelled: counts.cancelled,
        })
        .collect())
}

/// Cancels a queued or running job: it never runs, or its worker is told
/// to stop. Cancelling a cancelled job changes nothing; a job that already
/// succeeded or failed answers 409 `conflict`.
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/cancel",
    tag = "jobs",
    operation_id = "cancelJob",
    params(("id" = i64, Path, description = "The job's id.")),
    responses(
        (status = OK, description = "The job, cancelled.", body = Job),
    )
)]
pub async fn cancel_job(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Job>, ApiError> {
    let job = state.jobs().cancel(user.id(), id).await?;
    Ok(Json(job.into()))
}

/// Queues a failed or cancelled job again, with every try available and its
/// error cleared. Any other state answers 409 `conflict`, as does a job
/// whose duplicate is already queued or running.
///
/// Send an `Idempotency-Key` to make a repeated click retry once.
#[utoipa::path(
    post,
    path = "/api/v1/jobs/{id}/retry",
    tag = "jobs",
    operation_id = "retryJob",
    params(("id" = i64, Path, description = "The job's id."), IdempotencyHeader),
    responses(
        (
            status = OK,
            description = "The job, queued again; or, for a repeated `Idempotency-Key`, the first response.",
            body = Job,
            headers(
                ("Idempotent-Replayed" = String, description = "`true` on a replayed response."),
            )
        ),
    )
)]
pub async fn retry_job(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<i64>,
) -> Result<Json<Job>, ApiError> {
    let job = state.jobs().retry(user.id(), id).await?;
    Ok(Json(job.into()))
}

/// Pauses a queue: its queued jobs wait until it is resumed. Running jobs
/// go on; a long one (a drain) stops at its next step and waits too.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{kind}/pause",
    tag = "jobs",
    operation_id = "pauseQueue",
    params(("kind" = String, Path, description = "The kind of job, for example `archive.drain`.")),
    responses(
        (status = OK, description = "The queue, paused.", body = QueueResult),
    )
)]
pub async fn pause_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(kind): Path<String>,
) -> Result<Json<QueueResult>, ApiError> {
    let changed = state.jobs().pause(user.id(), &kind).await?;
    queue_result(&state, user.id(), &kind, u64::from(changed)).await
}

/// Resumes a paused queue.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{kind}/resume",
    tag = "jobs",
    operation_id = "resumeQueue",
    params(("kind" = String, Path, description = "The kind of job.")),
    responses(
        (status = OK, description = "The queue, running again.", body = QueueResult),
    )
)]
pub async fn resume_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(kind): Path<String>,
) -> Result<Json<QueueResult>, ApiError> {
    let changed = state.jobs().resume(user.id(), &kind).await?;
    queue_result(&state, user.id(), &kind, u64::from(changed)).await
}

/// Cancels every queued and running job of a queue.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{kind}/cancel-all",
    tag = "jobs",
    operation_id = "cancelQueue",
    params(("kind" = String, Path, description = "The kind of job.")),
    responses(
        (status = OK, description = "The queue; `affected` counts the jobs cancelled.", body = QueueResult),
    )
)]
pub async fn cancel_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(kind): Path<String>,
) -> Result<Json<QueueResult>, ApiError> {
    let cancelled = state.jobs().cancel_all(user.id(), &kind).await?;
    queue_result(&state, user.id(), &kind, cancelled).await
}

/// Deletes the finished (succeeded, failed and cancelled) jobs of a queue.
#[utoipa::path(
    post,
    path = "/api/v1/queues/{kind}/clear-finished",
    tag = "jobs",
    operation_id = "clearFinishedJobs",
    params(("kind" = String, Path, description = "The kind of job.")),
    responses(
        (status = OK, description = "The queue; `affected` counts the jobs deleted.", body = QueueResult),
    )
)]
pub async fn clear_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(kind): Path<String>,
) -> Result<Json<QueueResult>, ApiError> {
    let deleted = state.jobs().clear_finished(user.id(), &kind).await?;
    queue_result(&state, user.id(), &kind, deleted).await
}

/// The answer of a queue action on `kind`, which the action checked is
/// registered.
async fn queue_result(
    state: &AppState,
    user_id: &str,
    kind: &str,
    affected: u64,
) -> Result<Json<QueueResult>, ApiError> {
    let name = state
        .jobs()
        .registry()
        .get(kind)
        .map(crate::jobs::Kind::name)
        .ok_or_else(ApiError::not_found)?;
    let queue = summaries(state, user_id, Some(name))
        .await?
        .into_iter()
        .next()
        .ok_or_else(|| ApiError::internal(anyhow::anyhow!("no summary for a registered kind")))?;
    Ok(Json(QueueResult { queue, affected }))
}

fn encode_cursor(before: i64) -> String {
    URL_SAFE_NO_PAD.encode(format!("{CURSOR_TAG}{before}"))
}

fn decode_cursor(text: &str) -> Result<i64, ApiError> {
    let invalid = || ApiError::new(ErrorCode::InvalidCursor);
    if text.len() > MAX_CURSOR_CHARS {
        return Err(invalid());
    }
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
    let text = String::from_utf8(bytes).map_err(|_| invalid())?;
    text.strip_prefix(CURSOR_TAG)
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .ok_or_else(invalid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursors_round_trip_and_refuse_anything_else() {
        assert_eq!(decode_cursor(&encode_cursor(42)).unwrap(), 42);
        for bad in [
            String::new(),
            "not base64!".to_owned(),
            URL_SAFE_NO_PAD.encode("notifications.42"),
            URL_SAFE_NO_PAD.encode("jobs.x"),
            URL_SAFE_NO_PAD.encode("jobs.0"),
            "A".repeat(MAX_CURSOR_CHARS + 1),
        ] {
            let err = decode_cursor(&bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::InvalidCursor, "{bad}");
        }
    }
}
