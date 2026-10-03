//! Re-creatable bundles, excluded from user quotas and bounded by one live bundle
//! per user, available disk reservations and seven-day retention.
pub mod bundle;
use crate::config::create_private_dir;
use crate::control::{exports as rows, jobs as job_rows};
use crate::error::{ApiError, ErrorCode};
use crate::ids::{new_ulid, now_ms};
use crate::jobs::export::{KIND, Payload};
use crate::jobs::{JobContext, JobError};
use crate::state::{AppState, blocking};
use rusqlite::params;
use shelfy_core::repo::RepoError;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

pub const TTL: Duration = Duration::from_secs(7 * 86_400);
/// IDs are never accepted as arbitrary paths, even in durable job payloads.
pub fn directory(state: &AppState, user: &str) -> PathBuf {
    state
        .config()
        .data_dir
        .users_dir()
        .join(user)
        .join("exports")
}
pub fn valid_id(id: &str) -> bool {
    id.len() == 26
        && id
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit())
}
pub fn file(dir: &Path, id: &str) -> PathBuf {
    dir.join(format!("{id}.zip"))
}

/// A conservative bound: archive + online snapshot + metadata/ZIP overhead.
pub fn estimate(db: &rusqlite::Connection) -> rusqlite::Result<u64> {
    let pages = db
        .query_row("PRAGMA page_count", [], |r| r.get::<_, i64>(0))?
        .max(0) as u64;
    let page_size = db
        .query_row("PRAGMA page_size", [], |r| r.get::<_, i64>(0))?
        .max(0) as u64;
    let (media, count): (i64, i64) = db.query_row(
        "SELECT coalesce(sum(bytes),0), count(*) FROM media_objects",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(pages
        .saturating_mul(page_size)
        .saturating_mul(3)
        .saturating_add(media.max(0) as u64)
        .saturating_add((count.max(0) as u64).saturating_mul(2048))
        .saturating_add(1024 * 1024))
}
/// Creates export metadata and its queued job in the same control transaction.
/// Repeating a request returns the single live bundle (including a ready one).
pub async fn enqueue(state: &AppState, user: &str) -> Result<rows::Export, ApiError> {
    let db = state.user_db(user).await?;
    let amount =
        blocking(move || db.read(|c| estimate(c).map_err(shelfy_core::db::DbError::from))).await?;
    let dir = directory(state, user);
    let control = Arc::clone(state.control());
    let user = user.to_owned();
    let (export, job) = blocking(move || -> Result<(rows::Export, Option<job_rows::JobRow>), ApiError> {
        create_private_dir(&dir).map_err(ApiError::internal)?;
        let free = fs2::available_space(&dir).map_err(ApiError::internal)?;
        let now = now_ms();
        control.write(|tx| -> Result<(rows::Export, Option<job_rows::JobRow>), ApiError> {
            if let Some(existing) = rows::list(tx, &user, now)?.into_iter().next() { return Ok((existing, None)); }
            // Expiry makes room for the next bundle; cleanup is handled by the sweep.
            tx.execute("UPDATE exports SET deleted_at=?2 WHERE user_id=?1 AND expires_at<=?2 AND deleted_at IS NULL", params![user,now]).map_err(ApiError::internal)?;
            let reserved: i64 = tx.query_row("SELECT coalesce(sum(estimated_bytes),0) FROM exports WHERE bytes IS NULL AND deleted_at IS NULL AND expires_at>?1", [now], |r| r.get(0)).map_err(ApiError::internal)?;
            if amount > free.saturating_sub(reserved.max(0) as u64) { return Err(ApiError::new(ErrorCode::StorageFull)); }
            let id = new_ulid();
            let payload = serde_json::to_string(&Payload { export_id: id.clone() }).map_err(ApiError::internal)?;
            let new = job_rows::NewJobRow { user_id: &user, kind: KIND, dedupe_key: Some(KIND), priority: 0, payload_json: &payload, max_attempts: 2, run_at: now };
            let job = match job_rows::insert(tx, &new, now)? {
                job_rows::Inserted::Created(job) => job,
                job_rows::Inserted::Existing(_) => return Err(ApiError::new(ErrorCode::Conflict)),
            };
            let expires = now.saturating_add(TTL.as_millis() as i64);
            tx.execute("INSERT INTO exports (id,user_id,job_id,created_at,expires_at,estimated_bytes) VALUES (?1,?2,?3,?4,?5,?6)", params![id,user,job.id,now,expires,i64::try_from(amount).unwrap_or(i64::MAX)]).map_err(ApiError::internal)?;
            Ok((rows::get(tx, &user, &id, now)?.ok_or_else(ApiError::not_found)?, Some(job)))
        })
    }).await?;
    if let Some(job) = job {
        state.jobs().admit_committed(&job);
    }
    Ok(export)
}

struct Scratch {
    paths: Vec<PathBuf>,
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for path in &self.paths {
            let _ = fs::remove_file(path);
        }
    }
}
/// Builds on the blocking pool; cancellation and lease ownership fence publication.
pub async fn run(ctx: &JobContext, id: &str) -> Result<(), JobError> {
    if !valid_id(id) {
        return Err(JobError::permanent("invalid_payload"));
    }
    let user = ctx.user_id().to_owned();
    let control = Arc::clone(ctx.state().control());
    let id = id.to_owned();
    let existing = {
        let (control, user, id) = (control.clone(), user.clone(), id.clone());
        blocking(move || control.read(|c| rows::get(c, &user, &id, now_ms()))).await?
    }
    .ok_or_else(JobError::cancelled)?;
    if existing.job_id != ctx.id() {
        return Err(JobError::permanent("invalid_payload"));
    }
    if existing.bytes.is_some() {
        return Ok(());
    }
    ctx.progress(Some(0.0), Some("snapshot")).await;
    let dir = directory(ctx.state(), &user);
    let snapshot = dir.join(format!("{id}.sqlite"));
    let part = dir.join(format!("{id}.zip.part"));
    let entries = dir.join(format!("{id}.entries"));
    let users = ctx.state().config().data_dir.users_dir();
    let context = ctx.clone();
    ctx.user_db(move |library| {
        create_private_dir(&dir)
            .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
        let lock = fs::File::options()
            .create(true)
            .truncate(false)
            .write(true)
            .open(dir.join(format!("{id}.lock")))
            .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
        lock.try_lock()
            .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
        let _cleanup = Scratch {
            paths: vec![
                snapshot.clone(),
                part.clone(),
                entries.clone(),
                snapshot.with_extension("sqlite-journal"),
                snapshot.with_extension("sqlite-wal"),
                snapshot.with_extension("sqlite-shm"),
            ],
        };
        for path in [&snapshot, &part, &entries] {
            let _ = fs::remove_file(path);
        }
        let amount = library.read(|c| estimate(c).map_err(shelfy_core::db::DbError::from))?;
        let free = fs2::available_space(&dir)
            .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
        if amount > free {
            return Err(JobError::permanent("storage_full"));
        }
        // Export admission is durable before this barrier. A GC chunk that
        // checked for exports before admission may still hold the writer; let
        // it commit before taking the snapshot. Subsequent GC chunks observe
        // this queued/running export under the same writer and wait.
        library.write(|_| Ok::<_, shelfy_core::db::DbError>(()))?;
        library
            .read(|source| {
                bundle::snapshot(source, &snapshot, context.token(), &|| context.heartbeat())
            })
            .map_err(|e| {
                if context.is_cancelled() {
                    JobError::cancelled()
                } else {
                    JobError::transient("unavailable").with_detail(e.to_string())
                }
            })?;
        let size = bundle::write(
            bundle::Build {
                snapshot: &snapshot,
                users: &users,
                user: &user,
                part: &part,
                entries_path: &entries,
                created_at: existing.created_at,
            },
            context.token(),
            || context.heartbeat(),
        )
        .map_err(|e| {
            if context.is_cancelled() {
                JobError::cancelled()
            } else {
                JobError::permanent("validation_failed").with_detail(e.to_string())
            }
        })?;
        // Delete and publication share the control writer. The attempt fence rejects
        // stale workers after cancellation, lease loss or a recovered retry.
        control.write(|tx| -> Result<(), JobError> {
            let fence = job_rows::Fence {
                id: context.id(),
                attempts: context.attempt() - 1,
            };
            if context.is_cancelled()
                || !job_rows::is_current(tx, fence)?
                || rows::get(tx, &user, &id, now_ms())?.is_none()
            {
                return Err(JobError::cancelled());
            }
            fs::rename(&part, file(&dir, &id))
                .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
            fs::File::open(&dir)
                .and_then(|f| f.sync_all())
                .map_err(|e| JobError::transient("unavailable").with_detail(e.to_string()))?;
            tx.execute(
                "UPDATE exports SET bytes=?2 WHERE id=?1",
                params![id, i64::try_from(size).unwrap_or(i64::MAX)],
            )
            .map_err(RepoError::from)?;
            Ok(())
        })?;
        Ok(())
    })
    .await?;
    ctx.progress(Some(1.0), Some("ready")).await;
    Ok(())
}
/// Removes files first, rows second: failed removals remain retryable. Active
/// workers retain their tombstones until they have stopped and dropped their lock.
pub async fn sweep(state: &AppState, now: i64) -> usize {
    // Paused queues and never-started jobs still obey retention. Mark tombstones
    // before cancellation so publication cannot race the sweep.
    let control = Arc::clone(state.control());
    let expired = blocking(move || control.write(|tx| -> Result<Vec<(String,i64)>, RepoError> {
        tx.execute("UPDATE exports SET deleted_at=?1 WHERE expires_at<=?1 AND deleted_at IS NULL", [now])?;
        Ok(tx.prepare("SELECT user_id,job_id FROM exports WHERE deleted_at IS NOT NULL AND EXISTS (SELECT 1 FROM jobs WHERE jobs.id=exports.job_id AND state IN ('queued','running'))")?
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    })).await;
    match expired {
        Ok(jobs) => {
            for (user, id) in jobs {
                let _ = state.jobs().cancel(&user, id).await;
            }
        }
        Err(err) => {
            tracing::warn!(error=%err, "expiring export workers failed");
            return 0;
        }
    }
    let control = Arc::clone(state.control());
    let users = state.config().data_dir.users_dir();
    let result = blocking(move || -> Result<usize, ApiError> {
        let stale: Vec<(String,String)> = control.read(|c| -> Result<_, RepoError> {
            Ok(c.prepare("SELECT id,user_id FROM exports WHERE (expires_at<=?1 OR deleted_at IS NOT NULL) AND NOT EXISTS (SELECT 1 FROM jobs WHERE jobs.id=exports.job_id AND state IN ('queued','running'))")?
                .query_map([now], |r| Ok((r.get(0)?,r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
        })?;
        let mut removed = 0;
        for (id,user) in stale {
            if !valid_id(&id) || !shelfy_core::db::is_valid_user_id(&user) { continue; }
            let dir = users.join(user).join("exports");
            let lock_path = dir.join(format!("{id}.lock"));
            let lock = fs::File::options().write(true).create(true).truncate(false).open(&lock_path);
            let Ok(lock) = lock else {
                if !dir.exists() { control.write(|tx| tx.execute("DELETE FROM exports WHERE id=?1", [&id]).map_err(RepoError::from))?; removed += 1; }
                continue;
            };
            if lock.try_lock().is_err() { continue; }
            let paths = [file(&dir,&id), dir.join(format!("{id}.zip.part")), dir.join(format!("{id}.sqlite")), dir.join(format!("{id}.entries"))];
            if paths.iter().all(|p| fs::remove_file(p).is_ok() || !p.exists()) {
                control.write(|tx| tx.execute("DELETE FROM exports WHERE id=?1", [&id]).map_err(RepoError::from))?;
                removed += 1;
                drop(lock);
                let _ = fs::remove_file(lock_path);
            }
        }
        Ok(removed)
    }).await;
    match result {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error=%e, "export sweep failed");
            0
        }
    }
}
