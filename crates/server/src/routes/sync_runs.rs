//! Sync runs and sources (plan §2.16, §2.9; P2 contract C4; P2-09).
//!
//! | Route | Access | What |
//! |---|---|---|
//! | `POST /sync-runs` | `ingest` token | opens a run: resolves the folder mapping, reads whether the walk is incremental and the cursor to resume from, and returns them |
//! | `PATCH /sync-runs/{id}` | `ingest` token | ends or updates a run; a walk that reached the end of the feed marks the source full, a capped one stores its cursor; a manual, web or scheduled run posts a notification |
//! | `GET /sync-runs` | session | the user's runs, newest first, paged by an opaque cursor |
//! | `GET /extension/sources` | `ingest` token | the listings the extension has synced, for its planner |
//!
//! A run that has seen no activity for [`IDLE_MS`] is stopped when the user
//! next opens a run or looks at the list (P2-09). The counters a run carries
//! are raised by ingest ([`crate::ingest`]); `PATCH` sets `pages` and the
//! terminal state.

use axum::extract::{Path, Query, State};
use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use serde_json::json;
use shelfy_core::repo::collections::{self, NewCollection};
use shelfy_core::repo::sync::{
    self, NewRun, RunCursor, RunPatch, Source, SourceUpdate, SyncRun as Run,
};
use shelfy_core::repo::{self, RepoError};
use utoipa::{IntoParams, ToSchema};

use super::listing::page_size;
use super::model::Platform;
use crate::auth::bearer::{TokenUser, scopes};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::events::{
    self,
    model::{SyncListing, SyncProgressEvent},
};
use crate::extension::ExtensionHeaders;
use crate::extract::Json;
use crate::ids::{new_ulid, now_ms};
use crate::state::{AppState, blocking};
use crate::telemetry::metrics::SYNC_RUN_PAGES;

/// A run idle this long (no ingest, no patch) is stopped (P2-09).
pub const IDLE_MS: i64 = 2 * 60 * 60 * 1000;

// ── Closed sets (contract C4) ──────────────────────────────────────────────────

/// What started a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Trigger {
    /// The panel's "Sync now".
    Manual,
    /// The web app asked for it.
    Web,
    /// A scheduled run.
    Scheduled,
    /// Passive capture while browsing.
    Passive,
    /// The selection overlay's import.
    Selection,
    /// A per-post media refresh.
    Refresh,
}

impl Trigger {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Manual => "manual",
            Self::Web => "web",
            Self::Scheduled => "scheduled",
            Self::Passive => "passive",
            Self::Selection => "selection",
            Self::Refresh => "refresh",
        }
    }

    fn from_stored(s: &str) -> Option<Self> {
        [
            Self::Manual,
            Self::Web,
            Self::Scheduled,
            Self::Passive,
            Self::Selection,
            Self::Refresh,
        ]
        .into_iter()
        .find(|t| t.as_str() == s)
    }

    /// Whether the end of a run with this trigger is worth a notification: a
    /// run the user asked for, not a passive or housekeeping one.
    const fn notifies(self) -> bool {
        matches!(self, Self::Manual | Self::Web | Self::Scheduled)
    }
}

/// The kind of listing a run walks.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListingKind {
    /// Instagram saved posts.
    IgSaved,
    /// An Instagram saved collection (folder).
    IgCollection,
    /// X bookmarks.
    XBookmarks,
    /// A Pinterest board.
    PinBoard,
}

