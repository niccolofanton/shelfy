//! `bulk` (plan §2.9, §2.12; P1 assumption G6): a bulk action over more
//! posts than run inline (500), from `POST /posts/bulk` or `POST
//! /trash/restore` ([`crate::routes::bulk`]).
//!
//! **Work.** The payload holds the request: the action, its parameters, the
//! selection, and the request's stamp, the `deletedAt` of every post a
//! delete moves (the undo key). Each chunk changes its posts at its own time,
//! their `updatedAt`: a post a delete moves later than asked stays in the
//! trash for its full 30 days, and outside the purge of an emptying asked
//! before the move ([`shelfy_core::trash`]). The worker counts what is left
//! of the selection, then works through it in chunks of 500 posts in id
//! order ([`shelfy_core::bulk::next_chunk`]), one write transaction per chunk
//! on a handle taken for that chunk ([`JobContext::user_db`]). After each
//! chunk that changed posts it announces `posts.changed` (the action's
//! reason, the chunk's keys or `null`) and `stats.changed`, and reports its
//! progress (`stage` is the action, `progress` the share of the selection
//! done) on `job.updated`.
//!
//! **Where it stands** (P1-11 review M1). The payload also holds the largest
//! post id when the job was asked (`maxId`): posts added since (an install,
//! an extension's ingest) are never visited, even when a filter matches
//! them. After each chunk the job merges into its payload the largest id it
//! went through and how many posts it went through (`after`, `done`:
//! [`AttemptFence::checkpoint`]), so a later try (after a pause, a shutdown,
//! a locked library, a lost lease, or the user's retry of a cancelled job)
//! goes on from there and never redoes a chunk whose changes the user undid
//! in between. Should a try stop between a chunk's commit and its record (a
//! crash), the next one runs that chunk again: every action is idempotent
//! on each post.
//!
//! **Stopping.** Between chunks it stops when cancelled and yields when the
//! queue is paused. Each chunk checks, inside its write transaction, that its
//! try is still the job's current one ([`AttemptFence::is_current`]): once a
//! cancel is committed, the try writes nothing more, which is what makes the
//! undo of a running delete complete ([`crate::routes::trash`]). A chunk is
//! announced and recorded from the blocking task that committed it, so a
//! worker aborted after a commit loses neither (P1-11 review L7).
//!
//! **Limits.** Two at once overall, one per user (a user's bulk actions run
//! in the order they were asked), three tries, a 5-minute lease renewed by
//! every chunk.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use shelfy_core::bulk::{self, Action, CHUNK};
use shelfy_core::db::{UserDb, UserDbCache};
use shelfy_core::selector::Selector;

use super::codes;
use super::{
    AttemptFence, Clock, Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob,
    Outcome,
};
use crate::error::ApiError;
use crate::events::EventBus;
use crate::events::model::ChangeReason;
use crate::library;
use crate::routes::bulk::{BulkAction, BulkParams, changed_keys};
use crate::routes::selector::PostSelector;

/// The kind's name.
pub const KIND: &str = "bulk";
/// Bulk jobs running at once, over every user.
pub const GLOBAL: usize = 2;
/// Tries of a bulk job.
pub const MAX_ATTEMPTS: u32 = 3;
/// How long a bulk job may go without a sign of life: every chunk is one.
pub const LEASE: Duration = Duration::from_secs(300);

/// The kind, for the registry ([`super::kinds::registry`]).
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(GLOBAL)
            .per_user(1)
            .max_attempts(MAX_ATTEMPTS)
            .lease(LEASE),
        run,
    )
}

/// The posts a bulk job works on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Selection {
    /// The request's selector (`POST /posts/bulk`, `POST /trash/restore`).
    Selector(Box<PostSelector>),
    /// The posts one delete moved to the trash, by its stamp (`POST
    /// /trash/restore {deletedAt}`).
    DeletedAt(i64),
}

impl Selection {
    /// The selection of a request's `selector`.
    #[must_use]
    pub fn selector(selector: PostSelector) -> Self {
        Self::Selector(Box::new(selector))
    }

    /// The core selector.
    ///
    /// # Errors
    ///
    /// The selector does not resolve (422 `validation_failed`).
    pub fn resolve(&self) -> Result<Selector, ApiError> {
        match self {
            Self::Selector(selector) => PostSelector::clone(selector).resolve("selector"),
            Self::DeletedAt(at) => Ok(Selector::TrashedAt(*at)),
        }
    }
}

