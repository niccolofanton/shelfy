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

use crate::net;
use crate::tokens::SecretToken;

/// Name of the session cookie.
pub const SESSION_COOKIE: &str = "__Host-shelfy_session";

/// Attributes shared by every `Set-Cookie` of the session cookie.
const ATTRIBUTES: &str = "Path=/; HttpOnly; Secure; SameSite=Lax";

/// The session cookie's value, if the request carries one and no
/// `Authorization` header: a request with a token never authenticates with
/// its cookies (§2.9), which is why the CSRF check can skip it.
#[must_use]
pub fn session_token(headers: &HeaderMap) -> Option<&str> {
    if headers.contains_key(header::AUTHORIZATION) {
        return None;
    }
    raw_session_cookie(headers)
}

/// The first `__Host-shelfy_session` value across all `Cookie` headers
/// (HTTP/2 may split them). The prefix rules out a second one from this
/// origin. Headers are split as bytes, so another cookie with a non-ASCII
/// value (which a header string cannot hold) does not hide this one.
fn raw_session_cookie(headers: &HeaderMap) -> Option<&str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .flat_map(|value| value.as_bytes().split(|byte| *byte == b';'))
        .find_map(|pair| {
            let at = pair.iter().position(|byte| *byte == b'=')?;
            let (name, value) = (pair[..at].trim_ascii(), pair[at + 1..].trim_ascii());
            if name != SESSION_COOKIE.as_bytes() {
                return None;
            }
            std::str::from_utf8(value).ok()
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
    url.scheme() == "https" || url.host_str().is_some_and(net::is_loopback_host)
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
    fn a_non_ascii_cookie_does_not_hide_the_session() {
        let mut map = HeaderMap::new();
        map.append(
            header::COOKIE,
            HeaderValue::from_bytes(b"note=caf\xc3\xa9; __Host-shelfy_session=abc; z=\xff")
                .unwrap(),
        );
        assert!(map[header::COOKIE].to_str().is_err(), "not a header string");
        assert_eq!(session_token(&map), Some("abc"));

        let mut garbled = HeaderMap::new();
        garbled.append(
            header::COOKIE,
            HeaderValue::from_bytes(b"__Host-shelfy_session=\xff\xfe").unwrap(),
        );
        garbled.append(
            header::COOKIE,
            HeaderValue::from_static("__Host-shelfy_session=ok"),
        );
        assert_eq!(session_token(&garbled), Some("ok"), "the next valid value");
    }

    #[test]
    fn an_authorization_header_disables_the_cookie() {
        let map = headers(&[
            (header::COOKIE, "__Host-shelfy_session=abc"),
            (header::AUTHORIZATION, "Bearer shx_token"),
        ]);
        assert_eq!(session_token(&map), None);
        assert_eq!(raw_session_cookie(&map), Some("abc"), "present, not used");
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
