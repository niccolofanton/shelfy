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
//! **`--changed`** (the hourly timer): the control database is always copied,
//! a library only when its files changed since the copy in `<out>` was taken.
//! Before copying, the run records the size, modification time and inode of
//! `library.sqlite` and its `-wal` in `<out>/snapshot-state.json`; the next
//! run copies the library again when any of them differs. A committed write
//! lands in the WAL or, after a checkpoint, in the main file, so it always
//! moves one of them; a checkpoint without new data costs one extra copy,
//! never a missed one. A modification time within [`RACY_WINDOW`] of the
//! check is not trusted (a later write may share its clock tick), so that
//! library is copied again on the next run.
//!
//! A library locked for maintenance (`admin user lock`) is skipped and keeps
//! its previous copy. A run over every library (no `--user`) also removes the
//! copies of users whose library is gone (deleted accounts), so a restore
//! never brings them back. One snapshot at a time writes to a directory: the
//! run holds an exclusive lock on `<out>/.lock`.
//!
//! The restore side is in [`super::verify`], [`super::install`] and
//! [`super::user`].

use std::collections::BTreeMap;
use std::fs::{self, File, TryLockError};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use clap::Args;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use shelfy_core::db::{is_library_locked, is_valid_user_id, library_ids};
use shelfy_core::schema::Kind;

use crate::config::{CONTROL_DB_FILE, DataDir, create_private_dir};

/// How long a source may stay locked before the snapshot gives up.
const BUSY_RETRIES: u32 = 100;
const BUSY_PAUSE: Duration = Duration::from_millis(100);

/// The record of the sources' state, in the output directory.
pub const STATE_FILE: &str = "snapshot-state.json";
/// The lock file that keeps two snapshots from writing to one directory.
pub const LOCK_FILE: &str = ".lock";
/// A modification time this close to the check is not trusted (see the module
/// docs).
pub const RACY_WINDOW: Duration = Duration::from_secs(2);

/// Arguments of `admin snapshot`.
#[derive(Debug, Args)]
pub struct SnapshotArgs {
    /// Output directory. Default: `backup-staging/db` in the data directory.
    #[arg(long, value_name = "DIR")]
    pub out: Option<PathBuf>,

    /// Copy a library only if it changed since its copy in the output
    /// directory was taken. The control database is always copied.
    #[arg(long)]
    pub changed: bool,

    /// Copy only this user's library (repeatable). Default: every library.
    #[arg(long = "user", value_name = "USER_ID")]
    pub users: Vec<String>,
}

/// What to copy.
#[derive(Clone, Copy, Debug, Default)]
pub struct SnapshotOptions<'a> {
    /// Only these users' libraries; every library when empty.
    pub users: &'a [String],
    /// Skip the libraries that did not change since their last copy.
    pub changed: bool,
}

/// What happened to one database.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CopyStatus {
    /// Copied now.
    Copied,
    /// Unchanged since its copy was taken (`--changed`): not copied.
    Unchanged,
    /// Locked for maintenance: its previous copy, if any, was kept.
    Locked,
}

impl CopyStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Copied => "copied",
            Self::Unchanged => "unchanged",
            Self::Locked => "locked, previous copy kept",
        }
    }
}

/// One database of the snapshot.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotFile {
    /// Path relative to the output directory.
    pub name: String,
    /// Size of the copy in bytes (0 when there is none).
    pub bytes: u64,
    /// What happened to it.
    pub status: CopyStatus,
}

/// What a snapshot wrote.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotReport {
    /// The output directory.
    pub dir: PathBuf,
    /// The control database first, then the libraries by user id.
    pub files: Vec<SnapshotFile>,
    /// Copies removed because their library is gone.
    pub removed: Vec<String>,
    /// Libraries that could not be copied, with the reason; the others were.
    pub failed: Vec<(String, String)>,
}