/// What a bulk job works on: the request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// The action.
    pub action: BulkAction,
    /// Its parameters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub params: Option<BulkParams>,
    /// The posts.
    pub selection: Selection,
    /// The request's stamp, unix ms: the `deletedAt` of every post a delete
    /// moves. The posts' `updatedAt` is the time of their chunk.
    pub at: i64,
    /// The largest post id when the job was asked: posts added since are
    /// never visited. Absent from jobs queued before it existed: no bound.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_id: Option<i64>,
    /// Checkpoint: the largest post id the job went through, 0 before its
    /// first chunk. A try goes on after it.
    #[serde(default)]
    pub after: i64,
    /// Checkpoint: how many posts the job went through.
    #[serde(default)]
    pub done: u64,
}

impl Payload {
    /// A job of `action` with `params` over `selection`, stamped `at`, for
    /// the posts up to `max_id`: nothing done yet.
    #[must_use]
    pub fn new(
        action: BulkAction,
        params: Option<BulkParams>,
        selection: Selection,
        at: i64,
        max_id: i64,
    ) -> Self {
        Self {
            action,
            params,
            selection,
            at,
            max_id: Some(max_id),
            after: 0,
            done: 0,
        }
    }
}

/// Enqueues the bulk job of `payload` for `user_id`.
///
/// # Errors
///
/// 422 when the payload is over the job system's 64 KiB (a selector near the
/// request's own limit); the control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str, payload: &Payload) -> Result<Enqueued, ApiError> {
    let payload = serde_json::to_value(payload).map_err(ApiError::internal)?;
    jobs.enqueue(NewJob::new(user_id, KIND).payload(payload))
        .await
}

async fn run(ctx: JobContext) -> JobResult {
    let payload: Payload = ctx.payload_as()?;
    let invalid =
        |err: ApiError| JobError::permanent(codes::INVALID_PAYLOAD).with_detail(err.to_string());
    let action: Action = payload
        .action
        .resolve(payload.params.as_ref())
        .map_err(invalid)?;
    let selector = payload.selection.resolve().map_err(invalid)?;
    let stage = payload.action.as_str();
    let upto = payload.max_id.unwrap_or(i64::MAX);
    let (mut after, mut done) = (payload.after, payload.done);

    let counted = selector.clone();
    let left = ctx
        .user_db(move |db| {
            db.read(|conn| bulk::count_between(conn, &counted, after, upto))
                .map_err(JobError::from)
        })
        .await?;
    let total = done.saturating_add(left);
    ctx.progress(Some(share(done, total)), Some(stage)).await;
    loop {
        if ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        if ctx.is_paused() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        let chunk = ChunkRun {
            fence: ctx.attempt_fence(),
            cache: Arc::clone(ctx.state().user_dbs()),
            events: ctx.state().events().clone(),
            user: ctx.user_id().to_owned(),
            clock: *ctx.jobs().clock(),
            selector: selector.clone(),
            action: action.clone(),
            reason: payload.action.reason(),
            at: payload.at,
            upto,
            after,
            done,
        };
        match ctx.user_db(move |db| chunk.run(db)).await? {
            Step::Went {
                last,
                done: total_done,
            } => {
                after = last;
                done = total_done;
                ctx.progress(Some(share(done, total)), Some(stage)).await;
            }
            Step::Finished => break,
            // Cancelled meanwhile, or another try took the job over: the
            // scheduler knows, and records it.
            Step::Stopped => return Err(JobError::cancelled()),
        }
    }
    ctx.progress(Some(1.0), Some(stage)).await;
    Ok(Outcome::Succeeded)
}

/// One chunk of a job: everything it needs on its blocking task.
struct ChunkRun {
    fence: AttemptFence,
    cache: Arc<UserDbCache>,
    events: EventBus,
    user: String,
    clock: Clock,
    selector: Selector,
    action: Action,
    reason: ChangeReason,
    at: i64,
    upto: i64,
    after: i64,
    done: u64,
}

/// What a chunk did.
enum Step {
    /// It went through posts up to `last`; `done` in all so far.
    Went { last: i64, done: u64 },
    /// Nothing is left: the job is done.
    Finished,
    /// The try is no longer the job's current one: it changed nothing.
    Stopped,
}

