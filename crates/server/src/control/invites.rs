//! `invites` (plan §2.6, §2.11). Unused while E4 keeps the instance
//! owner-only: `admin invite` can create one, nothing accepts it yet.

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use super::users::Role;
use crate::tokens::TokenHash;

/// A stored invite (the token itself is never stored).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invite {
    /// Who may accept it; `None` = anyone holding the link.
    pub email: Option<String>,
    /// Role the new account gets.
    pub role: String,
    /// The owner who created it.
    pub created_by: String,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// Expiry, unix ms.
    pub expires_at: i64,
    /// When it was accepted.
    pub used_at: Option<i64>,
}

/// A new invite.
#[derive(Clone, Copy, Debug)]
pub struct NewInvite<'a> {
    /// SHA-256 of the token.
    pub token_hash: &'a TokenHash,
    /// Email lock, normalized.
    pub email: Option<&'a str>,
    /// Role of the account it creates.
    pub role: Role,
    /// Creator (the owner).
    pub created_by: &'a str,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Stores an invite.
///
/// # Errors
///
/// [`RepoError::Conflict`] if the hash exists (a token collision), or the
/// insert failed.
pub fn insert(conn: &Connection, invite: &NewInvite<'_>, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO invites (token_hash, email, role, created_by, created_at, expires_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            invite.token_hash.as_slice(),
            invite.email,
            invite.role.as_str(),
            invite.created_by,
            now,
            invite.expires_at
        ],
    )
    .map_err(|e| conflict_on_unique(e, "invite"))?;
    Ok(())
}

/// The invite whose token hashes to `token_hash`.
///
/// # Errors
///
/// The query failed.
pub fn find(conn: &Connection, token_hash: &TokenHash) -> Result<Option<Invite>> {
    conn.query_row(
        "SELECT email, role, created_by, created_at, expires_at, used_at \
         FROM invites WHERE token_hash = ?1",
        [token_hash.as_slice()],
        |row| {
            Ok(Invite {
                email: row.get(0)?,
                role: row.get(1)?,
                created_by: row.get(2)?,
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
                used_at: row.get(5)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}
