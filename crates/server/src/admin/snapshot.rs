//! `admin snapshot`: consistent copies of the control database and the user
//! libraries with SQLite's online backup API, while the server keeps running
//! (plan §3.5; restic then backs up the output directory).
//!
//! Output layout: `<out>/control.sqlite` and `<out>/users/<user_id>.sqlite`.
//! Each copy is taken in one backup step (one read snapshot of the source),
//! switched to a rollback journal so it is a single self-contained file,
//! checked with `PRAGMA quick_check`, fsynced and renamed into place, so a
//! reader of `<out>` never sees a partial file.
//!
//! P1-12 adds `--changed` (copy only the libraries changed since the last
//! run) and the restore side (`verify`, `user restore-db`,
//! `install-snapshots`).

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use clap::Args;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use shelfy_core::schema::Kind;

use crate::config::{CONTROL_DB_FILE, DataDir, create_private_dir};

/// How long a source may stay locked before the snapshot gives up.
const BUSY_RETRIES: u32 = 100;
const BUSY_PAUSE: Duration = Duration::from_millis(100);

/// Arguments of `admin snapshot`.
#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Output directory. Default: `backup-staging/db` in the data directory.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Copy only this user's library (repeatable). Default: every library.
    #[arg(long = "user", value_name = "USER_ID")]
    pub users: Vec<String>,
}

/// One copied database.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotFile {
    /// Path relative to the output directory.
    pub name: String,
    /// Size in bytes.
    pub bytes: u64,
}

/// What a snapshot wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReport {
    /// The output directory.
    pub dir: PathBuf,
    /// The control database first, then the libraries by user id.
    pub files: Vec<SnapshotFile>,
}

/// Runs `admin snapshot`.
///
/// # Errors
///
/// See [`snapshot`].
pub fn run(data: &DataDir, args: &SnapshotArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let dir = args.out.clone().unwrap_or_else(|| data.snapshot_dir());
    let report = snapshot(data, &dir, &args.users)?;
    let mut total = 0;
    for file in &report.files {
        writeln!(out, "{}\t{} bytes", file.name, file.bytes)?;
        total += file.bytes;
    }
    writeln!(
        out,
        "snapshot of {} databases ({total} bytes) in {}",
        report.files.len(),
        report.dir.display()
    )?;
    Ok(())
}

/// Copies the control database and the libraries of `users` (every library
/// on disk when empty) into `out`.
///
/// # Errors
///
/// A missing or foreign database, an invalid user id, a source locked for
/// over 10 s, a copy that fails its integrity check, or an I/O error.
pub fn snapshot(data: &DataDir, out: &Path, users: &[String]) -> anyhow::Result<SnapshotReport> {
    let control = data.control_db();
    if !control.is_file() {
        anyhow::bail!(
            "no control database at {}: check SHELFY_DATA_DIR",
            control.display()
        );
    }
    let users = if users.is_empty() {
        libraries_on_disk(data)?
    } else {
        for id in users {
            if !is_valid_user_id(id) {
                anyhow::bail!("invalid user id {id:?}: expected 1-64 ASCII letters and digits");
            }
            if !data.library_db(id).is_file() {
                anyhow::bail!("user {id} has no library in this data directory");
            }
        }
        let mut users = users.to_vec();
        users.sort();
        users.dedup();
        users
    };

    let users_out = out.join("users");
    create_private_dir(&users_out)
        .with_context(|| format!("cannot create {}", users_out.display()))?;
    let mut files = Vec::with_capacity(users.len() + 1);
    let bytes = copy_database(&control, &out.join(CONTROL_DB_FILE), Kind::Control)?;
    files.push(SnapshotFile {
        name: CONTROL_DB_FILE.to_owned(),
        bytes,
    });
    for id in users {
        let name = format!("users/{id}.sqlite");
        let bytes = copy_database(&data.library_db(&id), &out.join(&name), Kind::Library)?;
        files.push(SnapshotFile { name, bytes });
    }
    Ok(SnapshotReport {
        dir: out.to_path_buf(),
        files,
    })
}

