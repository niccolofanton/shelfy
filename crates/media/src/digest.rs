//! SHA-256 digests: the identity of every stored object.

use std::fmt;
use std::str::FromStr;

use sha2::{Digest as _, Sha256};

/// The SHA-256 of an object's bytes. It names the object's file (as 64
/// lowercase hex digits, plan §2.5) and is `media_objects.sha256` (the raw 32
/// bytes).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Digest([u8; 32]);

impl Digest {
    /// Length of the hex form.
    pub const HEX_LEN: usize = 64;

    /// The digest with these raw bytes.
    #[must_use]
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    /// The digest of `data`.
    #[must_use]
    pub fn of(data: &[u8]) -> Self {
        Self(Sha256::digest(data).into())
    }

    /// The digest of a finished hasher.
    pub(crate) fn from_hasher(hasher: Sha256) -> Self {
        Self(hasher.finalize().into())
    }

    /// The raw bytes, as stored in `media_objects.sha256`.
    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// The raw bytes of a `media_objects.sha256` value; `None` unless it is
    /// exactly 32 bytes long.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }

    /// Parses the file-name form: exactly 64 lowercase hex digits. Uppercase is
    /// refused, so every object has exactly one name.
    #[must_use]
    pub fn parse_hex(text: &str) -> Option<Self> {
        let text = text.as_bytes();
        if text.len() != Self::HEX_LEN {
            return None;
        }
        let mut bytes = [0u8; 32];
        let (pairs, _) = text.as_chunks::<2>();
        for (byte, &[high, low]) in bytes.iter_mut().zip(pairs) {
            *byte = (nibble(high)? << 4) | nibble(low)?;
        }
        Some(Self(bytes))
    }

    /// The name of the directory that holds the object: its first two hex
    /// digits (plan §2.5, `media/<aa>/`).
    #[must_use]
    pub fn shard(&self) -> String {
        let mut shard = String::with_capacity(2);
        push_hex(&mut shard, self.0[0]);
        shard
    }
}

fn nibble(c: u8) -> Option<u8> {
    match c {
        b'0'..=b'9' => Some(c - b'0'),
        b'a'..=b'f' => Some(c - b'a' + 10),
        _ => None,
    }
}

fn push_hex(out: &mut String, byte: u8) {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    out.push(char::from(DIGITS[usize::from(byte >> 4)]));
    out.push(char::from(DIGITS[usize::from(byte & 0xf)]));
}

impl fmt::Display for Digest {
    /// The 64 lowercase hex digits.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut hex = String::with_capacity(Self::HEX_LEN);
        for &byte in &self.0 {
            push_hex(&mut hex, byte);
        }
        f.write_str(&hex)
    }
}

impl fmt::Debug for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Digest({self})")
    }
}

/// Error of [`Digest::from_str`].
#[derive(Debug, thiserror::Error)]
#[error("not a SHA-256 in lowercase hex")]
pub struct InvalidDigest;

impl FromStr for Digest {
    type Err = InvalidDigest;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Self::parse_hex(s).ok_or(InvalidDigest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SHA-256 of the empty string.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn hex_round_trips() {
        let digest = Digest::of(b"");
        assert_eq!(digest.to_string(), EMPTY);
        assert_eq!(EMPTY.parse::<Digest>().unwrap(), digest);
        assert_eq!(digest.shard(), "e3");
        assert_eq!(Digest::from_slice(digest.as_bytes()), Some(digest));
        assert_eq!(Digest::from_slice(&[0; 31]), None);
    }

    #[test]
    fn only_the_canonical_name_parses() {
        for bad in [
            "",
            &EMPTY[..63],
            &format!("{EMPTY}0"),
            &EMPTY.to_uppercase(),
            &EMPTY.replace('e', "g"),
            &format!("../{}", &EMPTY[3..]),
        ] {
            assert!(Digest::parse_hex(bad).is_none(), "{bad:?} parsed");
        }
    }
}
