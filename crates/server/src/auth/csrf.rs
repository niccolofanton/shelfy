//! The CSRF and Origin guard (plan §2.9 CSRF, §7.1).
//!
//! Every state-changing request (any method but `GET`, `HEAD` and `OPTIONS`)
//! that carries the session cookie must:
//!
//! 1. not be marked cross-origin by the browser: `Sec-Fetch-Site`, when
//!    present, is `same-origin`;
//! 2. carry an `Origin` equal to `SHELFY_PUBLIC_URL` (byte for byte, as
//!    browsers serialize it);
//! 3. carry `X-Shelfy-Client: web`. A cross-origin page cannot add this header
//!    without a CORS preflight, which the server never grants, and a form
//!    cannot add it at all.
//!
//! Otherwise it answers 403 `csrf_failed` before any handler runs.
//! `SameSite=Lax` already keeps the cookie off cross-site subrequests; the
//! `Origin` check also covers same-site attackers (other subdomains of the
//! public host's site), which `SameSite` does not.
//!
//! Requests without the session cookie are not checked: they can only reach
//! public routes, whose bodies are JSON (a form cannot send JSON, and a
//! cross-origin `fetch` of JSON needs a preflight). Requests with an
//! `Authorization` header are not checked either: they never authenticate
//! with cookies ([`super::cookie::session_token`]), so there is no ambient
//! credential to forge. That is how the extension, the iOS Shortcut and the
//! migration CLI (bearer tokens, P1-17) call the API.
//!
//! The SPA therefore sends `X-Shelfy-Client: web` on every unsafe request
//! (`fetch`, including `keepalive`; `navigator.sendBeacon` cannot set it).

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderName, Method, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};

use super::cookie;
use crate::error::{ApiError, ErrorCode};
use crate::state::AppState;

/// Header the SPA sends on every unsafe request.
pub const CLIENT_HEADER: HeaderName = HeaderName::from_static("x-shelfy-client");

/// Value of [`CLIENT_HEADER`] for the web app.
pub const CLIENT_WEB: &str = "web";

const SEC_FETCH_SITE: HeaderName = HeaderName::from_static("sec-fetch-site");

/// Why a request failed the check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CsrfFailure {
    /// `Sec-Fetch-Site` says the request comes from another origin.
    CrossOrigin,
    /// No `Origin` header.
    MissingOrigin,
    /// `Origin` is not the public URL.
    OriginMismatch,
    /// `X-Shelfy-Client: web` is missing.
    MissingClientHeader,
}

impl CsrfFailure {
    /// The developer-facing detail of the 403.
    #[must_use]
    pub const fn detail(self) -> &'static str {
        match self {
            Self::CrossOrigin => "Sec-Fetch-Site must be same-origin",
            Self::MissingOrigin => "the Origin header is required",
            Self::OriginMismatch => "Origin must be the public URL",
            Self::MissingClientHeader => "X-Shelfy-Client: web is required",
        }
    }

    /// A short label for the logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CrossOrigin => "cross_origin",
            Self::MissingOrigin => "missing_origin",
            Self::OriginMismatch => "origin_mismatch",
            Self::MissingClientHeader => "missing_client_header",
        }
    }
}

/// Checks one request against `public_origin` (see the module docs).
///
/// # Errors
///
/// The first rule the request breaks.
pub fn check(method: &Method, headers: &HeaderMap, public_origin: &str) -> Result<(), CsrfFailure> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        || headers.contains_key(header::AUTHORIZATION)
        || !cookie::has_session_cookie(headers)
    {
        return Ok(());
    }
    if headers
        .get(SEC_FETCH_SITE)
        .is_some_and(|site| site.as_bytes() != b"same-origin")
    {
        return Err(CsrfFailure::CrossOrigin);
    }
    match headers.get(header::ORIGIN) {
        None => return Err(CsrfFailure::MissingOrigin),
        Some(origin) if origin.as_bytes() != public_origin.as_bytes() => {
            return Err(CsrfFailure::OriginMismatch);
        }
        Some(_) => {}
    }
    if headers
        .get(CLIENT_HEADER)
        .is_none_or(|client| client.as_bytes() != CLIENT_WEB.as_bytes())
    {
        return Err(CsrfFailure::MissingClientHeader);
    }
    Ok(())
}

