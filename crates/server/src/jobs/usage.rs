//! `usage.recompute` (plan §2.13 Quota and GC; P1-17, P4-07): counts a
//! user's storage.
//!
//! Usage is the media plus the database (§2.13): the sum of the library's
//! `media_objects.bytes`, and the size of its database (pages × page size).
//! [`recount`] counts both and stores them on the user's row
//! (`users.usage_bytes`, `usage_media_bytes`, `usage_db_bytes`,
//! `usage_updated_at`), where `GET /me/usage` and the quota checks
//! ([`crate::quota`]) read them.
//!
//! Between counts, the commits and releases of [`crate::quota`] keep the
//! media bytes up to date as objects are stored and deleted; the count is
//! the truth that corrects any drift (logged when it finds one) and the only
//! measure of the database file. It runs in a write transaction of the
//! library, the lock a store holds while it records objects and commits
//! their bytes, so a count never sees a store's rows without its commit.
//!
//! **When.** Nightly for every active user (03:00 UTC, with the other
//! nightly kinds), and after the events that change usage the most: the
//! install of a migrated library ([`crate::migrations::install`]), a purge
//! (P1-11) and, later, the GC and the resets ([`enqueue`]). `GET /me/usage`
//! enqueues one when the usage was never counted. A user without a library
//! uses nothing, and none is created for them.
//!
//! **Limits.** 2 at once overall, 1 per user, 3 tries, a 5-minute lease: a
//! count is one scan of `media_objects`, milliseconds at per-user scale.

use std::time::Duration;

use shelfy_core::repo::RepoError;

use super::{Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome};
use crate::error::ApiError;
use crate::state::{AppState, blocking};

/// The kind's name.
pub const KIND: &str = "usage.recompute";

/// The kind, for the registry ([`super::kinds::registry`]).
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(2)
            .max_attempts(3)
            .lease(Duration::from_secs(300))
            .nightly(true),
        run,
    )
}

/// Enqueues a count for `user_id`; an active one (queued or running) is
/// returned instead of a second.
///
/// # Errors
///
/// The control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str) -> Result<Enqueued, ApiError> {
    jobs.enqueue(NewJob::new(user_id, KIND).dedupe(KIND)).await
}

/// What a count found, in bytes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recount {
    /// The media objects.
    pub media_bytes: i64,
    /// The library's database file.
    pub db_bytes: i64,
    /// The media bytes counted minus those the commits and releases had
    /// kept: 0 when the accounting was exact.
    pub drift_bytes: i64,
}

/// Counts `user_id`'s storage now and stores it (see the module docs).
///
/// # Errors
///
/// 423 `user_locked` while the library is locked for maintenance; database
/// errors.
pub async fn recount(state: &AppState, user_id: &str) -> Result<Recount, ApiError> {
    let library = state.config().data_dir.library_db(user_id);
    let exists = tokio::fs::try_exists(&library)
        .await
        .map_err(ApiError::internal)?;
    let quotas = state.quota().clone();
    let user = user_id.to_owned();
    let counted = if exists {
        let db = state.user_db(user_id).await?;
        blocking(move || {
            // The write lock: no store records objects meanwhile.
            db.write(|tx| -> Result<Recount, RepoError> {
                let media: i64 = tx.query_row(
                    "SELECT coalesce(sum(bytes), 0) FROM media_objects",
                    [],
                    |row| row.get(0),
                )?;
                let pages: i64 = tx.query_row("PRAGMA page_count", [], |row| row.get(0))?;
                let page_size: i64 = tx.query_row("PRAGMA page_size", [], |row| row.get(0))?;
                let db_bytes = pages.saturating_mul(page_size);
                let drift = quotas.record_count(&user, media, db_bytes)?;
                Ok(Recount {
                    media_bytes: media,
                    db_bytes,
                    drift_bytes: drift,
                })
            })
        })
        .await?
    } else {
        blocking(move || -> Result<Recount, RepoError> {
            let drift = quotas.record_count(&user, 0, 0)?;
            Ok(Recount {
                drift_bytes: drift,
                ..Recount::default()
            })
        })
        .await?
    };
    if counted.drift_bytes != 0 {
        tracing::warn!(
            user_id = %user_id,
            drift_bytes = counted.drift_bytes,
            media_bytes = counted.media_bytes,
            "the usage count corrected a drift of the media bytes"
        );
    }
    Ok(counted)
}

async fn run(ctx: JobContext) -> JobResult {
    // A locked library (`user_locked`) waits for the unlock without using a
    // try; see `JobError`.
    let counted = recount(ctx.state(), ctx.user_id())
        .await
        .map_err(JobError::from)?;
    tracing::debug!(
        job_id = ctx.id(),
        media_bytes = counted.media_bytes,
        db_bytes = counted.db_bytes,
        "usage counted"
    );
    Ok(Outcome::Succeeded)
}
