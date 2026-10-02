//! `admin verify`: checks a directory of database copies, as `admin snapshot`
//! writes them and `restic restore` brings them back, against the live data
//! (plan §3.5: the monthly restore drill, and the end of a full restore).
//!
//! For every copy (`<dir>/control.sqlite` and `<dir>/users/<user_id>.sqlite`):
//!
//! - **integrity:** `PRAGMA integrity_check` (the full check, FTS indexes
//!   included) and `PRAGMA foreign_key_check`;
//! - **schema:** a Shelfy database of the right kind, at a version this build
//!   runs on (an older one is upgraded when it is next opened);
//! - **row counts** compared with the live database, table by table: a
//!   difference larger than `--max-drift` percent of the larger count fails.
//!   The default, 0, wants equal counts (right after `install-snapshots`);
//!   the drill passes 10, since the live data moved on after the snapshot.
//!   Volatile tables (sessions, jobs, the audit log, notifications, caches)
//!   change by the minute: they are reported, never compared;
//! - **media:** every object that a library's posts and captures reference
//!   resolves to a file of the recorded size in the live store
//!   (`users/<id>/media/`), with its `g480` rendition when it has one.
//!
//! The command exits with status 1 when anything failed. A copy without a
//! live database (a user deleted since the snapshot, a host not restored yet)
//! is checked on its own and noted.

use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use rusqlite::{Connection, OpenFlags};
use shelfy_core::db::is_valid_user_id;
use shelfy_core::schema::{self, Kind};
use shelfy_media::refs::REFERENCES;
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, MediaKind, Rendition, Variants};

use crate::config::{CONTROL_DB_FILE, DataDir};

/// Control-database tables whose rows change by the minute.
pub const VOLATILE_CONTROL_TABLES: &[&str] = &[
    "audit_log",
    "idempotency",
    "jobs",
    "magic_links",
    "pairing_codes",
    "queue_state",
    "sessions",
    "uploads",
    "usage_daily",
];

/// Library tables whose rows change by the minute.
pub const VOLATILE_LIBRARY_TABLES: &[&str] = &["ai_cache", "notifications", "sync_runs"];

/// How many problems of one kind a report lists before it summarizes.
const LISTED: usize = 5;

/// Arguments of `admin verify`.
#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Directory with `control.sqlite` and `users/<user_id>.sqlite`, as
    /// `admin snapshot` writes it.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// Check only this user's library (repeatable). Default: every library in
    /// DIR.
    #[arg(long = "user", value_name = "USER_ID")]
    pub users: Vec<String>,

    /// Largest accepted difference between a table's row count in a copy and
    /// in the live database, in percent of the larger count. 0 wants equal
    /// counts.
    #[arg(
        long,
        value_name = "PERCENT",
        default_value_t = 0,
        value_parser = clap::value_parser!(u8).range(0..=100)
    )]
    pub max_drift: u8,
}

/// What to check.
#[derive(Clone, Copy, Debug, Default)]
pub struct VerifyOptions<'a> {
    /// Only these users' libraries; every library in the directory when empty.
    pub users: &'a [String],
    /// See [`VerifyArgs::max_drift`].
    pub max_drift: u8,
}

/// A table's row count in the copy and in the live database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCount {
    /// The table.
    pub table: String,
    /// Rows in the copy; `None` when the copy has no such table.
    pub copy: Option<i64>,
    /// Rows in the live database; `None` without a live database or table.
    pub live: Option<i64>,
    /// Reported only, never compared.
    pub volatile: bool,
}

/// The media references of a library copy.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MediaCheck {
    /// Distinct referenced objects.
    pub referenced: usize,
    /// Referenced objects whose file is missing or has another size.
    pub broken: Vec<String>,
    /// `g480` renditions recorded but missing.
    pub missing_renditions: Vec<String>,
}

/// The checks of one database copy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DatabaseReport {
    /// Path relative to the directory (`control.sqlite`, `users/<id>.sqlite`).
    pub name: String,
    /// The schema version of the copy.
    pub version: usize,
    /// Row counts, by table.
    pub tables: Vec<TableCount>,
    /// The media references (libraries only).
    pub media: Option<MediaCheck>,
    /// Whatever failed.
    pub problems: Vec<String>,
    /// Facts worth knowing that are not failures.
    pub notes: Vec<String>,
}

