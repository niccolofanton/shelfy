//! `GET /api/v1/me`: the signed-in user (plan §2.9 Account).
//!
//! T10 returns the profile the SPA needs to know who is signed in. P1-17
//! extends this module: `capabilities`, settings, consent, usage, the
//! session list and API tokens.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::auth::no_store;
use crate::control::users::{self, Role};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(me))
}

/// The signed-in user.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    /// User id (ULID).
    pub id: String,
    /// Sign-in email address.
    pub email: String,
    /// Role.
    pub role: UserRole,
    /// Account creation time, unix ms.
    pub created_at: i64,
}

/// What a user may do on the instance.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum UserRole {
    /// The instance owner (admin routes, unlimited quota).
    Owner,
    /// An invited member.
    Member,
}

impl From<Role> for UserRole {
    fn from(role: Role) -> Self {
        match role {
            Role::Owner => Self::Owner,
            Role::Member => Self::Member,
        }
    }
}

/// Who is signed in. 401 `unauthorized` without a session: the SPA shows
/// its sign-in page.
#[utoipa::path(
    get,
    path = "/api/v1/me",
    tag = "account",
    operation_id = "getMe",
    responses(
        (status = OK, description = "The signed-in user.", body = Me),
    )
)]
pub async fn me(State(state): State<AppState>, user: CurrentUser) -> Result<Response, ApiError> {
    let control = Arc::clone(state.control());
    let id = user.id().to_owned();
    let found = blocking(move || control.read(|conn| users::get(conn, &id))).await?;
    // Deleted after the session lookup was cached: signed out.
    let Some(found) = found else {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    };
    let me = Me {
        id: found.id,
        email: found.email.into_inner(),
        role: found.role.into(),
        created_at: found.created_at,
    };
    Ok(no_store(Json(me).into_response()))
}
