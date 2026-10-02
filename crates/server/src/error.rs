//! Errors as `application/problem+json` (RFC 9457) with a stable `code`
//! (plan §2.9).
//!
//! Every error response of the API has the same shape, [`Problem`]. Handlers
//! return [`ApiError`]; [`problem_fallback`] turns the error responses that
//! other layers produce (unknown route, wrong method, body too large, handler
//! timeout, panic) into the same shape. The `code` is the contract: clients map
//! it to their own messages, and the server never sends UI prose. `detail` is
//! for developers and never carries user content or secrets.

use std::any::Any;
use std::borrow::Cow;
use std::fmt;

use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use shelfy_core::db::DbError;
use shelfy_core::repo::RepoError;
use utoipa::ToSchema;

/// Media type of every error body.
pub const PROBLEM_JSON: &str = "application/problem+json";

/// Stable, machine-readable error codes. Each one has a fixed HTTP status.
///
/// New codes may be added within `/api/v1`; existing ones never change meaning.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// 400: the request is malformed (JSON syntax, query string, path).
    BadRequest,
    /// 400: the pagination cursor was not produced by this query.
    InvalidCursor,
    /// 400: the sign-in link is unknown, already used or expired.
    InvalidLink,
    /// 400: the passkey ceremony is unknown, already finished, older than
    /// 5 minutes, or was started by another session; start it again.
    ChallengeExpired,
    /// 400: the passkey's answer failed verification, or names no passkey
    /// of the account.
    PasskeyInvalid,
    /// 401: the request needs an authenticated session or token.
    Unauthorized,
    /// 403: the caller may not perform this action.
    Forbidden,
    /// 403: a state-changing request with a session cookie failed the CSRF
    /// check: it needs `X-Shelfy-Client: web` and an `Origin` equal to the
    /// public URL.
    CsrfFailed,
    /// 403: the action needs a sign-in from the last 5 minutes;
    /// re-authenticate, then retry.
    ReauthRequired,
    /// 403: the user's storage quota is used up.
    QuotaExceeded,
    /// 404: no such resource for this user.
    NotFound,
    /// 405: the route exists, the method does not (see `Allow`).
    MethodNotAllowed,
    /// 409: the change clashes with existing data.
    Conflict,
    /// 413: the request body is larger than the route allows.
    PayloadTooLarge,
    /// 415: the request body has the wrong media type.
    UnsupportedMediaType,
    /// 422: a value failed validation; `errors` names the fields.
    ValidationFailed,
    /// 422: the AI provider refused the key.
    ProviderKeyInvalid,
    /// 422: the egress policy or the site refused the capture.
    CaptureBlocked,
    /// 423: the account is locked for maintenance (an operator is restoring
    /// its library); retry after `Retry-After` seconds.
    UserLocked,
    /// 426: the browser extension is older than the supported minimum.
    ExtensionOutdated,
    /// 429: too many requests; retry after `Retry-After` seconds.
    RateLimited,
    /// 500: a bug or an unexpected failure; the logs have the details.
    Internal,
    /// 503: a dependency is busy or the server is stopping; retry after
    /// `Retry-After` seconds.
    Unavailable,
    /// 504: the request took longer than the route's time limit.
    Timeout,
}

impl ErrorCode {
    /// The HTTP status of this code.
    #[must_use]
    pub const fn status(self) -> StatusCode {
        match self {
            Self::BadRequest
            | Self::InvalidCursor
            | Self::InvalidLink
            | Self::ChallengeExpired
            | Self::PasskeyInvalid => StatusCode::BAD_REQUEST,
            Self::Unauthorized => StatusCode::UNAUTHORIZED,
            Self::Forbidden | Self::CsrfFailed | Self::ReauthRequired | Self::QuotaExceeded => {
                StatusCode::FORBIDDEN
            }
            Self::NotFound => StatusCode::NOT_FOUND,
            Self::MethodNotAllowed => StatusCode::METHOD_NOT_ALLOWED,
            Self::Conflict => StatusCode::CONFLICT,
            Self::PayloadTooLarge => StatusCode::PAYLOAD_TOO_LARGE,
            Self::UnsupportedMediaType => StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Self::ValidationFailed | Self::ProviderKeyInvalid | Self::CaptureBlocked => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            Self::UserLocked => StatusCode::LOCKED,
            Self::ExtensionOutdated => StatusCode::UPGRADE_REQUIRED,
            Self::RateLimited => StatusCode::TOO_MANY_REQUESTS,
            Self::Internal => StatusCode::INTERNAL_SERVER_ERROR,
            Self::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
        }
    }

