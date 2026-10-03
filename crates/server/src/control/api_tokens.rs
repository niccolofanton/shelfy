//! `api_tokens` (plan §2.6, §2.11): scoped bearer tokens for the extension,
//! the iOS Shortcut, migration CLI and library API clients.
//!
//! A row keeps the SHA-256 of the whole value (`shx_…`), never the value.
//! `scopes` is a space-separated list of scope names (`ingest lookup`).
//! A token works until it is revoked (`revoked_at`) or expires (`expires_at`,
//! control schema v2; `NULL` for tokens that last until revoked).
//!
//! - The bearer check ([`crate::auth::bearer`]) finds tokens with
//!   [`find_active`] and records their use with [`touch`] (`last_used_at`,
//!   at most once a minute per token).
//! - Minting ([`crate::auth::api_tokens::mint`]) inserts with [`insert`].
//! - The account lists its tokens with [`list_active`] and revokes one with
//!   [`revoke`].
//! - Pairing ([`crate::extension::pairing`]) records which browser
//!   installation holds an extension token with [`set_install`] (control
//!   schema v4, `install_hash`), and pairing the same installation again
//!   revokes its earlier token with [`revoke_install`] (P2-G16).

use rusqlite::{Connection, OptionalExtension as _, Row, params};
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
    /// A desktop API client or MCP integration.
    Library,
}

impl TokenKind {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Extension => "extension",
            Self::Shortcut => "shortcut",
            Self::Migrate => "migrate",
            Self::Library => "library",
        }
    }

    /// The kind stored as `value`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        [
            Self::Extension,
            Self::Shortcut,
            Self::Migrate,
            Self::Library,
        ]
        .into_iter()
        .find(|kind| kind.as_str() == value)
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
    /// `extension`, `shortcut`, `migrate` or `library`.
    pub kind: String,
    /// Space-separated scope names.
    pub scopes: String,
    /// Expiry, unix ms; `None` when the token does not expire.
    pub expires_at: Option<i64>,
    /// Last request it authenticated, unix ms.
    pub last_used_at: Option<i64>,
}

/// A token as the account's list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TokenRow {
    /// Token id (ULID).
    pub id: String,
    /// Who holds it.
    pub kind: TokenKind,
    /// The user's name for it.
    pub label: Option<String>,
    /// Space-separated scope names.
    pub scopes: String,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// Last request it authenticated, unix ms.
    pub last_used_at: Option<i64>,
    /// Expiry, unix ms; `None` when it lasts until revoked.
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
        "SELECT t.id, t.user_id, t.kind, t.scopes, t.expires_at, t.last_used_at \
         FROM api_tokens t JOIN users u ON u.id = t.user_id \
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
                last_used_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records a use of token `id` at `now`. A late, out-of-order use never
/// moves `last_used_at` back.
///
/// # Errors
///
/// The update failed.
pub fn touch(conn: &Connection, id: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE api_tokens SET last_used_at = ?2 WHERE id = ?1 \
         AND (last_used_at IS NULL OR last_used_at < ?2)",
        params![id, now],
    )?;
    Ok(())
}

const ROW_COLUMNS: &str = "id, kind, label, scopes, created_at, last_used_at, expires_at";

/// The condition of a token that still works at `?2`, for `user_id` = `?1`.
const ACTIVE: &str =
    "user_id = ?1 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > ?2)";

fn from_row(row: &Row<'_>) -> rusqlite::Result<TokenRow> {
    let kind: String = row.get(1)?;
    Ok(TokenRow {
        id: row.get(0)?,
        // The schema's CHECK constraint admits only known kinds.
        kind: TokenKind::parse(&kind).unwrap_or(TokenKind::Extension),
        label: row.get(2)?,
        scopes: row.get(3)?,
        created_at: row.get(4)?,
        last_used_at: row.get(5)?,
        expires_at: row.get(6)?,
    })
}

