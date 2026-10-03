//! `bulk` (plan §2.9, §2.12; P1 assumption G6): a bulk action over more
//! posts than run inline (500), from `POST /posts/bulk` or `POST
//! /trash/restore` ([`crate::routes::bulk`]).
//!
//! **Work.** The payload holds the request: the action, its parameters, the
//! selection, and the request's stamp, the `deletedAt` of every post a
//! delete moves (the undo key). Each chunk changes its posts at its own time,
//! their `updatedAt`: a post a delete moves later than asked stays in the
//! trash for its full 30 days, and outside the purge of an emptying asked
//! before the move ([`shelfy_core::trash`]). The worker counts the
//! selection, then works through it in chunks of 500 posts in id order
//! ([`shelfy_core::bulk::next_chunk`]), one write transaction per chunk on a
//! handle taken for that chunk ([`JobContext::user_db`]). After each chunk
//! that changed posts it announces `posts.changed` (the action's reason, the
//! chunk's keys or `null`) and `stats.changed`, and reports its progress
//! (`stage` is the action, `progress` the share of the selection done) on
//! `job.updated`.
//!
//! **Stopping.** Between chunks it stops when cancelled (what is done stays
//! done) and yields when the queue is paused. Every action is idempotent on
//! each post, so a try that starts over (after a pause, a crash or a lost
//! lease) runs the whole selection again and changes only what is left.
//!
//! **Limits.** Two at once overall, one per user (a user's bulk actions run
//! in the order they were asked), three tries, a 5-minute lease renewed by
//! every chunk.

use std::time::Duration;

use serde::{Deserialize, Serialize};
use shelfy_core::bulk::{self, Action, CHUNK};
use shelfy_core::repo::RepoError;
use shelfy_core::selector::Selector;

use super::codes;
use super::{Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome};
use crate::error::ApiError;
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

/// What one chunk did.
struct Step {
    /// Posts in the chunk.
    len: usize,
    /// The chunk's largest id.
    last: i64,
    /// Whether it changed posts.
    changed: bool,
    /// The event keys of the posts it changed.
    keys: Option<Vec<String>>,
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
    let reason = payload.action.reason();
    let at = payload.at;

    let counted = selector.clone();
    let total = ctx
        .user_db(move |db| {
            db.read(|conn| bulk::count(conn, &counted))
                .map_err(JobError::from)
        })
        .await?;
    ctx.progress(Some(0.0), Some(stage)).await;
    let (mut after, mut done) = (0_i64, 0_u64);
    loop {
        if ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        if ctx.is_paused() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        let (selector, action) = (selector.clone(), action.clone());
        let clock = *ctx.jobs().clock();
        let step = ctx
            .user_db(move |db| {
                db.write(|tx| -> Result<Option<Step>, RepoError> {
                    let Some(chunk) = bulk::next_chunk(tx, &selector, after, CHUNK)? else {
                        return Ok(None);
                    };
                    // The stamp is the request's; the time is the chunk's
                    // own: when its posts really change (P1-11 review H1).
                    let now = clock.now_ms();
                    let applied = bulk::apply_stamped(tx, &chunk.selector, &action, at, now)?;
                    Ok(Some(Step {
                        len: chunk.len,
                        last: chunk.last,
                        changed: !applied.changed.is_empty(),
                        keys: changed_keys(tx, &applied.changed)?,
                    }))
                })
                .map_err(JobError::from)
            })
            .await?;
        let Some(step) = step else {
            break;
        };
        if step.changed {
            library::announce(ctx.state().events(), ctx.user_id(), reason, step.keys);
        }
        after = step.last;
        done += step.len as u64;
        ctx.progress(Some(share(done, total)), Some(stage)).await;
    }
    ctx.progress(Some(1.0), Some(stage)).await;
    Ok(Outcome::Succeeded)
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
    use serde_json::json;

    use super::*;

    #[test]
    fn payloads_round_trip() {
        let request: PostSelector = serde_json::from_value(json!({
            "filter": { "platform": "instagram", "trash": true },
            "exceptKeys": ["ig_1"],
        }))
        .unwrap();
        for selection in [Selection::selector(request), Selection::DeletedAt(42)] {
            let payload = Payload {
                action: BulkAction::AddToCollections,
                params: Some(BulkParams {
                    collection_ids: Some(vec![3]),
                    collection_id: None,
                }),
                selection,
                at: 7,
            };
            let value = serde_json::to_value(&payload).unwrap();
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
    }

    #[test]
    fn shares_stay_between_zero_and_one() {
        assert!((share(0, 0) - 1.0).abs() < f64::EPSILON);
        assert!((share(250, 1_000) - 0.25).abs() < f64::EPSILON);
        assert!((share(1_200, 1_000) - 1.0).abs() < f64::EPSILON);
    }
}
