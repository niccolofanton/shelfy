//! Checks of an uploaded bundle database before anything is installed from
//! it (plan §4.1 step 5, §4.3: the bundle is validated twice, by the CLI and
//! by the server).
//!
//! The file comes from a client, so it is opened read-only with
//! `trusted_schema` off, and it must be exactly a Shelfy library of the
//! current schema: the same tables, indexes and FTS table as a fresh one (no
//! triggers, no views), an intact file, no broken foreign key, and rows within
//! the limits. Every `media_objects` row must name a stored type with its
//! canonical extension and MIME type, so the names the store derives from it
//! are the ones the bundle declares.

use std::collections::BTreeSet;
use std::path::Path;

use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use shelfy_core::schema::{self, Kind};
use shelfy_media::refs::Role;
use shelfy_media::store::IngestLimits;
use shelfy_media::{Digest, MediaKind};

/// Limits of an installable bundle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BundleLimits {
    /// Most posts.
    pub max_posts: u64,
    /// Most `media_objects` rows.
    pub max_objects: u64,
    /// Largest object that is not a video.
    pub max_object_bytes: u64,
    /// Largest video.
    pub max_video_bytes: u64,
}

impl Default for BundleLimits {
    fn default() -> Self {
        Self {
            max_posts: 500_000,
            max_objects: 2_000_000,
            max_object_bytes: IngestLimits::UPLOAD.max_bytes,
            max_video_bytes: IngestLimits::VIDEO.max_bytes,
        }
    }
}

/// One `media_objects` row of the bundle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleObject {
    /// Row id in the bundle (kept by the install).
    pub id: i64,
    pub digest: Digest,
    pub kind: MediaKind,
    pub bytes: u64,
    pub role: Role,
}

/// What the install needs from a valid bundle.
#[derive(Clone, Debug)]
pub struct BundleFacts {
    pub posts: u64,
    pub objects: Vec<BundleObject>,
    /// The CLI's summary of the desktop library (`meta`), or null.
    pub summary: Value,
}

/// Why a bundle is refused: a developer-facing reason, without content.
#[derive(Debug, thiserror::Error)]
pub enum Invalid {
    #[error("{0}")]
    Bundle(String),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}

/// `meta.key` of the CLI's summary.
pub const SUMMARY_META_KEY: &str = "migration.summary";
/// Largest summary kept, in bytes.
const MAX_SUMMARY_BYTES: usize = 64 * 1024;

fn invalid(reason: impl Into<String>) -> Invalid {
    Invalid::Bundle(reason.into())
}

/// Checks the bundle database at `path`.
///
/// # Errors
///
/// [`Invalid::Bundle`] with the first rule the file breaks;
/// [`Invalid::Sqlite`] when it cannot be read at all.
pub fn validate(path: &Path, limits: &BundleLimits) -> Result<BundleFacts, Invalid> {
    validate_with_origin(path, limits, false)
}

/// Checks a v2 export snapshot through the migration validator. The schema,
/// integrity, foreign keys, types, roles and byte caps are identical; exports
/// also carry native CAS origins and unreferenced masters for lossless recovery.
///
/// # Errors
/// The same validation failures as [`validate`].
pub fn validate_export(path: &Path, limits: &BundleLimits) -> Result<BundleFacts, Invalid> {
    validate_with_origin(path, limits, true)
}

