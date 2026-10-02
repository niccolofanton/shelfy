//! `admin install-snapshots`: the full restore (plan §3.5, runbook "Full
//! host"): installs the database copies of a snapshot directory (a `restic
//! restore` of `backup-staging/db`) as the live databases, with the server
//! stopped.
//!
//! 1. Every copy is checked first ([`check_copy`]: integrity, foreign keys,
//!    schema), and so is the set ([`completeness`]): the control database
//!    has users, and each of them has a library copy. One bad or missing
//!    copy installs nothing.
//! 2. Unless `--force`, the data directory must hold no data yet: no user in
//!    the control database, no library. With `--force`, each database it
//!    replaces is kept next to it as `<name>.pre-restore-<time>.sqlite`.
//! 3. Every copy is staged next to its target ([`Staged`]: written, checked,
//!    fsynced) before anything is replaced, so a full disk stops the restore
//!    while the data directory is untouched. Then every live database is
//!    taken ([`take`]), which fails, still before any change, while one is
//!    open (the server runs). Then each one is replaced ([`Taken::replace`]).
//! 4. `admin verify` then compares the installed databases with the copies:
//!    equal row counts, and every media reference resolves (restore the
//!    media set before this command).
//!
//! **How a live database is replaced.** It is never renamed over. SQLite
//! finds a database's `-wal` and `-shm` files by name, so a connection that
//! opened the old file before a rename (a released server handle reopening
//! it, an operator command, the upgrade sweep) would pair its log with the
//! new file and corrupt it. Instead, the connection that proved the file
//! unused keeps its exclusive lock to the end: it copies the current content
//! to the kept file, then every page of the new content into the live file
//! with SQLite's online backup API, in one transaction (as the migration
//! install does, `crate::migrations::swap`). Any other connection waits on
//! that lock, then reads the old database or the new one, through the same
//! file and the same log. A live file SQLite cannot read (the reason for
//! many restores) is moved aside instead, with any `-wal`, `-shm` or
//! `-journal` next to it, as are the stray ones of a missing file: none of
//! them can belong to the new database. The path is then taken by a new
//! file, created and locked the same way before the copy.
//!
//! [`install_file`] is also how `admin user restore-db` swaps one library.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use rusqlite::backup::Backup;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use shelfy_core::db::is_valid_user_id;
use shelfy_core::schema::Kind;

use super::snapshot::{
    copy_all_pages, copy_database, copy_from, remove_if_exists, sidecar, sync_dir,
};
use super::verify::{
    self, VerifyOptions, VerifyReport, accounts, check_copy, completeness, open_read,
};
use crate::config::{CONTROL_DB_FILE, DataDir, create_private_dir};
use crate::ids::now_ms;

/// How often a wait for a database to be released checks again.
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

/// The files SQLite keeps next to a database: the write-ahead log, its
/// index, the rollback journal.
const SIDECARS: [&str; 3] = ["-wal", "-shm", "-journal"];

/// Arguments of `admin install-snapshots`.
#[derive(Debug, Args)]
pub struct InstallArgs {
    /// Directory with `control.sqlite` and `users/<user_id>.sqlite`: a
    /// restore of `backup-staging/db`.
    #[arg(value_name = "DIR")]
    pub dir: PathBuf,

    /// Replace existing data. Each replaced database is kept next to it as
    /// `<name>.pre-restore-<time>.sqlite`.
    #[arg(long)]
    pub force: bool,
}

/// One installed database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Installed {
    /// The live path it was installed at.
    pub path: PathBuf,
    /// The size of the copy in bytes.
    pub bytes: u64,
    /// Where the database it replaced was kept, if there was one.
    pub kept: Option<PathBuf>,
}

/// What `install-snapshots` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstallReport {
    /// The control database first, then the libraries by user id.
    pub installed: Vec<Installed>,
    /// The comparison of the installed databases with the copies.
    pub verify: VerifyReport,
}