impl ListingKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::IgSaved => "ig_saved",
            Self::IgCollection => "ig_collection",
            Self::XBookmarks => "x_bookmarks",
            Self::PinBoard => "pin_board",
        }
    }

    fn from_stored(s: &str) -> Option<Self> {
        [
            Self::IgSaved,
            Self::IgCollection,
            Self::XBookmarks,
            Self::PinBoard,
        ]
        .into_iter()
        .find(|k| k.as_str() == s)
    }

    /// The platform a listing of this kind lives on.
    const fn platform(self) -> repo::Platform {
        match self {
            Self::IgSaved | Self::IgCollection => repo::Platform::Instagram,
            Self::XBookmarks => repo::Platform::Twitter,
            Self::PinBoard => repo::Platform::Pinterest,
        }
    }

    /// Whether this kind names a folder or board (needs an external id).
    const fn needs_external_id(self) -> bool {
        matches!(self, Self::IgCollection | Self::PinBoard)
    }
}

/// The state of a run.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Going.
    Running,
    /// Finished normally.
    Done,
    /// Stopped (by the user, or idle).
    Stopped,
    /// Failed.
    Failed,
}

impl RunState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Done => "done",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    fn from_stored(s: &str) -> Option<Self> {
        [Self::Running, Self::Done, Self::Stopped, Self::Failed]
            .into_iter()
            .find(|v| v.as_str() == s)
    }
}

/// Why a run stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The feed ended.
    EndOfFeed,
    /// The incremental run reached its known-run threshold.
    KnownRun,
    /// The page cap stopped it.
    PageCap,
    /// The time cap stopped it.
    TimeCap,
    /// The user stopped it.
    User,
    /// A login wall.
    LoginRequired,
    /// An error.
    Error,
    /// No activity for too long (set by the server).
    Idle,
}

impl StopReason {
    const fn as_str(self) -> &'static str {
        match self {
            Self::EndOfFeed => "end_of_feed",
            Self::KnownRun => "known_run",
            Self::PageCap => "page_cap",
            Self::TimeCap => "time_cap",
            Self::User => "user",
            Self::LoginRequired => "login_required",
            Self::Error => "error",
            Self::Idle => "idle",
        }
    }

    fn from_stored(s: &str) -> Option<Self> {
        [
            Self::EndOfFeed,
            Self::KnownRun,
            Self::PageCap,
            Self::TimeCap,
            Self::User,
            Self::LoginRequired,
            Self::Error,
            Self::Idle,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

/// How a run maps its posts into a collection.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum CollectionMode {
    /// Find or create the collection for this folder or board.
    Auto,
    /// Use the given collection.
    Existing,
    /// Map into none.
    None,
}

// ── Request and response shapes ─────────────────────────────────────────────────

/// The listing a run walks (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Listing {
    /// Its kind.
    pub kind: ListingKind,
    /// The folder or board id, for `ig_collection` and `pin_board`.
    #[serde(default)]
    #[schema(required = true)]
    pub external_id: Option<String>,
    /// The folder or board name as the page shows it.
    #[serde(default)]
    #[schema(required = true)]
    pub name: Option<String>,
}

/// Where a run maps its posts (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CollectionTarget {
    /// The mapping mode.
    pub mode: CollectionMode,
    /// The collection, for `existing`.
    #[serde(default)]
    #[schema(required = true)]
    pub id: Option<i64>,
}

/// Opens a sync run (contract C4).
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunCreate {
    /// The platform.
    pub platform: Platform,
    /// What started it.
    pub trigger: Trigger,
    /// The listing it walks.
    pub listing: Listing,
    /// Where it maps its posts.
    pub collection: CollectionTarget,
}

/// The answer to opening a run (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunOpened {
    /// The run's id.
    pub id: String,
    /// Whether a known-run stop applies (the source's last full walk reached
    /// the end of the feed).
    pub incremental: bool,
    /// The consecutive-known threshold for an incremental stop.
    pub stop_after_known: i64,
    /// The collection the run maps into, when it maps into one.
    #[schema(required = true)]
    pub collection_id: Option<i64>,
    /// The cursor a previous capped walk left, to resume from.
    #[schema(required = true)]
    pub resume_cursor: Option<String>,
}

