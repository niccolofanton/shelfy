//! What installs leave behind, removed on a timer (plan §4.3, §2.13; P1-19):
//! the server's maintenance loop calls [`sweep`] every hour.
//!
//! - **Previous libraries.** An install keeps the library it replaced (or the
//!   library as it was before a merge) as `users/<id>/library.prev-<job
//!   id>.sqlite` for [`PREVIOUS_RETENTION`] (7 days), counted from the file's
//!   modification time, which is when the copy was made. The kept file of
//!   `admin user restore-db` (`library.pre-restore-*`) is not touched.
//! - **Uploads**, of every user: unfinished ones past their expiry (24 hours,
//!   [`crate::routes::uploads::UPLOAD_TTL`]), complete ones that no install
//!   consumed within [`COMPLETE_UPLOAD_RETENTION`] (7 days, the life of a
//!   `migrate` token), and files under `work/uploads/` whose row is gone
//!   (a crash between the two) once a day old.
//! - **Install work directories** (`work/migrations/<job id>/`) whose job is
//!   not queued or running: a try removes its own, so these are left by a
//!   crash.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use crate::control::uploads;
use crate::error::ApiError;
use crate::routes::uploads::remove_files;
use crate::state::{AppState, blocking};

/// File name prefix of a kept previous library; the job id follows.
pub const PREVIOUS_PREFIX: &str = "library.prev-";
/// How long a previous library is kept.
pub const PREVIOUS_RETENTION: Duration = Duration::from_secs(7 * 86_400);
/// How long a complete upload waits for an install.
pub const COMPLETE_UPLOAD_RETENTION: Duration = Duration::from_secs(7 * 86_400);
/// How old a file under `work/uploads/` without a row must be to go.
pub const ORPHAN_UPLOAD_AGE: Duration = Duration::from_secs(86_400);
/// How often the server runs [`sweep`].
pub const INTERVAL: Duration = Duration::from_secs(3600);

/// What a [`sweep`] removed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Swept {
    /// Previous libraries past their retention.
    pub previous: usize,
    /// Upload rows (with their files) expired or never consumed.
    pub uploads: usize,
    /// Upload files without a row.
    pub orphan_files: usize,
    /// Install work directories of jobs that are over.
    pub work_dirs: usize,
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// Removes what installs left behind for every user, as of `now` (unix ms).
/// Best effort: what cannot be removed now is logged and tried at the next
/// sweep.
pub async fn sweep(state: &AppState, now: i64) -> Swept {
    let mut swept = Swept::default();
    let users_dir = state.config().data_dir.users_dir();
    match blocking(move || Ok::<_, ApiError>(previous_libraries(&users_dir, now))).await {
        Ok(n) => swept.previous = n,
        Err(err) => tracing::warn!(error = %err, "sweeping previous libraries failed"),
    }
    let control = Arc::clone(state.control());
    let uploads_dir = state.config().data_dir.uploads_dir();
    let migrations_dir = state.config().data_dir.migrations_dir();
    let result = blocking(move || -> Result<(usize, usize, usize), ApiError> {
        let mut stale = control.read(|c| uploads::expired_all(c, now))?;
        stale.extend(control.read(|c| {
            uploads::complete_before(c, now.saturating_sub(millis(COMPLETE_UPLOAD_RETENTION)))
        })?);
        if !stale.is_empty() {
            remove_files(&uploads_dir, &stale);
            control.write(|tx| uploads::delete(tx, &stale))?;
        }
        let known = control.read(uploads::all_ids)?;
        let orphans = orphan_upload_files(&uploads_dir, &known, now);
        let active = control.read(active_installs)?;
        let work_dirs = finished_work_dirs(&migrations_dir, &active);
        Ok((stale.len(), orphans, work_dirs))
    })
    .await;
    match result {
        Ok((uploads, orphan_files, work_dirs)) => {
            swept.uploads = uploads;
            swept.orphan_files = orphan_files;
            swept.work_dirs = work_dirs;
        }
        Err(err) => tracing::warn!(error = %err, "sweeping migration uploads failed"),
    }
    if swept != Swept::default() {
        tracing::info!(
            previous = swept.previous,
            uploads = swept.uploads,
            orphan_files = swept.orphan_files,
            work_dirs = swept.work_dirs,
            "removed what migration installs left behind"
        );
    }
    swept
}

