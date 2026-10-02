//! Secret tokens (plan §2.6): 256-bit random values handed out once and
//! stored only as their SHA-256.
//!
//! Invites use them now; sessions, magic links, API tokens and pairing codes
//! (T10, P1-13, P1-17) build on the same two functions, so every secret is
//! generated and hashed one way.

use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use sha2::{Digest, Sha256};

/// A secret token in its transport form: 32 random bytes as unpadded
/// base64url (43 characters).
///
/// `Debug` never shows the value; [`SecretToken::expose`] is the one way out.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretToken(String);

impl SecretToken {
    /// A fresh token from the operating system's random number generator.
    ///
    /// # Panics
    ///
    /// When the random number generator fails.
    #[must_use]
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).expect("the OS random number generator failed");
        Self(URL_SAFE_NO_PAD.encode(bytes))
    }

    /// The token, to hand to its holder exactly once.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// The SHA-256 stored in place of the token.
    #[must_use]
    pub fn hash(&self) -> TokenHash {
        hash_token(&self.0)
    }
}

impl fmt::Debug for SecretToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretToken([redacted])")
    }
}

/// The SHA-256 of a token, as stored in the control database.
pub type TokenHash = [u8; 32];

/// The stored form of a token presented by a client: the SHA-256 of its
/// transport form.
#[must_use]
pub fn hash_token(token: &str) -> TokenHash {
    Sha256::digest(token.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_url_safe_and_hashed_consistently() {
        let token = SecretToken::generate();
        assert_eq!(token.expose().len(), 43);
        assert!(
            token
                .expose()
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
        assert_eq!(token.hash(), hash_token(token.expose()));
        assert_ne!(SecretToken::generate(), token);
    }

    #[test]
    fn debug_hides_the_value() {
        let token = SecretToken::generate();
        let debug = format!("{token:?}");
        assert!(!debug.contains(token.expose()));
        assert_eq!(debug, "SecretToken([redacted])");
    }

    #[test]
    fn hash_is_sha256() {
        // SHA-256("abc"), FIPS 180-2 test vector.
        let expected = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        let hex: String = hash_token("abc")
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(hex, expected);
    }
}