/// Updates or ends a run (contract C4). Absent counters keep theirs.
#[derive(Clone, Debug, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunUpdate {
    /// The new state.
    pub state: RunState,
    /// Pages scanned, absolute.
    #[serde(default)]
    #[schema(required = true)]
    pub pages: Option<i64>,
    /// Items scanned, absolute.
    #[serde(default)]
    #[schema(required = true)]
    pub scanned: Option<i64>,
    /// Why it stopped.
    #[serde(default)]
    #[schema(required = true)]
    pub stop_reason: Option<StopReason>,
    /// The cursor to resume from.
    #[serde(default)]
    #[schema(required = true)]
    pub resume_cursor: Option<String>,
    /// The error code, when it failed.
    #[serde(default)]
    #[schema(required = true)]
    pub error_code: Option<String>,
}

/// A sync run (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRun {
    /// Its id.
    pub id: String,
    /// The platform.
    pub platform: Platform,
    /// What started it.
    pub trigger: Trigger,
    /// The listing it walks.
    pub listing: Listing,
    /// The collection it maps into.
    #[schema(required = true)]
    pub collection_id: Option<i64>,
    /// Its state.
    pub state: RunState,
    /// Items scanned.
    pub scanned: i64,
    /// New posts.
    pub inserted: i64,
    /// Known posts that changed.
    pub updated: i64,
    /// Known posts.
    pub known: i64,
    /// Pages scanned.
    pub pages: i64,
    /// Why it stopped.
    #[schema(required = true)]
    pub stop_reason: Option<StopReason>,
    /// The cursor it left.
    #[schema(required = true)]
    pub resume_cursor: Option<String>,
    /// The error code.
    #[schema(required = true)]
    pub error_code: Option<String>,
    /// Whether it ran incrementally.
    pub incremental: bool,
    /// The consecutive-known threshold.
    pub stop_after_known: i64,
    /// When it started, unix ms.
    pub started_at: i64,
    /// When it ended, unix ms.
    #[schema(required = true)]
    pub finished_at: Option<i64>,
}

/// A page of runs (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct SyncRunPage {
    /// The runs, newest first.
    pub items: Vec<SyncRun>,
    /// The cursor to the next page, if any.
    #[schema(required = true)]
    pub next_cursor: Option<String>,
}

/// Query of `GET /sync-runs`.
#[derive(Clone, Debug, Default, Deserialize, IntoParams)]
#[serde(rename_all = "camelCase")]
#[into_params(parameter_in = Query)]
pub struct SyncRunQuery {
    /// Only this platform.
    pub platform: Option<Platform>,
    /// Only this state.
    pub state: Option<RunState>,
    /// The page cursor from a previous answer.
    pub cursor: Option<String>,
    /// Page size (1–100).
    pub limit: Option<u32>,
}

/// A listing the extension has synced (contract C4, `GET /extension/sources`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSource {
    /// Its platform.
    pub platform: Platform,
    /// The listing.
    pub listing: Listing,
    /// The collection it maps into.
    #[schema(required = true)]
    pub collection_id: Option<i64>,
    /// When a run of it last ran, unix ms.
    #[schema(required = true)]
    pub last_run_at: Option<i64>,
    /// When its last full walk reached the end of the feed, unix ms.
    #[schema(required = true)]
    pub last_full_at: Option<i64>,
}

/// The sources (contract C4).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionSources {
    /// The listings, newest run first.
    pub items: Vec<ExtensionSource>,
}

// ── Handlers ─────────────────────────────────────────────────────────────────

