//! Security headers on every response of the API listener: the web app's
//! pages, the API and media alike (plan §7.1, §3.3; P1-09).
//!
//! | Header | Value | When |
//! |---|---|---|
//! | `Content-Security-Policy` | [`CONTENT_SECURITY_POLICY`], the §7.1 policy verbatim (it carries `frame-ancestors 'none'`) | always. A policy the handler set is kept after it: media answers `<policy>; sandbox` |
//! | `Strict-Transport-Security` | [`STRICT_TRANSPORT_SECURITY`] | when `SHELFY_PUBLIC_URL` is https. The app sends it for the Shelfy host only (§3.3): no `includeSubDomains`, no `preload` |
//! | `X-Content-Type-Options` | `nosniff` | always |
//! | `Referrer-Policy` | `no-referrer` | always, as the web app's own `<meta name="referrer">` |
//!
//! [`apply`] runs right inside the request observation ([`crate::app`]), so
//! the answers of every inner layer get the headers too: CSRF refusals, 401s
//! of the access gate, problems, panics and the web app's files. It never
//! touches `Cache-Control`, which each route sets for itself.

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::Response;

use crate::config::PublicUrl;

/// The content security policy of plan §7.1, verbatim. The web app loads
/// nothing from another origin and runs no inline script or style.
pub const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; img-src 'self' data: blob:; \
    media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; object-src 'none'; \
    base-uri 'none'; form-action 'self'";

/// `Strict-Transport-Security`: one year, this host only.
pub const STRICT_TRANSPORT_SECURITY: &str = "max-age=31536000";

/// `Referrer-Policy`.
pub const REFERRER_POLICY: &str = "no-referrer";

/// The headers of [`apply`], fixed at start from the configuration.
#[derive(Clone, Debug)]
pub struct SecurityHeaders {
    hsts: bool,
}

impl SecurityHeaders {
    /// The headers for a server whose public origin is `public_url`.
    #[must_use]
    pub fn new(public_url: &PublicUrl) -> Self {
        Self {
            hsts: public_url.as_str().starts_with("https://"),
        }
    }

    /// Whether `Strict-Transport-Security` is sent.
    #[must_use]
    pub fn sends_hsts(&self) -> bool {
        self.hsts
    }

    /// Adds the headers to a response's `headers`.
    pub fn add_to(&self, headers: &mut HeaderMap) {
        let policy = match headers.get(header::CONTENT_SECURITY_POLICY) {
            None => HeaderValue::from_static(CONTENT_SECURITY_POLICY),
            Some(own)
                if own
                    .as_bytes()
                    .starts_with(CONTENT_SECURITY_POLICY.as_bytes()) =>
            {
                own.clone()
            }
            Some(own) => {
                let combined = [CONTENT_SECURITY_POLICY.as_bytes(), b"; ", own.as_bytes()].concat();
                HeaderValue::from_bytes(&combined)
                    .unwrap_or(HeaderValue::from_static(CONTENT_SECURITY_POLICY))
            }
        };
        headers.insert(header::CONTENT_SECURITY_POLICY, policy);
        headers.insert(
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        );
        headers.insert(
            header::REFERRER_POLICY,
            HeaderValue::from_static(REFERRER_POLICY),
        );
        if self.hsts {
            headers.insert(
                header::STRICT_TRANSPORT_SECURITY,
                HeaderValue::from_static(STRICT_TRANSPORT_SECURITY),
            );
        }
    }
}

/// Middleware: adds the security headers to every response.
pub async fn apply(
    State(headers): State<SecurityHeaders>,
    request: Request,
    next: Next,
) -> Response {
    let mut response = next.run(request).await;
    headers.add_to(response.headers_mut());
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers_for(public_url: &str, own_policy: Option<&'static str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        if let Some(own) = own_policy {
            headers.insert(
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(own),
            );
        }
        SecurityHeaders::new(&PublicUrl::parse(public_url).unwrap()).add_to(&mut headers);
        headers
    }

    #[test]
    fn the_policy_is_the_plan_s_verbatim() {
        // Copied from IMPLEMENTATION-PLAN.md §7.1 (SPA row), on one line.
        let plan = "default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; object-src 'none'; base-uri 'none'; form-action 'self'";
        assert_eq!(CONTENT_SECURITY_POLICY, plan);
    }

    #[test]
    fn https_hosts_get_hsts_and_local_ones_do_not() {
        let headers = headers_for("https://refs.example.test", None);
        assert_eq!(
            headers[header::STRICT_TRANSPORT_SECURITY],
            STRICT_TRANSPORT_SECURITY
        );
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY
        );
        assert_eq!(headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(headers["referrer-policy"], "no-referrer");

        let headers = headers_for("http://localhost:8080", None);
        assert!(headers.get(header::STRICT_TRANSPORT_SECURITY).is_none());
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            CONTENT_SECURITY_POLICY
        );
    }

    #[test]
    fn a_route_s_own_policy_is_kept_after_the_app_s() {
        let headers = headers_for("https://refs.example.test", Some("sandbox"));
        assert_eq!(
            headers[header::CONTENT_SECURITY_POLICY],
            format!("{CONTENT_SECURITY_POLICY}; sandbox").as_str()
        );
        // Applied twice (a replayed response), the policy is not repeated.
        let mut again = headers.clone();
        SecurityHeaders::new(&PublicUrl::parse("https://refs.example.test").unwrap())
            .add_to(&mut again);
        assert_eq!(again, headers);
    }
}
