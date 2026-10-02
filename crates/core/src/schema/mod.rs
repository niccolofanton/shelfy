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
//! embedded in the binary. The version is stored in `PRAGMA user_version`
//! (the number of applied migrations). Opening a database applies the
//! pending ones with [`upgrade`]; [`migrate`] and [`migrate_to`] apply the
//! same files with `rusqlite_migration`, for tests and fixtures. Migrations
//! are append-only: a released migration never changes, the next change is a
//! new file. Every committed version has a schema fixture under
//! `crates/core/tests/fixtures/schema/`; the schema tests check that the
//! migrations still produce it and that it upgrades to the latest version.
//!
//! # Upgrades and rollbacks (plan §3.8)
//!
//! Opening a database ([`crate::db::UserDb::open`], [`crate::db::ControlDb::open`])
//! runs [`upgrade`]: the server migrates the control database at boot and each
//! library lazily, when it is first opened, plus a low-priority sweep after
//! boot. The result is an [`Upgrade`]. [`upgrade`] reads the version again
//! inside its `BEGIN IMMEDIATE` transaction, so two connections that open the
//! same old file at once (the sweep and a first request, two processes) never
//! apply a migration twice: the second waits for the first and finds nothing
//! left to do.
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
//!
//! # Writes made during a rollback
//!
//! A build that runs on a newer library writes only what it knows about: it
//! does not maintain anything the newer migrations added. Rolling forward
//! does not run those migrations again (the file is already at their
//! version), so data they derive from the user's rows (a backfilled column, a
//! denormalized table, an index over the posts) misses the rows written in
//! between. So that this can be repaired, [`upgrade`] records durably, in the
//! library's `meta` table under [`OLDER_BUILD_META_KEY`], that a build older
//! than the file opened it: `{"buildVersion": N, "since": <unix ms>}`, where
//! `N` is the latest schema version of the oldest such build and `since` the
//! time of the first such open.
//!
//! **Rule for migrations.** A migration `K` that adds derived data must ship
//! an idempotent re-derivation of it, which the builds from `K` on run when
//! they open a library whose record has a `buildVersion` below `K` (and then
//! update the record). The first is library v2 (P1-05): `posts_infix`, the
//! trigram index of the posts' searchable text, whose re-derivation is
//! [`crate::search::index::rebuild_infix`]. Running the re-derivations when
//! the record asks for them is not wired yet: nothing reads the record today,
//! so a rollback across v2 needs `rebuild_infix` run by hand until it is. The
//! control database keeps no derived data and records nothing; a migration
//! that changes that extends the rule to it.

use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};
use rusqlite_migration::{M, Migrations};
use serde::{Deserialize, Serialize};

use crate::db::DbError;

/// `PRAGMA application_id` of a library database: ASCII `SHLB`.
pub const LIBRARY_APPLICATION_ID: i32 = 0x5348_4C42;
/// `PRAGMA application_id` of the control database: ASCII `SHLC`.
pub const CONTROL_APPLICATION_ID: i32 = 0x5348_4C43;

/// One migration: the SQL that brings a database from the previous version
/// to this one. Each must leave no dangling foreign key.
struct Migration {
    sql: &'static str,
    comment: &'static str,
}

const LIBRARY_MIGRATIONS: &[Migration] = &[
    Migration {
        sql: include_str!("../../migrations/library/0001_schema_v1.sql"),
        comment: "library schema v1",
    },
    Migration {
        sql: include_str!("../../migrations/library/0002_search_infix.sql"),
        comment: "library schema v2: posts_infix",
    },
];