fn validate_with_origin(
    path: &Path,
    limits: &BundleLimits,
    export: bool,
) -> Result<BundleFacts, Invalid> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.execute_batch("PRAGMA trusted_schema = OFF; PRAGMA query_only = ON;")?;
    let pragma = |name: &str| -> Result<i64, Invalid> {
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))
            .map_err(|_| invalid("not an SQLite database"))
    };
    if pragma("application_id")? != i64::from(Kind::Library.application_id()) {
        return Err(invalid("not a Shelfy library"));
    }
    let latest = i64::try_from(Kind::Library.latest_version()).unwrap_or(i64::MAX);
    let version = pragma("user_version")?;
    if version != latest {
        return Err(invalid(format!(
            "library schema v{version}, the server installs v{latest}"
        )));
    }
    let integrity: String = conn.query_row("PRAGMA integrity_check", [], |r| r.get(0))?;
    if integrity != "ok" {
        return Err(invalid("the database fails its integrity check"));
    }
    if schema_rows(&conn)? != expected_schema()? {
        return Err(invalid(
            "the database's tables, indexes or triggers differ from a Shelfy library",
        ));
    }
    let broken: i64 = conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
        r.get(0)
    })?;
    if broken > 0 {
        return Err(invalid(format!("{broken} rows break a foreign key")));
    }

    let count = |table: &str| -> Result<u64, Invalid> {
        let n: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    let posts = count("posts")?;
    if posts > limits.max_posts {
        return Err(invalid(format!(
            "{posts} posts, over the limit of {}",
            limits.max_posts
        )));
    }
    let object_rows = count("media_objects")?;
    if object_rows > limits.max_objects {
        return Err(invalid(format!(
            "{object_rows} objects, over the limit of {}",
            limits.max_objects
        )));
    }

    let mut objects = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT id, sha256, ext, mime, bytes, role, origin, unreferenced_since
         FROM media_objects ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let id: i64 = row.get(0)?;
        let sha: Vec<u8> = row.get(1)?;
        let ext: String = row.get(2)?;
        let mime: String = row.get(3)?;
        let bytes: i64 = row.get(4)?;
        let role: String = row.get(5)?;
        let origin: String = row.get(6)?;
        let unreferenced: Option<i64> = row.get(7)?;
        let digest = Digest::from_slice(&sha)
            .ok_or_else(|| invalid(format!("object {id}: sha256 is not 32 bytes")))?;
        let kind = MediaKind::from_ext(&ext)
            .filter(|k| k.mime() == mime)
            .ok_or_else(|| invalid(format!("object {id}: not a stored type")))?;
        let max = if kind.is_video() {
            limits.max_video_bytes
        } else {
            limits.max_object_bytes
        };
        let bytes = u64::try_from(bytes)
            .ok()
            .filter(|&b| (1..=max).contains(&b))
            .ok_or_else(|| invalid(format!("object {id}: size out of range")))?;
        let role =
            parse_role(&role).ok_or_else(|| invalid(format!("object {id}: unknown role")))?;
        let origin_valid = if export {
            matches!(
                origin.as_str(),
                "migration" | "server" | "extension" | "upload" | "capture"
            )
        } else {
            origin == "migration" && unreferenced.is_none()
        };
        if !origin_valid {
            return Err(invalid(format!(
                "object {id}: not a migrated, referenced object"
            )));
        }
        objects.push(BundleObject {
            id,
            digest,
            kind,
            bytes,
            role,
        });
    }

    let summary = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [SUMMARY_META_KEY],
            |r| r.get::<_, String>(0),
        )
        .ok()
        .filter(|raw| raw.len() <= MAX_SUMMARY_BYTES)
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .filter(Value::is_object)
        .unwrap_or(Value::Null);
    Ok(BundleFacts {
        posts,
        objects,
        summary,
    })
}

/// A `media_objects.role` value (plan §2.7).
#[must_use]
pub fn parse_role(role: &str) -> Option<Role> {
    [
        Role::Image,
        Role::Poster,
        Role::Video,
        Role::File,
        Role::Preview,
        Role::Screenshot,
        Role::Band,
        Role::Section,
        Role::Footer,
        Role::Filmstrip,
        Role::Favicon,
        Role::Og,
    ]
    .into_iter()
    .find(|r| r.as_str() == role)
}

type SchemaRow = (String, String, String, Option<String>);

/// Every schema object but SQLite's statistics tables.
fn schema_rows(conn: &Connection) -> rusqlite::Result<BTreeSet<SchemaRow>> {
    conn.prepare(
        "SELECT type, name, tbl_name, sql FROM sqlite_schema WHERE name NOT LIKE 'sqlite_stat%'",
    )?
    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
    .collect()
}

