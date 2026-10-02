//! `usage.recompute` (plan §2.13 Quota and GC; P1-17): counts a user's
//! storage.
//!
//! Usage is the media plus the database (§2.13): the sum of the library's
//! `media_objects.bytes`, and the size of its database (pages × page size).
//! The job counts both on one read snapshot of the library and stores them
//! on the user's row (`users.usage_bytes`, `usage_media_bytes`,
//! `usage_db_bytes`, `usage_updated_at`), where `GET /me/usage` reads them,
//! and the quota checks of P4 will.
//!
//! **When.** Nightly for every active user (03:00 UTC, with the other
//! nightly kinds), and after the events that change usage the most: the
//! install of a migrated library ([`crate::migrations::install`]) and a
//! purge (P1-11 calls [`enqueue`]). `GET /me/usage` enqueues one when the
//! usage was never counted. Per-write increments arrive with the quotas in
//! P4. A user without a library uses nothing, and none is created for them.
//!
//! **Limits.** 2 at once overall, 1 per user, 3 tries, a 5-minute lease: a
//! count is one scan of `media_objects`, milliseconds at per-user scale.

use std::sync::Arc;
use std::time::Duration;

use shelfy_core::db::DbError;

use super::{Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome};
use crate::control::users;
use crate::error::ApiError;
use crate::state::blocking;

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
struct Counted {
    media: i64,
    db: i64,
}

async fn run(ctx: JobContext) -> JobResult {
    let library = ctx.state().config().data_dir.library_db(ctx.user_id());
    let exists = tokio::fs::try_exists(&library).await.map_err(|err| {
        JobError::transient(super::codes::UNAVAILABLE).with_detail(err.to_string())
    })?;
    let counted = if exists {
        ctx.user_db(|db| {
            db.read(|conn| {
                let media: i64 = conn.query_row(
                    "SELECT coalesce(sum(bytes), 0) FROM media_objects",
                    [],
                    |row| row.get(0),
                )?;
                let pages: i64 = conn.query_row("PRAGMA page_count", [], |row| row.get(0))?;
                let page_size: i64 = conn.query_row("PRAGMA page_size", [], |row| row.get(0))?;
                Ok::<_, DbError>(Counted {
                    media,
                    db: pages.saturating_mul(page_size),
                })
            })
            .map_err(JobError::from)
        })
        .await?
    } else {
        Counted::default()
    };
    let control = Arc::clone(ctx.state().control());
    let user_id = ctx.user_id().to_owned();
    let now = ctx.jobs().clock().now_ms();
    blocking(move || {
        control.write(|tx| users::set_usage(tx, &user_id, counted.media, counted.db, now))
    })
    .await?;
    Ok(Outcome::Succeeded)
}
