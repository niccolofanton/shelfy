//! The ingest service (plan §2.16; P2 contract C5; P2-09): what `POST
//! /ingest/batches` does with a sanitized capture batch.
//!
//! In one `UserDb::write` ([`crate::library::write`], reason `ingest`):
//!
//! 1. [`merge::upsert_batch`] merges the batch; a merge that changes nothing
//!    writes nothing (P1-10);
//! 2. the run's collection, if any, gains the accepted posts (the folder or
//!    board mapping, [`collections::add_posts`]);
//! 3. [`archive::refresh_states`] derives the archive state of the posts the
//!    merge inserted or changed, with the archive's modes in effect
//!    ([`archive::modes`]) and P2-02's rule;
//! 4. the run's counters grow by what the batch brought
//!    ([`sync::add_counts`]).
//!
//! The library write announces `posts.changed` (reason `ingest`, the keys of
//! the posts that changed) and `stats.changed` itself. After it commits this
//! service, off the write:
//!
//! - raises `usage_daily.ingest_items` by the accepted items;
//! - counts `shelfy_ingest_items_total`;
//! - enqueues `archive.drain` when the merge left posts for the server
//!   (`server_work() > 0`);
//! - emits `sync.progress` with the run's running totals (throttled to one a
//!   second per run);
//! - leaves a seam where P2-14 wakes the extension's task poller when posts
//!   went to `client`.
//!
//! The route ([`crate::routes::ingest`]) sanitizes the batch, refuses a killed
//! source (409 `source_disabled`) and a batch whose run is unknown (404
//! `sync_run_not_found`) or of another platform (422), then calls
//! [`ingest_batch`].

use shelfy_core::ingest::archive::{self, ArchivePolicy, Scope};
use shelfy_core::ingest::merge::{UpsertOptions, UpsertSummary, upsert_batch};
use shelfy_core::ingest::sanitize::SanitizedBatch;
use shelfy_core::repo::sync::{self, SyncRun};
use shelfy_core::repo::{Platform, RepoError, collections};

use crate::control::usage_daily::Field;
use crate::error::ApiError;
use crate::events::model::{ChangeReason, SyncListing, SyncProgressEvent};
use crate::jobs::archive as archive_job;
use crate::library::{self, Change};
use crate::quota;
use crate::state::AppState;
use crate::telemetry::metrics::{INGEST_ITEMS_TOTAL, ingest_outcome};

/// What became of one accepted item (contract C5 `results[]`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ItemResult {
    /// The item's index in the request batch.
    pub index: usize,
    /// Its canonical key.
    pub key: String,
    /// Whether the key was new (`inserted`) or already saved (`known`).
    pub inserted: bool,
    /// Whether a known post's stored data changed.
    pub changed: bool,
}

/// What an ingest batch did, for the route's answer (contract C5).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ingested {
    /// New posts.
    pub inserted: usize,
    /// Known posts that changed.
    pub updated: usize,
    /// Known posts (changed or not).
    pub known: usize,
    /// One entry per accepted item, in batch order.
    pub results: Vec<ItemResult>,
}

/// What the write produced, carried to the post-commit steps.
struct Committed {
    ingested: Ingested,
    /// Items the batch accepted (inserted + known): the usage count.
    accepted: usize,
    /// Posts the server has media to fetch for after this batch.
    server_work: usize,
    /// Posts handed to the extension after this batch.
    client_work: usize,
}

