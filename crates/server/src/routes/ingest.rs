//! `POST /api/v1/ingest/batches` (plan §2.16; P2 contract C5; P2-09): the
//! browser extension's capture batches.
//!
//! An `ingest` token (and `X-Shelfy-Extension`, the version gate C1) sends a
//! batch tied to a sync run. The route sanitizes it (P2-02), refuses a killed
//! capture mode (409 `source_disabled`) and a batch whose run is unknown (404
//! `sync_run_not_found`) or of another platform (422), then hands it to the
//! ingest service ([`crate::ingest`]), which merges it, maps the posts into
//! the run's folder, derives their archive state and raises the run's
//! counters in one write. The answer maps the merge: `inserted`, `updated`
//! (known posts that changed) and `known`, with one `results` entry per
//! accepted item (its index, key, outcome and whether it changed) for the
//! extension's incremental stop (P2-G10), and the sanitizer's `rejected`.
//!
//! `Idempotency-Key` is the batch's ULID: a replay gets the first answer back
//! and does not ingest twice ([`crate::jobs::idempotency`]).

use axum::extract::State;
use serde::{Deserialize, Serialize};
use shelfy_core::ingest::sanitize::{self, RejectCode, SanitizeError};
use utoipa::ToSchema;

use super::model::Platform;
use crate::auth::bearer::{TokenUser, scopes};
use crate::error::{ApiError, ErrorCode};
use crate::extension::ExtensionHeaders;
use crate::extension::flags::CaptureMode;
use crate::extract::Json;
use crate::ids::now_ms;
use crate::ingest;
use crate::jobs::idempotency::IdempotencyHeader;
use crate::state::{AppState, blocking};

/// What produced a batch (contract C5). A kill switch can turn `passive`,
/// `replay` or `scroll` off for a platform; `selection` and `refresh` are the
/// user's own doing and always run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum IngestSource {
    /// Read while the user browsed a saved listing.
    Passive,
    /// The Instagram REST replay.
    Replay,
    /// The scroll of a sync run.
    Scroll,
    /// The selection overlay's "Import selected".
    Selection,
    /// A per-post refresh of expired media.
    Refresh,
}

impl IngestSource {
    /// The kill-switch mode this source is under, if any.
    #[must_use]
    pub const fn capture_mode(self) -> Option<CaptureMode> {
        match self {
            Self::Passive => Some(CaptureMode::Passive),
            Self::Replay => Some(CaptureMode::Replay),
            Self::Scroll => Some(CaptureMode::Scroll),
            Self::Selection | Self::Refresh => None,
        }
    }

    /// Its wire name.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Passive => "passive",
            Self::Replay => "replay",
            Self::Scroll => "scroll",
            Self::Selection => "selection",
            Self::Refresh => "refresh",
        }
    }
}

/// The extension's build, carried for parity reports (C5); informational.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct BatchClient {
    /// The extension's manifest version.
    pub ext: String,
    /// The desktop hook's build id.
    pub parser: String,
}

/// A capture batch (contract C5). Items are untrusted JSON for the sanitizer;
/// a batch holds at most 500.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestBatch {
    /// The run the batch belongs to.
    pub sync_run_id: String,
    /// The platform the batch was read on; must match the run's.
    pub platform: Platform,
    /// What produced it.
    pub source: IngestSource,
    /// Whether the listing has a next page, as the walker saw it;
    /// informational (the run's `PATCH` is authoritative).
    #[serde(default)]
    #[schema(required = true)]
    pub has_next_page: Option<bool>,
    /// The extension and parser build.
    #[serde(default)]
    #[schema(nullable = false)]
    pub client: Option<BatchClient>,
    /// The intercepted items, as the desktop hook shapes them.
    pub items: Vec<serde_json::Value>,
}

/// Whether an accepted item was new or already saved (contract C5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum ItemOutcome {
    /// A new key.
    Inserted,
    /// A key already in the library.
    Known,
}

/// One accepted item's result (contract C5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ItemResult {
    /// Its index in the request batch.
    pub index: usize,
    /// Its canonical key.
    pub key: String,
    /// New or known.
    pub outcome: ItemOutcome,
    /// Whether a known post's stored data changed (always `true` for a new
    /// one).
    pub changed: bool,
}

