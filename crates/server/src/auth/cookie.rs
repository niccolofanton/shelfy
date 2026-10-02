//! The session cookie (plan §2.11): `__Host-shelfy_session`, `Secure`,
//! `HttpOnly`, `SameSite=Lax`, `Path=/`, no `Domain`.
//!
//! The `__Host-` prefix makes browsers refuse the cookie unless it is
//! `Secure`, host-only and on `/`, so a sibling subdomain can neither set nor
//! shadow it. Browsers accept `Secure` cookies over plain HTTP only on
//! `localhost`: a non-local `SHELFY_PUBLIC_URL` must be `https`.
//!
//! The value is a [`SecretToken`]; the server stores only its SHA-256.

use std::time::Duration;

use axum::http::{HeaderMap, HeaderValue, header};

use crate::tokens::SecretToken;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "__Host-shelfy_session";

/// Attributes shared by every `Set-Cookie` of the session cookie.
const ATTRIBUTES: &str = "Path=/; HttpOnly; Secure; SameSite=Lax";

/// The session cookie's value, if the request carries one and no
/// `Authorization` header: a request with a token never authenticates with
/// its cookies (§2.9), so the cookie CSRF check can skip it safely.
#[must_use]
pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key(header::AUTHORIZATION) {
        return None;
    }
    raw_session_cookie(headers)
}

/// Whether the request carries the session cookie at all, valid or not.
#[must_use]
pub fn has_session_cookie(headers: &HeaderMap) -> bool {
    raw_session_cookie(headers).is_some()
}

/// The first `__Host-shelfy_session` value across all `Cookie` headers
/// (HTTP/2 may split them). The prefix rules out a second one from this
/// origin.
fn raw_session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (name, value) = pair.trim().split_once('=')?;
            (name.trim() == SESSION_COOKIE).then(|| value.trim())
        })
}

/// `Set-Cookie` for a new session that the browser keeps for `max_age`.
#[must_use]
pub fn set_session(token: &SecretToken, max_age: Duration) -> HeaderValue {
    let value = format!(
        "{SESSION_COOKIE}={}; Max-Age={}; {ATTRIBUTES}",
        token.expose(),
        max_age.as_secs()
    );
    HeaderValue::from_str(&value).expect("a base64url token is a valid cookie value")
}

/// `Set-Cookie` that removes the session cookie.
#[must_use]
pub fn clear_session() -> HeaderValue {
    HeaderValue::from_str(&format!("{SESSION_COOKIE}=; Max-Age=0; {ATTRIBUTES}"))
        .expect("a valid header value")
}

/// Whether browsers keep a `Secure` cookie set by `origin`: an `https`
/// origin, or a loopback one (`localhost`, `*.localhost`, `127.0.0.0/8`,
/// `[::1]`), which browsers treat as secure.
#[must_use]
pub fn secure_cookies_work(origin: &str) -> bool {
    let Ok(url) = url::Url::parse(origin) else {
        return false;
    };
    if url.scheme() == "https" {
        return true;
    }
    match url.host() {
        Some(url::Host::Domain(host)) => host == "localhost" || host.ends_with(".localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(pairs: &[(header::HeaderName, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.append(name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn the_session_cookie_is_found_among_others() {
        let map = headers(&[
            (
                header::COOKIE,
                "theme=dark; __Host-shelfy_session=abc123 ; x=1",
            ),
            (header::COOKIE, "__Host-shelfy_session=second"),
        ]);
        assert_eq!(session_token(&map), Some("abc123"));
        assert!(has_session_cookie(&map));

        let split = headers(&[
            (header::COOKIE, "theme=dark"),
            (header::COOKIE, "__Host-shelfy_session=v"),
        ]);
        assert_eq!(session_token(&split), Some("v"));

        for missing in [
            headers(&[]),
            headers(&[(header::COOKIE, "shelfy_session=abc")]),
            headers(&[(header::COOKIE, "__Host-shelfy_session2=abc")]),
            headers(&[(header::COOKIE, "__Host-shelfy_session")]),
        ] {
            assert_eq!(session_token(&missing), None, "{missing:?}");
        }
    }

    #[test]
    fn an_authorization_header_disables_the_cookie() {
        let map = headers(&[
            (header::COOKIE, "__Host-shelfy_session=abc"),
            (header::AUTHORIZATION, "Bearer shx_token"),
        ]);
        assert_eq!(session_token(&map), None);
        assert!(has_session_cookie(&map), "still visible to the CSRF check");
    }

    #[test]
    fn set_cookie_carries_every_flag() {
        let token = SecretToken::generate();
        let set = set_session(&token, Duration::from_secs(7_776_000));
        assert_eq!(
            set.to_str().unwrap(),
            format!(
                "__Host-shelfy_session={}; Max-Age=7776000; Path=/; HttpOnly; Secure; SameSite=Lax",
                token.expose()
            )
        );
        assert_eq!(
            clear_session().to_str().unwrap(),
            "__Host-shelfy_session=; Max-Age=0; Path=/; HttpOnly; Secure; SameSite=Lax"
        );
    }

    #[test]
    fn secure_cookies_need_https_or_a_loopback_origin() {
        for works in [
            "https://refs.niccolofanton.dev",
            "http://localhost:18090",
            "http://app.localhost:5173",
            "http://127.0.0.1:8080",
            "http://[::1]:8080",
        ] {
            assert!(secure_cookies_work(works), "{works}");
        }
        for fails in [
            "http://192.168.1.10:8080",
            "http://refs.example.test",
            "not a url",
        ] {
            assert!(!secure_cookies_work(fails), "{fails}");
        }
    }
}
