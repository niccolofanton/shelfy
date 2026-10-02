//! `/api/v1/auth/device/*`: the device flow that signs the migration CLI in
//! (RFC 8628; plan §2.9 Auth, §2.11, §4.1 step 2). The machinery is in
//! [`crate::auth::device`].
//!
//! | Route | Access | Answer |
//! |---|---|---|
//! | `POST /auth/device/start` | public; no CSRF headers needed | the device code, the user code and the approval page (the sign-in limit per client: 429) |
//! | `POST /auth/device/poll` `{deviceCode}` | public; no CSRF headers needed | `pending`, `slow_down`, or `approved` with a `migrate` token; 400 `invalid_device_code`; 429 past 20 polls a minute of one device code |
//! | `POST /auth/device/approve` `{userCode}` | session, signed in or re-authenticated in the last 5 minutes | 204; 400 `invalid_device_code`; 429 past 10 tries in 10 minutes |
//!
//! The CLI calls `start` and `poll` without a cookie, an `Origin` or
//! `X-Shelfy-Client`: they are the routes of
//! [`super::CSRF_EXEMPT_ROUTES`]. `approve` comes from the web app's
//! `/device` page, and passes the CSRF guard like every cookie request.
//!
//! `start` and `approve` count against the sign-in limit per client address
//! like every `/api/v1/auth/*` route ([`crate::rate_limit::by_client`]);
//! `poll` does not, and is paced per device code instead
//! ([`crate::rate_limit::UNCOUNTED_SIGN_IN_ROUTES`]).

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::auth::no_store;
use crate::auth::RecentAuth;
use crate::auth::bearer::Scope;
use crate::auth::device::{self, PollOutcome};
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::state::AppState;

/// The web app's page that approves a user code.
pub const APPROVAL_PAGE: &str = "/device";

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(start_device))
        .routes(routes!(poll_device))
        .routes(routes!(approve_device))
}

/// What `POST /auth/device/start` answers: RFC 8628's device authorization
/// response, in camelCase.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorization {
    /// The CLI's secret: it polls with it. Never show or log it.
    pub device_code: String,
    /// What the user approves: 8 letters as `XXXX-XXXX`; case, dashes and
    /// spaces do not matter.
    pub user_code: String,
    /// The page that approves it: `<public url>/device`.
    pub verification_uri: String,
    /// The same page with the code filled in: `<public url>/device#XXXX-XXXX`.
    pub verification_uri_complete: String,
    /// Seconds both codes stay valid.
    pub expires_in: u64,
    /// Seconds to wait between polls.
    pub interval: u64,
}

/// Body of `POST /api/v1/auth/device/poll`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DevicePollRequest {
    /// The `deviceCode` of `POST /auth/device/start`.
    pub device_code: String,
}

/// What a poll finds, by `status`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DevicePoll {
    /// Not approved yet: poll again in `interval` seconds.
    Pending {
        /// Seconds to wait before the next poll.
        interval: u64,
    },
    /// Polled sooner than the interval allows: wait `interval` seconds from
    /// now on (it grew by 5).
    SlowDown {
        /// Seconds to wait before the next poll.
        interval: u64,
    },
    /// Approved: the token, valid 7 days, delivered once. Keep it in a file
    /// only its owner can read.
    Approved {
        /// The `migrate` token (`shx_…`), for `Authorization: Bearer`.
        token: String,
        /// Its id, as the account's token list names it.
        #[serde(rename = "tokenId")]
        token_id: String,
        /// What it may do: `["migrate"]`.
        scopes: Vec<Scope>,
        /// When it stops working, unix ms.
        #[serde(rename = "expiresAt")]
        expires_at: i64,
    },
}

/// Body of `POST /api/v1/auth/device/approve`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct DeviceApproval {
    /// The code the CLI shows (`BCDF-GHJK`).
    pub user_code: String,
}