    /// The generic code of an error status produced outside a handler. Statuses
    /// without a dedicated code fall back to `bad_request` (4xx) or `internal`
    /// (5xx); the response keeps its original status.
    #[must_use]
    pub fn for_status(status: StatusCode) -> Self {
        match status {
            StatusCode::UNAUTHORIZED => Self::Unauthorized,
            StatusCode::FORBIDDEN => Self::Forbidden,
            StatusCode::NOT_FOUND => Self::NotFound,
            StatusCode::METHOD_NOT_ALLOWED => Self::MethodNotAllowed,
            StatusCode::CONFLICT => Self::Conflict,
            StatusCode::PAYLOAD_TOO_LARGE => Self::PayloadTooLarge,
            StatusCode::UNSUPPORTED_MEDIA_TYPE => Self::UnsupportedMediaType,
            StatusCode::UNPROCESSABLE_ENTITY => Self::ValidationFailed,
            StatusCode::LOCKED => Self::UserLocked,
            StatusCode::UPGRADE_REQUIRED => Self::ExtensionOutdated,
            StatusCode::TOO_MANY_REQUESTS => Self::RateLimited,
            StatusCode::SERVICE_UNAVAILABLE => Self::Unavailable,
            StatusCode::GATEWAY_TIMEOUT => Self::Timeout,
            s if s.is_server_error() => Self::Internal,
            _ => Self::BadRequest,
        }
    }

    /// The wire form, for example `not_found`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::InvalidCursor => "invalid_cursor",
            Self::InvalidLink => "invalid_link",
            Self::ChallengeExpired => "challenge_expired",
            Self::PasskeyInvalid => "passkey_invalid",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::CsrfFailed => "csrf_failed",
            Self::ReauthRequired => "reauth_required",
            Self::QuotaExceeded => "quota_exceeded",
            Self::NotFound => "not_found",
            Self::MethodNotAllowed => "method_not_allowed",
            Self::Conflict => "conflict",
            Self::PayloadTooLarge => "payload_too_large",
            Self::UnsupportedMediaType => "unsupported_media_type",
            Self::ValidationFailed => "validation_failed",
            Self::ProviderKeyInvalid => "provider_key_invalid",
            Self::CaptureBlocked => "capture_blocked",
            Self::UserLocked => "user_locked",
            Self::ExtensionOutdated => "extension_outdated",
            Self::RateLimited => "rate_limited",
            Self::Internal => "internal",
            Self::Unavailable => "unavailable",
            Self::Timeout => "timeout",
        }
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// An error body: RFC 9457 problem details plus the stable `code`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Problem {
    /// Always `about:blank`: `code` carries the problem type.
    #[serde(rename = "type")]
    pub kind: String,
    /// The reason phrase of `status`. Not meant for display.
    pub title: String,
    /// The HTTP status of the response.
    pub status: u16,
    /// What went wrong. Clients map it to their own message.
    pub code: ErrorCode,
    /// Developer-facing detail. Never UI prose, user content or secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schema(nullable = false)]
    pub detail: Option<String>,
    /// The fields that failed validation (`validation_failed`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub errors: Vec<FieldError>,
}

/// One field that failed validation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct FieldError {
    /// The field, as named in the request (camelCase).
    pub field: String,
    /// Why the value was refused, for developers.
    pub reason: String,
}