/// Runs `admin install-snapshots`.
///
/// # Errors
///
/// See [`install_snapshots`]; also when the verification after the install
/// failed.
pub fn run(data: &DataDir, args: &InstallArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let report = install_snapshots(data, &args.dir, args.force)?;
    for installed in &report.installed {
        write!(
            out,
            "installed {} ({} bytes)",
            installed.path.display(),
            installed.bytes
        )?;
        match &installed.kept {
            Some(kept) => writeln!(out, "; the previous one is kept as {}", kept.display())?,
            None => writeln!(out)?,
        }
    }
    report.verify.print(out)?;
    if !report.verify.is_ok() {
        anyhow::bail!(
            "the databases are installed, but {} checks failed: see above",
            report.verify.problem_count()
        );
    }
    writeln!(
        out,
        "full restore of {} databases done; start the server",
        report.installed.len()
    )?;
    Ok(())
}

/// Installs the copies in `dir` as the live databases of `data`, then
/// verifies them.
///
/// # Errors
///
/// `dir` has no `control.sqlite`; a copy fails its checks, or the set lacks
/// one (nothing is installed then); the data directory already holds data and
/// `force` is off; a live database is open (the server runs); or an I/O
/// error. Every error but one while replacing a database leaves the data
/// directory as it was.
pub fn install_snapshots(data: &DataDir, dir: &Path, force: bool) -> anyhow::Result<InstallReport> {
    let control = dir.join(CONTROL_DB_FILE);
    if !control.is_file() {
        anyhow::bail!("no {CONTROL_DB_FILE} in {}", dir.display());
    }
    let users = library_copies(&dir.join("users"))?;

    let mut problems = Vec::new();
    for (name, path, kind) in
        std::iter::once((CONTROL_DB_FILE.to_owned(), control.clone(), Kind::Control)).chain(
            users
                .iter()
                .map(|(id, path)| (format!("users/{id}.sqlite"), path.clone(), Kind::Library)),
        )
    {
        let check = check_copy(&path, kind)?;
        problems.extend(check.problems.into_iter().map(|p| format!("{name}: {p}")));
    }
    if problems.is_empty() {
        // Only an intact control database can say who needs a library.
        let ids: Vec<String> = users.iter().map(|(id, _)| id.clone()).collect();
        problems.extend(
            completeness(&accounts(&control)?, &ids)
                .into_iter()
                .map(|p| format!("{CONTROL_DB_FILE}: {p}")),
        );
    }
    if !problems.is_empty() {
        anyhow::bail!(
            "nothing was installed; these copies failed their checks:\n  {}",
            problems.join("\n  ")
        );
    }

    data.create_layout()
        .with_context(|| format!("cannot create the layout of {}", data.root().display()))?;
    if !force {
        let existing = existing_data(data)?;
        if !existing.is_empty() {
            anyhow::bail!(
                "the data directory already holds data ({}); pass --force to replace it, \
                 keeping each replaced database next to it",
                existing.join(", ")
            );
        }
    }

    // The control database first, then the libraries by user id.
    let mut targets = vec![(control, data.control_db(), Kind::Control)];
    for (id, copy) in users {
        let live = data.library_db(&id);
        if let Some(dir) = live.parent() {
            create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        targets.push((copy, live, Kind::Library));
    }
    // Every copy is staged before anything changes; dropping them removes
    // them, whatever happens next.
    let staged = targets
        .iter()
        .map(|(copy, live, kind)| Staged::new(copy, live, *kind))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("nothing was installed")?;
    let taken = targets
        .iter()
        .map(|(_, live, _)| take(live, Duration::ZERO))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("nothing was installed (is the server stopped?)")?;

    let stamp = restore_stamp(now_ms());
    let mut installed = Vec::with_capacity(targets.len());
    for (((_, live, _), staged), taken) in targets.iter().zip(&staged).zip(taken) {
        let done = taken
            .replace(staged, live, &kept_path(live, &stamp))
            .with_context(|| {
                format!(
                    "cannot install {} (installed so far: {} of {} databases)",
                    live.display(),
                    installed.len(),
                    targets.len()
                )
            })?;
        installed.push(done);
    }
    drop(staged);
    let verify = verify::verify(data, dir, &VerifyOptions::default())?;
    Ok(InstallReport { installed, verify })
}

/// What already holds data: users in the control database, libraries.
fn existing_data(data: &DataDir) -> anyhow::Result<Vec<String>> {
    let mut existing = Vec::new();
    let control = data.control_db();
    if control.is_file() {
        let users: i64 = open_read(&control)
            .and_then(|conn| conn.query_row("SELECT count(*) FROM users", [], |row| row.get(0)))
            .with_context(|| format!("cannot read {}", control.display()))?;
        if users > 0 {
            existing.push(format!("{users} users in {}", control.display()));
        }
    }
    let libraries = shelfy_core::db::library_ids(&data.users_dir())?;
    if !libraries.is_empty() {
        existing.push(format!(
            "{} libraries in {}",
            libraries.len(),
            data.users_dir().display()
        ));
    }
    Ok(existing)
}

/// The library copies in `users_dir`: `(user id, path)`, sorted.
fn library_copies(users_dir: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let entries = match fs::read_dir(users_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(err) => {
            return Err(err).with_context(|| format!("cannot list {}", users_dir.display()));
        }
    };
    let mut copies = Vec::new();
    for entry in entries {
        let path = entry?.path();
        if let Some(id) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".sqlite"))
            && is_valid_user_id(id)
        {
            copies.push((id.to_owned(), path));
        }
    }
    copies.sort();
    Ok(copies)
}

