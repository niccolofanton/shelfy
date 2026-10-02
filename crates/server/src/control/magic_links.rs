//! `magic_links` (plan §2.6, §2.11): single-use, short-lived email links.
//!
//! A link is keyed by the SHA-256 of its token. [`consume`] marks it used in
//! the same statement that checks it, so a link signs in at most once even
//! when two requests race (the control database has one writer).
//! [`is_redeemable`] runs the same check on a reader first, so a guessed or
//! stale token never takes the writer.

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use crate::tokens::TokenHash;

/// `magic_links.purpose`: what a link proves.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    /// Signs the user in (T10).
    Login,
    /// Confirms an email address (invites; unused while E4 holds).
    Verify,
    /// Re-authenticates a signed-in session (P1-13).
    Reauth,
}

impl Purpose {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::Verify => "verify",
            Self::Reauth => "reauth",
        }
    }
}

/// A new link.
#[derive(Clone, Copy, Debug)]
pub struct NewMagicLink<'a> {
    /// SHA-256 of the token.
    pub token_hash: &'a TokenHash,
    /// Whose link it is.
    pub user_id: &'a str,
    /// What it proves.
    pub purpose: Purpose,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Stores a link.
///
/// # Errors
///
/// [`RepoError::Conflict`] if the hash exists (a token collision), or the
/// insert failed.
pub fn insert(conn: &Connection, link: &NewMagicLink<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO magic_links (token_hash, user_id, purpose, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            link.token_hash.as_slice(),
            link.user_id,
            link.purpose.as_str(),
            link.expires_at
        ],
    )
    .map_err(|e| conflict_on_unique(e, "magic link"))?;
    Ok(())
}

/// Whether the link whose token hashes to `token_hash` would be used by
/// [`consume`] at `now`: the same conditions, read-only.
///
/// # Errors
///
/// The query failed.
pub fn is_redeemable(
    conn: &Connection,
    token_hash: &TokenHash,
    purpose: Purpose,
    now: i64,
) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM magic_links \
         WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3 \
         AND user_id IN (SELECT id FROM users WHERE status = 'active'))",
        params![token_hash.as_slice(), purpose.as_str(), now],
        |row| row.get(0),
    )
    .map_err(RepoError::from)
}

/// Uses the link whose token hashes to `token_hash`: it must have `purpose`,
/// be unused and unexpired at `now`, and belong to an active user. Marks it
/// used and returns its user; `None` when any condition fails.
///
/// # Errors
///
/// The update failed.
pub fn consume(
    conn: &Connection,
    token_hash: &TokenHash,
    purpose: Purpose,
    now: i64,
) -> Result<Option<String>> {
    conn.query_row(
        "UPDATE magic_links SET used_at = ?3 \
         WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3 \
         AND user_id IN (SELECT id FROM users WHERE status = 'active') \
         RETURNING user_id",
        params![token_hash.as_slice(), purpose.as_str(), now],
        |row| row.get(0),
    )
    .optional()
    .map_err(RepoError::from)
}

/// [`consume`] for a link of `user_id` only (a re-authentication link proves
/// who is signed in): another account's link is refused and left unused.
/// Returns whether the link was used.
///
/// # Errors
///
/// The update failed.
pub fn consume_for_user(
    conn: &Connection,
    token_hash: &TokenHash,
    purpose: Purpose,
    user_id: &str,
    now: i64,
) -> Result<bool> {
    let used = conn.execute(
        "UPDATE magic_links SET used_at = ?3 \
         WHERE token_hash = ?1 AND purpose = ?2 AND used_at IS NULL AND expires_at > ?3 \
         AND user_id = ?4 AND user_id IN (SELECT id FROM users WHERE status = 'active')",
        params![token_hash.as_slice(), purpose.as_str(), now, user_id],
    )?;
    Ok(used > 0)
}

