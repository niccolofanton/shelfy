//! Pairing the browser extension with an account (plan §2.11 device tokens;
//! P2-03, contract C2), so its long-lived token never passes through page
//! JavaScript:
//!
//! 1. The web app, signed in or re-authenticated in the last 5 minutes,
//!    asks for a code: `POST /me/tokens/pairing-code` ([`create_code`]).
//! 2. It hands the code to the extension with
//!    `chrome.runtime.sendMessage(EXTENSION_ID, …)` (contract C9).
//! 3. The extension exchanges it at `POST /extension/pair` ([`pair`]) for an
//!    `extension` token with the four scopes `ingest`, `tasks`, `uploads`
//!    and `lookup`, minted by [`crate::auth::api_tokens::mint`] (`via:
//!    pairing`).
//!
//! **The code** is 256 random bits (43 base64url characters), kept as its
//! SHA-256 in `pairing_codes` (kind `extension`), valid [`CODE_TTL`] (60 s)
//! and spent by its first use. An account holds at most [`MAX_LIVE_CODES`]
//! usable codes; past that, 429 until the oldest expires. Every code is
//! audited (`pairing_code.create`), never its value.
//!
//! **The exchange** is public: the code is the credential. It reads no
//! cookie and needs no CSRF headers (the extension's service worker sends
//! none), and the sign-in limit per client address counts it (10 a minute).
//! It checks, in this order, without spending the code on a refusal:
//! the code's shape (400 `invalid_pairing_code`), the body (422), the
//! extension's version (426 `extension_outdated` below `minVersion`), and
//! on a database reader whether the code is usable (400
//! `invalid_pairing_code` when it is unknown, used, expired or its account
//! is not active), so a guess never takes the writer. Then, in one write
//! transaction: the code is spent (checked again: two requests may race for
//! it), the earlier token of the same installation is revoked (P2-G16), the
//! account's cap of 50 working tokens is checked (409 `conflict`, and
//! nothing changes), and the token is minted and tied to the installation.
//!
//! **Installations.** The extension sends `installId`, a random id it keeps
//! while installed. The server keeps its SHA-256 (`api_tokens.install_hash`,
//! control schema v4): pairing the same installation again replaces its
//! token, so re-pairing never piles up working tokens. Only the account
//! that owns the code is affected: another account's tokens are never
//! revoked, whatever installation id is sent.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use sha2::{Digest, Sha256};
use shelfy_core::repo::RepoError;

use super::{ExtensionVersion, outdated};
use crate::auth::api_tokens::{self, MAX_ACTIVE_TOKENS, Mint, Via, allowed_scopes};
use crate::auth::bearer::Scope;
use crate::auth::millis;
use crate::auth::passkeys::normalize_label;
use crate::control::api_tokens::{self as tokens, TokenKind};
use crate::control::audit::{self, Entry};
use crate::control::pairing::{self as codes, KIND_EXTENSION, NewPairingCode};
use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, blocking};
use crate::telemetry::redact::Redacted;
use crate::tokens::{SecretToken, TokenHash, hash_token, is_token_shaped};

/// How long a pairing code works (§2.11: 60 seconds).
pub const CODE_TTL: Duration = Duration::from_secs(60);
/// Most usable pairing codes one account holds at once.
pub const MAX_LIVE_CODES: u64 = 10;
/// Shortest `installId` accepted.
pub const INSTALL_ID_MIN_LEN: usize = 16;
/// Longest `installId` accepted.
pub const INSTALL_ID_MAX_LEN: usize = 128;

/// Domain separation of the installation digests.
const INSTALL_PREFIX: &[u8] = b"shelfy.extension.install\0";