/// The ids of the `migrate` jobs that are queued or running.
fn active_installs(conn: &rusqlite::Connection) -> shelfy_core::repo::Result<HashSet<i64>> {
    let ids = conn
        .prepare("SELECT id FROM jobs WHERE kind = ?1 AND state IN ('queued', 'running')")?
        .query_map([crate::jobs::migrate::KIND], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// Unix ms of a file's modification time.
fn modified_ms(meta: &fs::Metadata) -> Option<i64> {
    let since = meta.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
    i64::try_from(since.as_millis()).ok()
}

/// Removes `users/*/library.prev-*.sqlite` older than the retention.
fn previous_libraries(users_dir: &Path, now: i64) -> usize {
    let cutoff = now.saturating_sub(millis(PREVIOUS_RETENTION));
    let Ok(users) = fs::read_dir(users_dir) else {
        return 0;
    };
    let mut removed = 0;
    for user in users.flatten() {
        let Ok(files) = fs::read_dir(user.path()) else {
            continue;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let name = name.to_string_lossy();
            if !(name.starts_with(PREVIOUS_PREFIX) && name.ends_with(".sqlite")) {
                continue;
            }
            let Ok(meta) = file.metadata() else {
                continue;
            };
            if !meta.is_file() || modified_ms(&meta).is_none_or(|at| at > cutoff) {
                continue;
            }
            let path = file.path();
            match remove_with_side_files(&path) {
                Ok(()) => removed += 1,
                Err(err) => {
                    tracing::warn!(error = %err, "cannot remove a previous library");
                }
            }
        }
    }
    removed
}

/// Removes a database file and its `-wal`, `-shm` and `-journal` files.
fn remove_with_side_files(path: &Path) -> io::Result<()> {
    for suffix in ["-wal", "-shm", "-journal"] {
        let mut side = path.as_os_str().to_owned();
        side.push(suffix);
        match fs::remove_file(side) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    fs::remove_file(path)
}

/// Removes files under `work/uploads/` older than [`ORPHAN_UPLOAD_AGE`]
/// whose upload has no row.
fn orphan_upload_files(dir: &Path, known: &HashSet<String>, now: i64) -> usize {
    let cutoff = now.saturating_sub(millis(ORPHAN_UPLOAD_AGE));
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let id = name.strip_suffix(".part").unwrap_or(&name);
        if known.contains(id) {
            continue;
        }
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        if !meta.is_file() || modified_ms(&meta).is_none_or(|at| at > cutoff) {
            continue;
        }
        if fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Removes `work/migrations/<id>/` of every install that is not active.
fn finished_work_dirs(dir: &Path, active: &HashSet<i64>) -> usize {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let busy = name
            .to_str()
            .and_then(|n| n.parse::<i64>().ok())
            .is_some_and(|id| active.contains(&id));
        if busy || !entry.path().is_dir() {
            continue;
        }
        if fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use std::time::SystemTime;

    use super::*;
    use crate::ids::now_ms;

    fn touch(path: &Path, age: Duration) {
        fs::write(path, b"x").unwrap();
        let file = fs::File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - age).unwrap();
    }

    #[test]
    fn previous_libraries_go_after_a_week() {
        let dir = tempfile::tempdir().unwrap();
        let user = dir.path().join("01J9Z3B8K4QW6TFX0V7G2N5RCA");
        fs::create_dir_all(&user).unwrap();
        let day = Duration::from_secs(86_400);
        touch(&user.join("library.prev-1.sqlite"), 8 * day);
        touch(&user.join("library.prev-1.sqlite-wal"), 8 * day);
        touch(&user.join("library.prev-2.sqlite"), 6 * day);
        touch(
            &user.join("library.pre-restore-20261002T000000Z.sqlite"),
            30 * day,
        );
        touch(&user.join("library.sqlite"), 30 * day);
        assert_eq!(previous_libraries(dir.path(), now_ms()), 1);
        let left: HashSet<String> = fs::read_dir(&user)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            left,
            [
                "library.prev-2.sqlite",
                "library.pre-restore-20261002T000000Z.sqlite",
                "library.sqlite"
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        );
        // A week later the second one goes too.
        assert_eq!(
            previous_libraries(dir.path(), now_ms() + millis(2 * day)),
            1
        );
    }

    #[test]
    fn orphan_files_and_finished_work_dirs_go() {
        let dir = tempfile::tempdir().unwrap();
        let uploads = dir.path().join("uploads");
        fs::create_dir_all(&uploads).unwrap();
        let day = Duration::from_secs(86_400);
        touch(&uploads.join("KNOWN.part"), 3 * day);
        touch(&uploads.join("GONE.part"), 3 * day);
        touch(&uploads.join("GONE2"), 3 * day);
        touch(&uploads.join("FRESH.part"), Duration::from_secs(60));
        let known: HashSet<String> = ["KNOWN".to_owned()].into();
        assert_eq!(orphan_upload_files(&uploads, &known, now_ms()), 2);
        assert!(uploads.join("KNOWN.part").exists());
        assert!(uploads.join("FRESH.part").exists());

        let work = dir.path().join("migrations");
        for id in ["1", "2", "01ARZ3NDEKTSV4RRFFQ69G5FAV"] {
            fs::create_dir_all(work.join(id)).unwrap();
        }
        assert_eq!(finished_work_dirs(&work, &[2].into()), 2);
        assert!(work.join("2").exists());
        assert!(!work.join("1").exists());
    }
}
