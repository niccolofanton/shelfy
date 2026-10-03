//! Nightly CAS collection. The library writer protects the fresh reference
//! check, file removal and quota release. Trash remains a reference. Files
//! untouched in `.tmp` for a day and taxonomy plans whose jobs were removed
//! are swept too; exports, uploads and capture/video caches have their own owners.
use std::sync::Arc;
use std::time::Duration;

use rusqlite::params;
use shelfy_core::db::{ControlDb, DbError, UserDb};
use shelfy_core::repo::RepoError;
use shelfy_media::refs;
use shelfy_media::store::{MediaStore, UserMedia};

use super::{AttemptFence, JobContext, JobError, JobResult, Kind, KindSpec, Outcome, codes, usage};
use crate::error::{ApiError, ErrorCode};
use crate::quota::Quotas;
use crate::state::AppState;
use crate::telemetry::metrics::{GC_BYTES_FREED_TOTAL, GC_OBJECTS_DELETED_TOTAL};

pub const KIND: &str = "gc";
const EXPORT_ACTIVE: &str = "gc_export_active";
const EXPORT_WAIT_MS: i64 = 5_000;
/// At most 500 object rows and their files per writer transaction.
pub const CHUNK: u32 = 500;
pub const RETENTION: Duration = Duration::from_secs(24 * 3600);

#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(1)
            .per_user(1)
            .max_attempts(3)
            .lease(Duration::from_secs(3600))
            .nightly(true),
        run,
    )
}

/// Aggregates; never includes object names, user content or file paths.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Report {
    pub objects: u64,
    pub bytes: u64,
    pub temporary_files: usize,
    pub taxonomy_plans: usize,
}

/// Collects an existing user's library through `cutoff` (Unix ms).
/// Resets may pass now; the nightly job passes now minus 24 hours.
/// A missing library stays missing. Usage is recounted on successful completion.
///
/// # Errors
///
/// Locked libraries return `user_locked`; database or filesystem failures.
pub async fn collect_user(state: &AppState, user: &str, cutoff: i64) -> Result<Report, ApiError> {
    let report = collect(state, user, cutoff, None)
        .await
        .map_err(api_error)?;
    usage::recount(state, user).await?;
    Ok(report)
}

fn api_error(error: JobError) -> ApiError {
    if error.code() == EXPORT_ACTIVE {
        ApiError::new(ErrorCode::Unavailable)
            .with_retry_after(5)
            .with_detail("collection waits for the active export")
    } else if error.is_user_locked() {
        ApiError::user_locked()
    } else {
        ApiError::internal(error)
    }
}

async fn run(ctx: JobContext) -> JobResult {
    let cutoff = ctx.jobs().clock().now_ms().saturating_sub(86_400_000);
    let result = collect(ctx.state(), ctx.user_id(), cutoff, Some(&ctx)).await;
    // Even a partial collection should correct DB-size accounting afterwards.
    if let Err(error) = usage::enqueue(ctx.jobs(), ctx.user_id()).await {
        tracing::warn!(job_id = ctx.id(), %error, "cannot enqueue usage after GC");
    }
    if ctx.is_paused()
        && !ctx.is_cancelled()
        && matches!(&result, Err(error) if error.code() == codes::CANCELLED)
    {
        return Ok(Outcome::Requeue { run_at: None });
    }
    match result {
        Err(error) if error.code() == EXPORT_ACTIVE => Ok(Outcome::Requeue {
            run_at: Some(ctx.jobs().clock().now_ms().saturating_add(EXPORT_WAIT_MS)),
        }),
        other => other.map(|_| Outcome::Succeeded),
    }
}

fn current(fence: Option<&AttemptFence>) -> Result<(), JobError> {
    if let Some(fence) = fence
        && !fence.is_current()?
    {
        return Err(JobError::cancelled());
    }
    Ok(())
}

async fn task<T, F>(
    state: &AppState,
    user: &str,
    ctx: Option<&JobContext>,
    f: F,
) -> Result<T, JobError>
where
    T: Send + 'static,
    F: FnOnce(&UserDb) -> Result<T, JobError> + Send + 'static,
{
    if let Some(ctx) = ctx {
        return ctx.user_db(f).await;
    }
    let cache = Arc::clone(state.user_dbs());
    let user = user.to_owned();
    tokio::task::spawn_blocking(move || {
        let db = cache.get(&user)?;
        f(&db)
    })
    .await
    .map_err(|e| JobError::transient(codes::INTERNAL).with_detail(e.to_string()))?
}