impl SnapshotReport {
    fn count(&self, status: CopyStatus) -> usize {
        self.files.iter().filter(|f| f.status == status).count()
    }
}

/// Runs `admin snapshot`.
///
/// # Errors
///
/// See [`snapshot`]; also when a library could not be copied (after copying
/// the others).
pub fn run(data: &DataDir, args: &SnapshotArgs, out: &mut dyn Write) -> anyhow::Result<()> {
    let dir = args.out.clone().unwrap_or_else(|| data.snapshot_dir());
    let options = SnapshotOptions {
        users: &args.users,
        changed: args.changed,
    };
    let report = snapshot(data, &dir, &options)?;
    let mut total = 0;
    for file in &report.files {
        let status = match file.status {
            CopyStatus::Locked if file.bytes == 0 => "locked, no copy yet",
            status => status.as_str(),
        };
        writeln!(out, "{}\t{} bytes\t{status}", file.name, file.bytes)?;
        total += file.bytes;
    }
    for name in &report.removed {
        writeln!(out, "removed {name}: the user has no library any more")?;
    }
    writeln!(
        out,
        "snapshot of {} databases ({total} bytes) in {}: {} copied, {} unchanged, {} locked",
        report.files.len(),
        report.dir.display(),
        report.count(CopyStatus::Copied),
        report.count(CopyStatus::Unchanged),
        report.count(CopyStatus::Locked),
    )?;
    if !report.failed.is_empty() {
        for (name, reason) in &report.failed {
            eprintln!("cannot snapshot {name}: {reason}");
        }
        anyhow::bail!(
            "{} of the libraries could not be copied",
            report.failed.len()
        );
    }
    Ok(())
}

/// Copies the control database and the libraries of `options.users` (every
/// library on disk when empty) into `out`.
///
/// # Errors
///
/// A missing or foreign control database, an invalid user id, a user without
/// a library, a control database locked for over 10 s, a copy that fails its
/// integrity check, another snapshot writing to `out`, or an I/O error. A
/// library that cannot be copied does not stop the others: it is listed in
/// [`SnapshotReport::failed`].
pub fn snapshot(
    data: &DataDir,
    out: &Path,
    options: &SnapshotOptions<'_>,
) -> anyhow::Result<SnapshotReport> {
    let control = data.control_db();
    if !control.is_file() {
        anyhow::bail!(
            "no control database at {}: check SHELFY_DATA_DIR",
            control.display()
        );
    }
    let all = options.users.is_empty();
    let users = if all {
        library_ids(&data.users_dir())
            .with_context(|| format!("cannot list {}", data.users_dir().display()))?
    } else {
        for id in options.users {
            if !is_valid_user_id(id) {
                anyhow::bail!("invalid user id {id:?}: expected 1-64 ASCII letters and digits");
            }
            if !data.library_db(id).is_file() {
                anyhow::bail!("user {id} has no library in this data directory");
            }
        }
        let mut users = options.users.to_vec();
        users.sort();
        users.dedup();
        users
    };

    let users_out = out.join("users");
    create_private_dir(&users_out)
        .with_context(|| format!("cannot create {}", users_out.display()))?;
    let _lock = lock_dir(out)?;
    let mut state = State::read(&out.join(STATE_FILE));

    let mut report = SnapshotReport {
        dir: out.to_path_buf(),
        files: Vec::with_capacity(users.len() + 1),
        removed: Vec::new(),
        failed: Vec::new(),
    };
    let bytes = copy_database(&control, &out.join(CONTROL_DB_FILE), Kind::Control)?;
    report.files.push(SnapshotFile {
        name: CONTROL_DB_FILE.to_owned(),
        bytes,
        status: CopyStatus::Copied,
    });

    for id in &users {
        let name = format!("users/{id}.sqlite");
        let target = out.join(&name);
        match snapshot_library(data, id, &target, &mut state, options.changed) {
            Ok(status) => report.files.push(SnapshotFile {
                bytes: fs::metadata(&target).map_or(0, |m| m.len()),
                name,
                status,
            }),
            Err(err) => {
                state.libraries.remove(id);
                report.failed.push((name, format!("{err:#}")));
            }
        }
    }

    if all {
        for (id, path) in copies_in(&users_out)? {
            if users.binary_search(&id).is_ok() || is_library_locked(&data.users_dir(), &id)? {
                continue;
            }
            fs::remove_file(&path).with_context(|| format!("cannot remove {}", path.display()))?;
            state.libraries.remove(&id);
            report.removed.push(format!("users/{id}.sqlite"));
        }
        state
            .libraries
            .retain(|id, _| users.binary_search(id).is_ok());
        sync_dir(&users_out)?;
    }
    state.write(&out.join(STATE_FILE))?;
    Ok(report)
}

