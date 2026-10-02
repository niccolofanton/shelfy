//! The maintenance lock of a user's library (plan §3.5, the restore
//! runbooks).
//!
//! `shelfy-server admin user lock <id>` creates the marker file
//! `users/<id>/LOCKED` ([`lock_library`]); `admin user unlock <id>` removes
//! it ([`unlock_library`]). While it exists:
//!
//! - [`UserDbCache::get`](super::UserDbCache::get) refuses the user with
//!   [`DbError::Locked`] and releases its cached handle, so the server stops
//!   using the library and closes it;
//! - [`UserDbCache::run_maintenance`](super::UserDbCache::run_maintenance)
//!   releases the cached handles of locked users even when no request comes;
//! - the server answers the user's requests with 423 `user_locked`, and the
//!   upgrade sweep and `admin snapshot` leave the library alone.
//!
//! The lock is a file next to the library rather than a row in the control
//! database: it guards the library file, every opener (the API, jobs, the
//! sweep, the operator commands) checks the same place before opening it,
//! the operator commands run in another process, and no schema change is
//! needed. The media backup excludes the marker, so a restored host has no
//! stale locks.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use super::DbError;
use super::cache::validate_user_id;

/// Name of the marker file inside `<users_dir>/<user_id>/`.
pub const LOCK_FILE_NAME: &str = "LOCKED";

/// Path of `user_id`'s lock marker under `users_dir`.
///
/// # Errors
///
/// [`DbError::InvalidUserId`] unless `user_id` is 1–64 ASCII letters and
/// digits.
pub fn library_lock_path(users_dir: &Path, user_id: &str) -> Result<PathBuf, DbError> {
    validate_user_id(user_id)?;
    Ok(users_dir.join(user_id).join(LOCK_FILE_NAME))
}

/// Whether `user_id`'s library is locked. One `stat`.
///
/// # Errors
///
/// [`DbError::InvalidUserId`], or the file system refused the check.
pub fn is_library_locked(users_dir: &Path, user_id: &str) -> Result<bool, DbError> {
    let path = library_lock_path(users_dir, user_id)?;
    Ok(path.try_exists()?)
}

/// Locks `user_id`'s library: creates its directory if needed (mode 0750)
/// and the marker, with `note` as its content, durably. Returns `false` when
/// it was already locked (the marker is left as it was).
///
/// # Errors
///
/// [`DbError::InvalidUserId`], or the file system refused.
pub fn lock_library(users_dir: &Path, user_id: &str, note: &str) -> Result<bool, DbError> {
    let path = library_lock_path(users_dir, user_id)?;
    let dir = path
        .parent()
        .expect("the marker is inside the user directory");
    create_private_dir(dir)?;
    let mut file = match OpenOptions::new().write(true).create_new(true).open(&path) {
        Ok(file) => file,
        Err(err) if err.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(err) => return Err(err.into()),
    };
    file.write_all(note.as_bytes())?;
    file.sync_all()?;
    sync_dir(dir)?;
    Ok(true)
}

/// Unlocks `user_id`'s library. Returns `false` when it was not locked.
///
/// # Errors
///
/// [`DbError::InvalidUserId`], or the file system refused.
pub fn unlock_library(users_dir: &Path, user_id: &str) -> Result<bool, DbError> {
    let path = library_lock_path(users_dir, user_id)?;
    match fs::remove_file(&path) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(err) => return Err(err.into()),
    }
    if let Some(dir) = path.parent() {
        sync_dir(dir)?;
    }
    Ok(true)
}

/// Creates `dir` and its missing parents; new directories get mode 0750
/// (plan §2.5).
fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o750);
    }
    builder.create(dir)
}

/// Makes a change to `dir`'s entries durable; a no-op where directories
/// cannot be opened.
fn sync_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "01J9Z3B8K4QW6TFX0V7G2N5RCA";

    #[test]
    fn a_lock_is_a_marker_next_to_the_library() {
        let dir = tempfile::tempdir().unwrap();
        let users = dir.path().join("users");
        assert!(!is_library_locked(&users, USER).unwrap());
        assert!(!unlock_library(&users, USER).unwrap(), "nothing to unlock");

        assert!(lock_library(&users, USER, "restore").unwrap());
        assert!(is_library_locked(&users, USER).unwrap());
        let marker = users.join(USER).join(LOCK_FILE_NAME);
        assert_eq!(fs::read_to_string(&marker).unwrap(), "restore");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = fs::metadata(users.join(USER)).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o750);
        }

        // Locking twice keeps the first marker.
        assert!(!lock_library(&users, USER, "again").unwrap());
        assert_eq!(fs::read_to_string(&marker).unwrap(), "restore");

        assert!(unlock_library(&users, USER).unwrap());
        assert!(!is_library_locked(&users, USER).unwrap());
        assert!(!marker.exists());
    }

    #[test]
    fn locks_need_a_safe_user_id() {
        let dir = tempfile::tempdir().unwrap();
        for bad in ["", "..", "a/b", "a.b"] {
            assert!(matches!(
                lock_library(dir.path(), bad, ""),
                Err(DbError::InvalidUserId)
            ));
            assert!(matches!(
                is_library_locked(dir.path(), bad),
                Err(DbError::InvalidUserId)
            ));
        }
    }
}
