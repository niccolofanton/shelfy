//! `/api/v1/me…`: the signed-in account (plan §2.9 Account, §2.11, §2.13,
//! §2.19; P1-17). The passkey routes under `/me/passkeys` are in
//! [`super::passkeys`].
//!
//! | Route | Access | Answer |
//! |---|---|---|
//! | `GET /me` | session | the profile, `capabilities` and `consent` |
//! | `GET /me/settings` | session | the settings, defaults filled in ([`settings`]) |
//! | `PUT /me/settings` | session | the settings after the change |
//! | `POST /me/consent` | session | the consent, after recording the disclaimer and privacy versions ([`consent`]) |
//! | `GET /me/usage` | session | the storage used and the quota ([`usage`]) |
//! | `GET /me/sessions` | session | the signed-in sessions, the current one flagged ([`sessions`]) |
//! | `DELETE /me/sessions/{id}` | session | 204: that session is signed out (the current one: a sign-out) |
//! | `DELETE /me/sessions` | session | 204: every other session is signed out |
//! | `GET /me/tokens` | session | the working API tokens ([`tokens`]) |
//! | `POST /me/tokens` | session, signed in or re-authenticated in the last 5 minutes | 201 with the token, shown once |
//! | `DELETE /me/tokens/{id}` | session | 204: the token is revoked |
//! | `POST /me/tokens/pairing-code` | session, signed in or re-authenticated in the last 5 minutes | 201 with a 60-second code that pairs the browser extension (P2-03) |
//!
//! Every route takes the session cookie only: an API token is refused with
//! 401, even next to a valid cookie. Another user's session or token is a
//! 404, like a missing one. Every answer is `Cache-Control: no-store`.

pub mod ai_usage;
pub mod consent;
pub mod providers;
pub mod sessions;
pub mod settings;
pub mod tokens;
pub mod usage;

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

pub use consent::Consent;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(me))
        .merge(settings::router())
        .merge(consent::router())
        .merge(usage::router())
        .merge(ai_usage::router())
        .merge(providers::router())
        .merge(sessions::router())
        .merge(tokens::router())
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
    /// What the account can do on this server: the web app shows what it
    /// can use and hides the rest.
    pub capabilities: Capabilities,
    /// The disclaimer and privacy notice the user accepted.
    pub consent: Consent,
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

/// What the account can do on this server (plan §2.19). A capability turns
/// on with the phase that brings it; until then it is `false`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct Capabilities {
    /// The owner's administration (`/api/v1/admin`).
    pub admin: bool,
    /// Passkeys work on this server: sign-in, re-authentication and
    /// registration (`GET /auth/methods` says the same before sign-in).
    pub passkeys: bool,
    /// "Email me a link" works: email is configured. Without it, sign-in and
    /// re-authentication links come from the operator (`admin login-link`).
    #[serde(rename = "emailLink")]
    pub email_link: bool,
    /// The browser extension can pair (`POST /me/tokens/pairing-code`) and
    /// talk to this server (P2).
    pub extension: bool,
    /// AI analysis and its tasks (P3).
    #[serde(rename = "ai.tasks")]
    pub ai_tasks: bool,
    /// Website capture (P4).
    pub capture: bool,
    /// Videos fetched on demand (P4).
    #[serde(rename = "video.onDemand")]
    pub video_on_demand: bool,
}

impl Capabilities {
    /// The capabilities of an account with `role` on this server.
    #[must_use]
    pub fn of(state: &AppState, role: Role) -> Self {
        Self {
            admin: role == Role::Owner,
            passkeys: state.auth().passkeys().is_enabled(),
            email_link: state.mailer().is_enabled(),
            extension: true,
            ai_tasks: state.ai().is_configured(),
            capture: false,
            video_on_demand: false,
        }
    }
}

/// Who is signed in, what the account can do, and what it accepted. 401
/// `unauthorized` without a session: the SPA shows its sign-in page.
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
    let found = blocking(move || {
        control.read(|conn| {
            let Some(user) = users::get(conn, &id)? else {
                return Ok(None);
            };
            let consent = users::consent(conn, &id)?.unwrap_or_default();
            Ok::<_, shelfy_core::repo::RepoError>(Some((user, consent)))
        })
    })
    .await?;
    // Deleted after the session lookup was cached: signed out.
    let Some((found, consent)) = found else {
        return Err(ApiError::new(ErrorCode::Unauthorized));
    };
    let me = Me {
        capabilities: Capabilities::of(&state, found.role),
        id: found.id,
        email: found.email.into_inner(),
        role: found.role.into(),
        created_at: found.created_at,
        consent: consent.into(),
    };
    Ok(no_store(Json(me).into_response()))
}