/// Copies one library unless it is locked or (with `changed`) unchanged, and
/// records the state it was copied from.
fn snapshot_library(
    data: &DataDir,
    id: &str,
    target: &Path,
    state: &mut State,
    changed: bool,
) -> anyhow::Result<CopyStatus> {
    if is_library_locked(&data.users_dir(), id)? {
        return Ok(CopyStatus::Locked);
    }
    let source = data.library_db(id);
    let now = SystemTime::now();
    let observed = SourceState::observe(&source, now)?;
    if changed
        && target.is_file()
        && state
            .libraries
            .get(id)
            .is_some_and(|previous| previous.unchanged(&observed))
    {
        return Ok(CopyStatus::Unchanged);
    }
    copy_database(&source, target, Kind::Library)?;
    state.libraries.insert(id.to_owned(), observed);
    Ok(CopyStatus::Copied)
}

/// The library copies in `<out>/users/`: `(user id, path)`.
fn copies_in(users_out: &Path) -> anyhow::Result<Vec<(String, PathBuf)>> {
    let mut copies = Vec::new();
    let entries =
        fs::read_dir(users_out).with_context(|| format!("cannot list {}", users_out.display()))?;
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
    Ok(copies)
}

/// Holds the exclusive lock of an output directory until dropped.
fn lock_dir(out: &Path) -> anyhow::Result<File> {
    let path = out.join(LOCK_FILE);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&path)
        .with_context(|| format!("cannot open {}", path.display()))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => anyhow::bail!(
            "another snapshot is writing to {}; try again when it is done",
            out.display()
        ),
        Err(TryLockError::Error(err)) => {
            Err(err).with_context(|| format!("cannot lock {}", path.display()))
        }
    }
}

/// The recorded state of the sources, per user id.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct State {
    #[serde(default)]
    libraries: BTreeMap<String, SourceState>,
}

impl State {
    /// The state in `path`; empty when it is missing or unreadable, which
    /// only makes the next run copy everything.
    fn read(path: &Path) -> Self {
        fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn write(&self, path: &Path) -> anyhow::Result<()> {
        let partial = partial_path(path);
        let json = serde_json::to_vec_pretty(self).expect("the state serializes");
        let mut file = File::create(&partial)
            .with_context(|| format!("cannot create {}", partial.display()))?;
        file.write_all(&json)?;
        file.sync_all()?;
        fs::rename(&partial, path).with_context(|| format!("cannot write {}", path.display()))?;
        Ok(())
    }
}

/// The files of a library when it was last copied.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct SourceState {
    main: FileStamp,
    wal: Option<FileStamp>,
    /// A modification time was too close to the check to be trusted.
    racy: bool,
}

impl SourceState {
    fn observe(library: &Path, now: SystemTime) -> anyhow::Result<Self> {
        let main =
            FileStamp::of(library)?.with_context(|| format!("{} is missing", library.display()))?;
        let wal = FileStamp::of(&sidecar(library, "-wal"))?;
        let threshold = nanos(now).saturating_sub(nanos_of(RACY_WINDOW));
        let racy = main.mtime_ns >= threshold || wal.is_some_and(|w| w.mtime_ns >= threshold);
        Ok(Self { main, wal, racy })
    }

