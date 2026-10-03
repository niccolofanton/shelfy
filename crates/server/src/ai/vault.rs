//! BYOK key sealing. Only sealed rows belong in the control DB and snapshots.
//! The operator provider is independent and never uses this vault.

use std::fmt;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    XChaCha20Poly1305, XNonce,
    aead::{Aead, KeyInit, Payload},
};
use hkdf::Hkdf;
use secrecy::{ExposeSecret as _, SecretString};
use sha2::Sha256;
use zeroize::Zeroizing;

use crate::control::provider_keys::SealedKey;
use crate::error::{ApiError, ErrorCode};

const DOMAIN: &[u8] = b"shelfy/provider-key-vault/v1";

/// A failure deliberately contains no key, ciphertext or caller input.
#[derive(Clone, Copy, Debug, thiserror::Error)]
pub enum VaultError {
    /// BYOK storage is disabled; the operator provider still works.
    #[error("the provider key vault is disabled")]
    Disabled,
    /// The row cannot be authenticated with the configured keys.
    #[error("the provider key cannot be opened")]
    CannotOpen,
    /// The OS random source failed, or the input was empty.
    #[error("the provider key cannot be sealed")]
    CannotSeal,
}

impl From<VaultError> for ApiError {
    fn from(error: VaultError) -> Self {
        match error {
            VaultError::Disabled => Self::new(ErrorCode::NotAvailable),
            _ => Self::internal(error),
        }
    }
}

#[derive(Clone)]
struct MasterKey {
    bytes: Zeroizing<[u8; 32]>,
    version: u32,
}

impl MasterKey {
    fn parse(raw: Option<SecretString>, variable: &'static str) -> Result<Option<Self>, String> {
        let Some(raw) = raw.filter(|s| !s.expose_secret().is_empty()) else {
            return Ok(None);
        };
        let mut bytes = Zeroizing::new([0u8; 32]);
        let length = STANDARD
            .decode_slice(raw.expose_secret(), bytes.as_mut())
            .map_err(|_| format!("{variable}: expected base64 of exactly 32 bytes"))?;
        if length != 32 {
            return Err(format!("{variable}: expected base64 of exactly 32 bytes"));
        }
        let mut version = Zeroizing::new([0u8; 4]);
        Hkdf::<Sha256>::new(Some(DOMAIN), bytes.as_ref())
            .expand(b"master-key-version", version.as_mut())
            .expect("fixed HKDF length");
        Ok(Some(Self {
            bytes,
            version: u32::from_be_bytes(*version),
        }))
    }

    fn user_key(&self, user: &str) -> Zeroizing<[u8; 32]> {
        let mut key = Zeroizing::new([0u8; 32]);
        let info = framed(&[b"provider-keys", user.as_bytes()]);
        Hkdf::<Sha256>::new(Some(DOMAIN), self.bytes.as_ref())
            .expand(&info, key.as_mut())
            .expect("fixed HKDF length");
        key
    }
}

/// Current and optional previous master key, wiped on drop and redacted in Debug.
/// There is deliberately no Serialize or Display implementation.
#[derive(Clone, Default)]
pub struct KeyVault {
    current: Option<MasterKey>,
    previous: Option<MasterKey>,
}

impl fmt::Debug for KeyVault {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyVault")
            .field("enabled", &self.enabled())
            .field("key_version", &self.key_version())
            .finish_non_exhaustive()
    }
}

impl KeyVault {
    /// Validates both settings, including when only a previous key is present.
    /// An empty value is unset. Previous alone never enables writes or reads.
    pub fn new(
        current: Option<SecretString>,
        previous: Option<SecretString>,
    ) -> Result<Self, String> {
        let current = MasterKey::parse(current, "SHELFY_MASTER_KEY")?;
        let previous = MasterKey::parse(previous, "SHELFY_MASTER_KEY_PREVIOUS")?;
        if let (Some(a), Some(b)) = (&current, &previous)
            && a.version == b.version
            && a.bytes.as_ref() != b.bytes.as_ref()
        {
            return Err("SHELFY_MASTER_KEY_PREVIOUS: master key version collision".into());
        }
        Ok(Self { current, previous })
    }

    #[must_use]
    pub fn enabled(&self) -> bool {
        self.current.is_some()
    }

    #[must_use]
    pub fn key_version(&self) -> Option<u32> {
        self.current.as_ref().map(|k| k.version)
    }

