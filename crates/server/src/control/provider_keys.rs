//! Sealed BYOK credentials in control v1. Queries never accept plaintext keys.

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use shelfy_core::repo::Result;

/// The cryptographic payload. Safe to debug: no plaintext or master key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SealedKey {
    pub key_version: u32,
    pub nonce: Vec<u8>,
    pub ciphertext: Vec<u8>,
    pub last4: String,
}

/// One stored key. This is server-internal and deliberately not serializable.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProviderKey {
    pub user_id: String,
    pub provider_id: String,
    pub sealed: SealedKey,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

const COLUMNS: &str =
    "user_id, provider_id, key_version, nonce, ciphertext, last4, created_at, last_used_at";

fn row(row: &Row<'_>) -> rusqlite::Result<ProviderKey> {
    Ok(ProviderKey {
        user_id: row.get(0)?,
        provider_id: row.get(1)?,
        sealed: SealedKey {
            key_version: row.get(2)?,
            nonce: row.get(3)?,
            ciphertext: row.get(4)?,
            last4: row.get(5)?,
        },
        created_at: row.get(6)?,
        last_used_at: row.get(7)?,
    })
}

/// Scoped lookup: a caller cannot read another user's row by provider id.
pub fn get(conn: &Connection, user: &str, provider: &str) -> Result<Option<ProviderKey>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLUMNS} FROM provider_keys WHERE user_id = ?1 AND provider_id = ?2"),
            params![user, provider],
            row,
        )
        .optional()?)
}

/// Inserts or replaces a user's sealed credential. New credentials reset usage.
pub fn put(
    conn: &Connection,
    user: &str,
    provider: &str,
    sealed: &SealedKey,
    now: i64,
) -> Result<()> {
    conn.execute("INSERT INTO provider_keys (user_id, provider_id, key_version, nonce, ciphertext, last4, created_at)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(user_id, provider_id) DO UPDATE SET key_version = excluded.key_version,
        nonce = excluded.nonce, ciphertext = excluded.ciphertext, last4 = excluded.last4,
        created_at = excluded.created_at, last_used_at = NULL",
        params![user, provider, sealed.key_version, sealed.nonce, sealed.ciphertext, sealed.last4, now])?;
    Ok(())
}

/// Deletes only the named user's credential.
pub fn delete(conn: &Connection, user: &str, provider: &str) -> Result<bool> {
    Ok(conn.execute(
        "DELETE FROM provider_keys WHERE user_id = ?1 AND provider_id = ?2",
        params![user, provider],
    )? != 0)
}

/// Throttled usage timestamp; the predicate also holds across concurrent calls.
pub fn touch(conn: &Connection, user: &str, provider: &str, now: i64) -> Result<bool> {
    Ok(conn.execute(
        "UPDATE provider_keys SET last_used_at = ?3 WHERE user_id = ?1 AND provider_id = ?2
        AND (last_used_at IS NULL OR last_used_at <= ?4)",
        params![user, provider, now, now.saturating_sub(60_000)],
    )? != 0)
}

/// Bounded, stable scan for the operator's resumable rotation.
pub(crate) fn batch(conn: &Connection, after: Option<(&str, &str)>) -> Result<Vec<ProviderKey>> {
    let (user, provider) = after.unwrap_or(("", ""));
    let mut query = conn.prepare(&format!(
        "SELECT {COLUMNS} FROM provider_keys
        WHERE (user_id, provider_id) > (?1, ?2) ORDER BY user_id, provider_id LIMIT 100"
    ))?;
    Ok(query
        .query_map(params![user, provider], row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Rotation keeps metadata and skips a credential replaced since the scan.
pub(crate) fn replace_sealed(
    conn: &Connection,
    old: &ProviderKey,
    new: &SealedKey,
) -> Result<bool> {
    Ok(conn.execute("UPDATE provider_keys SET key_version = ?3, nonce = ?4, ciphertext = ?5
        WHERE user_id = ?1 AND provider_id = ?2 AND key_version = ?6 AND nonce = ?7 AND ciphertext = ?8",
        params![old.user_id, old.provider_id, new.key_version, new.nonce, new.ciphertext,
            old.sealed.key_version, old.sealed.nonce, old.sealed.ciphertext])? != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::vault::KeyVault;
    use crate::control::testing::{NOW, control_with_users};
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    use secrecy::SecretString;

    #[test]
    fn user_scope_metadata_and_minute_usage() {
        let (db, owner, member) = control_with_users();
        let vault =
            KeyVault::new(Some(SecretString::from(STANDARD.encode([1; 32]))), None).unwrap();
        let sealed = vault
            .seal(
                &owner,
                "test",
                &SecretString::from("synthetic-key".to_owned()),
            )
            .unwrap();
        db.write(|tx| put(tx, &owner, "test", &sealed, NOW))
            .unwrap();
        db.read(|conn| {
            assert!(get(conn, &member, "test")?.is_none());
            Ok::<_, shelfy_core::repo::RepoError>(())
        })
        .unwrap();
        for (now, expected) in [
            (NOW, true),
            (NOW + 59_999, false),
            (NOW + 60_000, true),
            (NOW - 1, false),
        ] {
            assert_eq!(
                db.write(|tx| touch(tx, &owner, "test", now)).unwrap(),
                expected
            );
        }
        let key = db.read(|c| get(c, &owner, "test")).unwrap().unwrap();
        assert_eq!(key.created_at, NOW);
        assert_eq!(key.last_used_at, Some(NOW + 60_000));
        assert!(!db.write(|tx| delete(tx, &member, "test")).unwrap());
        db.write(|tx| put(tx, &owner, "test", &sealed, NOW + 1))
            .unwrap();
        assert!(
            db.read(|c| get(c, &owner, "test"))
                .unwrap()
                .unwrap()
                .last_used_at
                .is_none()
        );
        assert!(db.write(|tx| delete(tx, &owner, "test")).unwrap());
    }
    #[test]
    fn rotation_never_overwrites_a_concurrent_replacement() {
        let (db, owner, _) = control_with_users();
        let vault =
            KeyVault::new(Some(SecretString::from(STANDARD.encode([1; 32]))), None).unwrap();
        let first = vault
            .seal(&owner, "test", &SecretString::from("synthetic-first"))
            .unwrap();
        db.write(|tx| put(tx, &owner, "test", &first, NOW)).unwrap();
        let old = db.read(|c| get(c, &owner, "test")).unwrap().unwrap();
        let newer = vault
            .seal(&owner, "test", &SecretString::from("synthetic-newer"))
            .unwrap();
        db.write(|tx| put(tx, &owner, "test", &newer, NOW + 1))
            .unwrap();
        assert!(!db.write(|tx| replace_sealed(tx, &old, &first)).unwrap());
        assert_eq!(
            db.read(|c| get(c, &owner, "test")).unwrap().unwrap().sealed,
            newer
        );
    }
}
