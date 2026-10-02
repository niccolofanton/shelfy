//! `admin install-snapshots`: the full restore (plan §3.5, runbook "Full
//! host"): installs the database copies of a snapshot directory (a `restic
//! restore` of `backup-staging/db`) as the live databases, with the server
//! stopped.
//!
//! 1. Every copy is checked first ([`check_copy`]: integrity, foreign keys,
//!    schema); one bad copy installs nothing.
//! 2. Unless `--force`, the data directory must hold no data yet: no user in
//!    the control database, no library. With `--force`, each database it
//!    replaces is kept next to it as `<name>.pre-restore-<time>.sqlite`.
//! 3. Each copy is installed with [`install_file`]: written next to its
//!    target, checked, fsynced and renamed over it. A live database must not
//!    be open: the server has to be stopped.
//! 4. `admin verify` then compares the installed databases with the copies:
//!    equal row counts, and every media reference resolves (restore the
//!    media set before this command).
//!
//! [`install_file`] is also how `admin user restore-db` swaps one library.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use clap::Args;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use shelfy_core::db::is_valid_user_id;
use shelfy_core::schema::Kind;

use super::snapshot::{copy_database, remove_if_exists, sidecar, sync_dir};
use super::verify::{self, VerifyOptions, VerifyReport, check_copy, open_read};
use crate::config::{CONTROL_DB_FILE, DataDir, create_private_dir};
use crate::ids::now_ms;

/// How often a wait for a database to be released checks again.
const PROBE_INTERVAL: Duration = Duration::from_millis(250);

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
    /// Its size in bytes.
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
/// `dir` has no `control.sqlite`; a copy fails its checks (nothing is
/// installed then); the data directory already holds data and `force` is
/// off; a live database is open (the server runs); or an I/O error.
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

    let stamp = restore_stamp(now_ms());
    let mut installed = Vec::with_capacity(users.len() + 1);
    let live = data.control_db();
    installed.push(
        install_file(
            &control,
            &live,
            Kind::Control,
            &kept_path(&live, &stamp),
            Duration::ZERO,
        )
        .context("cannot install the control database (is the server stopped?)")?,
    );
    for (id, copy) in &users {
        let live = data.library_db(id);
        if let Some(dir) = live.parent() {
            create_private_dir(dir).with_context(|| format!("cannot create {}", dir.display()))?;
        }
        installed.push(
            install_file(
                copy,
                &live,
                Kind::Library,
                &kept_path(&live, &stamp),
                Duration::ZERO,
            )
            .with_context(|| format!("cannot install user {id}'s library"))?,
        );
    }
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

/// Puts the database copy at `source` in place at `live`, atomically:
///
/// 1. it is copied next to `live` (online backup, rollback journal,
///    `quick_check`, fsync);
/// 2. `live`, if it exists, must be unused: it is opened with an exclusive
///    lock, waiting up to `wait` for other connections (the server) to close
///    it, and its write-ahead log is checkpointed and removed, so the new
///    file can never pick up the old file's log. A file SQLite cannot read
///    (the reason for many restores) is replaced as it is;
/// 3. the old file is kept at `keep` (a hard link, or a copy where links are
///    not supported; a counter is added if the name is taken), with any
///    `-wal` and `-shm` it still has, and the new one renamed over `live`: a
///    reader of the path sees either the old database or the new one.
///
/// The caller checked `source` ([`check_copy`]) and keeps every opener away
/// from `live` meanwhile: the server is stopped, or the user is locked.
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
    let staged = sidecar(live, ".restore");
    let bytes = copy_database(source, &staged, kind)?;
    let result = swap(live, &staged, keep, wait);
    if result.is_err() {
        let _ = remove_if_exists(&staged);
    }
    let kept = result?;
    Ok(Installed {
        path: live.to_path_buf(),
        bytes,
        kept,
    })
}