/// Puts the database copy at `source` in place at `live`: stages it
/// ([`Staged`]), waits up to `wait` for other connections (the server) to
/// close `live` ([`take`]), then replaces it, keeping the current database
/// at `keep` ([`Taken::replace`]). Readers of `live` see the old database or
/// the new one, never a mix.
///
/// The caller checked `source` ([`check_copy`]) and keeps every opener of
/// the server away from `live` meanwhile: the server is stopped, or the user
/// is locked.
///
/// # Errors
///
/// `live` stays in use beyond `wait`, or SQLite or the file system failed;
/// `live` is unchanged then.
pub fn install_file(
    source: &Path,
    live: &Path,
    kind: Kind,
    keep: &Path,
    wait: Duration,
) -> anyhow::Result<Installed> {
    let staged = Staged::new(source, live, kind)?;
    take(live, wait)?.replace(&staged, live, keep)
}

/// A database copy staged next to its live path, as `<live>.restore`:
/// written with the online backup API, checked and fsynced ([`copy_database`]).
/// The file is removed when this is dropped.
#[derive(Debug)]
pub struct Staged {
    path: PathBuf,
    bytes: u64,
}

impl Staged {
    /// Stages the `kind` database at `source` next to `live`.
    ///
    /// # Errors
    ///
    /// The copy failed (a foreign file, a full disk).
    pub fn new(source: &Path, live: &Path, kind: Kind) -> anyhow::Result<Self> {
        let path = sidecar(live, ".restore");
        let bytes = copy_database(source, &path, kind)?;
        Ok(Self { path, bytes })
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = remove_if_exists(&self.path);
    }
}

/// A live database taken for replacement by [`take`].
#[derive(Debug)]
pub enum Taken {
    /// It exists and nobody else has it open: the connection holds an
    /// exclusive lock until the database is replaced.
    Held(Connection),
    /// It exists, but SQLite cannot read it.
    Unreadable(rusqlite::Error),
    /// There is none.
    Missing,
}

/// Takes the live database at `live` for replacement, waiting up to `wait`
/// while another connection (the server) has it open.
///
/// # Errors
///
/// `live` is still open after `wait`, or cannot be opened at all.
pub fn take(live: &Path, wait: Duration) -> anyhow::Result<Taken> {
    if !live.exists() {
        return Ok(Taken::Missing);
    }
    match wait_until_unused(live, wait)? {
        Probe::Free(conn) => Ok(Taken::Held(conn)),
        Probe::InUse => anyhow::bail!(
            "{} is still open in another process (the server); stop it, or lock the user \
             and wait for the server to release the library",
            live.display()
        ),
        Probe::Unreadable(err) => Ok(Taken::Unreadable(err)),
    }
}

