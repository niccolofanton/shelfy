//! Passkeys (plan §2.9 Auth and Account, §2.11): username-less sign-in and
//! the account's passkeys. The machinery is in [`crate::auth::passkeys`].
//!
//! | Route | Access | Answer |
//! |---|---|---|
//! | `POST /auth/passkeys/login/start` | public | `{ceremonyId, publicKey}` (rate-limited: 429) |
//! | `POST /auth/passkeys/login/finish` `{ceremonyId, credential}` | public | 204 with the session cookie; 400 `challenge_expired` or `passkey_invalid` (rate-limited: 429) |
//! | `GET /me/passkeys` | session | `{items}`: id, label, creation and last use |
//! | `POST /me/passkeys/start` | session, signed in or re-authenticated in the last 5 minutes | `{ceremonyId, publicKey}` |
//! | `POST /me/passkeys` `{ceremonyId, credential, label?}` | session, the one that started | 201 with the passkey; 400 `challenge_expired` or `passkey_invalid`; 409 `conflict` when the credential is registered already |
//! | `DELETE /me/passkeys/{id}` | session, signed in or re-authenticated in the last 5 minutes | 204; 404 for another account's passkey |
//!
//! A ceremony is two calls: `…/start` returns the options for
//! `navigator.credentials.create()` or `get()` (`publicKey`, in the WebAuthn
//! JSON form: [`webauthn`]) and a `ceremonyId`; the browser's answer
//! (`credential.toJSON()`) goes back with that id, within 5 minutes, once.
//! The ceremony routes answer 404 when passkeys are off
//! (`GET /auth/methods`). The sign-in routes share the per-client limit of
//! the other sign-in routes; a sign-in replaces the session the browser
//! held, like a sign-in link. Every `POST` and `DELETE` passes the CSRF
//! guard.

pub mod webauthn;

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use webauthn_rs::prelude::{PublicKeyCredential, RegisterPublicKeyCredential};
use webauthn_rs_proto::{PublicKeyCredentialCreationOptions, PublicKeyCredentialRequestOptions};

use super::auth::{no_store, signed_in, user_agent};
use crate::auth::passkeys::{self, normalize_label};
use crate::auth::{RecentAuth, SessionUser, cookie};
use crate::control::passkeys::PasskeyRow;
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::{Json, Path};
use crate::state::AppState;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_sign_in))
        .routes(routes!(finish_sign_in))
        .routes(routes!(list_passkeys, create_passkey))
        .routes(routes!(start_registration))
        .routes(routes!(delete_passkey))
}

/// The first answer of a registration: the options to create a passkey.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyRegistrationStart {
    /// Names the ceremony in `POST /me/passkeys`: valid 5 minutes, once,
    /// in this session.
    pub ceremony_id: String,
    /// The options for `navigator.credentials.create({publicKey})`.
    #[schema(value_type = webauthn::CreationOptions)]
    pub public_key: PublicKeyCredentialCreationOptions,
}

/// The first answer of a sign-in or a re-authentication: the options to
/// sign with a passkey.
#[derive(Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeyAssertionStart {
    /// Names the ceremony in the call that finishes it: valid 5 minutes,
    /// once.
    pub ceremony_id: String,
    /// The options for `navigator.credentials.get({publicKey})`.
    #[schema(value_type = webauthn::RequestOptions)]
    pub public_key: PublicKeyCredentialRequestOptions,
}

/// Body of `POST /api/v1/auth/passkeys/login/finish`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PasskeySignIn {
    /// The `ceremonyId` of `POST /auth/passkeys/login/start`.
    pub ceremony_id: String,
    /// The browser's answer: `credential.toJSON()`.
    #[schema(value_type = webauthn::AuthenticationResponse)]
    pub credential: PublicKeyCredential,
}

/// Body of `POST /api/v1/me/passkeys`.
#[derive(Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct NewPasskey {
    /// The `ceremonyId` of `POST /me/passkeys/start`.
    pub ceremony_id: String,
    /// The browser's answer: `credential.toJSON()`.
    #[schema(value_type = webauthn::RegistrationResponse)]
    pub credential: RegisterPublicKeyCredential,
    /// The user's name for the passkey, up to 64 characters; trimmed, and
    /// blank is none.
    #[serde(default)]
    #[schema(nullable = false)]
    pub label: Option<String>,
}

/// A passkey of the account.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Passkey {
    /// Its id, for `DELETE /me/passkeys/{id}`.
    pub id: i64,
    /// The user's name for it.
    #[schema(required = true)]
    pub label: Option<String>,
    /// Registration time, unix ms.
    pub created_at: i64,
    /// Last sign-in or re-authentication with it, unix ms.
    #[schema(required = true)]
    pub last_used_at: Option<i64>,
}

impl From<PasskeyRow> for Passkey {
    fn from(row: PasskeyRow) -> Self {
        Self {
            id: row.id,
            label: row.label,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        }
    }
}

/// The account's passkeys.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct PasskeyList {
    /// Oldest first.
    pub items: Vec<Passkey>,
}