/// A new pairing code: the code (its only copy) and its expiry.
#[derive(Debug)]
pub struct NewCode {
    /// The code, for the web app to hand to the extension.
    pub code: SecretToken,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

enum Created {
    Code(NewCode),
    TooMany { retry_after_ms: i64 },
}

/// Creates a pairing code for `user_id`, after pruning expired codes, and
/// writes `pairing_code.create` to the audit log.
///
/// # Errors
///
/// 429 `rate_limited`, with `Retry-After`, while the account holds
/// [`MAX_LIVE_CODES`] usable codes; the control database failed.
pub async fn create_code(state: &AppState, user_id: &str) -> Result<NewCode, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user_id.to_owned();
    let now = state.extension().now_ms();
    let created = blocking(move || {
        control.write(|tx| {
            codes::prune(tx, now)?;
            if codes::count_live(tx, &user_id, KIND_EXTENSION, now)? >= MAX_LIVE_CODES {
                let first = codes::first_live_expiry(tx, &user_id, KIND_EXTENSION, now)?;
                return Ok(Created::TooMany {
                    retry_after_ms: first.unwrap_or(now).saturating_sub(now),
                });
            }
            let code = SecretToken::generate();
            let code_hash = code.hash();
            let expires_at = now.saturating_add(millis(CODE_TTL));
            let new = NewPairingCode {
                code_hash: &code_hash,
                user_id: &user_id,
                kind: KIND_EXTENSION,
                expires_at,
            };
            codes::insert(tx, &new)?;
            let meta = json!({ "kind": KIND_EXTENSION, "expiresAt": expires_at });
            let entry = Entry {
                action: audit::PAIRING_CODE_CREATE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(Created::Code(NewCode { code, expires_at }))
        })
    })
    .await?;
    match created {
        Created::Code(code) => Ok(code),
        Created::TooMany { retry_after_ms } => {
            let seconds = retry_after_ms.div_euclid(1_000) + i64::from(retry_after_ms % 1_000 > 0);
            Err(ApiError::new(ErrorCode::RateLimited)
                .with_retry_after(u32::try_from(seconds.max(1)).unwrap_or(u32::MAX))
                .with_detail("too many pairing codes in use: wait for one to expire"))
        }
    }
}

/// What the extension sends to `POST /extension/pair`.
#[derive(Clone, Copy, Debug)]
pub struct PairRequest<'a> {
    /// The pairing code.
    pub code: &'a str,
    /// The installation's random id.
    pub install_id: &'a str,
    /// A name for the token list, such as "Chrome on macOS".
    pub label: Option<&'a str>,
    /// The extension's manifest version.
    pub version: &'a str,
}

/// A paired extension: its token, shown once.
#[derive(Debug)]
pub struct Paired {
    /// The account the token acts for.
    pub user_id: String,
    /// The token (`shx_…`), its only copy.
    pub token: Redacted<String>,
    /// Its id, as the account's token list names it.
    pub token_id: String,
    /// What it may do.
    pub scopes: Vec<Scope>,
}

enum Exchanged {
    InvalidCode,
    Paired {
        user_id: String,
        minted: api_tokens::Minted,
        replaced: Vec<String>,
    },
}

/// 400 `invalid_pairing_code`.
fn invalid_code() -> ApiError {
    ApiError::new(ErrorCode::InvalidPairingCode)
}

/// The stored form of an installation id: its domain-separated SHA-256.
///
/// # Errors
///
/// 422 `validation_failed` on `installId` unless it is
/// [`INSTALL_ID_MIN_LEN`] to [`INSTALL_ID_MAX_LEN`] characters of letters,
/// digits, `-` and `_`.
pub fn install_hash(install_id: &str) -> Result<TokenHash, ApiError> {
    let valid = (INSTALL_ID_MIN_LEN..=INSTALL_ID_MAX_LEN).contains(&install_id.len())
        && install_id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
    if !valid {
        return Err(ApiError::invalid_field(
            "installId",
            format!(
                "must be {INSTALL_ID_MIN_LEN} to {INSTALL_ID_MAX_LEN} letters, digits, '-' or '_'"
            ),
        ));
    }
    Ok(Sha256::new()
        .chain_update(INSTALL_PREFIX)
        .chain_update(install_id.as_bytes())
        .finalize()
        .into())
}

