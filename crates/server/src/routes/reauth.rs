//! `/api/v1/auth/reauth/{start,finish}`: re-authentication of the signed-in
//! session (plan §2.9 Auth, §2.11). The machinery is in
//! [`crate::auth::reauth`] and [`crate::auth::passkeys`].
//!
//! | Route | Body | Answer |
//! |---|---|---|
//! | `POST /auth/reauth/start` | `{method: passkey}` | 200 `{ceremonyId, publicKey}`, which name the account's passkeys; 404 without one, or when passkeys are off |
//! | `POST /auth/reauth/start` | `{method: email}` | 202: a link to the account's address; 404 when email is off (3 per hour per address: 429) |
//! | `POST /auth/reauth/finish` | `{method: passkey, ceremonyId, credential}` | 204; 400 `challenge_expired` or `passkey_invalid` |
//! | `POST /auth/reauth/finish` | `{method: link, token}` | 204; 400 `invalid_link` |
//!
//! Both routes need a session, and a passkey ceremony finishes in the
//! session that started it. After the 204, the routes that answered 403
//! `reauth_required` work for 5 minutes. The token of a link is what follows
//! `#` in `<public url>/login/reauth#<token>`, from the email or from
//! `shelfy-server admin login-link --purpose reauth`.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::PublicKeyCredential;

use super::auth::{hit, no_store};
use super::passkeys::{PasskeyAssertionStart, webauthn};
use crate::auth::{SessionUser, magic_link, passkeys, rate_limit, reauth};
use crate::control::users;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::ids::now_ms;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_reauth))
        .routes(routes!(finish_reauth))
}

/// How to re-authenticate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ReauthMethod {
    /// Sign with one of the account's passkeys.
    Passkey,
    /// Open a link emailed to the account's address.
    Email,
}

/// Body of `POST /api/v1/auth/reauth/start`.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
pub struct ReauthStart {
    /// How.
    pub method: ReauthMethod,
}

/// Body of `POST /api/v1/auth/reauth/finish`: the proof.
#[derive(Deserialize, ToSchema)]
#[serde(tag = "method", rename_all = "lowercase")]
pub enum ReauthFinish {
    /// The answer of the passkey ceremony that `start` began.
    Passkey {
        /// The `ceremonyId` of `POST /auth/reauth/start`.
        #[serde(rename = "ceremonyId")]
        ceremony_id: String,
        /// The browser's answer: `credential.toJSON()`.
        #[schema(value_type = webauthn::AuthenticationResponse)]
        credential: Box<PublicKeyCredential>,
    },
    /// A re-authentication link.
    Link {
        /// What follows `#` in `<public url>/login/reauth#<token>`.
        token: String,
    },
}

/// Starts re-authenticating the session.
///
/// `passkey`: answers the options for `navigator.credentials.get()`, which
/// name the account's passkeys; send the answer with the `ceremonyId` to
/// `POST /auth/reauth/finish` within 5 minutes. 404 when the account has no
/// passkey, or passkeys are off.
///
/// `email`: emails a re-authentication link to the account's address and
/// answers 202. 404 when email is off (`GET /auth/methods`); the operator
/// can mint the link instead (`admin login-link --purpose reauth`). Limit:
/// 3 emails per hour per address, shared with sign-in emails.
#[utoipa::path(
    post,
    path = "/api/v1/auth/reauth/start",
    tag = "auth",
    operation_id = "startReauth",
    request_body = ReauthStart,
    responses(
        (status = OK, description = "`passkey`: the options of the ceremony.", body = PasskeyAssertionStart),
        (status = ACCEPTED, description = "`email`: a link valid for 15 minutes is on its way."),
    )
)]
pub async fn start_reauth(
    State(state): State<AppState>,
    user: SessionUser,
    Json(request): Json<ReauthStart>,
) -> Result<Response, ApiError> {
    match request.method {
        ReauthMethod::Passkey => {
            let (ceremony_id, public_key) = passkeys::start_reauth(&state, &user).await?;
            let start = PasskeyAssertionStart {
                ceremony_id,
                public_key,
            };
            Ok(no_store(Json(start).into_response()))
        }
        ReauthMethod::Email => {
            if !state.mailer().is_enabled() {
                return Err(ApiError::not_found().with_detail("email is off on this server"));
            }
            let control = Arc::clone(state.control());
            let user_id = user.id().to_owned();
            let account = blocking(move || control.read(|conn| users::get(conn, &user_id)))
                .await?
                .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?;
            let address = account.email.expose().to_ascii_lowercase();
            hit(
                state.auth().address_limiter(),
                &rate_limit::key("email", &address),
                now_ms(),
            )?;
            magic_link::send_reauth_in_background(&state, account.id);
            Ok(no_store(StatusCode::ACCEPTED.into_response()))
        }
    }
}

/// Finishes re-authenticating the session, with a passkey's answer or a
/// re-authentication link.
///
/// After the 204, the routes that need a recent sign-in work for 5 minutes.
/// A passkey answer is refused with 400 `challenge_expired` (unknown, used,
/// expired or another session's ceremony) or `passkey_invalid`; a link that
/// is unknown, used, expired, of another kind or of another account, with
/// 400 `invalid_link`, and stays unused.
#[utoipa::path(
    post,
    path = "/api/v1/auth/reauth/finish",
    tag = "auth",
    operation_id = "finishReauth",
    request_body = ReauthFinish,
    responses(
        (status = NO_CONTENT, description = "Re-authenticated."),
    )
)]
pub async fn finish_reauth(
    State(state): State<AppState>,
    user: SessionUser,
    Json(request): Json<ReauthFinish>,
) -> Result<Response, ApiError> {
    match request {
        ReauthFinish::Passkey {
            ceremony_id,
            credential,
        } => passkeys::finish_reauth(&state, &user, &ceremony_id, *credential).await?,
        ReauthFinish::Link { token } => {
            if !reauth::with_link(&state, &user, &token).await? {
                return Err(ApiError::new(ErrorCode::InvalidLink));
            }
        }
    }
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