/// Why the sanitizer rejected an item (contract C5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RejectReason {
    /// Not a JSON object.
    BadItem,
    /// No usable id, or one that gives no canonical key.
    BadId,
}

impl From<RejectCode> for RejectReason {
    fn from(code: RejectCode) -> Self {
        match code {
            RejectCode::BadItem => Self::BadItem,
            RejectCode::BadId => Self::BadId,
        }
    }
}

/// An item the sanitizer rejected (contract C5).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct RejectedItem {
    /// Its index in the request batch.
    pub index: usize,
    /// Why.
    pub code: RejectReason,
}

/// What a batch did (contract C5).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct IngestResult {
    /// New posts.
    pub inserted: usize,
    /// Known posts that changed.
    pub updated: usize,
    /// Known posts (changed or not).
    pub known: usize,
    /// One entry per accepted item, in batch order.
    pub results: Vec<ItemResult>,
    /// The rejected items, in batch order.
    pub rejected: Vec<RejectedItem>,
}

/// Ingests a capture batch (contract C5).
///
/// An `ingest` token (the browser extension). 409 `source_disabled` when the
/// batch's capture mode is turned off for its platform; 404
/// `sync_run_not_found` when its run is unknown; 422 when the batch platform
/// differs from the run's, or a batch holds more than 500 items.
#[utoipa::path(
    post,
    path = "/api/v1/ingest/batches",
    tag = "extension",
    operation_id = "ingestBatch",
    security(("bearer" = ["ingest"])),
    params(IdempotencyHeader, ExtensionHeaders),
    request_body = IngestBatch,
    responses(
        (status = OK, description = "What the batch did.", body = IngestResult),
    )
)]
pub async fn ingest_batch(
    State(state): State<AppState>,
    token: TokenUser<scopes::Ingest>,
    Json(body): Json<IngestBatch>,
) -> Result<Json<IngestResult>, ApiError> {
    let user_id = token.id();
    let platform = body.platform.into();
    let now = now_ms();

    // A killed capture mode (a kill switch of this platform) refuses the batch.
    if let Some(mode) = body.source.capture_mode() {
        let flags = state.extension().flags().get(state.control()).await?;
        if !flags.config().allows(platform, mode) {
            return Err(
                ApiError::new(ErrorCode::SourceDisabled).with_detail(format!(
                    "{} capture is turned off for {platform}",
                    body.source.as_str()
                )),
            );
        }
    }

    let batch = sanitize::sanitize_batch(platform, &body.items, now).map_err(sanitize_error)?;

    // The run must exist in this user's library and share the batch platform.
    let db = state.user_db(user_id).await?;
    let run_id = body.sync_run_id.clone();
    let run = blocking(move || db.read(|conn| shelfy_core::repo::sync::get_run(conn, &run_id)))
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::SyncRunNotFound))?;
    if run.platform != platform {
        return Err(ApiError::invalid_field(
            "platform",
            "does not match the sync run",
        ));
    }

    let rejected: Vec<RejectedItem> = batch
        .rejected
        .iter()
        .map(|r| RejectedItem {
            index: r.index,
            code: r.code.into(),
        })
        .collect();

    let ingested = ingest::ingest_batch(&state, user_id, &run, batch, now).await?;
    let results = ingested
        .results
        .into_iter()
        .map(|r| ItemResult {
            index: r.index,
            key: r.key,
            outcome: if r.inserted {
                ItemOutcome::Inserted
            } else {
                ItemOutcome::Known
            },
            changed: r.changed,
        })
        .collect();
    Ok(Json(IngestResult {
        inserted: ingested.inserted,
        updated: ingested.updated,
        known: ingested.known,
        results,
        rejected,
    }))
}

/// Maps a sanitizer refusal of the whole batch to its status.
fn sanitize_error(err: SanitizeError) -> ApiError {
    match err {
        SanitizeError::TooManyItems { max, len } => ApiError::invalid_field(
            "items",
            format!("a batch holds at most {max} items, not {len}"),
        ),
        SanitizeError::Platform(platform) => ApiError::invalid_field(
            "platform",
            format!("{platform} posts do not come in capture batches"),
        ),
    }
}