async fn collect(
    state: &AppState,
    user: &str,
    cutoff: i64,
    ctx: Option<&JobContext>,
) -> Result<Report, JobError> {
    let media = MediaStore::new(state.config().data_dir.users_dir())
        .user(user)
        .map_err(|_| JobError::from(DbError::InvalidUserId))?;
    let users_dir = state.config().data_dir.users_dir();
    let lock_user = user.to_owned();
    let locked = tokio::task::spawn_blocking(move || {
        shelfy_core::db::is_library_locked(&users_dir, &lock_user)
    })
    .await
    .map_err(|e| JobError::transient(codes::INTERNAL).with_detail(e.to_string()))??;
    if locked {
        return Err(JobError::from(DbError::Locked));
    }
    if !tokio::fs::try_exists(state.config().data_dir.library_db(user))
        .await
        .map_err(|e| JobError::from(DbError::Io(e)))?
    {
        return Ok(Report::default());
    }
    let now = state.jobs().clock().now_ms();
    let fence = ctx.map(JobContext::attempt_fence);
    let first_fence = fence.clone();
    task(state, user, ctx, move |db| {
        db.write(|tx| {
            current(first_fence.as_ref())?;
            refs::restamp(tx, now)?;
            Ok::<_, JobError>(())
        })
    })
    .await?;
    let mut report = Report::default();
    loop {
        if let Some(ctx) = ctx {
            if ctx.is_cancelled() {
                return Err(JobError::cancelled());
            }
            if ctx.is_paused() {
                return Err(JobError::cancelled());
            }
        }
        let control = Arc::clone(state.control());
        let (media, quotas, user, fence) = (
            media.clone(),
            state.quota().clone(),
            user.to_owned(),
            fence.clone(),
        );
        let chunk = task(state, &user.clone(), ctx, move |db| {
            collect_chunk(db, &media, &quotas, &control, &user, cutoff, fence.as_ref())
        })
        .await?;
        report.objects += chunk.objects;
        report.bytes += chunk.bytes;
        if chunk.objects == 0 {
            break;
        }
        if let Some(ctx) = ctx {
            ctx.progress(None, Some(KIND)).await;
        }
    }
    let (control, user, fence) = (Arc::clone(state.control()), user.to_owned(), fence.clone());
    let swept = task(state, &user.clone(), ctx, move |db| {
        db.write(|tx| {
            current(fence.as_ref())?;
            let temporary_files = media.sweep_temp(RETENTION).map_err(DbError::Io)?;
            Ok::<_, JobError>((temporary_files, prune_taxonomy(tx, &control, &user)?))
        })
    })
    .await?;
    report.temporary_files = swept.0;
    report.taxonomy_plans = swept.1;
    Ok(report)
}

/// One bounded transaction; called by the worker and operator command.
pub(crate) fn collect_chunk(
    db: &UserDb,
    media: &UserMedia,
    quotas: &Quotas,
    control: &ControlDb,
    user: &str,
    cutoff: i64,
    fence: Option<&AttemptFence>,
) -> Result<Report, JobError> {
    let report = db.write(|tx| {
        current(fence)?;
        // Export snapshots include every object row, even unreferenced ones.
        // Its durable admission plus the export's writer barrier pins the CAS
        // until the worker reaches a final state, across processes too.
        let export_active = control.read(|conn| {
            Ok::<_, RepoError>(conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM jobs WHERE user_id=?1 AND kind='export' AND state IN ('queued','running'))",
                [user], |r| r.get::<_, bool>(0))?)
        })?;
        if export_active { return Err(JobError::transient(EXPORT_ACTIVE)); }
        let garbage = refs::collect_garbage(tx, media, cutoff, CHUNK)?;
        let bytes = garbage.iter().fold(0_u64, |sum, item| {
            sum.saturating_add(u64::try_from(item.size).unwrap_or(0))
        });
        quotas.release(user, bytes)?;
        Ok::<_, JobError>(Report {
            objects: garbage.len() as u64,
            bytes,
            ..Report::default()
        })
    })?;
    // The blocking task records committed chunks even if its async caller is aborted.
    metrics::counter!(GC_OBJECTS_DELETED_TOTAL).increment(report.objects);
    metrics::counter!(GC_BYTES_FREED_TOTAL).increment(report.bytes);
    Ok(report)
}

/// Remove only plans/finished markers whose corresponding job row is gone.
/// Failed/cancelled rows remain retryable until job retention removes them.
pub(crate) fn prune_taxonomy(
    conn: &rusqlite::Connection,
    control: &ControlDb,
    user: &str,
) -> Result<usize, RepoError> {
    let mut after = Vec::<u8>::new();
    let mut removed = 0;
    loop {
        let keys = conn.prepare_cached("SELECT key_hash FROM ai_cache WHERE kind='taxonomy.run' AND key_hash>?1 ORDER BY key_hash LIMIT 500")?
            .query_map([&after], |row| row.get::<_, Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        if keys.is_empty() {
            break;
        }
        let absent = control.read(|control| {
            let mut absent = Vec::new();
            let mut exists = control
                .prepare_cached("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND user_id=?2)")?;
            for key in &keys {
                let Ok(bytes) = <[u8; 8]>::try_from(key.as_slice()) else {
                    continue;
                };
                if !exists.query_row(params![i64::from_be_bytes(bytes), user], |r| {
                    r.get::<_, bool>(0)
                })? {
                    absent.push(key);
                }
            }
            Ok::<_, RepoError>(absent)
        })?;
        for key in absent {
            removed += conn.execute(
                "DELETE FROM ai_cache WHERE kind='taxonomy.run' AND key_hash=?1",
                [key],
            )?;
        }
        after = keys.last().expect("nonempty keys").clone();
    }
    Ok(removed)
}
