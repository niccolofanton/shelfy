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
//!
//! # Upgrades and rollbacks (plan §3.8)
//!
//! Opening a database ([`crate::db::UserDb::open`], [`crate::db::ControlDb::open`])
//! runs [`upgrade`]: the server migrates the control database at boot and each
//! library lazily, when it is first opened, plus a low-priority sweep after
//! boot. The result is an [`Upgrade`].
//!
//! Migrations follow expand/contract: release N only adds (tables, nullable
//! columns, indexes), and what N stops using is dropped in N+1. So release N-1
//! still runs on N's schema, and a rollback is just the previous image: a
//! database ahead of this build opens as is ([`Upgrade::Ahead`]) instead of
//! being refused. A migration that breaks that promise, a contract step that
//! drops something an older build still uses, records the oldest build that
//! can still run on the file in the one-row table `schema_compat`:
//!
//! ```sql
//! CREATE TABLE IF NOT EXISTS schema_compat (min_reader_version INTEGER NOT NULL);
//! DELETE FROM schema_compat;
//! INSERT INTO schema_compat (min_reader_version) VALUES (<N>);  -- N = that build's latest version
//! ```
//!
//! A build whose latest version is below `min_reader_version` refuses the
//! file with [`crate::db::DbError::SchemaTooNew`]. No migration writes the
//! table yet.

use rusqlite::{Connection, OptionalExtension as _};
use rusqlite_migration::{M, Migrations};

use crate::db::DbError;

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
/// `MigrationDefinitionError::DatabaseTooFarAhead`; opening a database goes
/// through [`upgrade`] instead, which accepts one under expand/contract.
///
/// # Errors
///
/// Fails when a migration fails; the database is then left unchanged.
pub fn migrate(conn: &mut Connection, kind: Kind) -> Result<(), rusqlite_migration::Error> {
    kind.migrations().to_latest(conn)
}

/// The table where a contract migration records the oldest build that can
/// still use the file (see the module docs).
pub const COMPAT_TABLE: &str = "schema_compat";

/// What [`upgrade`] found and did.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Upgrade {
    /// The file was already at this build's latest version.
    Current,
    /// The file was migrated from version `from` to `to`, this build's latest.
    /// `from` is 0 for a file that was just created.
    Upgraded {
        /// The version before.
        from: usize,
        /// The version after: [`Kind::latest_version`].
        to: usize,
    },
    /// A newer release already migrated the file to version `found`; this
    /// build runs on it as is (a rollback, plan §3.8).
    Ahead {
        /// The file's version.
        found: usize,
    },
}

/// Brings a database that is being opened up to date: migrates an older file
/// to the latest version in one transaction, and accepts a newer one unless
/// its `schema_compat` row says this build is too old for it.
///
/// The migration transaction starts with `BEGIN IMMEDIATE` when the
/// connection's default transaction behavior is immediate (the writers of
/// [`crate::db`]), so two processes opening the same old file serialize and
/// the second finds nothing left to do.
///
/// # Errors
///
/// [`DbError::SchemaTooNew`] for a file this build must not touch; otherwise
/// the errors of [`migrate`] or SQLite's. The file is unchanged on error.
pub fn upgrade(conn: &mut Connection, kind: Kind) -> Result<Upgrade, DbError> {
    let latest = kind.latest_version();
    let found = version(conn)?;
    if found == latest {
        return Ok(Upgrade::Current);
    }
    if found > latest {
        if let Some(needs) = min_reader_version(conn)?
            && needs > latest
        {
            return Err(DbError::SchemaTooNew {
                kind: kind.name(),
                found,
                needs,
                supported: latest,
            });
        }
        return Ok(Upgrade::Ahead { found });
    }
    migrate(conn, kind)?;
    Ok(Upgrade::Upgraded {
        from: found,
        to: latest,
    })
}

/// The oldest build (its latest schema version) that may use the database,
/// from [`COMPAT_TABLE`]; `None` when no contract migration set one.
///
/// # Errors
///
/// Fails when SQLite cannot read the schema or the table.
pub fn min_reader_version(conn: &Connection) -> rusqlite::Result<Option<usize>> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS (SELECT 1 FROM sqlite_schema WHERE type = 'table' AND name = ?1)",
        [COMPAT_TABLE],
        |row| row.get(0),
    )?;
    if !exists {
        return Ok(None);
    }
    let needs: Option<i64> = conn
        .query_row(
            &format!("SELECT max(min_reader_version) FROM {COMPAT_TABLE}"),
            [],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    Ok(needs.map(|v| usize::try_from(v).unwrap_or(0)))
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
