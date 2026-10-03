//! `pairing_codes` (plan §2.6, §2.11 device tokens): the short-lived,
//! single-use codes that pair the browser extension with an account (P2-03,
//! contract C2).
//!
//! A row keeps the SHA-256 of the code, never the code, with the user who
//! asked for it, its kind (`extension`), its expiry and when it was used.
//! [`consume`] checks and spends a code in one statement, so a code pairs at
//! most once even when two requests race (the control database has one
//! writer). The pairing itself is in [`crate::extension::pairing`].

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use crate::tokens::TokenHash;

/// `pairing_codes.kind` of the codes that pair the browser extension.
pub const KIND_EXTENSION: &str = "extension";

/// A code to store. Only its hash is kept.
#[derive(Clone, Copy, Debug)]
pub struct NewPairingCode<'a> {
    /// SHA-256 of the code.
    pub code_hash: &'a TokenHash,
    /// Whose account the code pairs with.
    pub user_id: &'a str,
    /// What the code pairs: [`KIND_EXTENSION`].
    pub kind: &'a str,
    /// Expiry, unix ms.
    pub expires_at: i64,
}

/// Stores a code.
///
/// # Errors
///
/// [`RepoError::Conflict`] if the hash exists (a collision of 256-bit
/// values), or the insert failed.
pub fn insert(conn: &Connection, code: &NewPairingCode<'_>) -> Result<()> {
    conn.execute(
        "INSERT INTO pairing_codes (code_hash, user_id, kind, expires_at) VALUES (?1, ?2, ?3, ?4)",
        params![
            code.code_hash.as_slice(),
            code.user_id,
            code.kind,
            code.expires_at
        ],
    )
    .map_err(|e| conflict_on_unique(e, "pairing code"))?;
    Ok(())
}

/// How many codes of `user_id` and `kind` are still usable at `now`:
/// unused and unexpired.
///
/// # Errors
///
/// The query failed.
pub fn count_live(conn: &Connection, user_id: &str, kind: &str, now: i64) -> Result<u64> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM pairing_codes \
         WHERE user_id = ?1 AND kind = ?2 AND used_at IS NULL AND expires_at > ?3",
        params![user_id, kind, now],
        |row| row.get(0),
    )?;
    Ok(u64::try_from(count).unwrap_or(0))
}

/// The earliest expiry of the codes [`count_live`] counts, if any.
///
/// # Errors
///
/// The query failed.
pub fn first_live_expiry(
    conn: &Connection,
    user_id: &str,
    kind: &str,
    now: i64,
) -> Result<Option<i64>> {
    conn.query_row(
        "SELECT min(expires_at) FROM pairing_codes \
         WHERE user_id = ?1 AND kind = ?2 AND used_at IS NULL AND expires_at > ?3",
        params![user_id, kind, now],
        |row| row.get(0),
    )
    .map_err(RepoError::from)
}

/// Whether [`consume`] would spend the code whose hash is `code_hash` at
/// `now`: the same conditions, read-only, so a guessed or stale code never
/// takes the writer.
///
/// # Errors
///
/// The query failed.
pub fn is_usable(conn: &Connection, code_hash: &TokenHash, kind: &str, now: i64) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM pairing_codes \
         WHERE code_hash = ?1 AND kind = ?2 AND used_at IS NULL AND expires_at > ?3 \
         AND user_id IN (SELECT id FROM users WHERE status = 'active'))",
        params![code_hash.as_slice(), kind, now],
        |row| row.get(0),
    )
    .map_err(RepoError::from)
}

/// Spends the code whose hash is `code_hash`: it must have `kind`, be
/// unused and unexpired at `now`, and belong to an active user. Marks it
/// used and returns its user; `None` when any condition fails, and then
/// nothing changes.
///
/// # Errors
///
/// The update failed.
pub fn consume(
    conn: &Connection,
    code_hash: &TokenHash,
    kind: &str,
    now: i64,
) -> Result<Option<String>> {
    conn.query_row(
        "UPDATE pairing_codes SET used_at = ?3 \
         WHERE code_hash = ?1 AND kind = ?2 AND used_at IS NULL AND expires_at > ?3 \
         AND user_id IN (SELECT id FROM users WHERE status = 'active') \
         RETURNING user_id",
        params![code_hash.as_slice(), kind, now],
        |row| row.get(0),
    )
    .optional()
    .map_err(RepoError::from)
}