/// Middleware: answers 403 `csrf_failed` when [`check`] fails.
pub async fn protect(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let public_origin = state.config().public_url.as_str();
    match check(request.method(), request.headers(), public_origin) {
        Ok(()) => next.run(request).await,
        Err(failure) => {
            tracing::info!(reason = failure.as_str(), "CSRF check refused a request");
            ApiError::new(ErrorCode::CsrfFailed)
                .with_detail(failure.detail())
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const PUBLIC: &str = "https://refs.example.test";

    fn request(pairs: &[(&str, &str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in pairs {
            headers.append(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        headers
    }

    const COOKIE: (&str, &str) = ("cookie", "__Host-shelfy_session=abc");
    const ORIGIN: (&str, &str) = ("origin", PUBLIC);
    const CLIENT: (&str, &str) = ("x-shelfy-client", "web");

    #[test]
    fn a_same_origin_spa_request_passes() {
        let ok = request(&[COOKIE, ORIGIN, CLIENT, ("sec-fetch-site", "same-origin")]);
        for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert_eq!(check(&method, &ok, PUBLIC), Ok(()), "{method}");
        }
        // Browsers without Sec-Fetch-* still pass on Origin and the header.
        let old_browser = request(&[COOKIE, ORIGIN, CLIENT]);
        assert_eq!(check(&Method::POST, &old_browser, PUBLIC), Ok(()));
    }

    #[test]
    fn cookie_requests_that_break_a_rule_fail() {
        let cases: [(&[(&str, &str)], CsrfFailure); 8] = [
            (&[COOKIE, ORIGIN], CsrfFailure::MissingClientHeader),
            (
                &[COOKIE, ORIGIN, ("x-shelfy-client", "extension")],
                CsrfFailure::MissingClientHeader,
            ),
            (&[COOKIE, CLIENT], CsrfFailure::MissingOrigin),
            (
                &[COOKIE, CLIENT, ("origin", "https://evil.example.test")],
                CsrfFailure::OriginMismatch,
            ),
            (
                &[
                    COOKIE,
                    CLIENT,
                    ("origin", "https://refs.example.test.evil.test"),
                ],
                CsrfFailure::OriginMismatch,
            ),
            (
                &[COOKIE, CLIENT, ("origin", "null")],
                CsrfFailure::OriginMismatch,
            ),
            (
                &[COOKIE, ORIGIN, CLIENT, ("sec-fetch-site", "same-site")],
                CsrfFailure::CrossOrigin,
            ),
            (
                &[COOKIE, ORIGIN, CLIENT, ("sec-fetch-site", "cross-site")],
                CsrfFailure::CrossOrigin,
            ),
        ];
        for (headers, failure) in cases {
            assert_eq!(
                check(&Method::POST, &request(headers), PUBLIC),
                Err(failure),
                "{headers:?}"
            );
        }
        let none = request(&[COOKIE, ORIGIN, CLIENT, ("sec-fetch-site", "none")]);
        assert_eq!(
            check(&Method::DELETE, &none, PUBLIC),
            Err(CsrfFailure::CrossOrigin)
        );
    }

    #[test]
    fn safe_methods_cookieless_and_token_requests_are_not_checked() {
        let cross = request(&[
            COOKIE,
            ("origin", "https://evil.test"),
            ("sec-fetch-site", "cross-site"),
        ]);
        for method in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert_eq!(check(&method, &cross, PUBLIC), Ok(()), "{method}");
        }
        assert_eq!(
            check(&Method::TRACE, &cross, PUBLIC),
            Err(CsrfFailure::CrossOrigin)
        );

        let cookieless = request(&[
            ("origin", "https://evil.test"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert_eq!(check(&Method::POST, &cookieless, PUBLIC), Ok(()));

        let token = request(&[
            COOKIE,
            ("authorization", "Bearer shx_token"),
            ("origin", "https://evil.test"),
            ("sec-fetch-site", "cross-site"),
        ]);
        assert_eq!(check(&Method::POST, &token, PUBLIC), Ok(()));
    }
}