/// Runs a sanitized `batch` of `run` into `user_id`'s library and reports
/// what it did (contract C5). `run` is already validated: it exists and its
/// platform matches the batch.
///
/// # Errors
///
/// The library write failed.
pub async fn ingest_batch(
    state: &AppState,
    user_id: &str,
    run: &SyncRun,
    batch: SanitizedBatch,
    now: i64,
) -> Result<Ingested, ApiError> {
    let platform = run.platform;
    let modes = archive_job::modes(state);
    let run_id = run.id.clone();
    let collection_id = run.collection_id;
    let rejected = batch.rejected.len();

    let committed = library::write(state, user_id, ChangeReason::Ingest, move |tx| {
        let summary = upsert_batch(tx, &batch.posts, UpsertOptions::default(), now)?;

        // The folder or board mapping: every accepted post joins the run's
        // collection (idempotent). A collection deleted since the run opened is
        // skipped, not an error.
        if let Some(collection_id) = collection_id {
            let ids: Vec<i64> = summary.posts.iter().map(|p| p.id).collect();
            match collections::add_posts(tx, &ids, &[collection_id], now) {
                Ok(_) | Err(RepoError::NotFound) => {}
                Err(err) => return Err(err),
            }
        }

        // The archive state of the posts the merge inserted or changed.
        let touched: Vec<i64> = summary
            .posts
            .iter()
            .filter(|p| p.inserted || p.changed)
            .map(|p| p.id)
            .collect();
        let policy = ArchivePolicy::read(tx, modes)?;
        let refreshed = archive::refresh_states(tx, Scope::Posts(&touched), &policy, now)?;

        let accepted = summary.posts.len();
        sync::add_counts(
            tx,
            &run_id,
            i64::try_from(accepted).unwrap_or(i64::MAX),
            i64::try_from(summary.inserted).unwrap_or(i64::MAX),
            i64::try_from(summary.changed).unwrap_or(i64::MAX),
            i64::try_from(summary.merged).unwrap_or(i64::MAX),
            now,
        )?;

        let (ingested, changed_keys) = answer(&summary, &batch);
        Ok(Change {
            value: Committed {
                ingested,
                accepted,
                server_work: refreshed.counts.server_work(),
                client_work: refreshed.counts.client,
            },
            // The posts that changed; `[]` when only the folder or counters
            // did (the client refreshes folders on `stats.changed`).
            keys: library::event_keys(changed_keys),
        })
    })
    .await?
    .value;

    record_metrics(platform, &committed.ingested, rejected);
    if committed.accepted > 0 {
        bump_usage(state, user_id, committed.accepted).await;
    }
    if committed.server_work > 0
        && let Err(err) = archive_job::enqueue(state.jobs(), user_id).await
    {
        // The posts are saved; the next ingest, or the drain sweeper, picks
        // them up.
        tracing::warn!(error = %err, "cannot enqueue the archive drain after ingest");
    }
    if committed.client_work > 0 {
        // P2-14 seam: wake the extension's `GET /ingest/tasks` long-poll so it
        // acts on the posts that went to `client` without waiting for its
        // 5-minute alarm. The tasks API does not exist yet; until it lands the
        // extension picks the work up on its next poll.
    }
    emit_progress(state, user_id, run, &committed.ingested);
    Ok(committed.ingested)
}

/// The answer and the keys of the posts that changed, from the merge summary
/// and the batch (both in batch order).
fn answer(summary: &UpsertSummary, batch: &SanitizedBatch) -> (Ingested, Vec<String>) {
    let mut results = Vec::with_capacity(summary.posts.len());
    let mut changed_keys = Vec::new();
    for ((index, post), done) in batch.indices.iter().zip(&batch.posts).zip(&summary.posts) {
        if done.inserted || done.changed {
            changed_keys.push(post.key.clone());
        }
        results.push(ItemResult {
            index: *index,
            key: post.key.clone(),
            inserted: done.inserted,
            changed: done.changed,
        });
    }
    (
        Ingested {
            inserted: summary.inserted,
            updated: summary.changed,
            known: summary.merged,
            results,
        },
        changed_keys,
    )
}

/// Counts the batch's items by outcome.
fn record_metrics(platform: Platform, ingested: &Ingested, rejected: usize) {
    let platform = platform.as_str();
    for (outcome, n) in [
        (ingest_outcome::INSERTED, ingested.inserted),
        (ingest_outcome::UPDATED, ingested.updated),
        (ingest_outcome::KNOWN, ingested.known),
        (ingest_outcome::REJECTED, rejected),
    ] {
        if n > 0 {
            metrics::counter!(INGEST_ITEMS_TOTAL, "platform" => platform, "outcome" => outcome)
                .increment(n as u64);
        }
    }
}

/// Raises the user's daily ingest count; a failure is logged, not fatal.
async fn bump_usage(state: &AppState, user_id: &str, accepted: usize) {
    let n = i64::try_from(accepted).unwrap_or(i64::MAX);
    if let Err(err) = quota::bump_daily(state, user_id, Field::IngestItems, n).await {
        tracing::warn!(error = %err, "cannot raise the daily ingest count");
    }
}

/// Emits the run's running totals as `sync.progress` (throttled per run).
fn emit_progress(state: &AppState, user_id: &str, run: &SyncRun, ingested: &Ingested) {
    let grow = |base: i64, by: usize| base.saturating_add(i64::try_from(by).unwrap_or(i64::MAX));
    let event = SyncProgressEvent {
        run_id: run.id.clone(),
        platform: run.platform.as_str().to_owned(),
        listing: SyncListing {
            kind: run.source_kind.clone(),
            external_id: run.listing_external_id.clone(),
            name: run.listing_name.clone(),
        },
        trigger: run.trigger.clone(),
        scanned: grow(run.scanned, ingested.inserted + ingested.known),
        inserted: grow(run.inserted, ingested.inserted),
        updated: grow(run.updated, ingested.updated),
        known: grow(run.known, ingested.known),
        pages: run.pages,
        state: run.state.clone(),
    };
    state.events().sync_progress(user_id, event);
}
