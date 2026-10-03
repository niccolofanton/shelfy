//! Cookie-only search chat. Tokens belong to this POST answer, not the replay bus.
use super::search::SearchScope;
use crate::ai::chat::{
    ChatRuns, ErrorEvent, HEARTBEAT, ResultEvent, RunEvent, RunGuard, TokenEvent,
};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::state::AppState;
use axum::extract::{Extension, Path, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::response::{
    IntoResponse, Response,
    sse::{KeepAlive, Sse},
};
use futures_util::{StreamExt, stream};
use serde::Deserialize;
use shelfy_core::{
    ai::chat::{self, Turn},
    repo::posts::SourceBucket,
};
use std::convert::Infallible;
use std::sync::Arc;
use utoipa::ToSchema;

#[derive(Clone, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
}
#[derive(Clone, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChatMessage {
    pub role: Role,
    pub content: String,
}
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChatBody {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub active_tags: Vec<String>,
    #[serde(default)]
    pub scope: SearchScope,
    pub provider_id: Option<String>,
}
/// The reply stream is ephemeral: no event ids, replay or automatic POST
/// resubmission. A new request cancels the prior run. Closing cancels the call.
#[utoipa::path(post,path="/api/v1/search/chat",tag="library",operation_id="searchChat",request_body=ChatBody,
    responses((status=OK,description="SSE run, token, result or error; heartbeat every 15 seconds. No replay: closing cancels this run.",body=String,content_type="text/event-stream")))]
pub async fn search_chat(
    State(state): State<AppState>,
    user: CurrentUser,
    Extension(runs): Extension<Arc<ChatRuns>>,
    headers: HeaderMap,
    Json(body): Json<ChatBody>,
) -> Result<Response, ApiError> {
    if headers.contains_key("last-event-id") {
        return Err(ApiError::invalid_field(
            "Last-Event-ID",
            "chat streams cannot resume; start a new request",
        ));
    }
    super::listing::check_values("activeTags", &body.active_tags)?;
    if body.messages.is_empty() || body.messages.len() > 64 {
        return Err(ApiError::invalid_field(
            "messages",
            "requires 1 to 64 turns",
        ));
    }
    if body
        .provider_id
        .as_ref()
        .is_some_and(|id| id.is_empty() || id.len() > 128)
    {
        return Err(ApiError::invalid_field(
            "providerId",
            "requires 1 to 128 bytes",
        ));
    }
    let turns: Vec<_> = body
        .messages
        .into_iter()
        .map(|m| Turn {
            role: match m.role {
                Role::User => "user",
                Role::Assistant => "assistant",
            }
            .into(),
            content: m.content,
        })
        .collect();
    if !turns
        .iter()
        .any(|t| t.role == "user" && !t.content.trim().is_empty())
    {
        return Err(ApiError::invalid_field(
            "messages",
            "requires a nonblank user message",
        ));
    }
    let turns = chat::truncate_history(&turns);
    if !turns.iter().any(|t| t.role == "user") {
        return Err(ApiError::invalid_field(
            "messages",
            "the retained history must contain a user turn",
        ));
    }
    let source = match body.scope {
        SearchScope::All => None,
        SearchScope::Sites => Some(SourceBucket::Web),
        SearchScope::Social => Some(SourceBucket::Social),
    };
    let guard = runs.start(user.id());
    let prepared =
        crate::ai::chat::prepare(&state, runs, user.id(), turns, body.active_tags, source).await?;
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    let ended = super::events::session_ended(
        state.clone(),
        crate::auth::cookie::session_token(&headers).map(str::to_owned),
        user.id().to_owned(),
    );
    tokio::spawn(crate::ai::chat::run(
        state,
        user.id().to_owned(),
        guard.id.clone(),
        guard.cancel.clone(),
        body.provider_id,
        prepared,
        tx,
    ));
    let events = stream::unfold((rx, guard), |(mut rx, guard): (_, RunGuard)| async move {
        rx.recv()
            .await
            .map(|event| (Ok::<_, Infallible>(event), (rx, guard)))
    });
    let sse = Sse::new(events.take_until(ended))
        .keep_alive(KeepAlive::new().interval(HEARTBEAT).text("heartbeat"));
    Ok((
        [
            (header::CACHE_CONTROL, HeaderValue::from_static("no-store")),
            (
                super::events::X_ACCEL_BUFFERING,
                HeaderValue::from_static("no"),
            ),
        ],
        sse,
    )
        .into_response())
}
#[utoipa::path(post,path="/api/v1/search/chat/{runId}/cancel",tag="library",operation_id="cancelSearchChat",params(("runId"=String,Path,description="Run id from this user's response.")),responses((status=NO_CONTENT,description="Cancelled.")))]
pub async fn cancel_search_chat(
    user: CurrentUser,
    Extension(runs): Extension<Arc<ChatRuns>>,
    Path(id): Path<String>,
) -> Result<axum::http::StatusCode, ApiError> {
    if !runs.cancel(user.id(), &id) {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    Ok(axum::http::StatusCode::NO_CONTENT)
}
// Components of named events are registered alongside the route's SSE string.
#[derive(ToSchema)]
#[allow(dead_code)]
pub struct ChatEvents {
    pub run: RunEvent,
    pub token: TokenEvent,
    pub result: ResultEvent,
    pub error: ErrorEvent,
}
