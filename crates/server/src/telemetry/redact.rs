//! Typed redaction (plan §3.7). What never reaches the logs: captions, post
//! URLs, query strings, tokens and session ids, provider keys, email
//! addresses and bodies.
//!
//! - [`Redacted`] holds a value that is never printed: its `Debug` and
//!   `Display` print `[redacted]`. A struct holding such a value derives
//!   `Debug` safely when the field is `Redacted<_>`, and
//!   `tracing::info!(email = %Redacted(&email))` records the field's
//!   presence without its content. Reading the value takes an explicit
//!   [`Redacted::expose`]. Secret tokens have their own type,
//!   [`crate::tokens::SecretToken`], whose `Debug` hides them too.
//! - [`ClientText`] is free text a client sent to be logged (a crash
//!   report's message and stacks). It prints with every URL, email address,
//!   query string and token-like string replaced, keeping only the web app's
//!   own asset URLs, which a stack trace needs.
//!
//! The rest is structural: the request logs carry the route template, never
//! the URL ([`super::http`]); handlers log codes and ids, never request or
//! library content. `tests/log_redaction.rs` plants each kind of secret in
//! real requests and library rows and checks that none reaches the logs.

use std::borrow::Cow;
use std::fmt;
use std::sync::LazyLock;

use regex::{Captures, Regex};

/// What a redacted value prints as.
pub const REDACTED: &str = "[redacted]";
/// What a URL in [`ClientText`] prints as.
pub const REDACTED_URL: &str = "[url]";
/// What an email address in [`ClientText`] prints as.
pub const REDACTED_EMAIL: &str = "[email]";

/// A value that is never printed.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Redacted<T>(pub T);

impl<T> Redacted<T> {
    /// The value, for the one place that needs it.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }

    /// Unwraps the value.
    #[must_use]
    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> From<T> for Redacted<T> {
    fn from(value: T) -> Self {
        Self(value)
    }
}

impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

/// Free text from a client, printed scrubbed (`Display` and `Debug`):
///
/// | In the text | Printed as |
/// |---|---|
/// | an absolute URL under `/assets/` (a frame of a stack trace) | the URL without credentials, query or fragment |
/// | any other absolute URL (a post, a page, a sign-in link) | `[url]` |
/// | an email address | `[email]` |
/// | a query string (`?name=…`) | `?[redacted]` |
/// | a run of letters, digits, `-` and `_` that looks random: 32 or more with a digit, or 40 or more mixing upper and lower case (a session id, a token, a key) | `[redacted]` |
///
/// Captions and notes cannot be told from other prose: reports carry none by
/// contract ([`crate::routes::client_errors`] refuses content fields).
#[derive(Clone, Copy)]
pub struct ClientText<'a>(pub &'a str);

impl ClientText<'_> {
    /// The scrubbed text.
    #[must_use]
    pub fn scrubbed(&self) -> String {
        let text = URL.replace_all(self.0, |caps: &Captures<'_>| asset_url(&caps[0]));
        let text = EMAIL.replace_all(&text, REDACTED_EMAIL);
        let text = QUERY.replace_all(&text, "?[redacted]");
        TOKEN
            .replace_all(&text, |caps: &Captures<'_>| {
                let run = &caps[0];
                if looks_random(run) {
                    REDACTED.to_owned()
                } else {
                    run.to_owned()
                }
            })
            .into_owned()
    }
}

impl fmt::Display for ClientText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.scrubbed())
    }
}

impl fmt::Debug for ClientText<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.scrubbed(), f)
    }
}

/// An absolute URL: a scheme, `://`, then anything RFC 3986 allows. A frame
/// in parentheses keeps its closing one out (`at f (https://…/x.js:1:2)`).
static URL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)\b[a-z][a-z0-9+.-]*://[^\s"'<>\\^`{|}()]+"#).expect("valid pattern")
});
/// An email address.
static EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)+").expect("valid pattern")
});
/// A query string: `?name=` and the rest of the word.
static QUERY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"\?[A-Za-z0-9_.~%+\[\]-]+=[^\s"'<>\\^`{|}()]*"#).expect("valid pattern")
});
/// A long run of base64url characters: a secret token is 43 (session ids,
/// sign-in links) or more (`shx_` API tokens).
static TOKEN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9_-]{32,}").expect("valid pattern"));

/// Whether a [`TOKEN`] run looks like a secret rather than a word: it has a
/// digit, or it is 40 or more long and mixes cases. Random base64url
/// almost always does both; a long identifier or a repeated letter does
/// neither.
fn looks_random(run: &str) -> bool {
    let has = |class: fn(&u8) -> bool| run.bytes().any(|b| class(&b));
    has(u8::is_ascii_digit)
        || (run.len() >= 40 && has(u8::is_ascii_lowercase) && has(u8::is_ascii_uppercase))
}

