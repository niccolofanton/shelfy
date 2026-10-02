//! Canonical identity of library items (plan §2.8).
//!
//! Every post has a `(platform, native_id)` pair, unique per library, and a
//! public `key` derived from it:
//!
//! | Platform  | `native_id`                         | `key`             |
//! |-----------|-------------------------------------|-------------------|
//! | Instagram | media `pk`, decimal                 | `ig_<pk>`         |
//! | X         | tweet id                            | `x_<id>`          |
//! | Pinterest | pin id                              | `pin_<id>`        |
//! | Web       | SHA-1 of the scheme-less URL (hex)  | `web_<sha1:20>`   |
//! | Manual    | ULID                                | `m_<ulid>`        |
//!
//! The submodules also decode the identifiers the desktop app stored in its
//! `posts.id` column, so a legacy library maps onto the same keys.

pub mod ig;
pub mod manual;
pub mod pinterest;
pub mod web;
pub mod x;

use std::fmt;

/// The platform of a library item. The string forms are the values of
/// `posts.platform` in both the desktop and the web schema.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Platform {
    Instagram,
    Twitter,
    Pinterest,
    Web,
    Manual,
}

impl Platform {
    /// Every platform, in schema order.
    pub const ALL: [Platform; 5] = [
        Platform::Instagram,
        Platform::Twitter,
        Platform::Pinterest,
        Platform::Web,
        Platform::Manual,
    ];

    /// The `posts.platform` value.
    pub fn as_str(self) -> &'static str {
        match self {
            Platform::Instagram => "instagram",
            Platform::Twitter => "twitter",
            Platform::Pinterest => "pinterest",
            Platform::Web => "web",
            Platform::Manual => "manual",
        }
    }

    /// Parses a `posts.platform` value (exact, lowercase).
    pub fn parse(value: &str) -> Option<Platform> {
        Platform::ALL.into_iter().find(|p| p.as_str() == value)
    }

    /// The prefix of the public key, including the underscore.
    pub fn key_prefix(self) -> &'static str {
        match self {
            Platform::Instagram => "ig_",
            Platform::Twitter => "x_",
            Platform::Pinterest => "pin_",
            Platform::Web => "web_",
            Platform::Manual => "m_",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A canonical identity: the `(platform, native_id)` pair and the public key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CanonicalId {
    platform: Platform,
    native_id: String,
    key: String,
}

impl CanonicalId {
    /// Builds an identity from an already validated native id.
    fn new(platform: Platform, native_id: String, key_suffix: &str) -> CanonicalId {
        let key = format!("{}{}", platform.key_prefix(), key_suffix);
        CanonicalId {
            platform,
            native_id,
            key,
        }
    }

    pub fn platform(&self) -> Platform {
        self.platform
    }

    /// The `posts.native_id` value.
    pub fn native_id(&self) -> &str {
        &self.native_id
    }

    /// The public `posts.key` value.
    pub fn key(&self) -> &str {
        &self.key
    }
}

impl fmt::Display for CanonicalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.key)
    }
}

/// Why an identifier could not be turned into a canonical identity.
///
/// The variants never carry the offending value: identifiers are user data
/// and errors end up in reports and logs.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum IdError {
    #[error("the id is empty")]
    Empty,
    #[error("the Instagram shortcode has a character outside the shortcode alphabet")]
    InvalidShortcode,
    #[error("the Instagram shortcode is longer than {max} characters")]
    ShortcodeTooLong { max: usize },
    #[error("the Instagram media id is neither `<pk>_<owner>`, a numeric pk nor a shortcode")]
    InvalidMediaId,
    #[error("the Instagram media pk is zero")]
    ZeroMediaPk,
    #[error("the tweet id is not numeric and the post URL has no status id")]
    InvalidTweetId,
    #[error("the pin id is not valid and the post URL has no pin id")]
    InvalidPinId,
    #[error("the manual id does not start with `manual:`")]
    InvalidManualId,
}

/// True when `value` is a non-empty run of ASCII digits.
pub(crate) fn is_ascii_digits(value: &str) -> bool {
    !value.is_empty() && value.bytes().all(|b| b.is_ascii_digit())
}

/// Strips leading zeros from a run of ASCII digits, keeping a single `0`.
pub(crate) fn trim_leading_zeros(digits: &str) -> &str {
    let trimmed = digits.trim_start_matches('0');
    if trimmed.is_empty() { "0" } else { trimmed }
}

/// Lowercase hex encoding.
pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        out.push(char::from(HEX[usize::from(b >> 4)]));
        out.push(char::from(HEX[usize::from(b & 0x0f)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_round_trips_through_its_string_form() {
        for p in Platform::ALL {
            assert_eq!(Platform::parse(p.as_str()), Some(p));
        }
        assert_eq!(Platform::parse("Instagram"), None);
        assert_eq!(Platform::parse("tiktok"), None);
    }

    #[test]
    fn key_prefixes_are_distinct() {
        let mut prefixes: Vec<_> = Platform::ALL.iter().map(|p| p.key_prefix()).collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        assert_eq!(prefixes.len(), Platform::ALL.len());
    }

    #[test]
    fn helpers() {
        assert!(is_ascii_digits("0123"));
        assert!(!is_ascii_digits(""));
        assert!(!is_ascii_digits("12a"));
        assert_eq!(trim_leading_zeros("000"), "0");
        assert_eq!(trim_leading_zeros("0042"), "42");
        assert_eq!(to_hex(&[0x00, 0xab, 0xff]), "00abff");
    }
}