/// Deletes the links that expired by `now`, used or not; returns how many.
/// Facts worth keeping live in `audit_log`.
///
/// # Errors
///
/// The delete failed.
pub fn prune(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM magic_links WHERE expires_at <= ?1", [now])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};
    use crate::tokens::SecretToken;

    const MINUTE: i64 = 60_000;

    fn link<'a>(user_id: &'a str, hash: &'a TokenHash, purpose: Purpose) -> NewMagicLink<'a> {
        NewMagicLink {
            token_hash: hash,
            user_id,
            purpose,
            expires_at: NOW + 15 * MINUTE,
        }
    }

    #[test]
    fn a_link_signs_in_once() {
        let (db, owner, _member) = control_with_users();
        let hash = SecretToken::generate().hash();
        db.write(|tx| insert(tx, &link(&owner, &hash, Purpose::Login)))
            .unwrap();
        let redeemable = |now| {
            db.read(|conn| is_redeemable(conn, &hash, Purpose::Login, now))
                .unwrap()
        };
        assert!(redeemable(NOW + MINUTE));
        assert!(!redeemable(NOW + 15 * MINUTE), "expired");
        let first = db
            .write(|tx| consume(tx, &hash, Purpose::Login, NOW + MINUTE))
            .unwrap();
        assert_eq!(first.as_deref(), Some(owner.as_str()));
        let second = db
            .write(|tx| consume(tx, &hash, Purpose::Login, NOW + 2 * MINUTE))
            .unwrap();
        assert_eq!(second, None, "single use");
        assert!(!redeemable(NOW + 2 * MINUTE), "used");
        let used_at: Option<i64> = db
            .read(|conn| {
                conn.query_row(
                    "SELECT used_at FROM magic_links WHERE token_hash = ?1",
                    [hash.as_slice()],
                    |row| row.get(0),
                )
                .map_err(RepoError::from)
            })
            .unwrap();
        assert_eq!(used_at, Some(NOW + MINUTE));
    }

    #[test]
    fn expired_foreign_purpose_unknown_or_inactive_links_are_refused() {
        let (db, owner, member) = control_with_users();
        let expired = SecretToken::generate().hash();
        let reauth = SecretToken::generate().hash();
        let disabled = SecretToken::generate().hash();
        db.write(|tx| {
            insert(tx, &link(&owner, &expired, Purpose::Login))?;
            insert(tx, &link(&owner, &reauth, Purpose::Reauth))?;
            insert(tx, &link(&member, &disabled, Purpose::Login))?;
            tx.execute(
                "UPDATE users SET status = 'disabled' WHERE id = ?1",
                [&member],
            )?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        let at_expiry = NOW + 15 * MINUTE;
        for (hash, purpose, now) in [
            (&expired, Purpose::Login, at_expiry),
            (&reauth, Purpose::Login, NOW),
            (&disabled, Purpose::Login, NOW),
            (&SecretToken::generate().hash(), Purpose::Login, NOW),
        ] {
            assert!(
                !db.read(|conn| is_redeemable(conn, hash, purpose, now))
                    .unwrap()
            );
            assert_eq!(
                db.write(|tx| consume(tx, hash, purpose, now)).unwrap(),
                None
            );
        }
        // A refused attempt does not burn the link.
        assert_eq!(
            db.write(|tx| consume(tx, &reauth, Purpose::Reauth, NOW))
                .unwrap()
                .as_deref(),
            Some(owner.as_str())
        );
    }

    #[test]
    fn a_reauth_link_proves_its_own_account_only() {
        let (db, owner, member) = control_with_users();
        let hash = SecretToken::generate().hash();
        db.write(|tx| insert(tx, &link(&owner, &hash, Purpose::Reauth)))
            .unwrap();
        let consume = |user: &str, purpose, now| {
            db.write(|tx| consume_for_user(tx, &hash, purpose, user, now))
                .unwrap()
        };
        assert!(!consume(&member, Purpose::Reauth, NOW), "another account");
        assert!(!consume(&owner, Purpose::Login, NOW), "another purpose");
        assert!(
            !consume(&owner, Purpose::Reauth, NOW + 15 * MINUTE),
            "expired"
        );
        assert!(
            consume(&owner, Purpose::Reauth, NOW),
            "the refusals left it unused"
        );
        assert!(
            !consume(&owner, Purpose::Reauth, NOW + MINUTE),
            "single use"
        );
    }

    #[test]
    fn expired_links_are_pruned() {
        let (db, owner, _member) = control_with_users();
        let old = SecretToken::generate().hash();
        let fresh = SecretToken::generate().hash();
        db.write(|tx| {
            insert(tx, &link(&owner, &old, Purpose::Login))?;
            let mut later = link(&owner, &fresh, Purpose::Login);
            later.expires_at = NOW + 30 * MINUTE;
            insert(tx, &later)
        })
        .unwrap();
        assert_eq!(
            db.write(|tx| prune(tx, NOW + 15 * MINUTE)).unwrap(),
            1,
            "only the expired link goes"
        );
        assert!(
            db.write(|tx| consume(tx, &fresh, Purpose::Login, NOW + 16 * MINUTE))
                .unwrap()
                .is_some()
        );
    }
}