/// The tokens of `user_id` that work at `now`, newest first.
///
/// # Errors
///
/// The query failed.
pub fn list_active(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<TokenRow>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {ROW_COLUMNS} FROM api_tokens WHERE {ACTIVE} ORDER BY created_at DESC, id DESC"
    ))?;
    let rows = statement
        .query_map(params![user_id, now], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// How many tokens of `user_id` work at `now`.
///
/// # Errors
///
/// The query failed.
pub fn count_active(conn: &Connection, user_id: &str, now: i64) -> Result<u64> {
    let count: i64 = conn.query_row(
        &format!("SELECT count(*) FROM api_tokens WHERE {ACTIVE}"),
        params![user_id, now],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// Revokes token `id` of `user_id` at `now`; returns it, or `None` when
/// `user_id` has no working token with this id (another user's token is
/// left alone, like a missing one).
///
/// # Errors
///
/// The update failed.
pub fn revoke(conn: &Connection, user_id: &str, id: &str, now: i64) -> Result<Option<TokenRow>> {
    conn.query_row(
        &format!(
            "UPDATE api_tokens SET revoked_at = ?2 WHERE id = ?3 AND {ACTIVE} \
             RETURNING {ROW_COLUMNS}"
        ),
        params![user_id, now, id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records that the browser installation whose id hashes to `install_hash`
/// holds token `id`.
///
/// # Errors
///
/// [`RepoError::NotFound`] when no token has this id; the update failed.
pub fn set_install(conn: &Connection, id: &str, install_hash: &TokenHash) -> Result<()> {
    let updated = conn.execute(
        "UPDATE api_tokens SET install_hash = ?2 WHERE id = ?1",
        params![id, install_hash.as_slice()],
    )?;
    if updated == 0 {
        return Err(RepoError::NotFound);
    }
    Ok(())
}

/// Revokes at `now` the working tokens of `user_id` that the installation
/// `install_hash` holds; returns them. Another user's tokens are left
/// alone, whatever installation holds them.
///
/// # Errors
///
/// The update failed.
pub fn revoke_install(
    conn: &Connection,
    user_id: &str,
    install_hash: &TokenHash,
    now: i64,
) -> Result<Vec<TokenRow>> {
    let mut statement = conn.prepare_cached(&format!(
        "UPDATE api_tokens SET revoked_at = ?2 WHERE install_hash = ?3 AND {ACTIVE} \
         RETURNING {ROW_COLUMNS}"
    ))?;
    let rows = statement
        .query_map(params![user_id, now, install_hash.as_slice()], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
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
        assert_eq!(found.last_used_at, None);
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

    fn token<'a>(id: &'a str, user: &'a str, hash: &'a TokenHash) -> NewApiToken<'a> {
        NewApiToken {
            id,
            user_id: user,
            kind: TokenKind::Shortcut,
            token_hash: hash,
            label: None,
            scopes: "links:create",
            expires_at: None,
        }
    }

    #[test]
    fn the_account_lists_and_revokes_its_working_tokens() {
        let (db, owner, member) = control_with_users();
        let hashes: Vec<TokenHash> = (0..4).map(|i| hash_token(&format!("shx_{i}"))).collect();
        db.write(|tx| {
            insert(tx, &token("A", &owner, &hashes[0]), NOW)?;
            let expiring = NewApiToken {
                expires_at: Some(NOW + 10),
                kind: TokenKind::Migrate,
                scopes: "migrate",
                ..token("B", &owner, &hashes[1])
            };
            insert(tx, &expiring, NOW + 1)?;
            insert(tx, &token("C", &owner, &hashes[2]), NOW + 2)?;
            insert(tx, &token("D", &member, &hashes[3]), NOW)
        })
        .unwrap();
        let ids = |at: i64| -> Vec<String> {
            db.read(|conn| list_active(conn, &owner, at))
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect()
        };
        assert_eq!(ids(NOW + 5), ["C", "B", "A"], "newest first, own tokens");
        assert_eq!(ids(NOW + 10), ["C", "A"], "expired tokens are gone");
        assert_eq!(
            db.read(|conn| count_active(conn, &owner, NOW + 5)).unwrap(),
            3
        );

        // Revoking: own working tokens only.
        let revoked = db.write(|tx| revoke(tx, &owner, "C", NOW + 6)).unwrap();
        assert_eq!(
            revoked.map(|row| (row.id, row.kind)),
            Some(("C".to_owned(), TokenKind::Shortcut))
        );
        assert_eq!(
            db.write(|tx| revoke(tx, &owner, "C", NOW + 7)).unwrap(),
            None
        );
        assert_eq!(
            db.write(|tx| revoke(tx, &owner, "D", NOW + 7)).unwrap(),
            None
        );
        assert_eq!(
            db.write(|tx| revoke(tx, &owner, "B", NOW + 20)).unwrap(),
            None,
            "expired"
        );
        assert_eq!(ids(NOW + 7), ["B", "A"]);
        assert!(
            db.read(|conn| find_active(conn, &hashes[2], NOW + 7))
                .unwrap()
                .is_none()
        );

        // Uses move forward only.
        db.write(|tx| touch(tx, "A", NOW + 50)).unwrap();
        db.write(|tx| touch(tx, "A", NOW + 40)).unwrap();
        let row = db.read(|conn| list_active(conn, &owner, NOW + 60)).unwrap();
        assert_eq!(row[0].last_used_at, Some(NOW + 50));
        assert_eq!(row[0].created_at, NOW);
        assert_eq!(row[0].label, None);
    }

    #[test]
    fn an_installations_working_tokens_of_one_user_are_revoked_together() {
        let (db, owner, member) = control_with_users();
        let hashes: Vec<TokenHash> = (0..5).map(|i| hash_token(&format!("shx_{i}"))).collect();
        let install = hash_token("install-1");
        let other_install = hash_token("install-2");
        db.write(|tx| {
            for (i, (id, user)) in [
                ("A", &owner),
                ("B", &owner),
                ("C", &owner),
                ("D", &member),
                ("E", &owner),
            ]
            .into_iter()
            .enumerate()
            {
                insert(tx, &token(id, user, &hashes[i]), NOW)?;
            }
            for id in ["A", "B", "D"] {
                set_install(tx, id, &install)?;
            }
            set_install(tx, "C", &other_install)?;
            revoke(tx, &owner, "B", NOW + 1)?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        assert!(matches!(
            db.write(|tx| set_install(tx, "nope", &install)),
            Err(RepoError::NotFound)
        ));

        let revoked = db
            .write(|tx| revoke_install(tx, &owner, &install, NOW + 2))
            .unwrap();
        let ids: Vec<&str> = revoked.iter().map(|row| row.id.as_str()).collect();
        assert_eq!(ids, ["A"], "B was revoked already, D is another user's");
        let working = |user: &str| -> Vec<String> {
            db.read(|conn| list_active(conn, user, NOW + 3))
                .unwrap()
                .into_iter()
                .map(|row| row.id)
                .collect()
        };
        assert_eq!(working(&owner), ["E", "C"]);
        assert_eq!(working(&member), ["D"]);
        assert_eq!(
            db.write(|tx| revoke_install(tx, &owner, &install, NOW + 4))
                .unwrap(),
            Vec::new()
        );
    }
}
