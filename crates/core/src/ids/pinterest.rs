//! Pinterest identity: the pin id (plan §2.8).
//!
//! The desktop stored the pin id in `posts.id` (`electron/webview-injected.ts`
//! `mapPin`, `electron/webview-select.ts` `pinFallback`); the `/pin/<id>/`
//! URL is the fallback. Pin ids are decimal today; other URL-safe ids are
//! accepted so an unexpected form is kept rather than lost.

use super::{CanonicalId, IdError, Platform};

/// Longest pin id accepted.
const MAX_PIN_ID_LEN: usize = 64;

/// The canonical identity `pin_<id>` of a desktop Pinterest post: `posts.id`
/// when it is a valid pin id, else the id in the `/pin/<id>/` segment of
/// `post_url`.
pub fn from_legacy(id: &str, post_url: Option<&str>) -> Result<CanonicalId, IdError> {
    let pin_id = pin_id(id)
        .or_else(|| post_url.and_then(pin_id_from_url))
        .ok_or(if id.is_empty() {
            IdError::Empty
        } else {
            IdError::InvalidPinId
        })?;
    Ok(CanonicalId::new(
        Platform::Pinterest,
        pin_id.to_owned(),
        pin_id,
    ))
}

/// True when the pin id is the usual decimal form.
pub fn is_numeric(native_id: &str) -> bool {
    super::is_ascii_digits(native_id)
}

/// A pin id: 1–64 characters of `[A-Za-z0-9_-]`, not all zeros.
fn pin_id(value: &str) -> Option<&str> {
    let valid = !value.is_empty()
        && value.len() <= MAX_PIN_ID_LEN
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        && value.bytes().any(|b| b != b'0');
    valid.then_some(value)
}

/// The id in a `…/pin/<id>/` URL path.
fn pin_id_from_url(url: &str) -> Option<&str> {
    let path = url.split(['?', '#']).next()?;
    let mut segments = path.split('/');
    while let Some(segment) = segments.next() {
        if segment == "pin" {
            return segments.next().and_then(pin_id);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn pin_ids_map_to_pin_keys() {
        let id = from_legacy("987654321012345678", None).unwrap();
        assert_eq!(id.key(), "pin_987654321012345678");
        assert_eq!(id.native_id(), "987654321012345678");
        assert!(is_numeric(id.native_id()));
    }

    #[test]
    fn the_pin_url_is_the_fallback() {
        let id = from_legacy("", Some("https://www.pinterest.com/pin/123456/")).unwrap();
        assert_eq!(id.key(), "pin_123456");
        let id = from_legacy(
            "bad id",
            Some("https://it.pinterest.com/pin/AbC-9_x/?nic=1"),
        )
        .unwrap();
        assert_eq!(id.key(), "pin_AbC-9_x");
        assert!(!is_numeric(id.native_id()));
    }

    #[test]
    fn invalid_ids_are_rejected() {
        assert_eq!(from_legacy("", None), Err(IdError::Empty));
        assert_eq!(from_legacy("a b", None), Err(IdError::InvalidPinId));
        assert_eq!(from_legacy("000", None), Err(IdError::InvalidPinId));
        assert_eq!(
            from_legacy("x/y", Some("https://www.pinterest.com/board/x/")),
            Err(IdError::InvalidPinId)
        );
    }

    proptest! {
        #[test]
        fn numeric_ids_round_trip(n in 1u64..) {
            let id = from_legacy(&n.to_string(), None).unwrap();
            prop_assert_eq!(id.native_id(), n.to_string());
            let via_url = from_legacy("", Some(&format!("https://www.pinterest.com/pin/{n}/"))).unwrap();
            prop_assert_eq!(via_url, id);
        }
    }
}
