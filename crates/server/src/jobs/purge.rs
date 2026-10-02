//! `purge` (plan §2.12, §2.13; P1-11): deletes trashed posts for good.
//!
//! **When.** `POST /trash/empty` enqueues one for everything trashed until
//! the request ([`enqueue`]: the payload's `through`); the nightly schedule
//! (03:00 UTC) enqueues one per active user with an empty payload, which
//! purges what has been in the trash for 30 days or more
//! ([`shelfy_core::trash::retention_cutoff`] on the job system's clock).
//! Posts trashed after the cutoff, and posts restored meanwhile, stay.
//!
//! **Work.** In chunks of 500 posts, oldest trash first, one write
//! transaction per chunk ([`shelfy_core::trash::purge`]): the index rows, the
//! post rows and what cascades from them (slides, tags, entities,
//! memberships, captures) go, and media objects that lose their last
//! reference get `unreferenced_since`. Deleting their files is the GC's
//! (P4). Each chunk is announced (`posts.changed`, reason `delete`, and
//! `stats.changed`) and reported on `job.updated` (`stage` `purge`). Once
//! posts were purged, the user's storage is counted again
//! ([`super::usage::enqueue`]). A user without a library has nothing to
//! purge, and none is created.
//!
//! **Idempotent.** A purged post is gone: a try that starts over, or a
//! second purge, finds only what is left.
//!
//! **Limits** (§2.12 `purge`, `gc`): one at a time overall, three tries, a
//! 60-minute lease renewed by every chunk.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use shelfy_core::bulk::CHUNK;
use shelfy_core::trash;

use super::{
    Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome, codes, usage,
};
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::library::{self, event_keys};

/// The kind's name.
pub const KIND: &str = "purge";
/// The `stage` of a purge's progress.
pub const STAGE: &str = "purge";
/// Tries of a purge.
pub const MAX_ATTEMPTS: u32 = 3;
/// How long a purge may go without a sign of life: every chunk is one.
pub const LEASE: Duration = Duration::from_secs(3600);

/// The kind, for the registry ([`super::kinds::registry`]).
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(1)
            .per_user(1)
            .max_attempts(MAX_ATTEMPTS)
            .lease(LEASE)
            .nightly(true),
        run,
    )
}

/// What a purge deletes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// The posts trashed at or before this time, unix ms (emptying the
    /// trash). Absent, as in the nightly job: those trashed at least 30 days
    /// ago.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<i64>,
}

/// Enqueues the purge of every post `user_id` trashed at or before
/// `through` (emptying the trash).
///
/// # Errors
///
/// The control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str, through: i64) -> Result<Enqueued, ApiError> {
    let payload = Payload {
        through: Some(through),
    };
    let payload = serde_json::to_value(payload).map_err(ApiError::internal)?;
    jobs.enqueue(NewJob::new(user_id, KIND).payload(payload))
        .await
}

async fn run(ctx: JobContext) -> JobResult {
    let payload: Payload = ctx.payload_as()?;
    let library = ctx.state().config().data_dir.library_db(ctx.user_id());
    let exists = tokio::fs::try_exists(&library)
        .await
        .map_err(|err| JobError::transient(codes::UNAVAILABLE).with_detail(err.to_string()))?;
    if !exists {
        return Ok(Outcome::Succeeded);
    }
    let through = payload
        .through
        .unwrap_or_else(|| trash::retention_cutoff(ctx.jobs().clock().now_ms()));
    let total = ctx
        .user_db(move |db| {
            db.read(|conn| trash::count_purgeable(conn, through))
                .map_err(JobError::from)
        })
        .await?;
    if total == 0 {
        return Ok(Outcome::Succeeded);
    }
    ctx.progress(Some(0.0), Some(STAGE)).await;
    let mut done = 0_u64;
    loop {
        if ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        if ctx.is_paused() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        let now = ctx.jobs().clock().now_ms();
        let purged = ctx
            .user_db(move |db| {
                db.write(|tx| {
                    let ids = trash::purgeable(tx, through, CHUNK)?;
                    trash::purge(tx, &ids, now)
                })
                .map_err(JobError::from)
            })
            .await?;
        if purged.is_empty() {
            break;
        }
        done += purged.len() as u64;
        library::announce(
            ctx.state().events(),
            ctx.user_id(),
            ChangeReason::Delete,
            event_keys(purged),
        );
        ctx.progress(Some(super::bulk::share(done, total)), Some(STAGE))
            .await;
    }
    if done > 0 {
        // The storage the library uses (`GET /me/usage`) is counted again.
        if let Err(err) = usage::enqueue(ctx.jobs(), ctx.user_id()).await {
            tracing::warn!(job_id = ctx.id(), error = %err, "cannot enqueue the usage count");
        }
    }
    tracing::info!(
        job_id = ctx.id(),
        user_id = %ctx.user_id(),
        purged = done,
        nightly = payload.through.is_none(),
        "trash purged"
    );
    Ok(Outcome::Succeeded)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn the_nightly_payload_is_empty() {
        let nightly: Payload = serde_json::from_value(json!({})).unwrap();
        assert_eq!(nightly, Payload::default());
        assert_eq!(serde_json::to_value(Payload::default()).unwrap(), json!({}));
        let empty: Payload = serde_json::from_value(json!({ "through": 5 })).unwrap();
        assert_eq!(empty.through, Some(5));
    }
}
