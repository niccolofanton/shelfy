//! `passkeys` (plan §2.6, §2.11): the WebAuthn credentials of the accounts.
//!
//! A row is one credential: `cred_id` (the raw credential id, unique across
//! accounts), `passkey_json` (what the verifier keeps of it: the public key,
//! the signature counter and the backup flags, as `webauthn-rs` serializes
//! its `Passkey`), the user's label, the creation time and the last use.
//! None of it is a secret, but credential ids and public keys never reach
//! the logs, and the label is user content.

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use super::users::Status;

/// Longest label kept, in characters.
pub const LABEL_MAX_CHARS: usize = 64;

/// A new passkey.
#[derive(Clone, Copy, Debug)]
pub struct NewPasskey<'a> {
    /// Its account.
    pub user_id: &'a str,
    /// The raw credential id.
    pub cred_id: &'a [u8],
    /// The verifier's state of the credential.
    pub passkey_json: &'a str,
    /// The user's name for it.
    pub label: Option<&'a str>,
}

/// A stored passkey. `Debug` leaves out the credential's id and state.
#[derive(Clone, PartialEq, Eq)]
pub struct PasskeyRow {
    /// Row id: how the API names the passkey.
    pub id: i64,
    /// Its account.
    pub user_id: String,
    /// The raw credential id.
    pub cred_id: Vec<u8>,
    /// The verifier's state of the credential.
    pub passkey_json: String,
    /// The user's name for it.
    pub label: Option<String>,
    /// Registration time, unix ms.
    pub created_at: i64,
    /// Last sign-in or re-authentication with it, unix ms.
    pub last_used_at: Option<i64>,
}

impl std::fmt::Debug for PasskeyRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PasskeyRow")
            .field("id", &self.id)
            .field("user_id", &self.user_id)
            .field("created_at", &self.created_at)
            .field("last_used_at", &self.last_used_at)
            .finish_non_exhaustive()
    }
}

const COLUMNS: &str =
    "p.id, p.user_id, p.cred_id, p.passkey_json, p.label, p.created_at, p.last_used_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<PasskeyRow> {
    Ok(PasskeyRow {
        id: row.get(0)?,
        user_id: row.get(1)?,
        cred_id: row.get(2)?,
        passkey_json: row.get(3)?,
        label: row.get(4)?,
        created_at: row.get(5)?,
        last_used_at: row.get(6)?,
    })
}

/// Stores a passkey; returns its row id.
///
/// # Errors
///
/// [`RepoError::Conflict`] (`passkey`) when the credential is registered
/// already, to this account or another; or the insert failed.
pub fn insert(conn: &Connection, passkey: &NewPasskey<'_>, now: i64) -> Result<i64> {
    conn.execute(
        "INSERT INTO passkeys (user_id, cred_id, passkey_json, label, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            passkey.user_id,
            passkey.cred_id,
            passkey.passkey_json,
            passkey.label,
            now
        ],
    )
    .map_err(|e| conflict_on_unique(e, "passkey"))?;
    Ok(conn.last_insert_rowid())
}

/// The passkeys of `user_id`, oldest first.
///
/// # Errors
///
/// The query failed.
pub fn list(conn: &Connection, user_id: &str) -> Result<Vec<PasskeyRow>> {
    let mut statement = conn.prepare_cached(&format!(
        "SELECT {COLUMNS} FROM passkeys p WHERE p.user_id = ?1 ORDER BY p.created_at, p.id"
    ))?;
    let rows = statement
        .query_map([user_id], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The passkey with the raw credential id `cred_id`, with the status of its
/// account.
///
/// # Errors
///
/// The query failed.
pub fn find_by_credential(
    conn: &Connection,
    cred_id: &[u8],
) -> Result<Option<(PasskeyRow, Status)>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS}, u.status FROM passkeys p JOIN users u ON u.id = p.user_id \
             WHERE p.cred_id = ?1"
        ),
        [cred_id],
        |row| {
            let status: String = row.get(7)?;
            // The schema's CHECK constraint admits only known values.
            Ok((
                from_row(row)?,
                Status::parse(&status).unwrap_or(Status::Disabled),
            ))
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records a sign-in or a re-authentication with passkey `id` at `now`, and
/// its new state when the verifier changed it (the counter, the backup
/// flags). Returns whether the passkey exists.
///
/// # Errors
///
/// The update failed.
pub fn record_use(
    conn: &Connection,
    id: i64,
    passkey_json: Option<&str>,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE passkeys SET passkey_json = COALESCE(?2, passkey_json), \
         last_used_at = MAX(COALESCE(last_used_at, 0), ?3) WHERE id = ?1",
        params![id, passkey_json, now],
    )?;
    Ok(changed > 0)
}

