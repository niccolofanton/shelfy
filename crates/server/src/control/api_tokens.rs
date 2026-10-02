//! `api_tokens` (plan §2.6, §2.11): scoped bearer tokens for the extension,
//! the iOS Shortcut and the migration CLI.
//!
//! **Seam.** T10 only reads tokens, for the bearer extractor
//! ([`crate::auth::bearer`]). P1-17 adds creation (shown once), listing,
//! `last_used_at` and revocation, and may change how `scopes` is stored; today
//! it is a space-separated list of scope names (`ingest lookup`).

use rusqlite::{Connection, OptionalExtension as _};
use shelfy_core::repo::{RepoError, Result};

use crate::tokens::TokenHash;

/// An unrevoked token of an active user.
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
}

/// The unrevoked token whose value hashes to `token_hash`, if its user is
/// active.
///
/// # Errors
///
/// The query failed.
pub fn find_active(conn: &Connection, token_hash: &TokenHash) -> Result<Option<ApiToken>> {
    conn.query_row(
        "SELECT t.id, t.user_id, t.kind, t.scopes FROM api_tokens t \
         JOIN users u ON u.id = t.user_id \
         WHERE t.token_hash = ?1 AND t.revoked_at IS NULL AND u.status = 'active'",
        [token_hash.as_slice()],
        |row| {
            Ok(ApiToken {
                id: row.get(0)?,
                user_id: row.get(1)?,
                kind: row.get(2)?,
                scopes: row.get(3)?,
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
            .read(|conn| find_active(conn, &hash_token("shx_live")))
            .unwrap()
            .unwrap();
        assert_eq!(found.id, "T1");
        assert_eq!(found.user_id, owner);
        assert_eq!(found.scopes, "ingest lookup");
        for token in ["shx_revoked", "shx_member", "shx_unknown"] {
            assert_eq!(
                db.read(|conn| find_active(conn, &hash_token(token)))
                    .unwrap(),
                None,
                "{token}"
            );
        }
    }
}