impl Taken {
    /// Replaces the database at `live` with the `staged` copy, keeping the
    /// current one at `keep` (or `keep` with a counter, if that name is
    /// taken). See the module docs.
    ///
    /// # Errors
    ///
    /// SQLite or the file system failed. Up to the copy into `live`, `live`
    /// is unchanged; the copy itself is one transaction.
    pub fn replace(self, staged: &Staged, live: &Path, keep: &Path) -> anyhow::Result<Installed> {
        let (conn, mut kept) = match self {
            Self::Held(conn) => (conn, None),
            Self::Unreadable(err) => {
                // Restoring a damaged database is what a restore is for.
                // Nothing can use it, so it is kept as it is.
                eprintln!(
                    "note: SQLite cannot read {} ({err}); it is kept as it is",
                    live.display()
                );
                let kept = move_aside(live, keep)?;
                (create_locked(live)?, kept)
            }
            Self::Missing => {
                let kept = move_aside(live, keep)?;
                (create_locked(live)?, kept)
            }
        };
        // A database with a schema is kept (also one that another process
        // created meanwhile); a new or empty one holds nothing to keep.
        let has_schema: bool = conn
            .query_row("SELECT EXISTS (SELECT 1 FROM sqlite_schema)", [], |row| {
                row.get(0)
            })
            .with_context(|| format!("cannot read {}", live.display()))?;
        if has_schema {
            // The connection holds the exclusive lock: this copy is exactly
            // what is replaced.
            let keep = free_name(keep);
            copy_from(&conn, live, &keep)
                .with_context(|| format!("cannot keep {} as {}", live.display(), keep.display()))?;
            kept = Some(keep);
        }
        copy_into(conn, &staged.path, live)?;
        if let Some(dir) = live.parent() {
            sync_dir(dir)?;
        }
        Ok(Installed {
            path: live.to_path_buf(),
            bytes: staged.bytes,
            kept,
        })
    }
}

