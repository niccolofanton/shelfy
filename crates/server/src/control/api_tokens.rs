//! `api_tokens` (plan §2.6, §2.11): scoped bearer tokens for the extension,
//! the iOS Shortcut and the migration CLI.
//!
//! The bearer extractor ([`crate::auth::bearer`]) looks tokens up with
//! [`find_active`]. T9 adds [`insert`] for `admin migrate-token` and the
//! expiry (control schema v2): a token whose `expires_at` has passed is
//! refused like a revoked one. P1-17 adds listing, `last_used_at`, revocation
//! and the device-code flow, and may change how `scopes` is stored; today it
//! is a space-separated list of scope names (`ingest lookup`).

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use crate::tokens::TokenHash;

/// `api_tokens.kind`: who holds the token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    /// The browser extension.
    Extension,
    /// The iOS Shortcut.
    Shortcut,
    /// The migration CLI (`shelfy-migrate`).
    Migrate,
}

impl TokenKind {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Shortcut => "shortcut",
            Self::Migrate => "migrate",
        }
    }
}

/// A token to store. Only the hash of its value is kept.
#[derive(Clone, Copy, Debug)]
pub struct NewApiToken<'a> {
    /// Token id (ULID), shown in the token list.
    pub id: &'a str,
    /// The user it acts for.
    pub user_id: &'a str,
    /// Who holds it.
    pub kind: TokenKind,
    /// SHA-256 of the whole value (`shx_…`).
    pub token_hash: &'a TokenHash,
    /// A name for the token list.
    pub label: Option<&'a str>,
    /// Space-separated scope names.
    pub scopes: &'a str,
    /// Expiry, unix ms; `None` for a token that lasts until revoked.
    pub expires_at: Option<i64>,
}

/// An unrevoked, unexpired token of an active user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiToken {
    /// Token id (ULID), shown in the token list.
    pub id: String,
    /// The user it acts for.
    pub user_id: String,
    /// `extension`, `shortcut` or `migrate`.
    pub kind: String,
    /// Space-separated scope names.
    pub scopes: String,
    /// Expiry, unix ms; `None` when the token does not expire.
    pub expires_at: Option<i64>,
}

/// Stores a new token, created at `now`.
///
/// # Errors
///
/// [`RepoError::Conflict`] when the id or the hash is taken; otherwise the
/// insert failed.
pub fn insert(conn: &Connection, token: &NewApiToken<'_>, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO api_tokens (id, user_id, kind, token_hash, label, scopes, created_at, \
         expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            token.id,
            token.user_id,
            token.kind.as_str(),
            token.token_hash.as_slice(),
            token.label,
            token.scopes,
            now,
            token.expires_at,
        ],
    )
    .map_err(|e| conflict_on_unique(e, "token"))?;
    Ok(())
}

/// The token whose value hashes to `token_hash`, if it is unrevoked, not
/// expired at `now`, and its user is active.
///
/// # Errors
///
/// The query failed.
pub fn find_active(
    conn: &Connection,
    token_hash: &TokenHash,
    now: i64,
) -> Result<Option<ApiToken>> {
    conn.query_row(
        "SELECT t.id, t.user_id, t.kind, t.scopes, t.expires_at FROM api_tokens t \
         JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = ?1 AND t.revoked_at IS NULL \
           AND (t.expires_at IS NULL OR t.expires_at > ?2) AND u.status = 'active'",
        params![token_hash.as_slice(), now],
        |row| {
            Ok(ApiToken {
                id: row.get(0)?,
                user_id: row.get(1)?,
                kind: row.get(2)?,
                scopes: row.get(3)?,
                expires_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

#[cfg(test)]
mod tests {
    use rusqlite::params;

    use super::*;
    use crate::control::testing::{NOW, control_with_users};
    use crate::tokens::hash_token;

    #[test]
    fn only_unrevoked_tokens_of_active_users_are_found() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            for (id, user, token, revoked) in [
                ("T1", &owner, "shx_live", None),
                ("T2", &owner, "shx_revoked", Some(NOW)),
                ("T3", &member, "shx_member", None),
            ] {
                tx.execute(
                    "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at, \
                     revoked_at) VALUES (?1, ?2, 'extension', ?3, 'ingest lookup', ?4, ?5)",
                    params![id, user, hash_token(token).as_slice(), NOW, revoked],
                )?;
            }
            tx.execute(
                "UPDATE users SET status = 'disabled' WHERE id = ?1",
                [&member],
            )?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        let found = db
            .read(|conn| find_active(conn, &hash_token("shx_live"), NOW))
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "T1");
        assert_eq!(found.user_id, owner);
        assert_eq!(found.scopes, "ingest lookup");
        assert_eq!(found.expires_at, None);
        for token in ["shx_revoked", "shx_member", "shx_unknown"] {
            assert_eq!(
                db.read(|conn| find_active(conn, &hash_token(token), NOW))
                    .unwrap(),
                None,
                "{token}"
            );
        }
    }

    #[test]
    fn a_token_stops_working_when_it_expires() {
        let (db, owner, _) = control_with_users();
        let hash = hash_token("shx_migrate");
        let token = NewApiToken {
            id: "T1",
            user_id: &owner,
            kind: TokenKind::Migrate,
            token_hash: &hash,
            label: Some("migration"),
            scopes: "migrate",
            expires_at: Some(NOW + 1_000),
        };
        db.write(|tx| insert(tx, &token, NOW)).unwrap();
        let found = |at| db.read(|conn| find_active(conn, &hash, at)).unwrap();
        let live = found(NOW + 999).expect("valid until its expiry");
        assert_eq!(live.kind, "migrate");
        assert_eq!(live.expires_at, Some(NOW + 1_000));
        assert_eq!(found(NOW + 1_000), None, "expired at expires_at");

        let again = db.write(|tx| insert(tx, &token, NOW)).unwrap_err();
        assert!(matches!(again, RepoError::Conflict("token")), "{again}");
    }
}
