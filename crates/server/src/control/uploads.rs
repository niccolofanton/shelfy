//! `uploads` (plan §2.6, §2.9): resumable tus uploads in progress and done.
//!
//! A row tracks one upload: its declared `length`, the bytes received so far
//! (`upload_offset`) and, once every byte arrived and was checked, its
//! `completed_at`. The bytes live in `<data>/work/uploads/` ([`file_path`]).
//! `meta_json` holds what the client declared at creation (the SHA-256 of
//! the content and, for a media object, its type), which the server checks
//! when the upload completes.
//!
//! T9 uses two purposes, both for the desktop migration: a media object of
//! the bundle and the bundle's database. Bookmarks and imports (P4) add their
//! own purposes; the expiry sweep (P4 GC) removes rows and files past
//! `expires_at` for every user, where T9 only sweeps the requesting user's.

use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use serde::{Deserialize, Serialize};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;

/// What an upload is for (`uploads.purpose`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum UploadPurpose {
    /// One media object of a migration bundle.
    MigrationObject,
    /// The database of a migration bundle.
    MigrationDb,
}

impl UploadPurpose {
    /// The stored value, also the `purpose` of the tus `Upload-Metadata`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MigrationObject => "migration-object",
            Self::MigrationDb => "migration-db",
        }
    }

    /// The purpose stored as `value`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "migration-object" => Some(Self::MigrationObject),
            "migration-db" => Some(Self::MigrationDb),
            _ => None,
        }
    }
}

/// What the client declared at creation (`uploads.meta_json`).
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UploadMeta {
    /// SHA-256 of the whole content, lowercase hex.
    pub sha256: String,
    /// File extension of a media object, from the store's allowlist.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ext: Option<String>,
}

/// An upload to create.
#[derive(Clone, Debug)]
pub struct NewUpload<'a> {
    /// Upload id (ULID).
    pub id: &'a str,
    /// The user it belongs to.
    pub user_id: &'a str,
    /// What it is for.
    pub purpose: UploadPurpose,
    /// Declared length in bytes.
    pub length: i64,
    /// What the client declared.
    pub meta: &'a UploadMeta,
    /// When an unfinished upload may be removed, unix ms.
    pub expires_at: i64,
}

/// A stored upload.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Upload {
    /// Upload id (ULID).
    pub id: String,
    /// The user it belongs to.
    pub user_id: String,
    /// What it is for; `None` for a purpose this build does not know.
    pub purpose: Option<UploadPurpose>,
    /// Declared length in bytes.
    pub length: i64,
    /// Bytes received so far.
    pub offset: i64,
    /// What the client declared.
    pub meta: UploadMeta,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// When it may be removed if unfinished, unix ms.
    pub expires_at: i64,
    /// When every byte arrived and was checked, unix ms.
    pub completed_at: Option<i64>,
}

impl Upload {
    /// Whether every byte arrived and was checked.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.completed_at.is_some()
    }
}

/// Where the bytes of upload `id` are: `<id>.part` while it is in progress,
/// `<id>` once complete. `uploads_dir` is `<data>/work/uploads`.
#[must_use]
pub fn file_path(uploads_dir: &Path, id: &str, complete: bool) -> PathBuf {
    if complete {
        uploads_dir.join(id)
    } else {
        uploads_dir.join(format!("{id}.part"))
    }
}

/// Stores a new upload at offset 0.
///
/// # Errors
///
/// [`RepoError::Conflict`] when the id is taken; otherwise the insert failed.
pub fn insert(conn: &Connection, upload: &NewUpload<'_>, now: i64) -> Result<()> {
    let meta = serde_json::to_string(upload.meta).expect("upload metadata serializes");
    conn.execute(
        "INSERT INTO uploads (id, user_id, purpose, length, upload_offset, meta_json, created_at, \
         expires_at) VALUES (?1, ?2, ?3, ?4, 0, ?5, ?6, ?7)",
        params![
            upload.id,
            upload.user_id,
            upload.purpose.as_str(),
            upload.length,
            meta,
            now,
            upload.expires_at,
        ],
    )
    .map_err(|e| conflict_on_unique(e, "upload"))?;
    Ok(())
}