/// `url` reduced to `scheme://host/assets/…` when it names a bundled asset,
/// or [`REDACTED_URL`].
fn asset_url(url: &str) -> Cow<'static, str> {
    let Some((scheme, rest)) = url.split_once("://") else {
        return Cow::Borrowed(REDACTED_URL);
    };
    let (authority, path) = match rest.find('/') {
        Some(slash) => rest.split_at(slash),
        None => return Cow::Borrowed(REDACTED_URL),
    };
    let host = authority.rsplit('@').next().unwrap_or_default();
    let path = path.split(['?', '#']).next().unwrap_or_default();
    if path.starts_with("/assets/") && !path.contains("..") {
        Cow::Owned(format!("{scheme}://{host}{path}"))
    } else {
        Cow::Borrowed(REDACTED_URL)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    #[allow(dead_code)] // fields are only read through `Debug`
    struct Login {
        user: &'static str,
        email: Redacted<String>,
    }

    #[test]
    fn redacted_values_never_print() {
        let email = Redacted("owner@example.test".to_owned());
        assert_eq!(format!("{email}"), REDACTED);
        assert_eq!(format!("{email:?}"), REDACTED);
        let login = Login {
            user: "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            email,
        };
        let debug = format!("{login:?}");
        assert!(!debug.contains("owner@example.test"), "{debug}");
        assert!(debug.contains(REDACTED));
        assert_eq!(login.email.expose(), "owner@example.test");
    }

    fn scrub(text: &str) -> String {
        ClientText(text).scrubbed()
    }

    #[test]
    fn client_text_keeps_stack_frames() {
        let chrome = "TypeError: x is undefined\n    at Modal (https://refs.example.test/assets/index-3f2a.js:1:2345)\n    at App (https://refs.example.test/assets/index-3f2a.js:9:10)";
        assert_eq!(scrub(chrome), chrome, "frames of bundled assets stay");
        let firefox = "Modal@https://refs.example.test/assets/index-3f2a.js:1:2345";
        assert_eq!(scrub(firefox), firefox);
        assert_eq!(
            scrub("at f (https://user:pw@refs.example.test/assets/a.js?v=1#x)"),
            "at f (https://refs.example.test/assets/a.js)",
            "credentials, query and fragment go"
        );
        assert_eq!(
            scrub("Cannot read properties of undefined (reading 'slides')"),
            "Cannot read properties of undefined (reading 'slides')",
            "prose stays"
        );
        assert_eq!(
            scrub("at useSyncExternalStoreWithSelector (x)"),
            "at useSyncExternalStoreWithSelector (x)",
            "long identifiers without digits stay"
        );
        assert_eq!(
            scrub("token abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ"),
            "token [redacted]",
            "43 characters of mixed case are a token, digits or not"
        );
        let filler = "m".repeat(60);
        assert_eq!(scrub(&filler), filler, "a repeated letter is no token");
        let words = "cannot_read_properties_of_undefined_reading_slides";
        assert_eq!(scrub(words), words);
    }

    #[test]
    fn client_text_drops_urls_addresses_queries_and_tokens() {
        let cases = [
            (
                "Failed to load https://www.instagram.com/p/DA1b2C3dE4f/?igsh=MWQ1 now",
                "Failed to load [url] now",
            ),
            (
                "open http://refs.example.test/login/magic#Q2xpZW50U2VjcmV0VG9rZW4xMjM0NTY3ODkw",
                "open [url]",
            ),
            (
                "GET /api/v1/search?q=private+lamp&limit=60 failed",
                "GET /api/v1/search?[redacted] failed",
            ),
            ("mail owner@example.test failed", "mail [email] failed"),
            (
                "bad session 3q2-7W_AbCdEfGhIjKlMnOpQrStUvWxYz0123456789abc",
                "bad session [redacted]",
            ),
            (
                "Bearer shx_4fV9kQ2mZ8pL1xR7tY3wN6cB0hJ5gD2s",
                "Bearer [redacted]",
            ),
            (
                "img blob:https://refs.example.test/1c9a-44e0 broke",
                "img blob:[url] broke",
            ),
            (
                "see (https://pbs.twimg.com/media/Fx.jpg?name=large)",
                "see ([url])",
            ),
            ("HTTPS://EXAMPLE.TEST/A?B=C", "[url]"),
            ("https://refs.example.test/assets/../p/ig_1 x", "[url] x"),
        ];
        for (text, scrubbed) in cases {
            assert_eq!(scrub(text), scrubbed, "{text}");
        }
        let text = "x https://example.test/p/1?a=b";
        assert_eq!(format!("{}", ClientText(text)), "x [url]");
        assert_eq!(format!("{:?}", ClientText(text)), "\"x [url]\"");
    }
}