/// The checks of a directory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VerifyReport {
    /// The control database first, then the libraries by user id.
    pub databases: Vec<DatabaseReport>,
}

impl VerifyReport {
    /// Whether nothing failed.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.databases.iter().all(|db| db.problems.is_empty())
    }

    /// Number of failed checks.
    #[must_use]
    pub fn problem_count(&self) -> usize {
        self.databases.iter().map(|db| db.problems.len()).sum()
    }

    /// Writes the report for an operator.
    ///
    /// # Errors
    ///
    /// Writing failed.
    pub fn print(&self, out: &mut dyn Write) -> io::Result<()> {
        for db in &self.databases {
            let verdict = if db.problems.is_empty() {
                "ok"
            } else {
                "FAILED"
            };
            writeln!(out, "{}: {verdict} (schema v{})", db.name, db.version)?;
            for table in &db.tables {
                let copy = table.copy.map_or("-".to_owned(), |n| n.to_string());
                let live = table.live.map_or("-".to_owned(), |n| n.to_string());
                let volatile = if table.volatile { ", volatile" } else { "" };
                writeln!(out, "  {}: {copy} (live {live}{volatile})", table.table)?;
            }
            if let Some(media) = &db.media {
                writeln!(
                    out,
                    "  media: {} referenced objects, {} missing, {} renditions missing",
                    media.referenced,
                    media.broken.len(),
                    media.missing_renditions.len()
                )?;
            }
            for note in &db.notes {
                writeln!(out, "  note: {note}")?;
            }
            for problem in &db.problems {
                writeln!(out, "  PROBLEM: {problem}")?;
            }
        }
        writeln!(
            out,
            "verify: {} databases checked, {} problems",
            self.databases.len(),
            self.problem_count()
        )
    }
}

/// Runs `admin verify`.
///
/// # Errors
///
/// See [`verify`]; also when a check failed.
pub fn run(data: &DataDir, args: &VerifyArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let options = VerifyOptions {
        users: &args.users,
        max_drift: args.max_drift,
    };
    let report = verify(data, &args.dir, &options)?;
    report.print(out)?;
    if !report.is_ok() {
        anyhow::bail!("{} checks failed", report.problem_count());
    }
    Ok(())
}

/// Checks the copies in `dir` against the live data of `data`.
///
/// # Errors
///
/// `dir` has no `control.sqlite`, a requested library has no copy, a user id
/// is invalid, or a file cannot be read at all. Failed checks are not errors:
/// they are in the report.
pub fn verify(
    data: &DataDir,
    dir: &Path,
    options: &VerifyOptions<'_>,
) -> anyhow::Result<VerifyReport> {
    let control = dir.join(CONTROL_DB_FILE);
    if !control.is_file() {
        anyhow::bail!("no {CONTROL_DB_FILE} in {}", dir.display());
    }
    let users = if options.users.is_empty() {
        copies_in(&dir.join("users"))?
    } else {
        let mut users = options.users.to_vec();
        users.sort();
        users.dedup();
        for id in &users {
            if !is_valid_user_id(id) {
                anyhow::bail!("invalid user id {id:?}: expected 1-64 ASCII letters and digits");
            }
            let copy = dir.join("users").join(format!("{id}.sqlite"));
            if !copy.is_file() {
                anyhow::bail!("no copy of user {id}'s library in {}", dir.display());
            }
        }
        users
    };

    let mut databases = Vec::with_capacity(users.len() + 1);
    let mut control_report = check_database(
        CONTROL_DB_FILE,
        &control,
        Kind::Control,
        &data.control_db(),
        options.max_drift,
    )?;
    let known = known_users(&control).unwrap_or_default();
    for id in &users {
        if !known.contains(id) {
            control_report.notes.push(format!(
                "users/{id}.sqlite belongs to no user of this control database"
            ));
        }
    }
    databases.push(control_report);

    let store = MediaStore::new(data.users_dir());
    for id in &users {
        let name = format!("users/{id}.sqlite");
        let copy = dir.join(&name);
        let mut report = check_database(
            &name,
            &copy,
            Kind::Library,
            &data.library_db(id),
            options.max_drift,
        )?;
        if report.version > 0 {
            let media = store
                .user(id)
                .map_err(|_| anyhow::anyhow!("invalid user id {id:?}"))?;
            match check_media(
                &copy,
                |digest, kind| media.object_path(digest, kind),
                |digest| media.rendition_path(digest, Rendition::G480),
            ) {
                Ok(check) => {
                    if let Some(first) = check.broken.first() {
                        report.problems.push(format!(
                            "{} of {} referenced media objects are missing or truncated in the \
                             live store (first: {first})",
                            check.broken.len(),
                            check.referenced
                        ));
                    }
                    if let Some(first) = check.missing_renditions.first() {
                        report.problems.push(format!(
                            "{} g480 renditions are missing in the live store (first: {first})",
                            check.missing_renditions.len()
                        ));
                    }
                    report.media = Some(check);
                }
                Err(err) => report
                    .problems
                    .push(format!("cannot read the media references: {err:#}")),
            }
        }
        databases.push(report);
    }
    Ok(VerifyReport { databases })
}