    /// Whether `now` shows the files as they were when this was recorded.
    fn unchanged(&self, now: &Self) -> bool {
        !self.racy && self.main == now.main && self.wal == now.wal
    }
}

/// Size, modification time and inode of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileStamp {
    len: u64,
    mtime_ns: u64,
    ino: u64,
}

impl FileStamp {
    fn of(path: &Path) -> anyhow::Result<Option<Self>> {
        let meta = match fs::metadata(path) {
            Ok(meta) => meta,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err).with_context(|| format!("cannot stat {}", path.display())),
        };
        #[cfg(unix)]
        let ino = std::os::unix::fs::MetadataExt::ino(&meta);
        #[cfg(not(unix))]
        let ino = 0;
        Ok(Some(Self {
            len: meta.len(),
            mtime_ns: nanos(meta.modified()?),
            ino,
        }))
    }
}

fn nanos(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

fn nanos_of(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

/// `<path><suffix>`: SQLite's `-wal` and `-shm` files.
pub(crate) fn sidecar(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// Copies the database at `source` (which must be a `kind` database) to
/// `target`; returns the copy's size.
///
/// # Errors
///
/// The source is missing, foreign or stays locked; the copy fails its quick
/// check; or an I/O error.
pub(crate) fn copy_database(source: &Path, target: &Path, kind: Kind) -> anyhow::Result<u64> {
    let context = || format!("cannot copy {}", source.display());
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

pub(crate) fn partial_path(target: &Path) -> PathBuf {
    sidecar(target, ".partial")
}

pub(crate) fn remove_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(err).with_context(|| format!("cannot remove {}", path.display())),
    }
}

/// Makes the rename durable (the directory entry); a no-op where directories
/// cannot be opened.
pub(crate) fn sync_dir(dir: &Path) -> anyhow::Result<()> {
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
    fn partial_files_and_sidecars_sit_next_to_their_target() {
        assert_eq!(
            partial_path(Path::new("/out/users/AB12.sqlite")),
            Path::new("/out/users/AB12.sqlite.partial")
        );
        assert_eq!(
            sidecar(Path::new("/u/A/library.sqlite"), "-wal"),
            Path::new("/u/A/library.sqlite-wal")
        );
    }

    #[test]
    fn a_recent_modification_time_is_not_trusted() {
        let stamp = |mtime_ns| FileStamp {
            len: 4096,
            mtime_ns,
            ino: 7,
        };
        let now = UNIX_EPOCH + Duration::from_secs(1_000);
        let old = nanos(now) - nanos_of(Duration::from_secs(60));

        let calm = SourceState {
            main: stamp(old),
            wal: Some(stamp(old)),
            racy: false,
        };
        assert!(calm.unchanged(&calm));
        let moved = SourceState {
            wal: Some(FileStamp {
                len: 8192,
                ..stamp(old)
            }),
            ..calm
        };
        assert!(!calm.unchanged(&moved), "the WAL grew");
        let replaced = SourceState {
            main: FileStamp {
                ino: 8,
                ..stamp(old)
            },
            ..calm
        };
        assert!(!calm.unchanged(&replaced), "the file was replaced");
        let racy = SourceState { racy: true, ..calm };
        assert!(!racy.unchanged(&calm), "a racy record is copied again");

        // Observing a file written within the window marks the record racy.
        let dir = tempfile::tempdir().unwrap();
        let library = dir.path().join("library.sqlite");
        fs::write(&library, b"x").unwrap();
        let observed = SourceState::observe(&library, SystemTime::now()).unwrap();
        assert!(observed.racy);
        assert_eq!(observed.wal, None);
        let later = SystemTime::now() + Duration::from_secs(10);
        assert!(!SourceState::observe(&library, later).unwrap().racy);
    }
}