/// Opens a sync run (contract C4). An `ingest` token.
#[utoipa::path(
    post,
    path = "/api/v1/sync-runs",
    tag = "extension",
    operation_id = "openSyncRun",
    security(("bearer" = ["ingest"])),
    params(ExtensionHeaders),
    request_body = SyncRunCreate,
    responses((status = CREATED, description = "The run.", body = SyncRunOpened)),
)]
pub async fn open_sync_run(
    State(state): State<AppState>,
    token: TokenUser<scopes::Ingest>,
    Json(body): Json<SyncRunCreate>,
) -> Result<(axum::http::StatusCode, Json<SyncRunOpened>), ApiError> {
    let user_id = token.id().to_owned();
    let platform: repo::Platform = body.platform.into();
    let kind = body.listing.kind;
    if kind.platform() != platform {
        return Err(ApiError::invalid_field(
            "listing.kind",
            "does not belong to the platform",
        ));
    }
    let external_id = body
        .listing
        .external_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if kind.needs_external_id() && external_id.is_none() {
        return Err(ApiError::invalid_field(
            "listing.externalId",
            "is required for this listing",
        ));
    }
    if body.collection.mode == CollectionMode::Existing && body.collection.id.is_none() {
        return Err(ApiError::invalid_field(
            "collection.id",
            "is required for the existing mode",
        ));
    }

    // The known-run threshold comes from the live extension config.
    let stop_after_known = stop_after_known(&state, platform).await?;
    let source_key = source_key(kind, external_id);

    let db = state.user_db(&user_id).await?;
    let id = new_ulid();
    let now = now_ms();
    let listing_name = body.listing.name.clone();
    let open = {
        let (id, source_key) = (id.clone(), source_key.clone());
        let name = listing_name.clone();
        let external_id = external_id.map(str::to_owned);
        let mode = body.collection.mode;
        let given = body.collection.id;
        let trigger = body.trigger;
        blocking(move || {
            db.write(|tx| -> Result<Opened, RepoError> {
                sync::sweep_idle(tx, IDLE_MS, now)?;
                let (collection_id, created) = resolve_collection(
                    tx,
                    platform,
                    mode,
                    given,
                    external_id.as_deref(),
                    name.as_deref(),
                    now,
                )?;
                let source = sync::get_source(tx, platform, &source_key)?;
                let incremental = source.as_ref().is_some_and(|s| s.last_full_at.is_some());
                let resume_cursor = source.and_then(|s| s.resume_cursor);
                sync::upsert_source(
                    tx,
                    &SourceUpdate {
                        platform,
                        source_key: &source_key,
                        source_kind: kind.as_str(),
                        external_id: external_id.as_deref(),
                        source_name: name.as_deref(),
                        collection_id,
                    },
                    now,
                )?;
                sync::insert_run(
                    tx,
                    &NewRun {
                        id: &id,
                        platform,
                        trigger: trigger.as_str(),
                        source_kind: kind.as_str(),
                        source_key: &source_key,
                        listing_external_id: external_id.as_deref(),
                        listing_name: name.as_deref(),
                        collection_id,
                        incremental,
                        stop_after_known,
                        resume_cursor: resume_cursor.as_deref(),
                        client_version: None,
                    },
                    now,
                )?;
                Ok(Opened {
                    collection_id,
                    incremental,
                    resume_cursor,
                    created_collection: created,
                })
            })
        })
        .await?
    };
    // A new folder joins the library: refresh the folder list.
    if open.created_collection {
        state.events().stats_changed(&user_id);
    }
    Ok((
        axum::http::StatusCode::CREATED,
        Json(SyncRunOpened {
            id,
            incremental: open.incremental,
            stop_after_known,
            collection_id: open.collection_id,
            resume_cursor: open.resume_cursor,
        }),
    ))
}

/// What opening a run settled, inside the write.
struct Opened {
    collection_id: Option<i64>,
    incremental: bool,
    resume_cursor: Option<String>,
    created_collection: bool,
}