/// Starts signing a device in: the migration CLI's `login`.
///
/// Show the user `userCode` and `verificationUri` (or open
/// `verificationUriComplete`), then poll `POST /auth/device/poll` with
/// `deviceCode` every `interval` seconds until it is approved, for at most
/// `expiresIn` seconds. Limit: the sign-in limit, 10 requests per minute per
/// client over every `/auth` route (429). No cookie and no CSRF headers are
/// needed.
#[utoipa::path(
    post,
    path = "/api/v1/auth/device/start",
    tag = "auth",
    operation_id = "startDeviceSignIn",
    security(()),
    responses(
        (status = OK, description = "The codes of a new device sign-in.", body = DeviceAuthorization),
    )
)]
pub async fn start_device(State(state): State<AppState>) -> Result<Response, ApiError> {
    let started = device::start(&state);
    let page = state.config().public_url.join(APPROVAL_PAGE);
    let user_code = started.user_code.into_inner();
    let authorization = DeviceAuthorization {
        device_code: started.device_code.expose().to_owned(),
        verification_uri_complete: format!("{page}#{user_code}"),
        verification_uri: page,
        user_code,
        expires_in: started.expires_in.as_secs(),
        interval: started.interval.as_secs(),
    };
    Ok(no_store(Json(authorization).into_response()))
}

/// Asks whether a device sign-in was approved.
///
/// `pending`: wait `interval` seconds and poll again. `slow_down`: the poll
/// came too soon; wait the new `interval`. `approved`: the `migrate` token,
/// delivered once; the codes stop working. 400 `invalid_device_code` for a
/// device code that is unknown, expired or used: start again. 429
/// `rate_limited` with `Retry-After` past 20 polls a minute of one device
/// code. The sign-in limit per client does not count polls. No cookie and no
/// CSRF headers are needed.
#[utoipa::path(
    post,
    path = "/api/v1/auth/device/poll",
    tag = "auth",
    operation_id = "pollDeviceSignIn",
    security(()),
    request_body = DevicePollRequest,
    responses(
        (status = OK, description = "Where the sign-in stands.", body = DevicePoll),
    )
)]
pub async fn poll_device(
    State(state): State<AppState>,
    Json(request): Json<DevicePollRequest>,
) -> Result<Response, ApiError> {
    let answer = match device::poll(&state, &request.device_code).await? {
        PollOutcome::Pending { interval } => DevicePoll::Pending {
            interval: interval.as_secs(),
        },
        PollOutcome::SlowDown { interval } => DevicePoll::SlowDown {
            interval: interval.as_secs(),
        },
        PollOutcome::Limited { retry_after } => {
            let seconds = retry_after.as_secs() + u64::from(retry_after.subsec_nanos() > 0);
            return Err(ApiError::new(ErrorCode::RateLimited)
                .with_retry_after(u32::try_from(seconds.max(1)).unwrap_or(u32::MAX))
                .with_detail("too many polls of this device code"));
        }
        PollOutcome::Approved(minted) => DevicePoll::Approved {
            scopes: Scope::parse_list(&minted.row.scopes),
            token_id: minted.row.id,
            expires_at: minted.row.expires_at.unwrap_or_default(),
            token: minted.token.into_inner(),
        },
    };
    Ok(no_store(Json(answer).into_response()))
}

/// Approves a device sign-in for the signed-in account: the device that
/// shows this code gets a `migrate` token for this account's library, valid
/// 7 days.
///
/// Needs a sign-in or a re-authentication from the last 5 minutes (403
/// `reauth_required` otherwise). 400 `invalid_device_code` for a code that
/// is unknown, expired, used or approved by another account; approving a
/// code again is a no-op. Limit: 10 tries per 10 minutes per account (429).
#[utoipa::path(
    post,
    path = "/api/v1/auth/device/approve",
    tag = "auth",
    operation_id = "approveDeviceSignIn",
    request_body = DeviceApproval,
    responses(
        (status = NO_CONTENT, description = "Approved: the device's next poll gets its token."),
    )
)]
pub async fn approve_device(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
    Json(request): Json<DeviceApproval>,
) -> Result<Response, ApiError> {
    device::approve(&state, &user, &request.user_code).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