/// An error returned by a handler; it renders as a [`Problem`].
pub struct ApiError {
    code: ErrorCode,
    status: StatusCode,
    detail: Option<Cow<'static, str>>,
    errors: Vec<FieldError>,
    retry_after: Option<u32>,
    source: Option<anyhow::Error>,
}

impl ApiError {
    /// An error with `code` and its status.
    #[must_use]
    pub fn new(code: ErrorCode) -> Self {
        Self {
            code,
            status: code.status(),
            detail: None,
            errors: Vec::new(),
            retry_after: None,
            source: None,
        }
    }

    /// The generic error for `status` (see [`ErrorCode::for_status`]); the
    /// response keeps `status`.
    #[must_use]
    pub fn from_status(status: StatusCode) -> Self {
        Self {
            status,
            ..Self::new(ErrorCode::for_status(status))
        }
    }

    /// A 500 caused by `source`, which is logged and never sent.
    #[must_use]
    pub fn internal(source: impl Into<anyhow::Error>) -> Self {
        Self::new(ErrorCode::Internal).with_source(source)
    }

    /// 404 `not_found`.
    #[must_use]
    pub fn not_found() -> Self {
        Self::new(ErrorCode::NotFound)
    }

    /// 422 `validation_failed` for one field.
    #[must_use]
    pub fn invalid_field(field: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::new(ErrorCode::ValidationFailed).with_field(field, reason)
    }

    /// Adds a developer-facing detail.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<Cow<'static, str>>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    /// Adds a field that failed validation.
    #[must_use]
    pub fn with_field(mut self, field: impl Into<String>, reason: impl Into<String>) -> Self {
        self.errors.push(FieldError {
            field: field.into(),
            reason: reason.into(),
        });
        self
    }

    /// Sets `Retry-After` (seconds).
    #[must_use]
    pub fn with_retry_after(mut self, seconds: u32) -> Self {
        self.retry_after = Some(seconds);
        self
    }

    /// Attaches the underlying error; it is logged for 5xx and never sent.
    #[must_use]
    pub fn with_source(mut self, source: impl Into<anyhow::Error>) -> Self {
        self.source = Some(source.into());
        self
    }

    /// The code.
    #[must_use]
    pub fn code(&self) -> ErrorCode {
        self.code
    }

    /// The HTTP status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.status
    }

    /// The body this error renders as.
    #[must_use]
    pub fn problem(&self) -> Problem {
        Problem {
            kind: "about:blank".to_owned(),
            title: self.status.canonical_reason().unwrap_or("Error").to_owned(),
            status: self.status.as_u16(),
            code: self.code,
            // A 5xx detail could leak internals: only the logs get the source.
            detail: if self.status.is_server_error() {
                None
            } else {
                self.detail.as_ref().map(|d| d.clone().into_owned())
            },
            errors: self.errors.clone(),
        }
    }

    fn log(&self) {
        let Some(source) = &self.source else {
            return;
        };
        if self.status == StatusCode::SERVICE_UNAVAILABLE {
            tracing::warn!(code = %self.code, error = %format!("{source:#}"), "dependency unavailable");
        } else if self.status.is_server_error() {
            tracing::error!(code = %self.code, error = %format!("{source:#}"), "internal error");
        }
    }
}

impl fmt::Debug for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ApiError")
            .field("code", &self.code)
            .field("status", &self.status)
            .field("detail", &self.detail)
            .field("errors", &self.errors)
            .field("source", &self.source.as_ref().map(|e| format!("{e:#}")))
            .finish_non_exhaustive()
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.detail {
            Some(detail) => write!(f, "{}: {detail}", self.code),
            None => write!(f, "{}", self.code),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        self.log();
        let mut headers = HeaderMap::new();
        if let Some(seconds) = self.retry_after {
            headers.insert(header::RETRY_AFTER, HeaderValue::from(seconds));
        }
        problem_response(self.status, &self.problem(), headers)
    }
}