fn swap(
    live: &Path,
    staged: &Path,
    keep: &Path,
    wait: Duration,
) -> anyhow::Result<Option<PathBuf>> {
    let kept = if live.exists() {
        match wait_until_unused(live, wait)? {
            Probe::Free(conn) => {
                conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))
                    .with_context(|| format!("cannot checkpoint {}", live.display()))?;
                // The last connection to close removes the write-ahead log.
                conn.close()
                    .map_err(|(_, err)| err)
                    .with_context(|| format!("cannot close {}", live.display()))?;
                let wal = sidecar(live, "-wal");
                if wal.exists() {
                    anyhow::bail!(
                        "{} is still there after the checkpoint: something opened the database",
                        wal.display()
                    );
                }
            }
            Probe::InUse => anyhow::bail!(
                "{} is still open in another process (the server); stop it, or lock the user \
                 and wait for the server to release the library",
                live.display()
            ),
            // Restoring a damaged database is what a restore is for. Nothing
            // can have it open for long, and the old file keeps its sidecars.
            Probe::Unreadable(err) => eprintln!(
                "note: SQLite cannot read {} ({err}); it is replaced as it is",
                live.display()
            ),
        }
        Some(keep_file(live, keep)?)
    } else {
        None
    };
    fs::rename(staged, live)
        .with_context(|| format!("cannot move the new database to {}", live.display()))?;
    if let Some(dir) = live.parent() {
        sync_dir(dir)?;
    }
    Ok(kept)
}

/// Keeps the file at `live` at `keep`, or at `keep` with a counter when that
/// name is taken (an earlier restore in the same second), with whatever
/// `-wal` and `-shm` files it still has; returns where. The file is kept as a
/// hard link, or a copy where the file system has none.
fn keep_file(live: &Path, keep: &Path) -> anyhow::Result<PathBuf> {
    let keep = free_name(keep);
    if fs::hard_link(live, &keep).is_err() {
        fs::copy(live, &keep)
            .and_then(|_| fs::File::open(&keep)?.sync_all())
            .with_context(|| format!("cannot keep {} as {}", live.display(), keep.display()))?;
    }
    for suffix in ["-wal", "-shm"] {
        let from = sidecar(live, suffix);
        if from.exists() {
            fs::rename(&from, sidecar(&keep, suffix))
                .with_context(|| format!("cannot move {} aside", from.display()))?;
        }
    }
    Ok(keep)
}

/// `path`, or `<stem>-2.<ext>`, `<stem>-3.<ext>`… if it exists.
fn free_name(path: &Path) -> PathBuf {
    if !path.exists() {
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
        .find(|candidate| !candidate.exists())
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
    // The first read takes the lock: an exclusive one at once in WAL mode,
    // a shared one in rollback mode, which BEGIN EXCLUSIVE then upgrades.
    let attempt = conn
        .pragma_update(None, "locking_mode", "EXCLUSIVE")
        .and_then(|()| conn.query_row("SELECT count(*) FROM sqlite_schema", [], |_| Ok(())))
        .and_then(|()| conn.execute_batch("BEGIN EXCLUSIVE; COMMIT;"));
    match attempt {
        Ok(()) => Ok(Probe::Free(conn)),
        Err(err) => match err.sqlite_error_code() {
            Some(ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) => Ok(Probe::InUse),
            Some(ErrorCode::DatabaseCorrupt | ErrorCode::NotADatabase) => {
                Ok(Probe::Unreadable(err))
            }
            _ => Err(err).with_context(context),
        },
    }
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
    fn kept_files_never_overwrite_each_other() {
        let dir = tempfile::tempdir().unwrap();
        let live = dir.path().join("library.sqlite");
        let keep = kept_path(&live, "20261002T000000Z");
        std::fs::write(&live, b"first").unwrap();
        std::fs::write(sidecar(&live, "-wal"), b"log").unwrap();
        assert_eq!(keep_file(&live, &keep).unwrap(), keep);
        assert_eq!(std::fs::read(sidecar(&keep, "-wal")).unwrap(), b"log");
        assert!(
            !sidecar(&live, "-wal").exists(),
            "the log moved with the kept file"
        );
        // The swap renames a new file over the live name.
        std::fs::remove_file(&live).unwrap();
        std::fs::write(&live, b"second").unwrap();
        let again = keep_file(&live, &keep).unwrap();
        assert_eq!(
            again,
            dir.path()
                .join("library.pre-restore-20261002T000000Z-2.sqlite")
        );
        assert_eq!(std::fs::read(&keep).unwrap(), b"first");
        assert_eq!(std::fs::read(&again).unwrap(), b"second");
    }
}