/// Ends or updates a run (contract C4). An `ingest` token.
#[utoipa::path(
    patch,
    path = "/api/v1/sync-runs/{id}",
    tag = "extension",
    operation_id = "updateSyncRun",
    security(("bearer" = ["ingest"])),
    params(
        ("id" = String, Path, description = "The run's id."),
        ExtensionHeaders,
    ),
    request_body = SyncRunUpdate,
    responses((status = OK, description = "The run.", body = SyncRun)),
)]
pub async fn update_sync_run(
    State(state): State<AppState>,
    token: TokenUser<scopes::Ingest>,
    Path(id): Path<String>,
    Json(body): Json<SyncRunUpdate>,
) -> Result<Json<SyncRun>, ApiError> {
    let user_id = token.id().to_owned();
    let now = now_ms();
    let db = state.user_db(&user_id).await?;

    let run = {
        let patch_id = id.clone();
        let (state_value, pages, scanned) = (body.state, body.pages, body.scanned);
        let (stop_reason, error_code) = (body.stop_reason, body.error_code.clone());
        let resume_cursor = body.resume_cursor.clone();
        blocking(move || {
            db.write(|tx| -> Result<Option<Run>, RepoError> {
                let patch = RunPatch {
                    state: state_value.as_str(),
                    pages,
                    scanned,
                    stop_reason: stop_reason.map(StopReason::as_str),
                    resume_cursor: resume_cursor.as_deref(),
                    error_code: error_code.as_deref(),
                };
                let Some(run) = sync::patch_run(tx, &patch_id, &patch, now)? else {
                    return Ok(None);
                };
                // A full walk makes the next run incremental; a capped one
                // leaves its cursor (P2-G1, P2-G2).
                if run.state != sync::STATE_RUNNING {
                    match stop_reason {
                        Some(StopReason::EndOfFeed) => {
                            sync::set_last_full(tx, run.platform, &run.source_key, now)?;
                        }
                        Some(StopReason::PageCap) => {
                            sync::set_resume_cursor(
                                tx,
                                run.platform,
                                &run.source_key,
                                resume_cursor.as_deref(),
                            )?;
                        }
                        _ => {}
                    }
                }
                Ok(Some(run))
            })
        })
        .await?
        .ok_or_else(|| ApiError::new(ErrorCode::SyncRunNotFound))?
    };

    if body.state != RunState::Running {
        record_pages(&run);
        emit_progress(&state, &user_id, &run);
        notify_end(&state, &user_id, &run, body.state).await;
    }
    Ok(Json(run_dto(run)?))
}

/// Lists the user's runs, newest first (contract C4). A session.
#[utoipa::path(
    get,
    path = "/api/v1/sync-runs",
    tag = "extension",
    operation_id = "listSyncRuns",
    params(SyncRunQuery),
    responses((status = OK, description = "A page of runs.", body = SyncRunPage)),
)]
pub async fn list_sync_runs(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<SyncRunQuery>,
) -> Result<Json<SyncRunPage>, ApiError> {
    let limit = page_size(query.limit) as usize;
    let platform = query.platform.map(Into::into);
    let state_filter = query.state.map(RunState::as_str);
    let before = query.cursor.as_deref().map(decode_cursor).transpose()?;
    let db = state.user_db(user.id()).await?;
    let now = now_ms();
    let (runs, next) = blocking(move || {
        db.write(|tx| -> Result<_, RepoError> {
            sync::sweep_idle(tx, IDLE_MS, now)?;
            sync::list_runs(tx, platform, state_filter, before.as_ref(), limit)
        })
    })
    .await?;
    let items = runs.into_iter().map(run_dto).collect::<Result<_, _>>()?;
    Ok(Json(SyncRunPage {
        items,
        next_cursor: next.map(|c| encode_cursor(&c)),
    }))
}

/// The listings the extension has synced (contract C4). An `ingest` token.
#[utoipa::path(
    get,
    path = "/api/v1/extension/sources",
    tag = "extension",
    operation_id = "listExtensionSources",
    security(("bearer" = ["ingest"])),
    params(ExtensionHeaders),
    responses((status = OK, description = "The sources.", body = ExtensionSources)),
)]
pub async fn list_extension_sources(
    State(state): State<AppState>,
    token: TokenUser<scopes::Ingest>,
) -> Result<Json<ExtensionSources>, ApiError> {
    let db = state.user_db(token.id()).await?;
    let sources = blocking(move || db.read(sync::list_sources)).await?;
    let items = sources
        .into_iter()
        .filter_map(source_dto)
        .collect::<Vec<_>>();
    Ok(Json(ExtensionSources { items }))
}