/// The integrity, schema and version of a database copy, as checked before
/// installing it ([`check_copy`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CopyCheck {
    /// The schema version of the copy.
    pub version: usize,
    /// Whatever failed; empty when the copy can be installed.
    pub problems: Vec<String>,
}

/// Checks that the database at `path` is an intact `kind` database this build
/// can run on: application id, schema version (and compat floor), the full
/// `PRAGMA integrity_check` and `PRAGMA foreign_key_check`. Read only. A file
/// SQLite cannot make sense of is a problem, not an error.
///
/// # Errors
///
/// The file cannot be opened (missing, unreadable).
pub fn check_copy(path: &Path, kind: Kind) -> anyhow::Result<CopyCheck> {
    let conn = open_read(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut problems = Vec::new();
    let version = match check_schema(&conn, kind) {
        Ok(Ok(version)) => version,
        Ok(Err(problem)) => {
            problems.push(problem);
            return Ok(CopyCheck {
                version: 0,
                problems,
            });
        }
        Err(err) => {
            problems.push(format!("cannot read the database: {err}"));
            return Ok(CopyCheck {
                version: 0,
                problems,
            });
        }
    };
    let latest = kind.latest_version();
    if version > latest {
        match schema::min_reader_version(&conn) {
            Ok(Some(needs)) if needs > latest => problems.push(format!(
                "schema v{version} needs a build that supports v{needs}; this one supports up \
                 to v{latest}"
            )),
            Ok(_) => {}
            Err(err) => problems.push(format!("cannot read the compat floor: {err}")),
        }
    }
    match integrity_messages(&conn) {
        Ok(messages) if messages == ["ok"] => {}
        Ok(messages) => {
            let mut problem = format!("integrity check failed: {}", messages.join("; "));
            if messages.len() > LISTED {
                problem.push_str("; …");
            }
            problems.push(problem);
        }
        Err(err) => problems.push(format!("integrity check failed: {err}")),
    }
    match conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
        row.get::<_, i64>(0)
    }) {
        Ok(0) => {}
        Ok(violations) => problems.push(format!("{violations} foreign key violations")),
        Err(err) => problems.push(format!("foreign key check failed: {err}")),
    }
    Ok(CopyCheck { version, problems })
}

/// The schema version of a `kind` database, or why it is not one.
fn check_schema(conn: &Connection, kind: Kind) -> rusqlite::Result<Result<usize, String>> {
    let application_id: i32 = conn.query_row("PRAGMA application_id", [], |row| row.get(0))?;
    if application_id != kind.application_id() {
        return Ok(Err(format!(
            "not a Shelfy {} database (application_id {application_id})",
            kind.name()
        )));
    }
    let version = schema::version(conn)?;
    if version == 0 {
        return Ok(Err("the database has no schema".to_owned()));
    }
    Ok(Ok(version))
}

/// The first messages of `PRAGMA integrity_check`: `["ok"]` when intact.
fn integrity_messages(conn: &Connection) -> rusqlite::Result<Vec<String>> {
    conn.prepare("PRAGMA integrity_check")?
        .query_map([], |row| row.get(0))?
        .take(LISTED + 1)
        .collect()
}