/// Builds the response for `problem`, with `headers` added.
fn problem_response(status: StatusCode, problem: &Problem, mut headers: HeaderMap) -> Response {
    let body = serde_json::to_vec(problem).expect("a problem always serializes");
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(PROBLEM_JSON));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    *response.headers_mut() = headers;
    response
}

impl From<RepoError> for ApiError {
    fn from(err: RepoError) -> Self {
        match err {
            RepoError::NotFound => Self::not_found(),
            RepoError::Conflict(what) => Self::new(ErrorCode::Conflict).with_detail(what),
            RepoError::Invalid { field, reason } => Self::invalid_field(field, reason),
            RepoError::InvalidCursor => Self::new(ErrorCode::InvalidCursor),
            RepoError::Db(db) => db.into(),
        }
    }
}

/// How long a client waits before retrying a locked account: a restore takes
/// minutes.
pub const USER_LOCKED_RETRY_AFTER_SECS: u32 = 60;

impl ApiError {
    /// 423 `user_locked`, with `Retry-After`.
    #[must_use]
    pub fn user_locked() -> Self {
        Self::new(ErrorCode::UserLocked).with_retry_after(USER_LOCKED_RETRY_AFTER_SECS)
    }
}

impl From<DbError> for ApiError {
    fn from(err: DbError) -> Self {
        if err.is_locked() {
            return Self::user_locked();
        }
        if is_transient(&err) {
            Self::new(ErrorCode::Unavailable)
                .with_retry_after(1)
                .with_source(err)
        } else {
            Self::internal(err)
        }
    }
}

/// Whether a database error clears up on its own: every reader stayed busy,
/// a lock outlasted `busy_timeout`, or the library is locked for maintenance
/// (an operator unlocks it after the restore). Job workers retry these.
pub(crate) fn is_transient(err: &DbError) -> bool {
    match err {
        DbError::ReaderTimeout | DbError::Locked => true,
        DbError::Sqlite(e) => matches!(
            e.sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
        ),
        DbError::Open(inner) => is_transient(inner),
        _ => false,
    }
}

/// Whether an error response is a bare one from a layer: no body type, or
/// plain text. Problems and deliberate bodies (the `/health` JSON on 503)
/// have another content type.
fn is_bare(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_none_or(|v| v.starts_with("text/plain"))
}

/// Middleware: replaces the body of every bare error response (axum's 404
/// and 405, the body limit's 413, the timeout's 504) with the [`Problem`] for
/// its status. The status and the other headers (`Allow`, `Retry-After`,
/// `Content-Range`…) are kept. Responses that already have a typed body are
/// left alone.
pub async fn problem_fallback(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let status = response.status();
    if !(status.is_client_error() || status.is_server_error()) || !is_bare(response.headers()) {
        return response;
    }
    let (mut parts, _body) = response.into_parts();
    for name in [
        header::CONTENT_TYPE,
        header::CONTENT_LENGTH,
        header::CONTENT_ENCODING,
        header::TRANSFER_ENCODING,
    ] {
        parts.headers.remove(name);
    }
    problem_response(
        status,
        &ApiError::from_status(status).problem(),
        parts.headers,
    )
}