/// The upload `id` of `user_id`, if it exists. Another user's upload reads
/// as missing.
///
/// # Errors
///
/// The query failed.
pub fn get(conn: &Connection, user_id: &str, id: &str) -> Result<Option<Upload>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM uploads WHERE id = ?1 AND user_id = ?2"),
        params![id, user_id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// Moves the offset of `id` from `from` to `to`. Returns false when the
/// offset was not `from` any more (another request moved it).
///
/// # Errors
///
/// The update failed.
pub fn advance(conn: &Connection, id: &str, from: i64, to: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE uploads SET upload_offset = ?3 WHERE id = ?1 AND upload_offset = ?2 \
         AND completed_at IS NULL",
        params![id, from, to],
    )?;
    Ok(changed == 1)
}

/// Marks `id` complete at `now`. Returns whether it was in progress.
///
/// # Errors
///
/// The update failed.
pub fn complete(conn: &Connection, id: &str, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE uploads SET completed_at = ?2, upload_offset = length \
         WHERE id = ?1 AND completed_at IS NULL",
        params![id, now],
    )?;
    Ok(changed == 1)
}

/// Deletes the rows `ids`; returns how many existed. The caller removes
/// their files.
///
/// # Errors
///
/// The delete failed.
pub fn delete(conn: &Connection, ids: &[String]) -> Result<usize> {
    let ids = serde_json::to_string(ids).expect("ids serialize");
    Ok(conn.execute(
        "DELETE FROM uploads WHERE id IN (SELECT value FROM json_each(?1))",
        [ids],
    )?)
}

/// Unfinished uploads of `user_id` that have not expired at `now`.
///
/// # Errors
///
/// The query failed.
pub fn count_unfinished(conn: &Connection, user_id: &str, now: i64) -> Result<u64> {
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM uploads WHERE user_id = ?1 AND completed_at IS NULL \
         AND expires_at > ?2",
        params![user_id, now],
        |r| r.get(0),
    )?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Unfinished uploads of `user_id` past their expiry at `now`: the sweep's