/// Copies every page of the database at `staged` into the live database
/// that `conn` holds with an exclusive lock, in one transaction, then closes
/// it. The copy goes through a rollback journal, which lets the page size
/// change; the file is then switched back to WAL mode, as the server keeps
/// it.
fn copy_into(mut conn: Connection, staged: &Path, live: &Path) -> anyhow::Result<()> {
    let context = || format!("cannot copy the restored database into {}", live.display());
    set_journal_mode(&conn, "DELETE").with_context(context)?;
    {
        let src = Connection::open_with_flags(
            staged,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .with_context(context)?;
        let backup = Backup::new(&src, &mut conn).with_context(context)?;
        copy_all_pages(&backup).with_context(context)?;
    }
    let check: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .with_context(context)?;
    if check != "ok" {
        anyhow::bail!(
            "{} failed its integrity check after the copy: {check}",
            live.display()
        );
    }
    set_journal_mode(&conn, "WAL").with_context(context)?;
    conn.close().map_err(|(_, err)| err).with_context(context)?;
    Ok(())
}

fn set_journal_mode(conn: &Connection, mode: &str) -> anyhow::Result<()> {
    let set: String = conn.pragma_update_and_check(None, "journal_mode", mode, |row| row.get(0))?;
    if !set.eq_ignore_ascii_case(mode) {
        anyhow::bail!("the journal mode stayed {set}, not {mode}");
    }
    Ok(())
}

/// Creates the database file at `path` (or opens the one that appeared
/// there meanwhile) and takes it with an exclusive lock, so no other
/// connection opens it before it holds the restored content.
fn create_locked(path: &Path) -> anyhow::Result<Connection> {
    let context = || format!("cannot create {}", path.display());
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(context)?;
    match lock_exclusively(&conn) {
        Ok(()) => Ok(conn),
        Err(err) if is_busy(&err) => anyhow::bail!(
            "{} was created and opened by another process meanwhile",
            path.display()
        ),
        Err(err) => Err(err).with_context(context),
    }
}

/// Moves the file at `live`, if there is one, and any `-wal`, `-shm` or
/// `-journal` next to it, to `keep` (or `keep` with a counter, if that name
/// is taken), so none of them can be paired with the database that takes the
/// path next. Returns where the file went.
fn move_aside(live: &Path, keep: &Path) -> anyhow::Result<Option<PathBuf>> {
    let keep = free_name(keep);
    let mut kept = None;
    if live.exists() {
        fs::rename(live, &keep).with_context(|| format!("cannot move {} aside", live.display()))?;
        kept = Some(keep.clone());
    }
    for suffix in SIDECARS {
        let from = sidecar(live, suffix);
        if from.exists() {
            let to = sidecar(&keep, suffix);
            fs::rename(&from, &to)
                .with_context(|| format!("cannot move {} aside", from.display()))?;
            if kept.is_none() {
                eprintln!(
                    "note: moved the stray {} aside, as {}",
                    from.display(),
                    to.display()
                );
            }
        }
    }
    Ok(kept)
}

/// `path`, or `<stem>-2.<ext>`, `<stem>-3.<ext>`… when it or one of its
/// `-wal`, `-shm` and `-journal` files exists.
fn free_name(path: &Path) -> PathBuf {
    let taken = |candidate: &Path| {
        candidate.exists()
            || SIDECARS
                .iter()
                .any(|suffix| sidecar(candidate, suffix).exists())
    };
    if !taken(path) {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("database");
    let ext = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or("sqlite");
    (2..)
        .map(|n| path.with_file_name(format!("{stem}-{n}.{ext}")))
        .find(|candidate| !taken(candidate))
        .expect("a free name exists")
}

/// What a probe of a database found.
#[derive(Debug)]
pub enum Probe {
    /// Nobody else has it open: this connection holds an exclusive lock.
    Free(Connection),
    /// Another connection, in this or another process, has it open.
    InUse,
    /// SQLite cannot read it: corrupt, or not a database.
    Unreadable(rusqlite::Error),
}

/// Takes the database at `path` for this process, retrying for up to `wait`
/// while another connection has it open.
///
/// In WAL mode every connection holds a shared lock on the database file for
/// as long as it is open, so the exclusive lock is granted only when no other
/// connection, in any process, uses the database.
///
/// # Errors
///
/// The file cannot be opened at all (missing, permissions).
pub fn wait_until_unused(path: &Path, wait: Duration) -> anyhow::Result<Probe> {
    let deadline = Instant::now() + wait;
    loop {
        match probe(path)? {
            Probe::InUse if Instant::now() < deadline => thread::sleep(PROBE_INTERVAL),
            other => return Ok(other),
        }
    }
}

/// One attempt of [`wait_until_unused`].
fn probe(path: &Path) -> anyhow::Result<Probe> {
    let context = || format!("cannot open {}", path.display());
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(context)?;
    match lock_exclusively(&conn) {
        Ok(()) => Ok(Probe::Free(conn)),
        Err(err) if is_busy(&err) => Ok(Probe::InUse),
        Err(err) => match err.sqlite_error_code() {
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Ok(Probe::Unreadable(err))
            }
            _ => Err(err).with_context(context),
        },
    }
}

/// Takes an exclusive lock on `conn`'s database for as long as `conn` is
/// open, without waiting: the first read takes the lock (an exclusive one at
/// once in WAL mode, a shared one in rollback mode, which `BEGIN EXCLUSIVE`
/// then upgrades), and the locking mode keeps it.
fn lock_exclusively(conn: &Connection) -> rusqlite::Result<()> {
    conn.busy_timeout(Duration::ZERO)?;
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")?;
    conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(()))?;
    conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;")
}

fn is_busy(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
    )
}

/// `<dir>/<stem>.pre-restore-<stamp>.sqlite` next to `live`.
pub fn kept_path(live: &Path, stamp: &str) -> PathBuf {
    let stem = live
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("database");
    live.with_file_name(format!("{stem}.pre-restore-{stamp}.sqlite"))
}