/// Starts a username-less sign-in with a passkey.
///
/// Pass `publicKey` to `navigator.credentials.get()`: the browser offers the
/// passkeys it holds for this site. Then send its answer with the
/// `ceremonyId` to `POST /auth/passkeys/login/finish`, within 5 minutes.
/// Limit: 10 sign-in requests per minute per client, shared with the other
/// sign-in routes. 404 when passkeys are off (see `GET /auth/methods`).
#[utoipa::path(
    post,
    path = "/api/v1/auth/passkeys/login/start",
    tag = "auth",
    operation_id = "startPasskeySignIn",
    security(()),
    responses(
        (status = OK, description = "The options of the sign-in.", body = PasskeyAssertionStart),
    )
)]
pub async fn start_sign_in(State(state): State<AppState>) -> Result<Response, ApiError> {
    let (ceremony_id, public_key) = passkeys::start_sign_in(&state)?;
    let start = PasskeyAssertionStart {
        ceremony_id,
        public_key,
    };
    Ok(no_store(Json(start).into_response()))
}

/// Finishes a passkey sign-in.
///
/// A valid answer from a passkey of an active account starts a session
/// (replacing the one the browser held). Otherwise 400: `challenge_expired`
/// (the ceremony is unknown, used or older than 5 minutes: start again) or
/// `passkey_invalid` (the detail names the reason). Limit: as for `start`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/passkeys/login/finish",
    tag = "auth",
    operation_id = "finishPasskeySignIn",
    security(()),
    request_body = PasskeySignIn,
    responses(
        (status = NO_CONTENT, description = "Signed in: the response sets the session cookie."),
    )
)]
pub async fn finish_sign_in(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<PasskeySignIn>,
) -> Result<Response, ApiError> {
    let session = passkeys::finish_sign_in(
        &state,
        &request.ceremony_id,
        request.credential,
        cookie::session_token(&headers),
        user_agent(&headers),
    )
    .await?;
    Ok(signed_in(
        &state,
        StatusCode::NO_CONTENT.into_response(),
        &session,
    ))
}

/// The account's passkeys, oldest first.
#[utoipa::path(
    get,
    path = "/api/v1/me/passkeys",
    tag = "account",
    operation_id = "listPasskeys",
    responses(
        (status = OK, description = "The passkeys.", body = PasskeyList),
    )
)]
pub async fn list_passkeys(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let rows = passkeys::list(&state, user.id()).await?;
    let list = PasskeyList {
        items: rows.into_iter().map(Passkey::from).collect(),
    };
    Ok(no_store(Json(list).into_response()))
}

/// Starts adding a passkey to the account.
///
/// Needs a sign-in or a re-authentication from the last 5 minutes (403
/// `reauth_required` otherwise). Pass `publicKey` to
/// `navigator.credentials.create()`, then send its answer with the
/// `ceremonyId` to `POST /me/passkeys`, from this session, within 5 minutes.
/// 404 when passkeys are off.
#[utoipa::path(
    post,
    path = "/api/v1/me/passkeys/start",
    tag = "account",
    operation_id = "startPasskeyRegistration",
    responses(
        (status = OK, description = "The options of the registration.", body = PasskeyRegistrationStart),
    )
)]
pub async fn start_registration(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
) -> Result<Response, ApiError> {
    let (ceremony_id, public_key) = passkeys::start_registration(&state, &user).await?;
    let start = PasskeyRegistrationStart {
        ceremony_id,
        public_key,
    };
    Ok(no_store(Json(start).into_response()))
}

/// Adds a passkey to the account: finishes the registration that
/// `POST /me/passkeys/start` began in this session.
///
/// 400 `challenge_expired` (unknown, used, expired or another session's
/// ceremony: start again) or `passkey_invalid`; 409 `conflict` when the
/// credential is registered already; 422 for a label over 64 characters.
#[utoipa::path(
    post,
    path = "/api/v1/me/passkeys",
    tag = "account",
    operation_id = "createPasskey",
    request_body = NewPasskey,
    responses(
        (status = CREATED, description = "The new passkey.", body = Passkey),
    )
)]
pub async fn create_passkey(
    State(state): State<AppState>,
    user: SessionUser,
    Json(request): Json<NewPasskey>,
) -> Result<Response, ApiError> {
    // Validated first, so a refused label does not use the ceremony up.
    let label = normalize_label(request.label.as_deref())?;
    let row = passkeys::finish_registration(
        &state,
        &user,
        &request.ceremony_id,
        &request.credential,
        label,
    )
    .await?;
    let response = (StatusCode::CREATED, Json(Passkey::from(row))).into_response();
    Ok(no_store(response))
}

/// Removes a passkey from the account.
///
/// Needs a sign-in or a re-authentication from the last 5 minutes (403
/// `reauth_required` otherwise). Another account's passkey is a 404, like a
/// missing one. The passkey stays in the authenticator, which can no longer
/// sign in with it.
#[utoipa::path(
    delete,
    path = "/api/v1/me/passkeys/{id}",
    tag = "account",
    operation_id = "deletePasskey",
    params(("id" = i64, Path, description = "The passkey's id.")),
    responses(
        (status = NO_CONTENT, description = "Removed."),
    )
)]
pub async fn delete_passkey(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
    Path(id): Path<i64>,
) -> Result<Response, ApiError> {
    passkeys::remove(&state, user.id(), id).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