// ── Helpers ────────────────────────────────────────────────────────────────────

/// The source's stable identity: its kind, plus the folder or board id.
fn source_key(kind: ListingKind, external_id: Option<&str>) -> String {
    match external_id {
        Some(external_id) => format!("{}:{external_id}", kind.as_str()),
        None => kind.as_str().to_owned(),
    }
}

/// The known-run threshold for `platform`, from the extension config.
async fn stop_after_known(state: &AppState, platform: repo::Platform) -> Result<i64, ApiError> {
    let flags = state.extension().flags().get(state.control()).await?;
    let platforms = &flags.config().platforms;
    let threshold = match platform {
        repo::Platform::Instagram => platforms.instagram.stop_after_known,
        repo::Platform::Twitter => platforms.twitter.stop_after_known,
        repo::Platform::Pinterest => platforms.pinterest.stop_after_known,
        repo::Platform::Web | repo::Platform::Manual => 0,
    };
    Ok(i64::from(threshold))
}

/// Resolves the collection a run maps into; the second field is whether it was
/// created now (so the caller refreshes the folder list).
fn resolve_collection(
    tx: &rusqlite::Transaction<'_>,
    platform: repo::Platform,
    mode: CollectionMode,
    given: Option<i64>,
    external_id: Option<&str>,
    name: Option<&str>,
    now: i64,
) -> Result<(Option<i64>, bool), RepoError> {
    match mode {
        CollectionMode::None => Ok((None, false)),
        CollectionMode::Existing => match given {
            // Validated to exist before the write; gone since means no mapping.
            Some(id) if collections::get(tx, id)?.is_some() => Ok((Some(id), false)),
            _ => Ok((None, false)),
        },
        CollectionMode::Auto => {
            let Some(external_id) = external_id else {
                return Ok((None, false)); // a whole feed, not a folder
            };
            if let Some(existing) = collections::find_linked(tx, platform, external_id)? {
                return Ok((Some(existing.id), false)); // a user's rename is kept
            }
            let created = collections::create(
                tx,
                &NewCollection {
                    name: name.unwrap_or(external_id).to_owned(),
                    color: None,
                    platform: Some(platform),
                    external_id: Some(external_id.to_owned()),
                    source_name: name.map(str::to_owned),
                },
                now,
            )?;
            Ok((Some(created.id), true))
        }
    }
}

/// Records a finished run's pages for the §6.2 "≤ 2 pages" exit metric.
fn record_pages(run: &Run) {
    metrics::histogram!(
        SYNC_RUN_PAGES,
        "platform" => run.platform.as_str(),
        "trigger" => run.trigger.clone(),
    )
    .record(run.pages as f64);
}

/// Emits the run's final counters as `sync.progress` so a live view settles.
fn emit_progress(state: &AppState, user_id: &str, run: &Run) {
    state.events().sync_progress(
        user_id,
        SyncProgressEvent {
            run_id: run.id.clone(),
            platform: run.platform.as_str().to_owned(),
            listing: SyncListing {
                kind: run.source_kind.clone(),
                external_id: run.listing_external_id.clone(),
                name: run.listing_name.clone(),
            },
            trigger: run.trigger.clone(),
            scanned: run.scanned,
            inserted: run.inserted,
            updated: run.updated,
            known: run.known,
            pages: run.pages,
            state: run.state.clone(),
        },
    );
}