const CONTROL_MIGRATIONS: &[Migration] = &[
    Migration {
        sql: include_str!("../../migrations/control/0001_schema_v1.sql"),
        comment: "control schema v1",
    },
    Migration {
        sql: include_str!("../../migrations/control/0002_api_token_expiry.sql"),
        comment: "control schema v2: api_tokens.expires_at",
    },
    Migration {
        sql: include_str!("../../migrations/control/0003_account.sql"),
        comment: "control schema v3: consent and usage parts; passkey ids never reused",
    },
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
    /// The ordered migration set of this kind, for `rusqlite_migration`
    /// ([`migrate`], [`migrate_to`]).
    #[must_use]
    pub fn migrations(self) -> Migrations<'static> {
        Migrations::new(
            self.steps()
                .iter()
                .map(|step| M::up(step.sql).comment(step.comment).foreign_key_check())
                .collect(),
        )
    }

    /// The migrations, in order.
    const fn steps(self) -> &'static [Migration] {
        match self {
            Self::Library => LIBRARY_MIGRATIONS,
            Self::Control => CONTROL_MIGRATIONS,
        }
    }

    /// The schema version a fully migrated database reports in `user_version`.
    #[must_use]
    pub const fn latest_version(self) -> usize {
        self.steps().len()
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
/// its `schema_compat` row says this build is too old for it. A library that
/// is ahead also records that this build opened it (see the module docs,
/// "Writes made during a rollback").
///
/// The version is read once without a lock, which is all a current file
/// costs, and again inside the migration's `BEGIN IMMEDIATE` transaction
/// before anything is applied: two connections opening the same old file
/// (the sweep and a first request, two processes) serialize there, and the
/// second finds nothing left to do. A file another connection migrated in
/// the meantime is [`Upgrade::Current`] for this one.
///
/// # Errors
///
/// [`DbError::SchemaTooNew`] for a file this build must not touch;
/// [`DbError::Migration`] naming the statement that failed (or the foreign
/// key check after it); otherwise SQLite's errors. The file is unchanged on
/// error.
pub fn upgrade(conn: &mut Connection, kind: Kind) -> Result<Upgrade, DbError> {
    let latest = kind.latest_version();
    let mut found = version(conn)?;
    if found < latest {
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        found = version(&tx)?;
        if found < latest {
            apply(&tx, kind.steps(), found)?;
            tx.commit()?;
            return Ok(Upgrade::Upgraded {
                from: found,
                to: latest,
            });
        }
        // Migrated by another connection meanwhile; nothing was written.
        tx.commit()?;
    }
    if found == latest {
        return Ok(Upgrade::Current);
    }
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
    if kind == Kind::Library {
        record_older_build(conn, latest)?;
    }
    Ok(Upgrade::Ahead { found })
}

/// Applies the `steps` after version `from` in `tx`, checking foreign keys
/// after each one (like `M::foreign_key_check`), and sets `user_version` to
/// the number of steps.
fn apply(tx: &Transaction<'_>, steps: &[Migration], from: usize) -> Result<(), DbError> {
    const FOREIGN_KEY_CHECK: &str = "SELECT count(*) FROM pragma_foreign_key_check";
    for (step, version) in steps.iter().zip(1_usize..).skip(from) {
        tx.execute_batch(step.sql)
            .map_err(|err| rusqlite_migration::Error::with_sql(err, step.sql))?;
        let violations: i64 = tx
            .query_row(FOREIGN_KEY_CHECK, [], |row| row.get(0))
            .map_err(|err| rusqlite_migration::Error::with_sql(err, FOREIGN_KEY_CHECK))?;
        if violations > 0 {
            let err = rusqlite::Error::SqliteFailure(
                rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_CONSTRAINT_FOREIGNKEY),
                Some(format!(
                    "{violations} foreign key violations after migration v{version} ({})",
                    step.comment
                )),
            );
            return Err(rusqlite_migration::Error::with_sql(err, FOREIGN_KEY_CHECK).into());
        }
    }
    let latest = i64::try_from(steps.len()).unwrap_or(i64::MAX);
    tx.pragma_update(None, "user_version", latest)?;
    Ok(())
}

/// `meta.key` of the record that a build older than the library's schema
/// opened it (see the module docs, "Writes made during a rollback"). The
/// value is the JSON of an [`OlderBuild`].
pub const OLDER_BUILD_META_KEY: &str = "schema.older_build";