/// [`check_copy`], then the row counts against the live database at `live`.
fn check_database(
    name: &str,
    copy: &Path,
    kind: Kind,
    live: &Path,
    max_drift: u8,
) -> anyhow::Result<DatabaseReport> {
    let CopyCheck {
        version,
        mut problems,
    } = check_copy(copy, kind)?;
    let mut notes = Vec::new();
    let copy_counts = if problems.is_empty() {
        table_counts(copy).unwrap_or_else(|err| {
            problems.push(format!("cannot count the rows: {err:#}"));
            Vec::new()
        })
    } else {
        Vec::new()
    };
    let live_counts = if !live.is_file() {
        notes.push(format!(
            "no live database at {}: nothing to compare with",
            live.display()
        ));
        None
    } else if copy_counts.is_empty() {
        None
    } else {
        match table_counts(live) {
            Ok(counts) => Some(counts),
            Err(err) => {
                problems.push(format!(
                    "cannot count the rows of the live database {}: {err:#}",
                    live.display()
                ));
                None
            }
        }
    };

    let volatile = match kind {
        Kind::Library => VOLATILE_LIBRARY_TABLES,
        Kind::Control => VOLATILE_CONTROL_TABLES,
    };
    let mut tables: Vec<TableCount> = copy_counts
        .iter()
        .map(|(table, n)| TableCount {
            table: table.clone(),
            copy: Some(*n),
            live: live_counts
                .as_ref()
                .and_then(|live| live.iter().find(|(t, _)| t == table))
                .map(|(_, n)| *n),
            volatile: volatile.contains(&table.as_str()),
        })
        .collect();
    if let Some(live) = &live_counts {
        for (table, n) in live {
            if !tables.iter().any(|t| &t.table == table) {
                tables.push(TableCount {
                    table: table.clone(),
                    copy: None,
                    live: Some(*n),
                    volatile: volatile.contains(&table.as_str()),
                });
            }
        }
    }
    let mut drifted = String::new();
    for table in &tables {
        if let (Some(copy), Some(live), false) = (table.copy, table.live, table.volatile)
            && !within_drift(copy, live, max_drift)
        {
            let _ = write!(
                drifted,
                "{}{} {copy} vs {live}",
                if drifted.is_empty() { "" } else { ", " },
                table.table
            );
        }
        if table.copy.is_none() || (table.live.is_none() && live_counts.is_some()) {
            notes.push(format!(
                "table {} exists only in the {} database (another schema version)",
                table.table,
                if table.copy.is_none() {
                    "live"
                } else {
                    "copied"
                }
            ));
        }
    }
    if !drifted.is_empty() {
        problems.push(format!(
            "row counts differ from the live database by more than {max_drift}%: {drifted}"
        ));
    }
    Ok(DatabaseReport {
        name: name.to_owned(),
        version,
        tables,
        media: None,
        problems,
        notes,
    })
}

/// Whether `a` and `b` differ by at most `percent` of the larger one (rounded
/// up, so a small table may move by one row once any drift is allowed).
#[must_use]
pub fn within_drift(a: i64, b: i64, percent: u8) -> bool {
    let larger = a.max(b).max(0);
    let allowed = (larger * i64::from(percent) + 99) / 100;
    (a - b).abs() <= allowed
}

/// Row counts of the user tables of the database at `path`, in one read
/// transaction (FTS and shadow tables left out).
fn table_counts(path: &Path) -> anyhow::Result<Vec<(String, i64)>> {
    let mut conn = open_read(path)?;
    let tx = conn.transaction()?;
    let tables: Vec<String> = tx
        .prepare(
            "SELECT name FROM sqlite_schema WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
               AND sql NOT LIKE 'CREATE VIRTUAL%'
               AND name NOT IN (SELECT name FROM pragma_table_list WHERE type = 'shadow')
             ORDER BY name",
        )?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut counts = Vec::with_capacity(tables.len());
    for table in tables {
        let n: i64 = tx.query_row(
            &format!("SELECT count(*) FROM \"{}\"", table.replace('"', "\"\"")),
            [],
            |row| row.get(0),
        )?;
        counts.push((table, n));
    }
    tx.commit()?;
    Ok(counts)
}

