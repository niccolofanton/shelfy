//! Database schemas and their migrations (plan §2.6, §2.7).
//!
//! Two kinds of database share one migration mechanism:
//!
//! - [`Kind::Library`]: `library.sqlite`, one per user, holding the posts and
//!   everything derived from them;
//! - [`Kind::Control`]: `control.sqlite`, one per server, holding users,
//!   sessions, tokens and jobs.
//!
//! Migrations are plain SQL files under `crates/core/migrations/<kind>/`,
//! embedded in the binary and applied with `rusqlite_migration`, which stores
//! the version in `PRAGMA user_version` (number of applied migrations). They are
//! append-only: a released migration never changes, the next change is a new
//! file. Every committed version has a schema fixture under
//! `crates/core/tests/fixtures/schema/`; the schema tests check that the migrations
//! still produce it and that it upgrades to the latest version.

use rusqlite::Connection;
use rusqlite_migration::{M, Migrations};

/// `PRAGMA application_id` of a library database: ASCII `SHLB`.
pub const LIBRARY_APPLICATION_ID: i32 = 0x5348_4C42;
/// `PRAGMA application_id` of the control database: ASCII `SHLC`.
pub const CONTROL_APPLICATION_ID: i32 = 0x5348_4C43;

const LIBRARY_MIGRATIONS: &[M<'static>] =
    &[
        M::up(include_str!("../../migrations/library/0001_schema_v1.sql"))
            .comment("library schema v1")
            .foreign_key_check(),
    ];

const CONTROL_MIGRATIONS: &[M<'static>] = &[
    M::up(include_str!("../../migrations/control/0001_schema_v1.sql"))
        .comment("control schema v1")
        .foreign_key_check(),
    M::up(include_str!(
        "../../migrations/control/0002_api_token_expiry.sql"
    ))
    .comment("control schema v2: api_tokens.expires_at")
    .foreign_key_check(),
];

/// Which database a connection belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    /// A user's `library.sqlite`.
    Library,
    /// The server's `control.sqlite`.
    Control,
}

impl Kind {
    /// The ordered migration set of this kind.
    #[must_use]
    pub const fn migrations(self) -> Migrations<'static> {
        match self {
            Self::Library => Migrations::from_slice(LIBRARY_MIGRATIONS),
            Self::Control => Migrations::from_slice(CONTROL_MIGRATIONS),
        }
    }

    /// The schema version a fully migrated database reports in `user_version`.
    #[must_use]
    pub const fn latest_version(self) -> usize {
        match self {
            Self::Library => LIBRARY_MIGRATIONS.len(),
            Self::Control => CONTROL_MIGRATIONS.len(),
        }
    }

    /// The `PRAGMA application_id` the first migration stamps on the file.
    #[must_use]
    pub const fn application_id(self) -> i32 {
        match self {
            Self::Library => LIBRARY_APPLICATION_ID,
            Self::Control => CONTROL_APPLICATION_ID,
        }
    }

    /// Short name used in errors and logs.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Library => "library",
            Self::Control => "control",
        }
    }
}

/// Brings `conn` to the latest schema version of `kind`, in one transaction.
///
/// A database newer than this build (a downgrade) is refused with
/// `MigrationDefinitionError::DatabaseTooFarAhead`.
///
/// # Errors
///
/// Fails when a migration fails; the database is then left unchanged.
pub fn migrate(conn: &mut Connection, kind: Kind) -> Result<(), rusqlite_migration::Error> {
    kind.migrations().to_latest(conn)
}

/// Brings `conn` to exactly `version` of `kind`. Used by tests and fixture
/// generation to build historical schemas.
///
/// # Errors
///
/// Fails when `version` is beyond the latest one, when a step needs a down
/// migration (none are defined), or when a migration fails.
pub fn migrate_to(
    conn: &mut Connection,
    kind: Kind,
    version: usize,
) -> Result<(), rusqlite_migration::Error> {
    kind.migrations().to_version(conn, version)
}

/// The schema version stored in `PRAGMA user_version`.
///
/// # Errors
///
/// Fails when SQLite cannot read the pragma.
pub fn version(conn: &Connection) -> rusqlite::Result<usize> {
    let v: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    Ok(usize::try_from(v).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_sets_are_valid() {
        Kind::Library
            .migrations()
            .validate()
            .expect("library migrations");
        Kind::Control
            .migrations()
            .validate()
            .expect("control migrations");
    }

    #[test]
    fn application_ids_spell_their_tags() {
        assert_eq!(&LIBRARY_APPLICATION_ID.to_be_bytes(), b"SHLB");
        assert_eq!(&CONTROL_APPLICATION_ID.to_be_bytes(), b"SHLC");
    }
}
