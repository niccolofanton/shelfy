//! `feature_flags` (plan §2.6, §3.9): server-wide switches the operator sets
//! with `shelfy-server admin flags` (E4: no `/admin` page), such as the
//! extension's kill switches (P2-03).
//!
//! A row is a key and its value as JSON. Only the keys of a typed registry
//! are written ([`crate::extension::flags`] validates them), and readers
//! fall back to the registry's default when a row is missing or does not
//! fit its type, so a hand-edited row can never break a route.

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

/// A stored flag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FlagRow {
    /// The key, such as `extension.instagram.replay`.
    pub key: String,
    /// The value, as JSON text.
    pub value_json: String,
    /// When it was last set, unix ms.
    pub updated_at: i64,
}

/// Every stored flag, by key.
///
/// # Errors
///
/// The query failed.
pub fn list(conn: &Connection) -> Result<Vec<FlagRow>> {
    let mut statement =
        conn.prepare_cached("SELECT key, value_json, updated_at FROM feature_flags ORDER BY key")?;
    let rows = statement
        .query_map([], |row| {
            Ok(FlagRow {
                key: row.get(0)?,
                value_json: row.get(1)?,
                updated_at: row.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The stored flag `key`, if set.
///
/// # Errors
///
/// The query failed.
pub fn get(conn: &Connection, key: &str) -> Result<Option<FlagRow>> {
    conn.query_row(
        "SELECT key, value_json, updated_at FROM feature_flags WHERE key = ?1",
        [key],
        |row| {
            Ok(FlagRow {
                key: row.get(0)?,
                value_json: row.get(1)?,
                updated_at: row.get(2)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Sets `key` to `value_json` at `now`, replacing any earlier value.
///
/// # Errors
///
/// The write failed.
pub fn set(conn: &Connection, key: &str, value_json: &str, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO feature_flags (key, value_json, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO UPDATE SET value_json = excluded.value_json, \
         updated_at = excluded.updated_at",
        params![key, value_json, now],
    )?;
    Ok(())
}

/// Removes `key`, so its default applies again; returns whether it was set.
///
/// # Errors
///
/// The delete failed.
pub fn unset(conn: &Connection, key: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM feature_flags WHERE key = ?1", [key])? > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    #[test]
    fn flags_are_set_replaced_listed_and_unset() {
        let (db, _owner, _member) = control_with_users();
        assert_eq!(db.read(list).unwrap(), Vec::new());
        db.write(|tx| {
            set(tx, "extension.twitter.scroll", "false", NOW)?;
            set(tx, "extension.minVersion", "\"0.2.0\"", NOW)?;
            set(tx, "extension.twitter.scroll", "true", NOW + 5)
        })
        .unwrap();
        let rows = db.read(list).unwrap();
        let keys: Vec<&str> = rows.iter().map(|row| row.key.as_str()).collect();
        assert_eq!(keys, ["extension.minVersion", "extension.twitter.scroll"]);
        assert_eq!(
            db.read(|conn| get(conn, "extension.twitter.scroll"))
                .unwrap(),
            Some(FlagRow {
                key: "extension.twitter.scroll".into(),
                value_json: "true".into(),
                updated_at: NOW + 5,
            })
        );
        assert!(
            db.write(|tx| unset(tx, "extension.twitter.scroll"))
                .unwrap()
        );
        assert!(
            !db.write(|tx| unset(tx, "extension.twitter.scroll"))
                .unwrap()
        );
        assert_eq!(
            db.read(|conn| get(conn, "extension.twitter.scroll"))
                .unwrap(),
            None
        );
    }
}