/// Deletes the codes that expired by `now`, used or not; returns how many.
/// What happened with them is in `audit_log`.
///
/// # Errors
///
/// The delete failed.
pub fn prune(conn: &Connection, now: i64) -> Result<usize> {
    Ok(conn.execute("DELETE FROM pairing_codes WHERE expires_at <= ?1", [now])?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};
    use crate::tokens::hash_token;

    fn code<'a>(hash: &'a TokenHash, user: &'a str, expires_at: i64) -> NewPairingCode<'a> {
        NewPairingCode {
            code_hash: hash,
            user_id: user,
            kind: KIND_EXTENSION,
            expires_at,
        }
    }

    #[test]
    fn a_code_is_spent_once_before_it_expires() {
        let (db, owner, _member) = control_with_users();
        let hash = hash_token("code-1");
        db.write(|tx| insert(tx, &code(&hash, &owner, NOW + 60_000)))
            .unwrap();
        let spend = |at: i64| {
            db.write(|tx| consume(tx, &hash, KIND_EXTENSION, at))
                .unwrap()
        };
        let usable = |hash: &TokenHash, kind: &str, at: i64| {
            db.read(|conn| is_usable(conn, hash, kind, at)).unwrap()
        };
        assert_eq!(
            db.write(|tx| consume(tx, &hash, "other", NOW)).unwrap(),
            None,
            "the kind must match"
        );
        assert!(!usable(&hash, "other", NOW));
        assert!(usable(&hash, KIND_EXTENSION, NOW + 59_999));
        assert!(!usable(&hash, KIND_EXTENSION, NOW + 60_000));
        assert!(!usable(&hash_token("unknown"), KIND_EXTENSION, NOW));
        assert_eq!(spend(NOW + 59_999), Some(owner.clone()));
        assert!(!usable(&hash, KIND_EXTENSION, NOW + 59_999), "used");
        assert_eq!(spend(NOW + 59_999), None, "used");

        let late = hash_token("code-2");
        db.write(|tx| insert(tx, &code(&late, &owner, NOW + 60_000)))
            .unwrap();
        assert_eq!(
            db.write(|tx| consume(tx, &late, KIND_EXTENSION, NOW + 60_000))
                .unwrap(),
            None,
            "expired at expires_at"
        );
        assert_eq!(
            db.write(|tx| consume(tx, &hash_token("unknown"), KIND_EXTENSION, NOW))
                .unwrap(),
            None
        );
        let again = db
            .write(|tx| insert(tx, &code(&late, &owner, NOW + 60_000)))
            .unwrap_err();
        assert!(
            matches!(again, RepoError::Conflict("pairing code")),
            "{again}"
        );
    }

    #[test]
    fn a_disabled_users_code_is_refused() {
        let (db, _owner, member) = control_with_users();
        let hash = hash_token("code-1");
        db.write(|tx| {
            insert(tx, &code(&hash, &member, NOW + 60_000))?;
            tx.execute(
                "UPDATE users SET status = 'disabled' WHERE id = ?1",
                [&member],
            )?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        assert_eq!(
            db.write(|tx| consume(tx, &hash, KIND_EXTENSION, NOW))
                .unwrap(),
            None
        );
        let used: Option<i64> = db
            .read(|conn| {
                conn.query_row("SELECT used_at FROM pairing_codes", [], |row| row.get(0))
                    .map_err(RepoError::from)
            })
            .unwrap();
        assert_eq!(used, None, "a refused code is left as it was");
    }

    #[test]
    fn live_codes_are_counted_per_user_and_pruned_once_expired() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            for (name, user, expires_at) in [
                ("a", &owner, NOW + 10_000),
                ("b", &owner, NOW + 20_000),
                ("c", &owner, NOW - 1),
                ("d", &member, NOW + 30_000),
            ] {
                insert(tx, &code(&hash_token(name), user, expires_at))?;
            }
            consume(tx, &hash_token("b"), KIND_EXTENSION, NOW)?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        let live = |user: &str| {
            db.read(|conn| count_live(conn, user, KIND_EXTENSION, NOW))
                .unwrap()
        };
        assert_eq!(live(&owner), 1, "b is used, c expired");
        assert_eq!(live(&member), 1);
        assert_eq!(
            db.read(|conn| first_live_expiry(conn, &owner, KIND_EXTENSION, NOW))
                .unwrap(),
            Some(NOW + 10_000)
        );
        assert_eq!(
            db.read(|conn| first_live_expiry(conn, &owner, "other", NOW))
                .unwrap(),
            None
        );
        assert_eq!(db.write(|tx| prune(tx, NOW)).unwrap(), 1);
        assert_eq!(db.write(|tx| prune(tx, NOW + 20_000)).unwrap(), 2);
        assert_eq!(live(&member), 1);
    }
}