/// work list.
///
/// # Errors
///
/// The query failed.
pub fn expired(conn: &Connection, user_id: &str, now: i64) -> Result<Vec<String>> {
    let ids = conn
        .prepare(
            "SELECT id FROM uploads WHERE user_id = ?1 AND completed_at IS NULL \
             AND expires_at <= ?2 ORDER BY id",
        )?
        .query_map(params![user_id, now], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// The complete uploads of `user_id` with `purpose` whose declared SHA-256 is
/// one of `hashes`: `(sha256, upload)` pairs, oldest upload first.
///
/// # Errors
///
/// The query failed.
pub fn complete_by_sha256(
    conn: &Connection,
    user_id: &str,
    purpose: UploadPurpose,
    hashes: &[String],
) -> Result<Vec<Upload>> {
    if hashes.is_empty() {
        return Ok(Vec::new());
    }
    let hashes = serde_json::to_string(hashes).expect("hashes serialize");
    let rows = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM uploads
             WHERE user_id = ?1 AND purpose = ?2 AND completed_at IS NOT NULL
               AND json_extract(meta_json, '$.sha256') IN (SELECT value FROM json_each(?3))
             ORDER BY created_at, id"
        ))?
        .query_map(params![user_id, purpose.as_str(), hashes], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// Every complete upload of `user_id` with `purpose`, oldest first.
///
/// # Errors
///
/// The query failed.
pub fn complete_of(
    conn: &Connection,
    user_id: &str,
    purpose: UploadPurpose,
) -> Result<Vec<Upload>> {
    let rows = conn
        .prepare(&format!(
            "SELECT {COLUMNS} FROM uploads
             WHERE user_id = ?1 AND purpose = ?2 AND completed_at IS NOT NULL
             ORDER BY created_at, id"
        ))?
        .query_map(params![user_id, purpose.as_str()], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

const COLUMNS: &str =
    "id, user_id, purpose, length, upload_offset, meta_json, created_at, expires_at, completed_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<Upload> {
    let meta: Option<String> = row.get(5)?;
    Ok(Upload {
        id: row.get(0)?,
        user_id: row.get(1)?,
        purpose: UploadPurpose::parse(&row.get::<_, String>(2)?),
        length: row.get(3)?,
        offset: row.get(4)?,
        meta: meta
            .and_then(|m| serde_json::from_str(&m).ok())
            .unwrap_or_default(),
        created_at: row.get(6)?,
        expires_at: row.get(7)?,
        completed_at: row.get(8)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    const SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

    #[test]
    fn an_upload_moves_forward_completes_and_is_found_by_hash() {
        let (db, owner, member) = control_with_users();
        let meta = UploadMeta {
            sha256: SHA.into(),
            ext: Some("jpg".into()),
        };
        let new = NewUpload {
            id: "U1",
            user_id: &owner,
            purpose: UploadPurpose::MigrationObject,
            length: 10,
            meta: &meta,
            expires_at: NOW + 1_000,
        };
        db.write(|tx| insert(tx, &new, NOW)).unwrap();
        let get_as = |user: &str| db.read(|c| get(c, user, "U1")).unwrap();
        assert_eq!(get_as(&member), None, "another user's upload is missing");
        let upload = get_as(&owner).unwrap();
        assert_eq!(
            (upload.offset, upload.length, upload.is_complete()),
            (0, 10, false)
        );
        assert_eq!(upload.meta, meta);
        assert_eq!(upload.purpose, Some(UploadPurpose::MigrationObject));

        assert!(db.write(|tx| advance(tx, "U1", 0, 6)).unwrap());
        assert!(
            !db.write(|tx| advance(tx, "U1", 0, 6)).unwrap(),
            "stale offset"
        );
        let hashes = vec![SHA.to_owned()];
        let found = |user: &str| {
            db.read(|c| complete_by_sha256(c, user, UploadPurpose::MigrationObject, &hashes))
                .unwrap()
        };
        assert!(found(&owner).is_empty(), "not complete yet");
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 1);

        assert!(db.write(|tx| complete(tx, "U1", NOW)).unwrap());
        let done = get_as(&owner).unwrap();
        assert_eq!((done.offset, done.completed_at), (10, Some(NOW)));
        assert_eq!(found(&owner).len(), 1);
        assert!(found(&member).is_empty());
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 0);
        assert_eq!(db.write(|tx| delete(tx, &["U1".to_owned()])).unwrap(), 1);
    }

    #[test]
    fn unfinished_uploads_expire() {
        let (db, owner, _) = control_with_users();
        let meta = UploadMeta {
            sha256: SHA.into(),
            ext: None,
        };
        for (id, expires_at) in [("U1", NOW - 1), ("U2", NOW + 1)] {
            let new = NewUpload {
                id,
                user_id: &owner,
                purpose: UploadPurpose::MigrationDb,
                length: 10,
                meta: &meta,
                expires_at,
            };
            db.write(|tx| insert(tx, &new, NOW)).unwrap();
        }
        assert_eq!(db.read(|c| expired(c, &owner, NOW)).unwrap(), ["U1"]);
        assert_eq!(db.read(|c| count_unfinished(c, &owner, NOW)).unwrap(), 1);
    }

    #[test]
    fn files_are_named_by_id_and_state() {
        let dir = Path::new("/data/work/uploads");
        assert_eq!(file_path(dir, "U1", false), dir.join("U1.part"));
        assert_eq!(file_path(dir, "U1", true), dir.join("U1"));
    }
}
