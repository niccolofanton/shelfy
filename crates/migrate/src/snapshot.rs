//! A consistent copy of the desktop library for `run` to read (OI-8).
//!
//! `run` must not race the desktop app. A library with no `-wal` or
//! `-journal` file next to it is closed: `run` reads it in place, opened
//! immutable by the legacy reader. Otherwise a connection has it open, or
//! had it open and exited uncleanly; `run` then first copies it into the
//! work directory with SQLite's online backup API, in one step, which is one
//! consistent snapshot of the committed state, and reads the copy. The
//! desktop library itself is only read.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use anyhow::Context as _;
use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};

/// File name of the snapshot inside the work directory.
pub const SNAPSHOT_FILE: &str = "source.sqlite";

/// How long the snapshot waits for the desktop app to release a lock.
const BUSY_RETRIES: u32 = 100;
const BUSY_PAUSE: Duration = Duration::from_millis(100);

/// The library `run` reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Source {
    /// The file to open with the legacy reader.
    pub path: PathBuf,
    /// Whether it is a snapshot in the work directory (the library had a
    /// `-wal` or `-journal` file).
    pub snapshot: bool,
}

/// Whether a `-wal` or `-journal` file sits next to `db`: a connection has
/// the library open, or had it and did not close cleanly.
#[must_use]
pub fn is_live(db: &Path) -> bool {
    ["-wal", "-journal"].iter().any(|suffix| {
        let mut name = db.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name).exists()
    })
}

/// The file to read for `db`: `db` itself when it is closed, else a
/// snapshot written to `work_dir`.
///
/// # Errors
///
/// The library cannot be opened or stays locked for 10 s, or the copy
/// cannot be written or fails its integrity check.
pub fn prepare(db: &Path, work_dir: &Path) -> anyhow::Result<Source> {
    if !is_live(db) {
        return Ok(Source {
            path: db.to_path_buf(),
            snapshot: false,
        });
    }
    let target = work_dir.join(SNAPSHOT_FILE);
    copy(db, &target)?;
    Ok(Source {
        path: target,
        snapshot: true,
    })
}

/// Copies `source` to `target` (replaced) in one backup step.
fn copy(source: &Path, target: &Path) -> anyhow::Result<()> {
    let context = || format!("cannot snapshot {}", source.display());
    let src = Connection::open_with_flags(
        source,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .with_context(context)?;
    src.busy_timeout(Duration::from_secs(5))
        .with_context(context)?;
    match std::fs::remove_file(target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(e).with_context(|| format!("cannot replace {}", target.display()));
        }
    }
    let mut dst =
        Connection::open(target).with_context(|| format!("cannot create {}", target.display()))?;
    {
        let backup = Backup::new(&src, &mut dst).with_context(context)?;
        let mut done = false;
        for _ in 0..BUSY_RETRIES {
            if backup.step(-1).with_context(context)? == StepResult::Done {
                done = true;
                break;
            }
            thread::sleep(BUSY_PAUSE);
        }
        if !done {
            anyhow::bail!(
                "{} stayed locked: close the Shelfy desktop app and try again",
                source.display()
            );
        }
    }
    drop(src);
    // The copy inherits the WAL flag; a rollback journal keeps it one file,
    // which the legacy reader then opens immutable.
    let mode: String = dst
        .pragma_update_and_check(None, "journal_mode", "DELETE", |row| row.get(0))
        .with_context(context)?;
    if !mode.eq_ignore_ascii_case("delete") {
        anyhow::bail!("the snapshot stayed in {mode} mode");
    }
    let check: String = dst
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .with_context(context)?;
    if check != "ok" {
        anyhow::bail!("the snapshot failed its integrity check");
    }
    dst.close().map_err(|(_, e)| e).with_context(context)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_closed_library_is_read_in_place_and_a_live_one_is_copied() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("shelfy.sqlite");
        let work = dir.path().join("work");
        std::fs::create_dir(&work).unwrap();
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch("CREATE TABLE t (x); INSERT INTO t VALUES (1);")
                .unwrap();
        }
        let closed = prepare(&db, &work).unwrap();
        assert_eq!(
            closed,
            Source {
                path: db.clone(),
                snapshot: false
            }
        );

        // A writer holds the library open in WAL mode, with a commit still in
        // the WAL.
        let live = Connection::open(&db).unwrap();
        live.pragma_update(None, "journal_mode", "WAL").unwrap();
        live.execute("INSERT INTO t VALUES (2)", []).unwrap();
        assert!(is_live(&db));
        let source = prepare(&db, &work).unwrap();
        assert!(source.snapshot);
        assert_eq!(source.path, work.join(SNAPSHOT_FILE));
        assert!(!is_live(&source.path), "the snapshot is a single file");
        let copy = Connection::open(&source.path).unwrap();
        let rows: i64 = copy
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 2, "the snapshot has the committed WAL content");
    }
}
