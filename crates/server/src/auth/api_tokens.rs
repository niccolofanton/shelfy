//! Minting API tokens (plan §2.9 Auth, §2.11 device tokens, §7.1).
//!
//! Every token is minted by [`mint`], whoever asks:
//!
//! | Via | Kind | Scopes | Lifetime | Who |
//! |---|---|---|---|---|
//! | `account` (`POST /me/tokens`, re-authentication within 5 minutes) | `extension` or `shortcut` | a non-empty subset of the kind's scopes, all by default | until revoked | the signed-in user |
//! | `device` (the device flow, [`super::device`]) | `migrate` | `migrate` | 7 days | the migration CLI, approved by a signed-in user |
//! | `cli` (`admin migrate-token`) | `migrate` | `migrate` | 7 days | the operator |
//! | `pairing` (`POST /extension/pair`, [`crate::extension::pairing`]) | `extension` | all four | until revoked | the browser extension, with a pairing code a signed-in user asked for |
//!
//! A token's value is `shx_` and 43 base64url characters (256 random bits),
//! returned once; the database keeps its SHA-256 ([`crate::tokens`]). Each
//! mint writes `api_token.create` (`id`, `kind`, `via`) to the audit log, in
//! the transaction that stores the token. A kind holds only its own scopes
//! ([`allowed_scopes`], §7.1 "narrow scopes"): an extension token never
//! migrates, a Shortcut token only creates links. Extension tokens come from
//! pairing (P2-03: `POST /me/tokens/pairing-code` in the web app, then
//! `POST /extension/pair` from the extension); minting one from the account
//! stays possible, for trying an unpacked build by hand (E3).

use std::time::Duration;

use rusqlite::Connection;
use serde_json::json;
use shelfy_core::repo::RepoError;

use super::bearer::{Scope, TOKEN_PREFIX};
use super::millis;
use crate::control::api_tokens::{self, NewApiToken, TokenKind, TokenRow};
use crate::control::audit::{self, Entry};
use crate::ids::new_ulid;
use crate::telemetry::redact::Redacted;
use crate::tokens::{SecretToken, hash_token};

/// How long a `migrate` token works (§2.11: 7 days).
pub const MIGRATE_TOKEN_TTL: Duration = Duration::from_secs(7 * 24 * 3600);

/// The label of the tokens the migration CLI gets.
pub const MIGRATE_LABEL: &str = "shelfy-migrate";

/// Most working tokens one account may hold; past it, minting from the
/// account answers 409 until one is revoked.
pub const MAX_ACTIVE_TOKENS: u64 = 50;

/// The scopes a token of `kind` may hold (§7.1).
#[must_use]
pub const fn allowed_scopes(kind: TokenKind) -> &'static [Scope] {
    match kind {
        TokenKind::Extension => &[Scope::Ingest, Scope::Tasks, Scope::Uploads, Scope::Lookup],
        TokenKind::Shortcut => &[Scope::LinksCreate],
        TokenKind::Migrate => &[Scope::Migrate],
    }
}

/// Where a token came from, for the audit log.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// `admin migrate-token`.
    Cli,
    /// `POST /me/tokens`.
    Account,
    /// The device flow.
    Device,
    /// `POST /extension/pair`: the browser extension spent a pairing code.
    Pairing,
}

impl Via {
    /// The audit value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Account => "account",
            Self::Device => "device",
            Self::Pairing => "pairing",
        }
    }
}

/// A token to mint.
#[derive(Clone, Copy, Debug)]
pub struct Mint<'a> {
    /// The user it acts for.
    pub user_id: &'a str,
    /// Who holds it.
    pub kind: TokenKind,
    /// What it may do: a subset of [`allowed_scopes`] of `kind`.
    pub scopes: &'a [Scope],
    /// A name for the token list.
    pub label: Option<&'a str>,
    /// How long it works; `None` until revoked.
    pub ttl: Option<Duration>,
    /// Where it came from.
    pub via: Via,
    /// Who asked, for the audit log: the user, or `None` for the operator.
    pub actor: Option<&'a str>,
}

/// A minted token: its row, and its value (`shx_…`), the only copy.
#[derive(Clone, Debug)]
pub struct Minted {
    /// The token as the account's list shows it.
    pub row: TokenRow,
    /// The value, to hand out once.
    pub token: Redacted<String>,
}

