//! Repositories over `library.sqlite` (plan §2.7): plain functions that take a
//! connection and return domain structs.
//!
//! Read functions take any `&Connection`; call them inside [`UserDb::read`] so
//! multi-query reads share one snapshot. Write functions take the transaction of
//! [`UserDb::write`] (a `&Transaction` derefs to `&Connection`), keep derived
//! data (tag rows, the FTS index) consistent in that same transaction, and take
//! the current time (`now`, unix ms) as an argument so tests stay deterministic.
//!
//! The semantics mirror the desktop's `electron/db.ts` (filters, sort, stats,
//! collections) except where the plan fixes a defect; the deviations are noted on
//! each function.
//!
//! [`UserDb::read`]: crate::db::UserDb::read
//! [`UserDb::write`]: crate::db::UserDb::write

pub mod collections;
pub mod media;
pub mod notifications;
pub mod posts;
pub mod settings;
pub mod stats;
pub mod sync;
pub(crate) mod tags;

use std::fmt;
use std::str::FromStr;

use rusqlite::types::{FromSql, FromSqlError, FromSqlResult, ToSql, ToSqlOutput, ValueRef};
use rusqlite::{ErrorCode, Row};
use serde::{Deserialize, Serialize};

use crate::db::DbError;

/// Errors of repository functions.
#[derive(Debug, thiserror::Error)]
pub enum RepoError {
    /// The addressed row does not exist.
    #[error("not found")]
    NotFound,
    /// A uniqueness rule refused the write (the payload names what clashed).
    #[error("conflict: {0}")]
    Conflict(&'static str),
    /// An argument failed validation.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// The offending field.
        field: &'static str,
        /// Why it was refused.
        reason: &'static str,
    },
    /// A pagination cursor that this query did not produce.
    #[error("invalid cursor")]
    InvalidCursor,
    /// The database failed.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl From<rusqlite::Error> for RepoError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Db(DbError::from(e))
    }
}

/// Result of repository functions.
pub type Result<T, E = RepoError> = std::result::Result<T, E>;

/// Maps a UNIQUE or PRIMARY KEY violation to [`RepoError::Conflict`].
fn conflict_on_unique(e: rusqlite::Error, what: &'static str) -> RepoError {
    match e.sqlite_error() {
        Some(err)
            if err.code == ErrorCode::ConstraintViolation
                && matches!(
                    err.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                ) =>
        {
            RepoError::Conflict(what)
        }
        _ => e.into(),
    }
}

/// Source platform of a post (`posts.platform`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    /// Instagram.
    Instagram,
    /// X (stored as `twitter`, as on the desktop).
    Twitter,
    /// Pinterest.
    Pinterest,
    /// A captured website.
    Web,
    /// A manual bookmark (upload or link).
    Manual,
}

impl Platform {
    /// Every platform, in a stable order.
    pub const ALL: [Self; 5] = [
        Self::Instagram,
        Self::Twitter,
        Self::Pinterest,
        Self::Web,
        Self::Manual,
    ];

    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::Twitter => "twitter",
            Self::Pinterest => "pinterest",
            Self::Web => "web",
            Self::Manual => "manual",
        }
    }
}

impl fmt::Display for Platform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Error of [`Platform::from_str`].
#[derive(Debug, thiserror::Error)]
#[error("unknown platform")]
pub struct UnknownPlatform;

impl FromStr for Platform {
    type Err = UnknownPlatform;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|p| p.as_str() == s)
            .ok_or(UnknownPlatform)
    }
}

impl ToSql for Platform {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(ToSqlOutput::from(self.as_str()))
    }
}

impl FromSql for Platform {
    fn column_result(value: ValueRef<'_>) -> FromSqlResult<Self> {
        value
            .as_str()?
            .parse()
            .map_err(|e| FromSqlError::Other(Box::new(e)))
    }
}

/// A stored media object, as the API needs it to build `/media/…` URLs.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    /// Lowercase hex SHA-256 of the content (the object's file name).
    pub sha256: String,
    /// File extension, from the allowlist.
    pub ext: String,
    /// MIME type.
    pub mime: String,
    /// Size in bytes.
    pub bytes: i64,
    /// Pixel width, when known.
    pub width: Option<i64>,
    /// Pixel height, when known.
    pub height: Option<i64>,
    /// Duration of audio/video, when known.
    pub duration_ms: Option<i64>,
    /// Whether the 480 px WebP rendition exists (`variants & 1`).
    pub has_g480: bool,
}

/// Columns selecting an object for [`object_ref_at`], with `alias` as the
/// `media_objects` table alias.
pub(crate) fn object_columns(alias: &str) -> String {
    format!(
        "{a}.sha256, {a}.ext, {a}.mime, {a}.bytes, {a}.width, {a}.height, {a}.duration_ms, {a}.variants",
        a = alias
    )
}

/// Reads the eight columns of [`object_columns`] starting at `start`; `None`
/// when the join found no object.
pub(crate) fn object_ref_at(row: &Row<'_>, start: usize) -> rusqlite::Result<Option<ObjectRef>> {
    let Some(sha) = row.get::<_, Option<Vec<u8>>>(start)? else {
        return Ok(None);
    };
    Ok(Some(ObjectRef {
        sha256: hex(&sha),
        ext: row.get(start + 1)?,
        mime: row.get(start + 2)?,
        bytes: row.get(start + 3)?,
        width: row.get(start + 4)?,
        height: row.get(start + 5)?,
        duration_ms: row.get(start + 6)?,
        has_g480: row.get::<_, i64>(start + 7)? & 1 == 1,
    }))
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        s.push(char::from(DIGITS[usize::from(b >> 4)]));
        s.push(char::from(DIGITS[usize::from(b & 0xf)]));
    }
    s
}

/// The string items of a JSON array column; anything else reads as empty (the
/// desktop's defensive `parseTags`).
pub(crate) fn json_strings(raw: Option<&str>) -> Vec<String> {
    match raw.map(serde_json::from_str::<serde_json::Value>) {
        Some(Ok(serde_json::Value::Array(items))) => items
            .into_iter()
            .filter_map(|v| match v {
                serde_json::Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// A JSON array column value; `NULL` for an empty list.
fn json_array_or_null(items: &[String]) -> Option<String> {
    (!items.is_empty()).then(|| serde_json::to_string(items).expect("strings serialize"))
}

/// Parses an optional JSON column, dropping invalid JSON.
pub(crate) fn json_value(raw: Option<&str>) -> Option<serde_json::Value> {
    raw.and_then(|s| serde_json::from_str(s).ok())
}

/// A JSON array of ids, the parameter of `IN (SELECT value FROM json_each(?))`.
pub(crate) fn id_list(ids: &[i64]) -> String {
    serde_json::to_string(ids).expect("integers serialize")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_round_trips() {
        for p in Platform::ALL {
            assert_eq!(p.as_str().parse::<Platform>().unwrap(), p);
        }
        assert!("tiktok".parse::<Platform>().is_err());
    }

    #[test]
    fn hex_is_lowercase() {
        assert_eq!(hex(&[0x00, 0xab, 0x0f]), "00ab0f");
    }
}