/// What a chunk's write transaction did.
enum Wrote {
    Chunk {
        len: usize,
        last: i64,
        changed: bool,
        keys: Option<Vec<String>>,
    },
    Nothing,
    NotCurrent,
}

impl ChunkRun {
    /// Runs the chunk on `db`, then, from this blocking task, announces what
    /// it changed and records where the job stands (module docs).
    fn run(self, db: &UserDb) -> Result<Step, JobError> {
        let wrote = db.write(|tx| -> Result<Wrote, JobError> {
            // Under the writer: a cancel committed before is seen here, and a
            // write that starts after it waits for this chunk.
            if !self.fence.is_current()? {
                return Ok(Wrote::NotCurrent);
            }
            let Some(chunk) = bulk::next_chunk(tx, &self.selector, self.after, self.upto, CHUNK)?
            else {
                return Ok(Wrote::Nothing);
            };
            // The stamp is the request's; the time is the chunk's own: when
            // its posts really change (P1-11 review H1).
            let now = self.clock.now_ms();
            let applied = bulk::apply_stamped(tx, &chunk.selector, &self.action, self.at, now)?;
            let changed = !applied.changed.is_empty();
            let keys = if changed {
                changed_keys(tx, &applied.changed)?
            } else {
                None
            };
            Ok(Wrote::Chunk {
                len: chunk.len,
                last: chunk.last,
                changed,
                keys,
            })
        })?;
        let (len, last) = match wrote {
            Wrote::NotCurrent => return Ok(Step::Stopped),
            Wrote::Nothing => return Ok(Step::Finished),
            Wrote::Chunk {
                len,
                last,
                changed,
                keys,
            } => {
                if changed {
                    library::committed(
                        &self.cache,
                        &self.events,
                        &self.user,
                        db,
                        self.reason,
                        keys,
                    );
                }
                (len, last)
            }
        };
        let done = self.done.saturating_add(len as u64);
        let recorded = self
            .fence
            .checkpoint(&json!({ "after": last, "done": done }))?;
        Ok(if recorded {
            Step::Went { last, done }
        } else {
            // Its lease was taken back: another try goes on from the last
            // record, and runs this chunk again, which changes nothing more.
            Step::Stopped
        })
    }
}

/// `done` of `total`, from 0 to 1 (a selection may grow while the job runs).
#[allow(clippy::cast_precision_loss)] // counts of posts are far below 2^52
pub(crate) fn share(done: u64, total: u64) -> f64 {
    if total == 0 {
        1.0
    } else {
        (done as f64 / total as f64).min(1.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn payloads_round_trip() {
        let request: PostSelector = serde_json::from_value(json!({
            "filter": { "platform": "instagram", "trash": true },
            "exceptKeys": ["ig_1"],
        }))
        .unwrap();
        for selection in [Selection::selector(request), Selection::DeletedAt(42)] {
            let params = BulkParams {
                collection_ids: Some(vec![3]),
                collection_id: None,
            };
            let payload = Payload::new(
                BulkAction::AddToCollections,
                Some(params),
                selection,
                7,
                900,
            );
            let value = serde_json::to_value(&payload).unwrap();
            assert_eq!(
                (&value["maxId"], &value["after"], &value["done"]),
                (&json!(900), &json!(0), &json!(0))
            );
            let back: Payload = serde_json::from_value(value).unwrap();
            assert_eq!(back, payload);
            assert!(back.selection.resolve().is_ok());
        }
        let value = serde_json::to_value(Selection::DeletedAt(42)).unwrap();
        assert_eq!(value, json!({ "deletedAt": 42 }));
        assert_eq!(
            Selection::DeletedAt(42).resolve().unwrap(),
            Selector::TrashedAt(42)
        );
        // A job queued before the checkpoints: no bound, from the start.
        let old: Payload = serde_json::from_value(json!({
            "action": "delete",
            "selection": { "deletedAt": 42 },
            "at": 7,
        }))
        .unwrap();
        assert_eq!((old.max_id, old.after, old.done), (None, 0, 0));
    }

    #[test]
    fn shares_stay_between_zero_and_one() {
        assert!((share(0, 0) - 1.0).abs() < f64::EPSILON);
        assert!((share(250, 1_000) - 0.25).abs() < f64::EPSILON);
        assert!((share(1_200, 1_000) - 1.0).abs() < f64::EPSILON);
    }
}