/// The record under [`OLDER_BUILD_META_KEY`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OlderBuild {
    /// The latest schema version of the oldest build that opened the
    /// library while it was ahead of that build.
    pub build_version: usize,
    /// When a build older than the library first opened it, unix ms.
    pub since: i64,
}

/// The [`OlderBuild`] record of a library, if a build older than its schema
/// ever opened it.
///
/// # Errors
///
/// SQLite cannot read the `meta` table.
pub fn older_build(conn: &Connection) -> rusqlite::Result<Option<OlderBuild>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key = ?1",
            [OLDER_BUILD_META_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(value.and_then(|v| serde_json::from_str(&v).ok()))
}

/// Records that this build, whose latest version is `build`, opened a
/// library ahead of it, unless an older or equal build is on record already
/// (then nothing is written).
fn record_older_build(conn: &mut Connection, build: usize) -> Result<(), DbError> {
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let recorded = older_build(&tx)?;
    if recorded.is_some_and(|r| r.build_version <= build) {
        return Ok(());
    }
    let record = OlderBuild {
        build_version: build,
        since: recorded.map_or_else(unix_ms, |r| r.since),
    };
    let value = serde_json::to_string(&record).expect("the record serializes");
    tx.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![OLDER_BUILD_META_KEY, value],
    )?;
    tx.commit()?;
    Ok(())
}

fn unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
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
    fn upgrade_and_migrate_apply_the_same_migrations() {
        for kind in [Kind::Library, Kind::Control] {
            for from in 0..kind.latest_version() {
                let mut upgraded = Connection::open_in_memory().unwrap();
                migrate_to(&mut upgraded, kind, from).unwrap();
                assert_eq!(
                    upgrade(&mut upgraded, kind).unwrap(),
                    Upgrade::Upgraded {
                        from,
                        to: kind.latest_version()
                    }
                );
                let mut migrated = Connection::open_in_memory().unwrap();
                migrate(&mut migrated, kind).unwrap();
                let schema = |conn: &Connection| -> Vec<(String, Option<String>)> {
                    conn.prepare("SELECT name, sql FROM sqlite_schema ORDER BY name")
                        .unwrap()
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                        .unwrap()
                        .collect::<rusqlite::Result<_>>()
                        .unwrap()
                };
                assert_eq!(
                    schema(&upgraded),
                    schema(&migrated),
                    "{} from v{from}",
                    kind.name()
                );
                assert_eq!(version(&upgraded).unwrap(), kind.latest_version());
                let application_id: i32 = upgraded
                    .query_row("PRAGMA application_id", [], |r| r.get(0))
                    .unwrap();
                assert_eq!(application_id, kind.application_id());
            }
        }
    }

    #[test]
    fn a_migration_that_leaves_a_dangling_reference_fails() {
        let steps = [
            Migration {
                sql: "CREATE TABLE parent (id INTEGER PRIMARY KEY);
                      CREATE TABLE child (parent_id INTEGER REFERENCES parent(id));",
                comment: "v1",
            },
            Migration {
                sql: "INSERT INTO child (parent_id) VALUES (7);",
                comment: "v2: a broken backfill",
            },
        ];
        let mut conn = Connection::open_in_memory().unwrap();
        // Without enforcement, as on a connection that turned it off: only
        // the check after the migration sees the broken reference.
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        let tx = conn.transaction().unwrap();
        apply(&tx, &steps[..1], 0).unwrap();
        let err = apply(&tx, &steps, 1).unwrap_err();
        assert!(
            err.to_string()
                .contains("1 foreign key violations after migration v2"),
            "{err}"
        );
    }

    #[test]
    fn application_ids_spell_their_tags() {
        assert_eq!(&LIBRARY_APPLICATION_ID.to_be_bytes(), b"SHLB");
        assert_eq!(&CONTROL_APPLICATION_ID.to_be_bytes(), b"SHLC");
    }
}