/// Deletes passkey `id` of `user_id`. Returns whether it existed; another
/// account's passkey is left alone, like a missing one.
///
/// # Errors
///
/// The delete failed.
pub fn delete(conn: &Connection, user_id: &str, id: i64) -> Result<bool> {
    let deleted = conn.execute(
        "DELETE FROM passkeys WHERE id = ?1 AND user_id = ?2",
        params![id, user_id],
    )?;
    Ok(deleted > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    fn new<'a>(user_id: &'a str, cred_id: &'a [u8], label: Option<&'a str>) -> NewPasskey<'a> {
        NewPasskey {
            user_id,
            cred_id,
            passkey_json: r#"{"cred":{}}"#,
            label,
        }
    }

    #[test]
    fn passkeys_belong_to_one_account_and_one_credential() {
        let (db, owner, member) = control_with_users();
        let first = db
            .write(|tx| insert(tx, &new(&owner, b"cred-1", Some("Laptop")), NOW))
            .unwrap();
        let second = db
            .write(|tx| insert(tx, &new(&owner, b"cred-2", None), NOW + 1))
            .unwrap();
        db.write(|tx| insert(tx, &new(&member, b"cred-3", None), NOW))
            .unwrap();

        // A credential is registered once, whoever asks.
        for user in [&owner, &member] {
            let err = db
                .write(|tx| insert(tx, &new(user, b"cred-1", None), NOW))
                .unwrap_err();
            assert!(matches!(err, RepoError::Conflict("passkey")), "{err}");
        }

        let rows = db.read(|conn| list(conn, &owner)).unwrap();
        let ids: Vec<i64> = rows.iter().map(|row| row.id).collect();
        assert_eq!(ids, [first, second], "oldest first, own passkeys only");
        assert_eq!(rows[0].label.as_deref(), Some("Laptop"));
        assert_eq!((rows[0].created_at, rows[0].last_used_at), (NOW, None));
        assert!(
            !format!("{:?}", rows[0]).contains("cred"),
            "Debug hides the state"
        );

        let (row, status) = db
            .read(|conn| find_by_credential(conn, b"cred-2"))
            .unwrap()
            .unwrap();
        assert_eq!((row.id, row.user_id.as_str()), (second, owner.as_str()));
        assert_eq!(row.cred_id, b"cred-2");
        assert_eq!(status, Status::Active);
        assert!(
            db.read(|conn| find_by_credential(conn, b"nope"))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn uses_are_recorded_and_deletes_stay_in_the_account() {
        let (db, owner, member) = control_with_users();
        let id = db
            .write(|tx| insert(tx, &new(&owner, b"cred-1", None), NOW))
            .unwrap();

        assert!(db.write(|tx| record_use(tx, id, None, NOW + 5)).unwrap());
        assert!(
            db.write(|tx| record_use(tx, id, Some(r#"{"cred":{"counter":2}}"#), NOW + 9))
                .unwrap()
        );
        // A late, out-of-order use never moves the last use back.
        assert!(db.write(|tx| record_use(tx, id, None, NOW + 1)).unwrap());
        let row = db.read(|conn| list(conn, &owner)).unwrap().remove(0);
        assert_eq!(row.last_used_at, Some(NOW + 9));
        assert_eq!(row.passkey_json, r#"{"cred":{"counter":2}}"#);
        assert!(!db.write(|tx| record_use(tx, id + 1, None, NOW)).unwrap());

        assert!(
            !db.write(|tx| delete(tx, &member, id)).unwrap(),
            "not theirs"
        );
        assert!(db.write(|tx| delete(tx, &owner, id)).unwrap());
        assert!(!db.write(|tx| delete(tx, &owner, id)).unwrap(), "gone");
        assert!(db.read(|conn| list(conn, &owner)).unwrap().is_empty());
    }
}
