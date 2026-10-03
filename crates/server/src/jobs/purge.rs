//! `purge` (plan §2.12, §2.13; P1-11): deletes trashed posts for good.
//!
//! **When.** `POST /trash/empty` enqueues one for exactly the posts in the
//! trash when it was asked ([`enqueue`]: the payload's `through`, the cut of
//! [`shelfy_core::trash::emptying`]); the nightly schedule (03:00 UTC)
//! enqueues one per active user with an empty payload, which purges the
//! posts whose stamp and move into the trash are 30 days old or more
//! ([`shelfy_core::trash::retention_cutoff`] on the job system's clock).
//! Posts that enter the trash after the cut, such as those a bulk delete
//! moves after the request, and posts restored meanwhile, stay.
//!
//! **After the user's earlier bulk jobs.** A purge does not run while its
//! user has an active `bulk` job enqueued before it (P1-11 review H1): it
//! goes back to the queue for [`BULK_WAIT`], without using a try, and looks
//! again before each chunk. So a restore asked before emptying the trash (an
//! undo past 500 posts, say) brings its posts back before the purge could
//! delete them, whatever the two queues do; and a bulk delete asked before
//! has moved its posts, past the purge's cut, before the purge starts.
//!
//! **Work.** In chunks of 500 posts, oldest trash first, one write
//! transaction per chunk ([`shelfy_core::trash::purge`]): the index rows, the
//! post rows and what cascades from them (slides, tags, entities,
//! memberships, captures) go, and media objects that lose their last
//! reference get `unreferenced_since`. Deleting their files is the GC's
//! (P4). Each chunk checks, inside its write transaction, that its try is
//! still current ([`AttemptFence::is_current`]): a cancelled purge deletes
//! nothing more. Each chunk is announced (`posts.changed`, reason `delete`,
//! and `stats.changed`) from the blocking task that committed it, and
//! reported on `job.updated` (`stage` `purge`). Once posts were purged, the
//! user's storage is counted again ([`super::usage::enqueue`]). A user
//! without a library has nothing to purge, and none is created.
//!
//! **Idempotent.** A purged post is gone: a try that starts over, or a
//! second purge, finds only what is left.
//!
//! **Limits** (§2.12 `purge`, `gc`): one at a time overall and per user,
//! three tries, a 60-minute lease renewed by every chunk.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use shelfy_core::bulk::CHUNK;
use shelfy_core::db::{UserDb, UserDbCache};
use shelfy_core::trash;

use super::{
    AttemptFence, Clock, Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob,
    Outcome, codes, usage,
};
use crate::control::jobs as rows;
use crate::error::ApiError;
use crate::events::EventBus;
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
/// How long a purge waits before it looks again while an earlier `bulk`
/// job of its user is active (module docs). It waits in the queue, without
/// using a try.
pub const BULK_WAIT: Duration = Duration::from_secs(5);

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
    /// The cut of an emptying of the trash ([`trash::emptying`]): the posts
    /// whose stamp and `updated_at` are at or before it, unix ms. Absent, as
    /// in the nightly job: those 30 days old ([`trash::retention_cutoff`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub through: Option<i64>,
}

/// Enqueues the purge of `user_id`'s trash through the cut `through` of an
/// emptying ([`trash::emptying`]).
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
        if after_earlier_bulk(&ctx).await? {
            let run_at = ctx
                .jobs()
                .clock()
                .now_ms()
                .saturating_add(millis(BULK_WAIT));
            return Ok(Outcome::Requeue {
                run_at: Some(run_at),
            });
        }
        let chunk = PurgeChunk {
            fence: ctx.attempt_fence(),
            cache: Arc::clone(ctx.state().user_dbs()),
            events: ctx.state().events().clone(),
            user: ctx.user_id().to_owned(),
            clock: *ctx.jobs().clock(),
            through,
        };
        match ctx.user_db(move |db| chunk.run(db)).await? {
            Purged::Posts(n) => {
                done += n;
                ctx.progress(Some(super::bulk::share(done, total)), Some(STAGE))
                    .await;
            }
            Purged::Nothing => break,
            // Cancelled meanwhile, or another try took the job over.
            Purged::Stopped => return Err(JobError::cancelled()),
        }
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

/// One chunk of a purge: everything it needs on its blocking task.
struct PurgeChunk {
    fence: AttemptFence,
    cache: Arc<UserDbCache>,
    events: EventBus,
    user: String,
    clock: Clock,
    through: i64,
}

/// What a chunk of a purge did.
enum Purged {
    /// It deleted this many posts.
    Posts(u64),
    /// Nothing was left: the purge is done.
    Nothing,
    /// The try is no longer the job's current one: it deleted nothing.
    Stopped,
}

impl PurgeChunk {
    /// Purges the next chunk on `db`, then announces it from this blocking
    /// task, so a worker aborted after the commit still announces it (P1-11
    /// review L7).
    fn run(self, db: &UserDb) -> Result<Purged, JobError> {
        let purged = db.write(|tx| -> Result<Option<Vec<String>>, JobError> {
            // Under the writer: a cancel committed before is seen here.
            if !self.fence.is_current()? {
                return Ok(None);
            }
            let ids = trash::purgeable(tx, self.through, CHUNK)?;
            Ok(Some(trash::purge(tx, &ids, self.clock.now_ms())?))
        })?;
        let Some(keys) = purged else {
            return Ok(Purged::Stopped);
        };
        if keys.is_empty() {
            return Ok(Purged::Nothing);
        }
        let n = keys.len() as u64;
        library::committed(
            &self.cache,
            &self.events,
            &self.user,
            db,
            ChangeReason::Delete,
            event_keys(keys),
        );
        Ok(Purged::Posts(n))
    }
}

/// Whether the user has an active `bulk` job enqueued before this purge,
/// which it waits for (module docs).
async fn after_earlier_bulk(ctx: &JobContext) -> Result<bool, JobError> {
    let control = Arc::clone(ctx.state().control());
    let (user, id) = (ctx.user_id().to_owned(), ctx.id());
    tokio::task::spawn_blocking(move || {
        control.read(|conn| rows::has_active_before(conn, &user, super::bulk::KIND, id))
    })
    .await
    .map_err(|err| JobError::transient(codes::INTERNAL).with_detail(err.to_string()))?
    .map_err(JobError::from)
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
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