/// User ids with a library on disk, sorted.
fn libraries_on_disk(data: &DataDir) -> anyhow::Result<Vec<String>> {
    let dir = data.users_dir();
    let mut ids = Vec::new();
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(ids),
        Err(err) => return Err(err).with_context(|| format!("cannot list {}", dir.display())),
    };
    for entry in entries {
        let entry = entry.with_context(|| format!("cannot list {}", dir.display()))?;
        if let Some(id) = entry.file_name().to_str()
            && is_valid_user_id(id)
            && data.library_db(id).is_file()
        {
            ids.push(id.to_owned());
        }
    }
    ids.sort();
    Ok(ids)
}

/// The user-id rule of the core's database cache: 1–64 ASCII letters and
/// digits, a single safe path component.
fn is_valid_user_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// Copies the database at `source` (which must be a `kind` database) to
/// `target`; returns the copy's size.
fn copy_database(source: &Path, target: &Path, kind: Kind) -> anyhow::Result<u64> {
    let context = || format!("cannot snapshot {}", source.display());
    // Read-write without CREATE: never creates a file, and can open the WAL
    // index even when the server is not running. Nothing is written.
    let src = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(context)?;
    src.busy_timeout(Duration::from_secs(5))
        .with_context(context)?;
    let application_id: i32 = src
        .query_row("PRAGMA application_id", [], |row| row.get(0))
        .with_context(context)?;
    if application_id != kind.application_id() {
        anyhow::bail!(
            "{} is not a Shelfy {} database",
            source.display(),
            kind.name()
        );
    }

    let partial = partial_path(target);
    remove_if_exists(&partial)?;
    let mut dst = Connection::open(&partial)
        .with_context(|| format!("cannot create {}", partial.display()))?;
    {
        let backup = Backup::new(&src, &mut dst).with_context(context)?;
        copy_all_pages(&backup).with_context(context)?;
    }
    drop(src);
    // The copy inherits the source's WAL flag; a rollback journal makes it
    // one self-contained file.
    let mode: String = dst
        .pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))
        .with_context(context)?;
    if !mode.eq_ignore_ascii_case("delete") {
        anyhow::bail!("the copy of {} stayed in {mode} mode", source.display());
    }
    let check: String = dst
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .with_context(context)?;
    if check != "ok" {
        anyhow::bail!(
            "the copy of {} failed its integrity check: {check}",
            source.display()
        );
    }
    dst.close().map_err(|(_, err)| err).with_context(context)?;

    File::open(&partial)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("cannot sync {}", partial.display()))?;
    fs::rename(&partial, target)
        .with_context(|| format!("cannot move the copy to {}", target.display()))?;
    if let Some(dir) = target.parent() {
        sync_dir(dir)?;
    }
    Ok(fs::metadata(target)?.len())
}

/// Copies every page in one step, so the copy is one consistent snapshot of
/// the source; retries while the source is locked.
fn copy_all_pages(backup: &Backup<'_, '_>) -> anyhow::Result<()> {
    for _ in 0..BUSY_RETRIES {
        match backup.step(-1)? {
            StepResult::Done => return Ok(()),
            _ => thread::sleep(BUSY_PAUSE),
        }
    }
    anyhow::bail!("the database stayed locked; try again")
}

fn partial_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".partial");
    target.with_file_name(name)
}

fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// Makes the rename durable (the directory entry); a no-op where directories
/// cannot be opened.
fn sync_dir(dir: &Path) -> anyhow::Result<()> {
    #[cfg(unix)]
    File::open(dir)
        .and_then(|f| f.sync_all())
        .with_context(|| format!("cannot sync {}", dir.display()))?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_files_sit_next_to_their_target() {
        assert_eq!(
            partial_path(Path::new("/out/users/AB12.sqlite")),
            Path::new("/out/users/AB12.sqlite.partial")
        );
    }

    #[test]
    fn user_ids_are_single_safe_components() {
        assert!(is_valid_user_id("01ARZ3NDEKTSV4RRFFQ69G5FAV"));
        for bad in ["", "..", "a/b", "a.b", "é", &"a".repeat(65)] {
            assert!(!is_valid_user_id(bad), "{bad:?}");
        }
    }
}