/// A UTC time stamp for file names, `20261002T153000Z`, from unix ms.
#[must_use]
pub fn restore_stamp(unix_ms: i64) -> String {
    let secs = unix_ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z")
}

#[cfg(test)]
mod tests {
    use shelfy_core::db::{DbError, UserDb, UserDbConfig};

    use super::*;

    #[test]
    fn stamps_are_utc_and_sortable() {
        assert_eq!(restore_stamp(0), "19700101T000000Z");
        assert_eq!(restore_stamp(1_790_899_200_000), "20261002T000000Z");
        assert_eq!(
            restore_stamp(951_782_400_000 + 3_723_000),
            "20000229T010203Z"
        );
        assert_eq!(
            kept_path(Path::new("/d/users/A/library.sqlite"), "20261002T000000Z"),
            Path::new("/d/users/A/library.pre-restore-20261002T000000Z.sqlite")
        );
    }

    #[test]
    fn an_open_database_is_not_free_until_every_connection_closes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("library.sqlite");
        let db = UserDb::open(&path, &UserDbConfig::default()).unwrap();
        db.read(|_| Ok::<_, DbError>(())).unwrap();
        assert!(
            matches!(probe(&path).unwrap(), Probe::InUse),
            "the writer and a reader are open"
        );
        db.release();
        assert!(matches!(probe(&path).unwrap(), Probe::Free(_)), "released");

        // Waiting gives the holder time to let go.
        let db = UserDb::open(&path, &UserDbConfig::default()).unwrap();
        let holder = thread::spawn(move || {
            thread::sleep(Duration::from_millis(300));
            drop(db);
        });
        let taken = wait_until_unused(&path, Duration::from_secs(5)).unwrap();
        assert!(matches!(taken, Probe::Free(_)));
        holder.join().unwrap();
        // The exclusive connection keeps everyone else out until it closes.
        assert!(matches!(probe(&path).unwrap(), Probe::InUse));
        drop(taken);
        assert!(matches!(
            wait_until_unused(&path, Duration::ZERO).unwrap(),
            Probe::Free(_)
        ));

        // A file that is not a database is reported as such.
        let junk = dir.path().join("junk.sqlite");
        std::fs::write(&junk, vec![0x5a; 8192]).unwrap();
        assert!(matches!(probe(&junk).unwrap(), Probe::Unreadable(_)));
    }

    #[test]
    fn files_moved_aside_never_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("library.sqlite");
        let keep = kept_path(&live, "20261002T000000Z");
        std::fs::write(&live, b"first").unwrap();
        std::fs::write(sidecar(&live, "-wal"), b"log").unwrap();
        std::fs::write(sidecar(&live, "-journal"), b"journal").unwrap();
        assert_eq!(move_aside(&live, &keep).unwrap(), Some(keep.clone()));
        assert!(!live.exists());
        assert_eq!(std::fs::read(sidecar(&keep, "-wal")).unwrap(), b"log");
        assert_eq!(
            std::fs::read(sidecar(&keep, "-journal")).unwrap(),
            b"journal"
        );
        for suffix in SIDECARS {
            assert!(!sidecar(&live, suffix).exists(), "{suffix} moved");
        }

        // A second file in the same second gets a name of its own.
        std::fs::write(&live, b"second").unwrap();
        let again = move_aside(&live, &keep).unwrap().unwrap();
        assert_eq!(
            again,
            dir.path()
                .join("library.pre-restore-20261002T000000Z-2.sqlite")
        );
        assert_eq!(std::fs::read(&keep).unwrap(), b"first");
        assert_eq!(std::fs::read(&again).unwrap(), b"second");

        // A missing file's stray log moves too, under a free name.
        std::fs::write(sidecar(&live, "-wal"), b"stray").unwrap();
        assert_eq!(move_aside(&live, &keep).unwrap(), None);
        let third = dir
            .path()
            .join("library.pre-restore-20261002T000000Z-3.sqlite");
        assert_eq!(std::fs::read(sidecar(&third, "-wal")).unwrap(), b"stray");
        assert!(!sidecar(&live, "-wal").exists());
    }
}