/// Checks that every object referenced by the library copy at `copy` has its
/// file (of the recorded size) at `object_path`, and its `g480` rendition at
/// `rendition_path` when the row says it has one.
fn check_media(
    copy: &Path,
    object_path: impl Fn(&Digest, MediaKind) -> PathBuf,
    rendition_path: impl Fn(&Digest) -> PathBuf,
) -> anyhow::Result<MediaCheck> {
    let conn = open_read(copy)?;
    let referenced = REFERENCES
        .iter()
        .map(|(table, column)| format!("SELECT {column} FROM {table} WHERE {column} IS NOT NULL"))
        .collect::<Vec<_>>()
        .join(" UNION ");
    let mut stmt = conn.prepare(&format!(
        "SELECT sha256, ext, bytes, variants FROM media_objects WHERE id IN ({referenced})
         ORDER BY id"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, Vec<u8>>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;
    let mut check = MediaCheck::default();
    for row in rows {
        let (sha256, ext, bytes, variants) = row?;
        check.referenced += 1;
        let Some(digest) = Digest::from_slice(&sha256) else {
            check
                .broken
                .push(format!("a digest of {} bytes", sha256.len()));
            continue;
        };
        let name = format!("{digest}.{ext}");
        let Some(kind) = MediaKind::from_ext(&ext) else {
            check.broken.push(format!("{name} (unknown type)"));
            continue;
        };
        let size = fs::metadata(object_path(&digest, kind))
            .ok()
            .filter(|m| m.is_file())
            .map(|m| m.len());
        if size != u64::try_from(bytes).ok() {
            check.broken.push(name);
        }
        if Variants::from_bits(variants).contains(Rendition::G480)
            && !rendition_path(&digest).is_file()
        {
            check.missing_renditions.push(format!("{digest}.g480.webp"));
        }
    }
    Ok(check)
}

/// The library copies in `users_dir`: user ids, sorted.
fn copies_in(users_dir: &Path) -> anyhow::Result<Vec<String>> {
    let entries = match fs::read_dir(users_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("cannot list {}", users_dir.display()));
        }
    };
    let mut ids = Vec::new();
    for entry in entries {
        let name = entry?.file_name();
        if let Some(id) = name.to_str().and_then(|n| n.strip_suffix(".sqlite"))
            && is_valid_user_id(id)
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

/// The user ids of the control database copy at `path`.
fn known_users(path: &Path) -> anyhow::Result<Vec<String>> {
    let conn = open_read(path)?;
    let ids = conn
        .prepare("SELECT id FROM users")?
        .query_map([], |row| row.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Opens a database to read it: read-write without CREATE (so the WAL index
/// of a live database can be opened even when the server is not running, and
/// a write-protected file falls back to read only), with writes refused.
pub(crate) fn open_read(path: &Path) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "query_only", true)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drift_is_a_share_of_the_larger_count() {
        assert!(within_drift(100, 100, 0));
        assert!(!within_drift(100, 101, 0));
        assert!(within_drift(6000, 6600, 10));
        assert!(!within_drift(6000, 6700, 10));
        assert!(within_drift(6700, 6100, 10), "either direction");
        assert!(within_drift(2, 3, 10), "a small table may move by one row");
        assert!(!within_drift(1, 3, 10));
        assert!(within_drift(0, 0, 0));
    }

    #[test]
    fn every_table_of_the_latest_schema_is_classified() {
        // A new table is compared unless listed as volatile; this test makes
        // the choice explicit when a migration adds one.
        for (kind, volatile, compared) in [
            (
                Kind::Control,
                VOLATILE_CONTROL_TABLES,
                &[
                    "api_tokens",
                    "feature_flags",
                    "invites",
                    "passkeys",
                    "provider_keys",
                    "users",
                ][..],
            ),
            (
                Kind::Library,
                VOLATILE_LIBRARY_TABLES,
                &[
                    "collections",
                    "media_objects",
                    "meta",
                    "post_collections",
                    "post_entities",
                    "post_media",
                    "post_tags",
                    "posts",
                    "settings",
                    "sync_sources",
                    "tag_alias",
                    "tag_cluster",
                    "tag_cluster_membership",
                    "tag_embeddings",
                    "web_capture_assets",
                    "web_captures",
                ][..],
            ),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("db.sqlite");
            let mut conn = Connection::open(&path).unwrap();
            schema::migrate(&mut conn, kind).unwrap();
            drop(conn);
            let tables: Vec<String> = table_counts(&path)
                .unwrap()
                .into_iter()
                .map(|(t, _)| t)
                .collect();
            let mut classified: Vec<&str> = volatile.iter().chain(compared).copied().collect();
            classified.sort_unstable();
            assert_eq!(tables, classified, "{} tables", kind.name());
        }
    }
}