/// Exchanges a pairing code for an `extension` token (see the module docs
/// for the order of the checks).
///
/// # Errors
///
/// 400 `invalid_pairing_code`; 422 `validation_failed` (`installId`,
/// `label`, `version`); 426 `extension_outdated`; 409 `conflict` when the
/// account holds 50 working tokens; the control database failed.
pub async fn pair(state: &AppState, request: &PairRequest<'_>) -> Result<Paired, ApiError> {
    if !is_token_shaped(request.code) {
        return Err(invalid_code());
    }
    let install = install_hash(request.install_id)?;
    let label = normalize_label(request.label)?;
    let Some(version) = ExtensionVersion::parse(request.version.trim()) else {
        return Err(ApiError::invalid_field(
            "version",
            "must be the extension's manifest version, such as \"0.2.0\"",
        ));
    };
    let snapshot = state.extension().flags().get(state.control()).await?;
    if version < snapshot.min_version() {
        return Err(outdated(snapshot.min_version()));
    }
    let code_hash = hash_token(request.code);
    let now = state.extension().now_ms();
    // On a reader first: a guessed, used or expired code never takes the
    // control database's one writer.
    let reader = Arc::clone(state.control());
    let usable = blocking(move || {
        reader.read(|conn| codes::is_usable(conn, &code_hash, KIND_EXTENSION, now))
    })
    .await?;
    if !usable {
        return Err(invalid_code());
    }
    let control = Arc::clone(state.control());
    let exchanged = blocking(move || {
        control.write(|tx| {
            let Some(user_id) = codes::consume(tx, &code_hash, KIND_EXTENSION, now)? else {
                return Ok(Exchanged::InvalidCode);
            };
            let replaced = tokens::revoke_install(tx, &user_id, &install, now)?;
            for row in &replaced {
                let meta = json!({ "id": row.id, "kind": row.kind.as_str(), "via": "pairing" });
                let entry = Entry {
                    action: audit::API_TOKEN_REVOKE,
                    actor_user_id: Some(&user_id),
                    target: Some(&user_id),
                    meta: Some(&meta),
                };
                audit::record(tx, &entry, now)?;
            }
            if tokens::count_active(tx, &user_id, now)? >= MAX_ACTIVE_TOKENS {
                return Err(RepoError::Conflict("too many tokens"));
            }
            let mint = Mint {
                user_id: &user_id,
                kind: TokenKind::Extension,
                scopes: allowed_scopes(TokenKind::Extension),
                label: label.as_deref(),
                ttl: None,
                via: Via::Pairing,
                actor: Some(&user_id),
            };
            let minted = api_tokens::mint(tx, &mint, now)?;
            tokens::set_install(tx, &minted.row.id, &install)?;
            Ok(Exchanged::Paired {
                replaced: replaced.into_iter().map(|row| row.id).collect(),
                user_id,
                minted,
            })
        })
    })
    .await?;
    let (user_id, minted, replaced) = match exchanged {
        Exchanged::InvalidCode => return Err(invalid_code()),
        Exchanged::Paired {
            user_id,
            minted,
            replaced,
        } => (user_id, minted, replaced),
    };
    for token_id in &replaced {
        super::token_revoked(state, &user_id, token_id);
    }
    tracing::info!(
        user_id = %user_id,
        token_id = %minted.row.id,
        replaced = replaced.len(),
        "extension paired"
    );
    Ok(Paired {
        scopes: Scope::parse_list(&minted.row.scopes),
        token_id: minted.row.id,
        token: minted.token,
        user_id,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installation_ids_are_checked_and_hashed_apart() {
        let uuid = "3f2b8c1e-5d4a-4f7e-9a6b-1c2d3e4f5a6b";
        let a = install_hash(uuid).unwrap();
        assert_eq!(a, install_hash(uuid).unwrap());
        assert_ne!(a, install_hash("01J9Z3B8K4QW6TFX0V7G2N5RCA").unwrap());
        assert_ne!(a, hash_token(uuid), "domain-separated from token digests");
        for ok in [
            "a".repeat(16),
            "Z".repeat(128),
            "AbC_-09xyzAbC_-0".to_owned(),
        ] {
            assert!(install_hash(&ok).is_ok(), "{ok}");
        }
        for bad in [
            String::new(),
            "a".repeat(15),
            "a".repeat(129),
            format!("{} ", "a".repeat(16)),
            format!("{}é", "a".repeat(16)),
            format!("{}/", "a".repeat(16)),
            format!("{}.", "a".repeat(16)),
        ] {
            let err = install_hash(&bad).unwrap_err();
            assert_eq!(err.code(), ErrorCode::ValidationFailed, "{bad:?}");
            assert_eq!(err.problem().errors[0].field, "installId");
        }
    }
}
