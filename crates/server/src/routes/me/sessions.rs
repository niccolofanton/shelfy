//! `GET,DELETE /api/v1/me/sessions` (plan §2.9 Account, §7.1 "session list
//! with remote logout"): the account's signed-in sessions, and signing them
//! out from here. The machinery is in [`crate::auth::session_list`].

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::SessionUser;
use crate::auth::session_list::{self, Ended, ListedSession};
use crate::error::ApiError;
use crate::extract::{Json, Path};
use crate::routes::auth::{cleared, no_store};
use crate::state::AppState;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_sessions, end_other_sessions))
        .routes(routes!(end_session))
}

/// A signed-in session of the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// Its id, for `DELETE /me/sessions/{id}`: 32 hex digits, derived from
    /// the session; it is not the cookie.
    pub id: String,
    /// Whether this is the session of the request.
    pub current: bool,
    /// Sign-in time, unix ms.
    pub created_at: i64,
    /// Last use, unix ms (recorded at most hourly).
    pub last_seen_at: i64,
    /// When it ends unless used again, unix ms: 30 days after its last use,
    /// and never more than 90 days after sign-in.
    pub expires_at: i64,
    /// The browser's `User-Agent` at sign-in, cut to 256 bytes.
    #[schema(required = true)]
    pub user_agent: Option<String>,
}

impl From<ListedSession> for Session {
    fn from(session: ListedSession) -> Self {
        Self {
            id: session.id,
            current: session.current,
            created_at: session.created_at,
            last_seen_at: session.last_seen_at,
            expires_at: session.expires_at,
            user_agent: session.user_agent,
        }
    }
}

/// The account's sessions.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct SessionList {
    /// The current session first, then the most recently used.
    pub items: Vec<Session>,
}

/// The account's signed-in sessions, the current one flagged.
#[utoipa::path(
    get,
    path = "/api/v1/me/sessions",
    tag = "account",
    operation_id = "listSessions",
    responses(
        (status = OK, description = "The sessions.", body = SessionList),
    )
)]
pub async fn list_sessions(
    State(state): State<AppState>,
    user: SessionUser,
) -> Result<Response, ApiError> {
    let sessions = session_list::list(&state, &user).await?;
    let list = SessionList {
        items: sessions.into_iter().map(Session::from).collect(),
    };
    Ok(no_store(Json(list).into_response()))
}

/// Signs out one of the account's sessions: it stops working at once.
/// Signing out the current one is a sign-out, and clears the cookie. Another
/// account's session is a 404, like a missing one.
#[utoipa::path(
    delete,
    path = "/api/v1/me/sessions/{id}",
    tag = "account",
    operation_id = "endSession",
    params(("id" = String, Path, description = "The session's id.")),
    responses(
        (status = NO_CONTENT, description = "Signed out; for the current session, the cookie is cleared."),
    )
)]
pub async fn end_session(
    State(state): State<AppState>,
    user: SessionUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    match session_list::end(&state, &user, &id).await? {
        Some(Ended::Current) => Ok(cleared(StatusCode::NO_CONTENT.into_response())),
        Some(Ended::Other) => Ok(no_store(StatusCode::NO_CONTENT.into_response())),
        None => Err(ApiError::not_found()),
    }
}

/// Signs out every session of the account but this one.
#[utoipa::path(
    delete,
    path = "/api/v1/me/sessions",
    tag = "account",
    operation_id = "endOtherSessions",
    responses(
        (status = NO_CONTENT, description = "Every other session is signed out."),
    )
)]
pub async fn end_other_sessions(
    State(state): State<AppState>,
    user: SessionUser,
) -> Result<Response, ApiError> {
    session_list::end_others(&state, &user).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
