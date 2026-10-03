//! Social analysis and its durable item queue, session authenticated.
use axum::extract::State;
use axum::http::HeaderMap;
use serde::{Deserialize, Serialize};
use shelfy_core::ai::queue::{Mode, Reach};
use utoipa::{IntoParams, ToSchema};

use super::selector::PostSelector;
use crate::ai::{self, Caller, Task};
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Query};
use crate::jobs::idempotency::IdempotencyHeader;
use crate::state::AppState;

/// Analyze mode, distinct from selection itself.
#[derive(Clone, Copy, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum AnalyzeMode {
    Missing,
    Selected,
    All,
}
impl From<AnalyzeMode> for Mode {
    fn from(value: AnalyzeMode) -> Self {
        match value {
            AnalyzeMode::Missing => Self::Missing,
            AnalyzeMode::Selected => Self::Selected,
            AnalyzeMode::All => Self::All,
        }
    }
}
/// An estimate, or its confirmed enqueue.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AnalyzeRequest {
    pub selector: PostSelector,
    pub mode: AnalyzeMode,
    #[serde(default)]
    pub deep: bool,
    pub confirm_token: Option<String>,
    /// Dry run even for one post. Returns a confirmation token without queue writes.
    #[serde(default)]
    pub estimate_only: bool,
}

#[utoipa::path(post, path="/api/v1/ai/analyze", tag="jobs", operation_id="analyzePosts", params(IdempotencyHeader), request_body=AnalyzeRequest,
    responses((status=OK, description="Estimate and confirmation, or enqueued work.", body=ai::queue::AnalyzeResult)))]
pub async fn analyze(
    State(state): State<AppState>,
    user: CurrentUser,
    headers: HeaderMap,
    Json(request): Json<AnalyzeRequest>,
) -> Result<Json<ai::queue::AnalyzeResult>, ApiError> {
    if request.confirm_token.is_some() && !headers.contains_key("idempotency-key") {
        return Err(ApiError::invalid_field(
            "Idempotency-Key",
            "is required for confirmation",
        ));
    }
    let selector = request.selector.resolve("selector")?;
    if matches!(request.mode, AnalyzeMode::Selected)
        && !matches!(selector, shelfy_core::selector::Selector::Keys(_))
    {
        return Err(ApiError::invalid_field(
            "selector",
            "selected mode requires explicit keys",
        ));
    }
    // No content is queued without a route the account may use.
    let owner = crate::jobs::ai_drain::is_owner(&state, user.id()).await?;
    state
        .ai()
        .route(&state, Caller::new(user.id(), owner), Task::Catalog)
        .await?;
    Ok(Json(
        ai::queue::analyze_with_preview(
            &state,
            user.id(),
            selector,
            request.mode.into(),
            request.confirm_token,
            request.deep,
            request.estimate_only,
        )
        .await?,
    ))
}

/// Item-state filter and opaque keyset cursor.
#[derive(Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in=Query)]
pub struct QueueQuery {
    pub state: Option<String>,
    pub cursor: Option<String>,
}
#[utoipa::path(get, path="/api/v1/ai/queue", tag="jobs", operation_id="getAiQueue", params(QueueQuery),
    responses((status=OK, description="Items, counts and provider availability.", body=ai::queue::QueueView)))]
pub async fn get_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<QueueQuery>,
) -> Result<Json<ai::queue::QueueView>, ApiError> {
    if query
        .state
        .as_deref()
        .is_some_and(|s| !["pending", "analyzing", "done", "error"].contains(&s))
    {
        return Err(ApiError::invalid_field("state", "unknown item state"));
    }
    let cursor = query
        .cursor
        .map(|c| {
            c.parse::<i64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or_else(|| ApiError::new(crate::error::ErrorCode::InvalidCursor))
        })
        .transpose()?;
    let view = ai::queue::queue_view(&state, user.id(), query.state, cursor).await?;
    Ok(Json(view))
}

/// Exactly one of explicit post keys or all queued items.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct QueueRequest {
    pub keys: Option<Vec<String>>,
    #[serde(default)]
    pub all: bool,
}
impl QueueRequest {
    fn resolve(self) -> Result<Reach, ApiError> {
        match (self.keys, self.all) {
            (None, true) => Ok(Reach::All),
            (Some(keys), false) if !keys.is_empty() => {
                shelfy_core::selector::Selector::Keys(keys.clone()).validate()?;
                if keys
                    .iter()
                    .any(|k| k.is_empty() || k.len() > super::posts::MAX_KEY_BYTES)
                {
                    return Err(ApiError::invalid_field("keys", "invalid post key"));
                }
                Ok(Reach::Keys(keys))
            }
            _ => Err(ApiError::invalid_field(
                "keys",
                "give keys or all, exclusively",
            )),
        }
    }
}
/// Number of items changed by a queue action.
#[derive(Serialize, ToSchema)]
pub struct QueueChanged {
    pub changed: u64,
}
#[utoipa::path(post, path="/api/v1/ai/queue/cancel", tag="jobs", operation_id="cancelAiQueue", request_body=QueueRequest,
    responses((status=OK, description="Queued analyses cancelled.", body=QueueChanged)))]
pub async fn cancel_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<QueueRequest>,
) -> Result<Json<QueueChanged>, ApiError> {
    Ok(Json(QueueChanged {
        changed: ai::queue::cancel(&state, user.id(), request.resolve()?).await?,
    }))
}
#[utoipa::path(post, path="/api/v1/ai/queue/retry", tag="jobs", operation_id="retryAiQueue", params(IdempotencyHeader), request_body=QueueRequest,
    responses((status=OK, description="Failed analyses queued again.", body=QueueChanged)))]
pub async fn retry_queue(
    State(state): State<AppState>,
    user: CurrentUser,
    Json(request): Json<QueueRequest>,
) -> Result<Json<QueueChanged>, ApiError> {
    Ok(Json(QueueChanged {
        changed: ai::queue::retry(&state, user.id(), request.resolve()?).await?,
    }))
}
