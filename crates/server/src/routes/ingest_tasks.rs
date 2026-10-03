//! `GET /api/v1/ingest/tasks` and `POST /api/v1/ingest/tasks/{id}/complete`
//! (plan §2.13, §2.16; P2 contract C6; P2-14): what the browser extension
//! does for the archive, over [`crate::extension::tasks`].
//!
//! Both take the extension's `tasks` token and its version header (C1). The
//! poll is a long poll in the `streams` group: it answers at once when the
//! poller has tasks, else waits up to `wait` seconds for posts to go to the
//! extension, and ends when the server shuts down. The tasks it returns are
//! leased to the poller for 5 minutes. The uploads of `upload_media` go
//! through tus first (`POST /uploads`, purpose `archive-object`, `sha256`
//! and `ext` declared), then the completion names the upload.

use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};

use super::model::Platform;
use crate::auth::bearer::{TokenUser, scopes};
use crate::error::ApiError;
use crate::extension::ExtensionHeaders;
use crate::extension::tasks::{
    self, DEFAULT_LIMIT, MAX_LIMIT, MAX_WAIT_SECS, Task, TaskKind, TaskOutcome, Waiting,
};
use crate::extract::{Json, Path, Query};
use crate::state::AppState;

/// Query of `GET /ingest/tasks`.
#[derive(Clone, Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct TaskQuery {
    /// Seconds to wait for a task when none is ready (0–25; default 0).
    pub wait: Option<u64>,
    /// Most tasks to lease (1–50; default 20).
    pub limit: Option<usize>,
}

/// One task (contract C6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionTask {
    /// Its id, for the completion: stable while the work is the same.
    pub id: String,
    /// What to do.
    pub kind: TaskKind,
    /// The post's platform.
    pub platform: Platform,
    /// The post's key.
    pub post_key: String,
    /// The post's native id (Instagram's media pk, for `/api/v1/media/<pk>/info/`).
    pub native_id: String,
    /// Instagram's code.
    #[schema(required = true)]
    pub shortcode: Option<String>,
    /// The link to the post.
    pub post_url: String,
    /// `upload_media` of a slide: its position. `null` for the cover, and
    /// for the tasks about the whole post.
    #[schema(required = true)]
    pub position: Option<i64>,
    /// `upload_media`: the URL to fetch (`credentials: "omit"`) and upload.
    #[schema(required = true)]
    pub url: Option<String>,
    /// When `url` expires (`upload_media`), or the first expired URL
    /// (`refresh_media`), unix ms.
    #[schema(required = true)]
    pub expires_at: Option<i64>,
    /// Until when the task is this poller's, unix ms.
    pub lease_until: i64,
    /// Opaque generation of this token's lease; echo it on completion.
    pub lease_id: String,
}

impl ExtensionTask {
    fn of(task: &Task, lease_until: i64, lease_id: &str) -> Self {
        Self {
            id: task.id.to_string(),
            kind: task.id.kind,
            platform: task.platform.into(),
            post_key: task.id.key.clone(),
            native_id: task.native_id.clone(),
            shortcode: task.shortcode.clone(),
            post_url: task.post_url.clone(),
            position: task.position(),
            url: task.url.clone(),
            expires_at: task.expires_at,
            lease_until,
            lease_id: lease_id.to_owned(),
        }
    }
}

/// The answer of `GET /ingest/tasks` (contract C6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionTasks {
    /// The tasks leased to this poller.
    pub tasks: Vec<ExtensionTask>,
    /// Every task that waits, per platform, leased ones included.
    pub waiting: Waiting,
}

/// How a task ended (contract C6).
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct TaskCompletion {
    /// The `leaseId` returned with the task, bound to this token.
    pub lease_id: String,
    /// The outcome.
    pub outcome: TaskOutcome,
    /// `uploaded`: the complete `archive-object` upload holding the bytes.
    #[serde(default)]
    #[schema(required = false)]
    pub upload_id: Option<String>,
    /// `failed`, `skipped`: why, as a short code (`http_403`,
    /// `no_instagram_tab`); kept with the item's tries.
    #[serde(default)]
    #[schema(required = false)]
    pub error_code: Option<String>,
}

/// Leases the extension's tasks (contract C6).
///
/// A `tasks` token. Answers at once when tasks are ready for this poller,
/// else waits up to `wait` seconds for some, or until the server shuts
/// down. The tasks are leased to the token for 5 minutes; a poll by the same
/// token returns its leased tasks again, with the lease renewed.
#[utoipa::path(
    get,
    path = "/api/v1/ingest/tasks",
    tag = "extension",
    operation_id = "listExtensionTasks",
    security(("bearer" = ["tasks"])),
    params(TaskQuery, ExtensionHeaders),
    responses((status = OK, description = "The leased tasks.", body = ExtensionTasks)),
)]
pub async fn list_extension_tasks(
    State(state): State<AppState>,
    token: TokenUser<scopes::Tasks>,
    Query(query): Query<TaskQuery>,
) -> Result<Json<ExtensionTasks>, ApiError> {
    let wait = query.wait.unwrap_or(0);
    if wait > MAX_WAIT_SECS {
        return Err(ApiError::invalid_field(
            "wait",
            format!("at most {MAX_WAIT_SECS} seconds"),
        ));
    }
    let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
    if !(1..=MAX_LIMIT).contains(&limit) {
        return Err(ApiError::invalid_field(
            "limit",
            format!("from 1 to {MAX_LIMIT}"),
        ));
    }
    let polled = tasks::poll(
        &state,
        token.id(),
        token.token_id(),
        Duration::from_secs(wait),
        limit,
    )
    .await?;
    Ok(Json(ExtensionTasks {
        tasks: polled
            .tasks
            .iter()
            .map(|(task, until, lease)| ExtensionTask::of(task, *until, lease))
            .collect(),
        waiting: polled.waiting,
    }))
}

/// Ends one of the extension's tasks (contract C6).
///
/// A `tasks` token and its current `leaseId`. A stale or foreign lease gets
/// 409 `conflict`; a repeat by the holder is a no-op for five minutes after
/// completion. `uploaded` stores the upload `uploadId` (purpose
/// `archive-object`) in the task's slot; 422 when the upload is not a
/// complete one of the user, 409 `upload_consumed` when it was used before
/// and the slot is still empty. 422 for an outcome the task's kind does not
/// have; 404 for a text that is not a task id.
#[utoipa::path(
    post,
    path = "/api/v1/ingest/tasks/{id}/complete",
    tag = "extension",
    operation_id = "completeExtensionTask",
    security(("bearer" = ["tasks"])),
    params(
        ("id" = String, Path, description = "The task's id."),
        ExtensionHeaders,
    ),
    request_body = TaskCompletion,
    responses(
        (status = NO_CONTENT, description = "Done, obsolete or replayed by the lease holder."),
        (status = CONFLICT, description = "The lease is stale or belongs to another token, or the upload was consumed."),
    ),
)]
pub async fn complete_extension_task(
    State(state): State<AppState>,
    token: TokenUser<scopes::Tasks>,
    Path(id): Path<String>,
    Json(body): Json<TaskCompletion>,
) -> Result<StatusCode, ApiError> {
    tasks::complete(
        &state,
        token.id(),
        token.token_id(),
        &id,
        tasks::Completion {
            generation: &body.lease_id,
            outcome: body.outcome,
            upload_id: body.upload_id.as_deref(),
            error: body.error_code.as_deref(),
        },
    )
    .await?;
    Ok(StatusCode::NO_CONTENT)
}