/// The schema of a fresh library.
fn expected_schema() -> Result<BTreeSet<SchemaRow>, Invalid> {
    let mut fresh = Connection::open_in_memory()?;
    schema::migrate(&mut fresh, Kind::Library)
        .map_err(|e| invalid(format!("cannot build the reference schema: {e}")))?;
    Ok(schema_rows(&fresh)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn library(dir: &Path) -> std::path::PathBuf {
        let path = dir.join("library.sqlite");
        let mut conn = Connection::open(&path).unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        path
    }

    fn object(conn: &Connection, sha: &[u8], ext: &str, mime: &str, origin: &str) {
        conn.execute(
            "INSERT INTO media_objects (sha256, ext, mime, bytes, role, origin, created_at)
             VALUES (?1, ?2, ?3, 10, 'image', ?4, 0)",
            rusqlite::params![sha, ext, mime, origin],
        )
        .unwrap();
    }

    #[test]
    fn a_fresh_library_with_objects_passes() {
        let dir = tempfile::tempdir().unwrap();
        let path = library(dir.path());
        let conn = Connection::open(&path).unwrap();
        object(&conn, &[7; 32], "jpg", "image/jpeg", "migration");
        conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, '{\"posts\":{}}')",
            [SUMMARY_META_KEY],
        )
        .unwrap();
        drop(conn);
        let facts = validate(&path, &BundleLimits::default()).unwrap();
        assert_eq!(facts.posts, 0);
        assert_eq!(facts.objects.len(), 1);
        assert_eq!(facts.objects[0].kind, MediaKind::Jpeg);
        assert_eq!(facts.objects[0].digest, Digest::from_bytes([7; 32]));
        assert!(facts.summary.is_object());
    }

    #[test]
    fn export_policy_accepts_native_unreferenced_objects_but_keeps_type_and_origin_checks() {
        for origin in ["migration", "server", "extension", "upload", "capture"] {
            let dir = tempfile::tempdir().unwrap();
            let path = library(dir.path());
            let conn = Connection::open(&path).unwrap();
            object(&conn, &[1; 32], "jpg", "image/jpeg", origin);
            conn.execute("UPDATE media_objects SET unreferenced_since=1", [])
                .unwrap();
            assert!(validate(&path, &BundleLimits::default()).is_err());
            assert_eq!(
                validate_export(&path, &BundleLimits::default())
                    .unwrap()
                    .objects
                    .len(),
                1
            );
            conn.execute("UPDATE media_objects SET origin='outside-store'", [])
                .unwrap();
            assert!(validate_export(&path, &BundleLimits::default()).is_err());
            conn.execute(
                "UPDATE media_objects SET origin='server',mime='text/html'",
                [],
            )
            .unwrap();
            assert!(validate_export(&path, &BundleLimits::default()).is_err());
        }
    }

    #[test]
    fn foreign_files_and_tampered_schemas_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let other = dir.path().join("other.sqlite");
        Connection::open(&other)
            .unwrap()
            .execute_batch("CREATE TABLE t (x);")
            .unwrap();
        let err = validate(&other, &BundleLimits::default()).unwrap_err();
        assert!(err.to_string().contains("not a Shelfy library"), "{err}");

        let path = library(dir.path());
        Connection::open(&path)
            .unwrap()
            .execute_batch(
                "CREATE TRIGGER sneaky AFTER INSERT ON posts BEGIN DELETE FROM posts; END;",
            )
            .unwrap();
        let err = validate(&path, &BundleLimits::default()).unwrap_err();
        assert!(err.to_string().contains("differ"), "{err}");
    }

    #[test]
    fn objects_must_be_stored_types_of_the_migration() {
        for (ext, mime, origin) in [
            ("jpeg", "image/jpeg", "migration"),
            ("jpg", "image/png", "migration"),
            ("svg", "image/svg+xml", "migration"),
            ("jpg", "image/jpeg", "server"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = library(dir.path());
            object(
                &Connection::open(&path).unwrap(),
                &[1; 32],
                ext,
                mime,
                origin,
            );
            assert!(
                validate(&path, &BundleLimits::default()).is_err(),
                "{ext} {mime} {origin}"
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let path = library(dir.path());
        object(
            &Connection::open(&path).unwrap(),
            &[1; 32],
            "jpg",
            "image/jpeg",
            "migration",
        );
        let tight = BundleLimits {
            max_objects: 0,
            ..BundleLimits::default()
        };
        assert!(validate(&path, &tight).is_err());
    }
}
