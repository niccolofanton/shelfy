//! Whether the desktop app holds its library open (plan §4.1 step 3: `plan`
//! "refuses if the desktop app holds it open for writing"; OI-8).
//!
//! The desktop opens `shelfy.sqlite` in WAL mode when it starts and keeps the
//! connection until it quits, so it may write at any moment while it runs.
//! On Unix every open WAL connection holds two POSIX advisory locks, which
//! SQLite takes and the kernel tracks per process:
//!
//! - a read lock on the database file's shared range (bytes `0x40000002` to
//!   `0x400001ff`), from its first read until it closes;
//! - a read lock on the "dead man switch" byte of the `-shm` file (byte
//!   128), while it has the WAL index mapped.
//!
//! [`state`] asks the kernel with `F_GETLK` whether another process holds
//! either lock: it takes no lock and writes nothing. Elsewhere (Windows) a
//! `-wal` file next to the library stands for an open connection, as T9's
//! snapshot rule does ([`crate::snapshot`]).
//!
//! Call it before this process opens the library: POSIX locks belong to the
//! process, and closing any descriptor of the file drops all of them.

use std::path::{Path, PathBuf};

/// Where SQLite's lock bytes start in a database file (`PENDING_BYTE`).
const PENDING_BYTE: i64 = 0x4000_0000;
/// The shared range: `SHARED_FIRST` and `SHARED_SIZE`.
const SHARED_FIRST: i64 = PENDING_BYTE + 2;
const SHARED_SIZE: i64 = 510;
/// The dead man switch byte of the `-shm` file (`UNIX_SHM_DMS`).
const SHM_DMS: i64 = 128;

/// Whether another program has the library open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopState {
    /// Nobody holds it.
    Closed,
    /// Another process holds it open: the desktop app (its process id when
    /// the system tells it).
    Open {
        /// The process holding a lock.
        pid: Option<u32>,
    },
}

impl DesktopState {
    /// Whether the library is open elsewhere.
    #[must_use]
    pub fn is_open(self) -> bool {
        matches!(self, DesktopState::Open { .. })
    }
}

fn with_suffix(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Whether another process has the library at `db` open.
///
/// # Errors
///
/// The library cannot be opened for reading.
pub fn state(db: &Path) -> std::io::Result<DesktopState> {
    #[cfg(unix)]
    {
        let file = std::fs::File::open(db)?;
        if let Some(pid) = unix::conflicting_lock(&file, SHARED_FIRST, SHARED_SIZE)? {
            return Ok(DesktopState::Open { pid: Some(pid) });
        }
        if let Ok(shm) = std::fs::File::open(with_suffix(db, "-shm"))
            && let Some(pid) = unix::conflicting_lock(&shm, SHM_DMS, 1)?
        {
            return Ok(DesktopState::Open { pid: Some(pid) });
        }
        Ok(DesktopState::Closed)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(db)?;
        let _ = (SHARED_FIRST, SHARED_SIZE, SHM_DMS);
        Ok(if with_suffix(db, "-wal").exists() {
            DesktopState::Open { pid: None }
        } else {
            DesktopState::Closed
        })
    }
}

#[cfg(unix)]
mod unix {
    use std::fs::File;
    use std::io;
    use std::os::fd::AsRawFd as _;

    /// The process holding a lock that conflicts with a write lock on
    /// `len` bytes from `start` of `file`, if any (`F_GETLK`).
    // The C types differ between systems: a cast is a no-op on some.
    #[allow(unsafe_code, clippy::unnecessary_cast)]
    pub(super) fn conflicting_lock(file: &File, start: i64, len: i64) -> io::Result<Option<u32>> {
        // SAFETY: `flock` is a plain C struct of integers, for which all
        // zero bytes are a valid value; the fields that matter are set below.
        let mut lock: libc::flock = unsafe { std::mem::zeroed() };
        lock.l_type = libc::F_WRLCK as libc::c_short;
        lock.l_whence = libc::SEEK_SET as libc::c_short;
        lock.l_start = start as libc::off_t;
        lock.l_len = len as libc::off_t;
        // SAFETY: `F_GETLK` reads the open descriptor that `file` owns for
        // the whole call and writes only into `lock`, a valid `flock` on this
        // stack frame. It sets no lock.
        let rc = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETLK, &mut lock) };
        if rc == -1 {
            return Err(io::Error::last_os_error());
        }
        if i64::from(lock.l_type) == i64::from(libc::F_UNLCK) {
            Ok(None)
        } else {
            Ok(Some(u32::try_from(lock.l_pid).unwrap_or(0)))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{BufRead as _, BufReader, Write as _};
    use std::process::{Command, Stdio};

    use rusqlite::Connection;

    use super::*;

    const HOLD_ENV: &str = "SHELFY_TEST_HOLD_DB";

    /// Not a test of its own: the child process of the test below runs it to
    /// hold a library open as the desktop app does, until its stdin closes.
    #[test]
    fn hold_library_for_parent() {
        let Ok(path) = std::env::var(HOLD_ENV) else {
            return;
        };
        let conn = Connection::open(&path).unwrap();
        conn.pragma_update(None, "journal_mode", "WAL").unwrap();
        let rows: i64 = conn
            .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        println!("ready");
        std::io::stdout().flush().unwrap();
        let mut line = String::new();
        let _ = std::io::stdin().read_line(&mut line);
        drop(conn);
    }

    #[test]
    fn a_library_another_process_holds_open_is_seen() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("shelfy.sqlite");
        {
            let conn = Connection::open(&db).unwrap();
            conn.execute_batch(
                "PRAGMA journal_mode = WAL; CREATE TABLE t (x); INSERT INTO t VALUES (1);",
            )
            .unwrap();
        }
        assert_eq!(state(&db).unwrap(), DesktopState::Closed);

        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "desktop::tests::hold_library_for_parent",
                "--nocapture",
                "--test-threads=1",
            ])
            .env(HOLD_ENV, &db)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut out = BufReader::new(child.stdout.take().unwrap());
        let mut line = String::new();
        while !line.contains("ready") {
            line.clear();
            assert!(out.read_line(&mut line).unwrap() > 0, "the child stopped");
        }
        let seen = state(&db).unwrap();
        #[cfg(unix)]
        assert_eq!(
            seen,
            DesktopState::Open {
                pid: Some(child.id())
            }
        );
        assert!(seen.is_open());

        drop(child.stdin.take());
        assert!(child.wait().unwrap().success());
        #[cfg(unix)]
        assert_eq!(state(&db).unwrap(), DesktopState::Closed);
        assert!(state(&dir.path().join("missing.sqlite")).is_err());
    }
}
