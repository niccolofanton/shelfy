//! `/api/v1/auth/*`: sign-in methods, sign-in links and sign-out (plan §2.9
//! Auth, §2.11). The machinery lives in [`crate::auth`]; passkey sign-in is
//! in [`super::passkeys`], re-authentication in [`super::reauth`].
//!
//! | Route | Access | Answer |
//! |---|---|---|
//! | `GET /auth/methods` | public | `{emailLink, passkeys}` |
//! | `POST /auth/magic-links` `{email}` | public | always 202 (rate-limited: 429) |
//! | `POST /auth/magic-links/redeem` `{token}` | public | 204 with the session cookie, or 400 `invalid_link` (rate-limited: 429) |
//! | `POST /auth/logout` | public | 204; the cookie is cleared when the request sent one |
//! | `POST /auth/logout-all` | session | 204, every session of the user ended |
//!
//! A link opens the SPA page `/login/magic#<token>`, which calls `redeem`; no
//! route redeems on `GET`. Every `POST` here passes the CSRF guard
//! ([`crate::auth::csrf`]), session cookie or not.
//!
//! Every `/api/v1/auth/*` route shares the sign-in limit per client address
//! (10 a minute), counted before the route runs
//! ([`crate::rate_limit::by_client`]); sign-in emails have their own limit
//! per address (3 an hour), counted here.

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::magic_link;
use crate::auth::rate_limit::{self, RateLimiter};
use crate::auth::session::SignedIn;
use crate::auth::{AuthMethods, cookie, session};
use crate::control::users;
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::ids::now_ms;
use crate::state::AppState;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(methods))
        .routes(routes!(request_magic_link))
        .routes(routes!(redeem_magic_link))
        .routes(routes!(logout))
        .routes(routes!(logout_all))
}

/// How a user can sign in on this instance.
#[utoipa::path(
    get,
    path = "/api/v1/auth/methods",
    tag = "auth",
    operation_id = "getAuthMethods",
    security(()),
    responses(
        (status = OK, description = "The sign-in methods this instance offers.", body = AuthMethods),
    )
)]
pub async fn methods(State(state): State<AppState>) -> Response {
    let methods = AuthMethods {
        email_link: state.mailer().is_enabled(),
        passkeys: state.auth().passkeys().is_enabled(),
    };
    no_store(Json(methods).into_response())
}

/// Body of `POST /api/v1/auth/magic-links`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct MagicLinkRequest {
    /// The account's email address.
    pub email: String,
}

/// Emails a sign-in link to the address, if it belongs to an account.
///
/// The answer is 202 whether or not the account exists, and whether or not
/// email is configured (see `GET /auth/methods`). The email carries the link
/// `<public url>/login/magic#<token>`. Limits: 10 requests per minute per
/// client and 3 per hour per address (429 `rate_limited` with
/// `Retry-After`). A malformed address answers 422 `validation_failed`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/magic-links",
    tag = "auth",
    operation_id = "requestMagicLink",
    security(()),
    request_body = MagicLinkRequest,
    responses(
        (status = ACCEPTED, description = "Accepted. If the address has an account, a link valid for 15 minutes is on its way."),
    )
)]
pub async fn request_magic_link(
    State(state): State<AppState>,
    Json(request): Json<MagicLinkRequest>,
) -> Result<Response, ApiError> {
    let email = users::normalize_email(&request.email)?;
    hit(
        state.auth().address_limiter(),
        &rate_limit::key("email", &email),
        now_ms(),
    )?;
    magic_link::send_in_background(&state, email);
    Ok(no_store(StatusCode::ACCEPTED.into_response()))
}

/// Body of `POST /api/v1/auth/magic-links/redeem`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RedeemRequest {
    /// The link's token: what follows `#` in its URL.
    pub token: String,
}

/// Redeems a sign-in link: the SPA page `/login/magic#<token>` sends the
/// token from its URL's fragment, which never reaches a server log.
///
/// A usable link starts a session (replacing the one the browser held);
/// otherwise 400 `invalid_link`. The link works once. Limit: 10 requests per
/// minute per client, shared with `POST /auth/magic-links`.
#[utoipa::path(
    post,
    path = "/api/v1/auth/magic-links/redeem",
    tag = "auth",
    operation_id = "redeemMagicLink",
    security(()),
    request_body = RedeemRequest,
    responses(
        (status = NO_CONTENT, description = "Signed in: the response sets the session cookie."),
    )
)]
pub async fn redeem_magic_link(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<RedeemRequest>,
) -> Result<Response, ApiError> {
    let redeemed = magic_link::redeem(
        &state,
        &request.token,
        cookie::session_token(&headers),
        user_agent(&headers),
    )
    .await?;
    let Some(redeemed) = redeemed else {
        return Err(ApiError::new(ErrorCode::InvalidLink));
    };
    Ok(signed_in(
        &state,
        StatusCode::NO_CONTENT.into_response(),
        &redeemed,
    ))
}

/// Signs out: ends the session of the request's cookie and clears the
/// cookie. Answers 204 even without a session; without a session cookie the
/// answer sets no cookie.
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout",
    tag = "auth",
    operation_id = "logout",
    security(()),
    responses(
        (status = NO_CONTENT, description = "Signed out; the session cookie, if sent, is cleared."),
    )
)]
pub async fn logout(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let Some(token) = cookie::session_token(&headers) else {
        return Ok(no_store(StatusCode::NO_CONTENT.into_response()));
    };
    if let Some(user_id) = session::end_session(&state, token).await? {
        tracing::Span::current().record("user_id", user_id.as_str());
    }
    Ok(cleared(StatusCode::NO_CONTENT.into_response()))
}

/// Signs out everywhere: ends every session of the signed-in user, this one
/// included, and clears the cookie.
#[utoipa::path(
    post,
    path = "/api/v1/auth/logout-all",
    tag = "auth",
    operation_id = "logoutAll",
    responses(
        (status = NO_CONTENT, description = "Every session of the user ended; the session cookie is cleared."),
    )
)]
pub async fn logout_all(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    session::end_all_sessions(&state, user.id()).await?;
    Ok(cleared(StatusCode::NO_CONTENT.into_response()))
}

/// Counts a hit on `limiter`, or refuses with 429 `rate_limited`.
pub(crate) fn hit(limiter: &RateLimiter, key: &rate_limit::Key, now: i64) -> Result<(), ApiError> {
    limiter
        .hit(key, now)
        .map_err(|seconds| ApiError::new(ErrorCode::RateLimited).with_retry_after(seconds))
}

/// The `User-Agent`, for the session list.
pub(crate) fn user_agent(headers: &HeaderMap) -> Option<String> {
    headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

/// Adds `Cache-Control: no-store`.
pub(crate) fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

/// Sets the cookie of the session a sign-in started, and names the user in
/// the request span.
pub(crate) fn signed_in(
    state: &AppState,
    mut response: Response,
    signed_in: &SignedIn,
) -> Response {
    tracing::Span::current().record("user_id", signed_in.user_id.as_str());
    let lifetime = state.auth().config().session_lifetime;
    response.headers_mut().append(
        header::SET_COOKIE,
        cookie::set_session(&signed_in.token, lifetime),
    );
    no_store(response)
}

/// Clears the session cookie.
pub(crate) fn cleared(mut response: Response) -> Response {
    response
        .headers_mut()
        .append(header::SET_COOKIE, cookie::clear_session());
    no_store(response)
}