/// Mints a token inside the caller's write transaction and writes
/// `api_token.create` to the audit log.
///
/// # Errors
///
/// [`RepoError::Invalid`] (`scopes`) when the scopes are empty or not the
/// kind's; a query failed.
pub fn mint(tx: &Connection, mint: &Mint<'_>, now: i64) -> Result<Minted, RepoError> {
    let allowed = allowed_scopes(mint.kind);
    if mint.scopes.is_empty() || mint.scopes.iter().any(|scope| !allowed.contains(scope)) {
        return Err(RepoError::Invalid {
            field: "scopes",
            reason: "must be a non-empty subset of the kind's scopes",
        });
    }
    let token = format!("{TOKEN_PREFIX}{}", SecretToken::generate().expose());
    let token_hash = hash_token(&token);
    let id = new_ulid();
    let scopes = Scope::list(mint.scopes);
    let expires_at = mint.ttl.map(|ttl| now.saturating_add(millis(ttl)));
    let new = NewApiToken {
        id: &id,
        user_id: mint.user_id,
        kind: mint.kind,
        token_hash: &token_hash,
        label: mint.label,
        scopes: &scopes,
        expires_at,
    };
    api_tokens::insert(tx, &new, now)?;
    let meta = json!({ "id": id, "kind": mint.kind.as_str(), "via": mint.via.as_str() });
    let entry = Entry {
        action: audit::API_TOKEN_CREATE,
        actor_user_id: mint.actor,
        target: Some(mint.user_id),
        meta: Some(&meta),
    };
    audit::record(tx, &entry, now)?;
    Ok(Minted {
        row: TokenRow {
            id,
            kind: mint.kind,
            label: mint.label.map(str::to_owned),
            scopes,
            created_at: now,
            last_used_at: None,
            expires_at,
        },
        token: Redacted(token),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};
    use crate::tokens::is_token_shaped;

    #[test]
    fn a_minted_token_is_stored_as_its_hash_and_audited() {
        let (db, owner, _member) = control_with_users();
        let minted = db
            .write(|tx| {
                mint(
                    tx,
                    &Mint {
                        user_id: &owner,
                        kind: TokenKind::Extension,
                        scopes: &[Scope::Lookup],
                        label: Some("Chrome"),
                        ttl: None,
                        via: Via::Account,
                        actor: Some(&owner),
                    },
                    NOW,
                )
            })
            .unwrap();
        let value = minted.token.expose();
        assert!(value.starts_with("shx_"));
        assert!(is_token_shaped(&value[4..]));
        assert!(
            !format!("{minted:?}").contains(&value[4..]),
            "Debug hides it"
        );
        assert_eq!(minted.row.scopes, "lookup");
        assert_eq!(minted.row.expires_at, None);

        let found = db
            .read(|conn| api_tokens::find_active(conn, &hash_token(value), NOW))
            .unwrap()
            .unwrap();
        assert_eq!(
            (found.id.as_str(), found.kind.as_str()),
            (minted.row.id.as_str(), "extension")
        );
        let (meta, actor): (String, String) = db
            .read(|conn| {
                conn.query_row(
                    "SELECT meta_json, actor_user_id FROM audit_log \
                     WHERE action = 'api_token.create'",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(RepoError::from)
            })
            .unwrap();
        assert_eq!(actor, owner);
        let meta: serde_json::Value = serde_json::from_str(&meta).unwrap();
        assert_eq!(
            meta,
            json!({ "id": minted.row.id, "kind": "extension", "via": "account" })
        );
        assert!(!meta.to_string().contains(&value[4..]));
    }

    #[test]
    fn a_kind_holds_only_its_own_scopes() {
        let (db, owner, _member) = control_with_users();
        let attempt = |kind, scopes: &[Scope]| {
            db.write(|tx| {
                mint(
                    tx,
                    &Mint {
                        user_id: &owner,
                        kind,
                        scopes,
                        label: None,
                        ttl: Some(MIGRATE_TOKEN_TTL),
                        via: Via::Device,
                        actor: Some(&owner),
                    },
                    NOW,
                )
            })
        };
        for (kind, scopes) in [
            (TokenKind::Shortcut, &[Scope::Lookup][..]),
            (TokenKind::Extension, &[Scope::Migrate][..]),
            (TokenKind::Migrate, &[Scope::Migrate, Scope::Ingest][..]),
            (TokenKind::Shortcut, &[][..]),
        ] {
            let err = attempt(kind, scopes).unwrap_err();
            assert!(
                matches!(
                    err,
                    RepoError::Invalid {
                        field: "scopes",
                        ..
                    }
                ),
                "{kind:?} {scopes:?}: {err}"
            );
        }
        let migrate = attempt(TokenKind::Migrate, &[Scope::Migrate]).unwrap();
        assert_eq!(migrate.row.expires_at, Some(NOW + 7 * 86_400_000));
        assert_eq!(
            Scope::list(allowed_scopes(TokenKind::Extension)),
            "ingest tasks uploads lookup"
        );
    }
}