/// Response of a handler that panicked (for `CatchPanicLayer`): a 500
/// problem, with the panic message in the logs only.
#[must_use]
pub fn panic_response(panic: Box<dyn Any + Send + 'static>) -> Response {
    let message = panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "non-string panic payload".to_owned());
    tracing::error!(panic = %message, "handler panicked");
    ApiError::new(ErrorCode::Internal).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_code_round_trips_and_keeps_its_status() {
        let codes = [
            ErrorCode::BadRequest,
            ErrorCode::InvalidCursor,
            ErrorCode::InvalidLink,
            ErrorCode::ChallengeExpired,
            ErrorCode::PasskeyInvalid,
            ErrorCode::Unauthorized,
            ErrorCode::Forbidden,
            ErrorCode::CsrfFailed,
            ErrorCode::ReauthRequired,
            ErrorCode::QuotaExceeded,
            ErrorCode::NotFound,
            ErrorCode::MethodNotAllowed,
            ErrorCode::Conflict,
            ErrorCode::PayloadTooLarge,
            ErrorCode::UnsupportedMediaType,
            ErrorCode::ValidationFailed,
            ErrorCode::ProviderKeyInvalid,
            ErrorCode::CaptureBlocked,
            ErrorCode::UserLocked,
            ErrorCode::ExtensionOutdated,
            ErrorCode::RateLimited,
            ErrorCode::Internal,
            ErrorCode::Unavailable,
            ErrorCode::Timeout,
        ];
        for code in codes {
            let json = serde_json::to_value(code).unwrap();
            assert_eq!(json, code.as_str(), "serde and as_str disagree");
            let back: ErrorCode = serde_json::from_value(json).unwrap();
            assert_eq!(back, code);
            assert!(code.status().is_client_error() || code.status().is_server_error());
        }
    }

    #[test]
    fn statuses_map_to_generic_codes() {
        assert_eq!(
            ErrorCode::for_status(StatusCode::NOT_FOUND),
            ErrorCode::NotFound
        );
        assert_eq!(
            ErrorCode::for_status(StatusCode::GATEWAY_TIMEOUT),
            ErrorCode::Timeout
        );
        assert_eq!(
            ErrorCode::for_status(StatusCode::RANGE_NOT_SATISFIABLE),
            ErrorCode::BadRequest
        );
        assert_eq!(
            ErrorCode::for_status(StatusCode::BAD_GATEWAY),
            ErrorCode::Internal
        );
        let err = ApiError::from_status(StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(err.problem().status, 416);
    }

    #[test]
    fn repository_errors_map_to_their_codes() {
        let cases: [(RepoError, ErrorCode); 5] = [
            (RepoError::NotFound, ErrorCode::NotFound),
            (RepoError::Conflict("name"), ErrorCode::Conflict),
            (
                RepoError::Invalid {
                    field: "limit",
                    reason: "out of range",
                },
                ErrorCode::ValidationFailed,
            ),
            (RepoError::InvalidCursor, ErrorCode::InvalidCursor),
            (
                RepoError::Db(DbError::ReaderTimeout),
                ErrorCode::Unavailable,
            ),
        ];
        for (repo, code) in cases {
            let err = ApiError::from(repo);
            assert_eq!(err.code(), code);
        }
        let invalid = ApiError::from(RepoError::Invalid {
            field: "limit",
            reason: "out of range",
        })
        .problem();
        assert_eq!(
            invalid.errors,
            [FieldError {
                field: "limit".into(),
                reason: "out of range".into()
            }]
        );
        let busy = ApiError::from(DbError::Open(std::sync::Arc::new(DbError::ReaderTimeout)));
        assert_eq!(busy.code(), ErrorCode::Unavailable);
        assert_eq!(busy.retry_after, Some(1));
        let broken = ApiError::from(DbError::InvalidUserId);
        assert_eq!(broken.code(), ErrorCode::Internal);
        for locked in [
            DbError::Locked,
            DbError::Open(std::sync::Arc::new(DbError::Locked)),
        ] {
            let err = ApiError::from(locked);
            assert_eq!(err.code(), ErrorCode::UserLocked);
            assert_eq!(err.status(), StatusCode::LOCKED);
            assert_eq!(err.retry_after, Some(USER_LOCKED_RETRY_AFTER_SECS));
        }
        assert_eq!(
            ErrorCode::for_status(StatusCode::LOCKED),
            ErrorCode::UserLocked
        );
    }

    #[test]
    fn server_errors_hide_their_detail() {
        let err = ApiError::new(ErrorCode::Internal).with_detail("SELECT secret FROM x");
        assert_eq!(err.problem().detail, None);
        let err = ApiError::new(ErrorCode::Conflict).with_detail("name");
        assert_eq!(err.problem().detail.as_deref(), Some("name"));
    }
}