/// Posts the end-of-run notification of a run the user asked for.
async fn notify_end(state: &AppState, user_id: &str, run: &Run, final_state: RunState) {
    let Some(trigger) = Trigger::from_stored(&run.trigger).filter(|t| t.notifies()) else {
        return;
    };
    let _ = trigger;
    let login = run.stop_reason.as_deref() == Some(StopReason::LoginRequired.as_str())
        || run.error_code.as_deref() == Some("login_required");
    let code = match final_state {
        RunState::Done => "sync.done",
        RunState::Failed if login => "sync.login_required",
        RunState::Failed => "sync.failed",
        // A stopped (by the user or idle) run is not worth a notification.
        RunState::Stopped | RunState::Running => return,
    };
    let params = json!({
        "platform": run.platform.as_str(),
        "inserted": run.inserted,
        "known": run.known,
        "scanned": run.scanned,
    });
    let new = shelfy_core::repo::notifications::NewNotification {
        kind: "sync".to_owned(),
        code: code.to_owned(),
        params: params.as_object().cloned().unwrap_or_default(),
        target: None,
    };
    if let Err(err) = events::notify(state, user_id, new).await {
        tracing::warn!(error = %err, "cannot post the sync notification");
    }
}

/// Maps a stored run to its wire shape; a stored value that no longer parses
/// is a server bug (500).
fn run_dto(run: Run) -> Result<SyncRun, ApiError> {
    let unknown = |what: &str| ApiError::internal(anyhow::anyhow!("stored sync run has {what}"));
    let trigger =
        Trigger::from_stored(&run.trigger).ok_or_else(|| unknown("an unknown trigger"))?;
    let kind =
        ListingKind::from_stored(&run.source_kind).ok_or_else(|| unknown("an unknown listing"))?;
    let state = RunState::from_stored(&run.state).ok_or_else(|| unknown("an unknown state"))?;
    let stop_reason = match run.stop_reason.as_deref() {
        Some(reason) => {
            Some(StopReason::from_stored(reason).ok_or_else(|| unknown("a stop reason"))?)
        }
        None => None,
    };
    Ok(SyncRun {
        id: run.id,
        platform: run.platform.into(),
        trigger,
        listing: Listing {
            kind,
            external_id: run.listing_external_id,
            name: run.listing_name,
        },
        collection_id: run.collection_id,
        state,
        scanned: run.scanned,
        inserted: run.inserted,
        updated: run.updated,
        known: run.known,
        pages: run.pages,
        stop_reason,
        resume_cursor: run.resume_cursor,
        error_code: run.error_code,
        incremental: run.incremental,
        stop_after_known: run.stop_after_known,
        started_at: run.started_at,
        finished_at: run.finished_at,
    })
}

/// A source to its wire shape; a source with an unknown listing kind is left
/// out (it cannot be acted on).
fn source_dto(source: Source) -> Option<ExtensionSource> {
    let kind = source
        .source_kind
        .as_deref()
        .and_then(ListingKind::from_stored)?;
    Some(ExtensionSource {
        platform: source.platform.into(),
        listing: Listing {
            kind,
            external_id: source.external_id,
            name: source.source_name,
        },
        collection_id: source.collection_id,
        last_run_at: source.last_run_at,
        last_full_at: source.last_full_at,
    })
}

/// Encodes a keyset cursor as url-safe base64 of `started_at:id`.
fn encode_cursor(cursor: &RunCursor) -> String {
    URL_SAFE_NO_PAD.encode(format!("{}:{}", cursor.started_at, cursor.id))
}

/// Decodes a cursor; a malformed one is 400 `invalid_cursor`.
fn decode_cursor(text: &str) -> Result<RunCursor, ApiError> {
    let invalid = || ApiError::new(ErrorCode::InvalidCursor);
    let bytes = URL_SAFE_NO_PAD.decode(text).map_err(|_| invalid())?;
    let decoded = String::from_utf8(bytes).map_err(|_| invalid())?;
    let (started_at, id) = decoded.split_once(':').ok_or_else(invalid)?;
    Ok(RunCursor {
        started_at: started_at.parse().map_err(|_| invalid())?,
        id: id.to_owned(),
    })
}