    /// Seals a provider credential under the current per-user key.
    pub fn seal(
        &self,
        user: &str,
        provider: &str,
        plaintext: &SecretString,
    ) -> Result<SealedKey, VaultError> {
        let master = self.current.as_ref().ok_or(VaultError::Disabled)?;
        if plaintext.expose_secret().is_empty() {
            return Err(VaultError::CannotSeal);
        }
        let key = master.user_key(user);
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("32-byte key");
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|_| VaultError::CannotSeal)?;
        let aad = aad(user, provider, master.version);
        let ciphertext = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: plaintext.expose_secret().as_bytes(),
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::CannotSeal)?;
        // A very short credential must not be returned in full as its suffix.
        let last4 = if plaintext.expose_secret().chars().count() > 4 {
            plaintext
                .expose_secret()
                .chars()
                .rev()
                .take(4)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect()
        } else {
            String::new()
        };
        Ok(SealedKey {
            key_version: master.version,
            nonce: nonce.to_vec(),
            ciphertext,
            last4,
        })
    }

    /// Opens a row only for its authenticated user, provider and master version.
    pub fn open(
        &self,
        user: &str,
        provider: &str,
        sealed: &SealedKey,
    ) -> Result<SecretString, VaultError> {
        let current = self.current.as_ref().ok_or(VaultError::Disabled)?;
        let master = std::iter::once(current)
            .chain(self.previous.iter())
            .find(|k| k.version == sealed.key_version)
            .ok_or(VaultError::CannotOpen)?;
        let nonce: &XNonce = sealed
            .nonce
            .as_slice()
            .try_into()
            .map_err(|_| VaultError::CannotOpen)?;
        let key = master.user_key(user);
        let cipher = XChaCha20Poly1305::new_from_slice(key.as_ref()).expect("32-byte key");
        let aad = aad(user, provider, sealed.key_version);
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    nonce,
                    Payload {
                        msg: &sealed.ciphertext,
                        aad: &aad,
                    },
                )
                .map_err(|_| VaultError::CannotOpen)?,
        );
        let text = std::str::from_utf8(&plaintext).map_err(|_| VaultError::CannotOpen)?;
        Ok(SecretString::from(text.to_owned()))
    }
}

fn framed(parts: &[&[u8]]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for part in parts {
        bytes.extend_from_slice(&(part.len() as u64).to_be_bytes());
        bytes.extend_from_slice(part);
    }
    bytes
}

fn aad(user: &str, provider: &str, version: u32) -> Vec<u8> {
    framed(&[
        DOMAIN,
        user.as_bytes(),
        provider.as_bytes(),
        &version.to_be_bytes(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn vault(byte: u8, previous: Option<u8>) -> KeyVault {
        let encode = |b| SecretString::from(STANDARD.encode([b; 32]));
        KeyVault::new(Some(encode(byte)), previous.map(encode)).unwrap()
    }

    #[test]
    fn binding_randomness_and_tampering() {
        let key_vault = vault(1, None);
        let secret = SecretString::from("synthetic-provider-credential".to_owned());
        let row = key_vault.seal("alice", "provider", &secret).unwrap();
        let other = key_vault.seal("alice", "provider", &secret).unwrap();
        assert_ne!(row.nonce, other.nonce);
        assert_ne!(row.ciphertext, other.ciphertext);
        assert_eq!(
            key_vault
                .open("alice", "provider", &row)
                .unwrap()
                .expose_secret(),
            secret.expose_secret()
        );
        for (user, provider) in [
            ("bob", "provider"),
            ("alice", "other"),
            ("alic", "eprovider"),
        ] {
            assert!(key_vault.open(user, provider, &row).is_err());
        }
        for changed in 0..4 {
            let mut bad = row.clone();
            match changed {
                0 => bad.nonce[0] ^= 1,
                1 => bad.ciphertext[0] ^= 1,
                2 => bad.key_version ^= 1,
                _ => bad.nonce.pop().map(|_| ()).unwrap(),
            }
            assert!(key_vault.open("alice", "provider", &bad).is_err());
        }
        assert!(vault(2, None).open("alice", "provider", &row).is_err());
        assert!(vault(2, Some(1)).open("alice", "provider", &row).is_ok());
    }

    #[test]
    fn validates_without_echoing_and_unset_disables() {
        for raw in ["planted-invalid-secret", "AA==", &STANDARD.encode([7; 33])] {
            let err = KeyVault::new(Some(SecretString::from(raw.to_owned())), None).unwrap_err();
            assert!(!err.contains(raw));
            assert!(err.contains("SHELFY_MASTER_KEY"));
        }
        assert!(KeyVault::new(None, Some(SecretString::from("bad".to_owned()))).is_err());
        let disabled = KeyVault::new(Some(SecretString::from(String::new())), None).unwrap();
        assert!(!disabled.enabled());
        let secret = SecretString::from("abcd".to_owned());
        assert!(disabled.seal("u", "p", &secret).is_err());
        assert_eq!(vault(1, None).seal("u", "p", &secret).unwrap().last4, "");
        assert!(
            !KeyVault::new(None, Some(SecretString::from(STANDARD.encode([1; 32]))))
                .unwrap()
                .enabled()
        );
    }
}
