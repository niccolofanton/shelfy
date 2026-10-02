//! X (Twitter) identity: the tweet id (plan §2.8).
//!
//! The desktop stored the tweet `rest_id` in `posts.id`
//! (`electron/webview-injected.ts`, `electron/webview-select.ts`); the status
//! URL is the fallback.

use super::{CanonicalId, IdError, Platform, is_ascii_digits, trim_leading_zeros};

/// Longest tweet id accepted: a 64-bit unsigned value has 20 decimal digits.
const MAX_TWEET_ID_DIGITS: usize = 20;

/// The canonical identity `x_<tweet id>` of a desktop X post: `posts.id` when
/// it is a tweet id, else the id in the `/status/<id>` segment of `post_url`.
pub fn from_legacy(id: &str, post_url: Option<&str>) -> Result<CanonicalId, IdError> {
    let tweet_id = tweet_id(id)
        .or_else(|| post_url.and_then(status_id_from_url))
        .ok_or(if id.is_empty() {
            IdError::Empty
        } else {
            IdError::InvalidTweetId
        })?;
    Ok(canonical(&tweet_id))
}

/// The canonical identity of a validated tweet id.
fn canonical(tweet_id: &str) -> CanonicalId {
    CanonicalId::new(Platform::Twitter, tweet_id.to_owned(), tweet_id)
}

/// A decimal tweet id in canonical form, or `None`.
fn tweet_id(value: &str) -> Option<String> {
    if !is_ascii_digits(value) {
        return None;
    }
    let digits = trim_leading_zeros(value);
    (digits != "0" && digits.len() <= MAX_TWEET_ID_DIGITS).then(|| digits.to_owned())
}

/// The id in a `…/status/<id>` or `…/statuses/<id>` URL path.
fn status_id_from_url(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let mut segments = path.split('/');
    while let Some(segment) = segments.next() {
        if segment == "status" || segment == "statuses" {
            return segments.next().and_then(tweet_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn tweet_ids_map_to_x_keys() {
        let id = from_legacy("1700000000000000001", None).unwrap();
        assert_eq!(id.key(), "x_1700000000000000001");
        assert_eq!(id.native_id(), "1700000000000000001");
        assert_eq!(id.platform(), Platform::Twitter);
    }

    #[test]
    fn the_status_url_is_the_fallback() {
        let from_url = from_legacy(
            "odd",
            Some("https://x.com/someone/status/1234567890123?s=20"),
        )
        .unwrap();
        assert_eq!(from_url.key(), "x_1234567890123");
        // The repaired form of an author-less URL (`electron/db.ts` migrate v1).
        let repaired = from_legacy("", Some("https://x.com/i/status/42")).unwrap();
        assert_eq!(repaired.key(), "x_42");
        let legacy_api = from_legacy("x", Some("https://twitter.com/a/statuses/7")).unwrap();
        assert_eq!(legacy_api.key(), "x_7");
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert_eq!(from_legacy("", None), Err(IdError::Empty));
        assert_eq!(from_legacy("abc", None), Err(IdError::InvalidTweetId));
        assert_eq!(from_legacy("0", None), Err(IdError::InvalidTweetId));
        assert_eq!(
            from_legacy("123456789012345678901", None),
            Err(IdError::InvalidTweetId)
        );
        assert_eq!(
            from_legacy("abc", Some("https://x.com/a/status/abc")),
            Err(IdError::InvalidTweetId)
        );
    }

    proptest! {
        #[test]
        fn every_u64_tweet_id_round_trips(n in 1u64..) {
            let id = from_legacy(&n.to_string(), None).unwrap();
            prop_assert_eq!(id.native_id(), n.to_string());
            let via_url = from_legacy("", Some(&format!("https://x.com/u/status/{n}"))).unwrap();
            prop_assert_eq!(via_url, id);
        }
    }
}
