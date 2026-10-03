//! Conditional GET (plan §2.9): weak ETags derived from the user's library
//! generation and the request, and `304 Not Modified` for unchanged views.
//!
//! A view's ETag is a hash of:
//!
//! - the view (which route) and its normalized parameters;
//! - the user's id;
//! - the library [`Generation`]: its `counter` changes with every committed
//!   write that changes rows, and its `instance` changes whenever the database
//!   handle is reopened (cache eviction, restart), so an old ETag never matches
//!   a counter that restarted.
//!
//! The check needs no SQLite: the generation is an atomic of the open handle,
//! so an unchanged view answers 304 without a query.
//!
//! **Order matters.** A handler reads the generation *before* it opens its read
//! snapshot. A write that commits in between makes the body newer than its
//! ETag, which costs one extra full response later; the reverse, an ETag newer
//! than its body, cannot happen, so a 304 never hides a change.
//!
//! Responses carry `Cache-Control: private, no-cache`: the browser stores them
//! but revalidates every time, so a plain `fetch` gets the 304 path for free.
//! Shared caches (Cloudflare) never store them.

use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use sha2::{Digest, Sha256};
use shelfy_core::db::Generation;
use utoipa::IntoParams;

/// `Cache-Control` of every conditional response.
pub const CACHE_CONTROL: &str = "private, no-cache";

/// The request header of a conditional GET, for the OpenAPI document
/// (`params(ConditionalHeaders)`). Handlers read it with [`ETag::matches`].
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct ConditionalHeaders {
    /// The `ETag` of an earlier response. When the view has not changed since,
    /// the answer is 304 with no body.
    #[param(rename = "If-None-Match", nullable = false)]
    pub if_none_match: Option<String>,
}

/// Bumped when the shape of a cached response changes without a restart
/// (never, today: a deploy restarts the process, which changes every
/// `Generation::instance`).
const ETAG_VERSION: &[u8] = b"shelfy-etag-1";

/// A weak entity tag, `W/"<22 base64url characters>"`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ETag(HeaderValue);

impl ETag {
    /// The ETag of `view` (a route name such as `posts.list`) with `params`
    /// (its normalized parameters) for `user_id` at `generation`.
    ///
    /// # Panics
    ///
    /// When `params` cannot be serialized to JSON (a map with non-string
    /// keys); every parameter type of the API serializes.
    #[must_use]
    pub fn for_view(
        view: &str,
        user_id: &str,
        generation: Generation,
        params: &impl Serialize,
    ) -> Self {
        let params = serde_json::to_vec(params).expect("view parameters serialize");
        let mut hash = Sha256::new();
        for part in [
            ETAG_VERSION,
            view.as_bytes(),
            user_id.as_bytes(),
            &generation.instance.to_le_bytes(),
            &generation.counter.to_le_bytes(),
            &params,
        ] {
            // Length-prefixed, so no two inputs concatenate to the same bytes.
            hash.update((part.len() as u64).to_le_bytes());
            hash.update(part);
        }
        let digest = hash.finalize();
        let value = format!("W/\"{}\"", URL_SAFE_NO_PAD.encode(&digest[..16]));
        Self(HeaderValue::from_str(&value).expect("base64url is a valid header value"))
    }

    /// The ETag of a document that is the same for every user and does not
    /// come from a library, such as the extension's configuration: a hash of
    /// `view` and the document itself, so it changes exactly when the
    /// document does.
    ///
    /// # Panics
    ///
    /// When `document` cannot be serialized to JSON (a map with non-string
    /// keys); every response type of the API serializes.
    #[must_use]
    pub fn for_document(view: &str, document: &impl Serialize) -> Self {
        let none = Generation {
            instance: 0,
            counter: 0,
        };
        Self::for_view(view, "", none, document)
    }

    /// The header value.
    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.to_str().expect("built from ASCII")
    }

    /// Whether the request's `If-None-Match` names this ETag, by the weak
    /// comparison of RFC 9110 §8.8.3.2 (the `W/` prefix is ignored), or is `*`.
    #[must_use]
    pub fn matches(&self, headers: &HeaderMap) -> bool {
        let ours = self.opaque_tag();
        headers
            .get_all(header::IF_NONE_MATCH)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .any(|tag| tag == "*" || tag.strip_prefix("W/").unwrap_or(tag) == ours)
    }

    /// `304 Not Modified` with this ETag and no body.
    #[must_use]
    pub fn not_modified(&self) -> Response {
        let mut response = StatusCode::NOT_MODIFIED.into_response();
        self.stamp(response.headers_mut());
        response
    }

    /// `body` as the response, with this ETag and the cache policy.
    #[must_use]
    pub fn respond(&self, body: impl IntoResponse) -> Response {
        let mut response = body.into_response();
        self.stamp(response.headers_mut());
        response
    }

    fn stamp(&self, headers: &mut HeaderMap) {
        headers.insert(header::ETAG, self.0.clone());
        headers.insert(
            header::CACHE_CONTROL,
            HeaderValue::from_static(CACHE_CONTROL),
        );
    }

    /// The quoted part, without `W/`.
    fn opaque_tag(&self) -> &str {
        let value = self.as_str();
        value.strip_prefix("W/").unwrap_or(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GEN: Generation = Generation {
        instance: 7,
        counter: 3,
    };

    fn if_none_match(values: &[&str]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for value in values {
            headers.append(header::IF_NONE_MATCH, HeaderValue::from_str(value).unwrap());
        }
        headers
    }

    #[test]
    fn etags_change_with_every_input() {
        let base = ETag::for_view("posts.list", "u1", GEN, &("q", 1));
        assert!(base.as_str().starts_with("W/\""), "{}", base.as_str());
        assert_eq!(base.as_str().len(), 2 + 1 + 22 + 1);
        assert_eq!(base, ETag::for_view("posts.list", "u1", GEN, &("q", 1)));
        let others = [
            ETag::for_view("posts.get", "u1", GEN, &("q", 1)),
            ETag::for_view("posts.list", "u2", GEN, &("q", 1)),
            ETag::for_view(
                "posts.list",
                "u1",
                Generation { instance: 8, ..GEN },
                &("q", 1),
            ),
            ETag::for_view(
                "posts.list",
                "u1",
                Generation { counter: 4, ..GEN },
                &("q", 1),
            ),
            ETag::for_view("posts.list", "u1", GEN, &("q", 2)),
            // Shifting bytes between fields does not collide.
            ETag::for_view("posts.lis", "tu1", GEN, &("q", 1)),
        ];
        for other in others {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn if_none_match_uses_the_weak_comparison() {
        let etag = ETag::for_view("v", "u", GEN, &());
        let strong = etag.as_str().trim_start_matches("W/").to_owned();
        assert!(etag.matches(&if_none_match(&[etag.as_str()])));
        assert!(etag.matches(&if_none_match(&[&strong])));
        assert!(etag.matches(&if_none_match(&["*"])));
        assert!(etag.matches(&if_none_match(&[&format!("\"x\", {}", etag.as_str())])));
        assert!(etag.matches(&if_none_match(&["\"x\"", etag.as_str()])));
        assert!(!etag.matches(&if_none_match(&[])));
        assert!(!etag.matches(&if_none_match(&["\"x\", W/\"y\""])));
    }

    #[test]
    fn responses_carry_the_etag_and_the_cache_policy() {
        let etag = ETag::for_view("v", "u", GEN, &());
        let response = etag.not_modified();
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag.as_str());
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL);
        let response = etag.respond("body");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::ETAG], etag.as_str());
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL);
    }
}
