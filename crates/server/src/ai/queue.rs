//! The analyze API and the AI queue (plan §2.9 `analyze`, §2.12, G3-6, G3-8;
//! P3-13): the orchestration behind `POST /ai/analyze`, `GET /ai/queue`,
//! `POST /ai/queue/{cancel,retry}`, and the capture seam
//! [`enqueue`](enqueue). The item-work state lives in the library
//! ([`shelfy_core::ai::queue`]); this module resolves selectors, counts and
//! estimates, holds the short-lived confirmation tokens and the operator's
//! measured pace, writes the state through [`crate::library::write`] (so the
//! change is announced and generations retire), and arms the drain job.
//!
//! The estimate and confirmation guard the shared operator node (plan lane
//! rule 10): a first `POST /ai/analyze` returns counts, an estimate and a
//! token; the second, with the token and an `Idempotency-Key`, enqueues. A
//! single post skips the token and enqueues on the first call.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use rusqlite::OptionalExtension as _;
use serde::Serialize;
use shelfy_core::ai::estimate::{Estimate, Pace};
use shelfy_core::ai::queue::{self as core_queue, Item, Mode, Reach, ScopeCounts, StateCounts};
use shelfy_core::repo::settings::{self, AiPrices};
use shelfy_core::selector::Selector;
use tokio::time::Duration;
use utoipa::ToSchema;

use crate::events::model::ProviderState;
use crate::library::{self, Change};
use crate::state::AppState;
use crate::{error::ApiError, events::model::ChangeReason};

/// How long a confirmation token is valid (plan §2.9 `analyze`).
pub const CONFIRM_TTL: Duration = Duration::from_secs(10 * 60);
/// A page of the queue view.
pub const PAGE_SIZE: u32 = 50;

/// A pending confirmation: whose it is and what it would enqueue.
struct Confirm {
    user_id: String,
    selector: Selector,
    mode: Mode,
    expires: i64,
    deep: bool,
    ids: Vec<i64>,
    provider: String,
    model: String,
}

/// The process's live confirmation tokens, keyed by the token string. Owner
/// only and short-lived, so a plain map is enough; a restart drops them (the
/// user simply asks for a new estimate).
static CONFIRMS: LazyLock<Mutex<HashMap<String, Confirm>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// The operator node's measured pace, shared by the estimate and the queue
/// ETA; the drain records finished calls into it.
static PACE: LazyLock<Mutex<Pace>> = LazyLock::new(|| Mutex::new(Pace::new()));

fn confirms() -> MutexGuard<'static, HashMap<String, Confirm>> {
    CONFIRMS.lock().unwrap_or_else(PoisonError::into_inner)
}

fn pace() -> MutexGuard<'static, Pace> {
    PACE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Records one finished catalog call of `ms` into the operator pace (the
/// drain calls this).
pub fn record_duration(ms: u64) {
    pace().record(ms);
}

/// The operator's mean ms per post, when measured.
#[must_use]
pub fn measured_ms_per_post() -> Option<u64> {
    pace().ms_per_post()
}

// ── Analyze ──────────────────────────────────────────────────────────────────

/// The counts an analyze request reports.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeCounts {
    /// Posts that will be enqueued.
    pub analyzable: u64,
    /// Posts that need media before they can be analyzed (G3-9).
    pub waiting_for_media: u64,
    /// Posts already queued.
    pub already_queued: u64,
}

impl From<ScopeCounts> for AnalyzeCounts {
    fn from(c: ScopeCounts) -> Self {
        Self {
            analyzable: c.analyzable,
            waiting_for_media: c.waiting_for_media,
            already_queued: c.already_queued,
        }
    }
}

/// The token estimate of an analyze request.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeEstimate {
    /// Estimated prompt tokens.
    pub input_tokens: u64,
    /// Estimated answer tokens.
    pub output_tokens: u64,
    /// Estimated wall-clock time for the operator node, ms; `null` for a
    /// priced cloud route or when no pace is known.
    #[schema(required = true)]
    pub eta_ms: Option<u64>,
    /// The estimated price in US dollars for a priced BYOK route; `null` for
    /// the operator node.
    #[schema(required = true)]
    pub cost_usd: Option<f64>,
}

impl AnalyzeEstimate {
    fn of(estimate: Estimate, route: &super::Route, prices: Option<AiPrices>) -> Self {
        Self {
            input_tokens: estimate.input_tokens,
            output_tokens: estimate.output_tokens,
            eta_ms: route
                .provider
                .is_operator()
                .then_some(estimate.eta_ms)
                .flatten(),
            cost_usd: if route.provider.is_operator() {
                None
            } else {
                estimate.cost_usd(
                    prices.map(|p| p.input_per_million_usd),
                    prices.map(|p| p.output_per_million_usd),
                )
            },
        }
    }
}

/// The answer of `POST /ai/analyze`.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AnalyzeResult {
    /// What the request would do.
    pub counts: AnalyzeCounts,
    /// The estimate.
    pub estimate: AnalyzeEstimate,
    /// Whether the work was enqueued (a single post, or the confirm call).
    pub queued: bool,
    /// How many posts were enqueued (when `queued`).
    pub enqueued: u64,
    /// A confirmation token, valid for 10 minutes, to send with the second
    /// call; `null` when the work was enqueued already or there is nothing to
    /// do.
    #[schema(required = true)]
    pub confirm_token: Option<String>,
}

/// Runs an analyze request (plan §2.9 `analyze`): the first call (no token)
/// counts and estimates, and either enqueues at once (a single post) or
/// returns a token; the second call (with the token) enqueues.
///
/// # Errors
///
/// 422 `confirm_token_invalid` for an unknown or expired token; the selector
/// is over its caps; database errors.
pub async fn analyze(
    state: &AppState,
    user_id: &str,
    selector: Selector,
    mode: Mode,
    confirm_token: Option<String>,
    deep: bool,
) -> Result<AnalyzeResult, ApiError> {
    analyze_with_preview(state, user_id, selector, mode, confirm_token, deep, false).await
}

/// Runs an analyze request with an explicit preview option for UI confirmation.
/// A preview never writes the queue, including single-post requests.
///
/// # Errors
/// The same errors as [`analyze`]; a preview with a confirmation token is invalid.
pub async fn analyze_with_preview(
    state: &AppState,
    user_id: &str,
    selector: Selector,
    mode: Mode,
    confirm_token: Option<String>,
    deep: bool,
    estimate_only: bool,
) -> Result<AnalyzeResult, ApiError> {
    if estimate_only && confirm_token.is_some() {
        return Err(ApiError::invalid_field(
            "estimateOnly",
            "cannot confirm in a preview",
        ));
    }
    let now = state.jobs().clock().now_ms();
    let owner = crate::jobs::ai_drain::is_owner(state, user_id).await?;
    let route = state
        .ai()
        .route(
            state,
            super::Caller::new(user_id, owner),
            super::Task::Catalog,
        )
        .await?;
    let prices = match &route.provider {
        super::ProviderRef::Operator => None,
        super::ProviderRef::User(id) => {
            let id = id.clone();
            read(state, user_id, move |conn| {
                Ok(settings::read(conn)?
                    .ai
                    .providers
                    .into_iter()
                    .find(|p| p.id == id)
                    .and_then(|p| p.prices))
            })
            .await?
        }
    };
    if let Some(token) = confirm_token {
        let confirm = take_confirm(&token, user_id, &selector, mode, deep, now)
            .ok_or_else(|| ApiError::new(crate::error::ErrorCode::ConfirmTokenInvalid))?;
        if confirm.provider != route.provider.id() || confirm.model != route.model {
            return Err(ApiError::new(crate::error::ErrorCode::ConfirmTokenInvalid));
        }
        let (enqueued, web) =
            enqueue_ids(state, user_id, confirm.ids, confirm.mode, confirm.deep, now).await?;
        return Ok(AnalyzeResult {
            counts: AnalyzeCounts {
                analyzable: enqueued,
                waiting_for_media: 0,
                already_queued: 0,
            },
            estimate: AnalyzeEstimate::of(
                Estimate::of_catalogs(enqueued.saturating_sub(web), web, measured_ms_per_post()),
                &route,
                prices,
            ),
            queued: true,
            enqueued,
            confirm_token: None,
        });
    }

    let (counts, ids, web) = {
        let selector = selector.clone();
        read(state, user_id, move |conn| {
            // The displayed estimate and confirmed population come from the
            // same snapshot, even while another import changes the library.
            let counts = core_queue::scope_counts(conn, &selector, mode, now)?;
            let ids = core_queue::eligible_ids(conn, &selector, mode)?;
            let web = shelfy_core::ai::web_inputs::count_ids(conn, &ids)?;
            Ok((counts, ids, web))
        })
        .await?
    };
    let estimate = Estimate::of_catalogs(
        counts.analyzable.saturating_sub(web),
        web,
        measured_ms_per_post(),
    );

    // Nothing to do, or a single post: no confirmation needed (plan §2.9).
    if counts.analyzable == 0 {
        return Ok(AnalyzeResult {
            counts: counts.into(),
            estimate: AnalyzeEstimate::of(estimate, &route, prices),
            queued: false,
            enqueued: 0,
            confirm_token: None,
        });
    }
    if !estimate_only && matches!(&selector, Selector::Keys(keys) if keys.len() == 1) {
        let enqueued = enqueue_now(state, user_id, &selector, mode, deep, now).await?;
        return Ok(AnalyzeResult {
            counts: counts.into(),
            estimate: AnalyzeEstimate::of(estimate, &route, prices),
            queued: true,
            enqueued,
            confirm_token: None,
        });
    }

    let token = store_confirm(user_id, selector, mode, deep, ids, now, &route);
    Ok(AnalyzeResult {
        counts: counts.into(),
        estimate: AnalyzeEstimate::of(estimate, &route, prices),
        queued: false,
        enqueued: 0,
        confirm_token: Some(token),
    })
}

/// Enqueues the analyzable posts of `selector` in `mode`, announces the change
/// and arms the drain. Returns how many were newly enqueued.
async fn enqueue_now(
    state: &AppState,
    user_id: &str,
    selector: &Selector,
    mode: Mode,
    deep: bool,
    now: i64,
) -> Result<u64, ApiError> {
    let selector = selector.clone();
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let ids = core_queue::eligible_ids(tx, &selector, mode)?;
        let n = core_queue::mark_pending(tx, &selector, mode, now)?;
        core_queue::set_deep(tx, &ids, deep, now)?;
        // Many posts change their AI status: the client reloads the view.
        Ok(Change {
            value: n,
            keys: None,
        })
    })
    .await?;
    if written.value > 0 {
        super::super::jobs::ai_drain::enqueue(state.jobs(), user_id).await?;
    }
    Ok(written.value)
}

/// Marks the given posts `pending` and arms the drain: the seam P4's capture
/// ingest and a recapture use (`ai::queue::enqueue`, the cross-phase
/// dependency). Returns how many changed.
///
/// # Errors
///
/// Database errors.
pub async fn enqueue(state: &AppState, user_id: &str, post_ids: Vec<i64>) -> Result<u64, ApiError> {
    if post_ids.is_empty() {
        return Ok(0);
    }
    let now = state.jobs().clock().now_ms();
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let n = core_queue::set_pending(tx, &post_ids, now)?;
        Ok(Change {
            value: n,
            keys: None,
        })
    })
    .await?;
    if written.value > 0 {
        super::super::jobs::ai_drain::enqueue(state.jobs(), user_id).await?;
    }
    Ok(written.value)
}

/// Auto-analysis after a committed capture (and capture recovery). The current
/// version, feature preference, vision route and capture dedupe are checked here.
pub async fn enqueue_web(
    state: &AppState,
    user_id: &str,
    key: &str,
    capture_id: i64,
) -> Result<u64, ApiError> {
    let enabled = read(state, user_id, |conn| {
        Ok(shelfy_core::repo::settings::read(conn)?)
    })
    .await?
    .ai
    .auto_analyze_websites;
    if !enabled {
        return Ok(0);
    }
    let owner = crate::jobs::ai_drain::is_owner(state, user_id).await?;
    if state
        .ai()
        .route(
            state,
            super::Caller::new(user_id, owner),
            super::Task::Catalog,
        )
        .await
        .is_err()
    {
        return Ok(0);
    }
    let key = key.to_owned();
    let now = state.jobs().clock().now_ms();
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let n = core_queue::set_web_pending(tx, &key, capture_id, now)?;
        Ok(Change {
            value: n,
            keys: Some(vec![key]),
        })
    })
    .await?;
    // Wake an already-pending row too: recover the library/control commit gap.
    let pending = read(state, user_id, |conn| {
        Ok(core_queue::next_pending_at(conn)?)
    })
    .await?;
    if pending.is_some() {
        crate::jobs::ai_drain::enqueue(state.jobs(), user_id).await?;
    }
    Ok(written.value)
}

// ── Cancel and retry ──────────────────────────────────────────────────────────

/// Cancels queued work (plan §2.9, G3-6): resets `pending`/`analyzing` posts,
/// and — when cancelling everything — stops the running drain. Returns how
/// many items changed.
///
/// # Errors
///
/// Database errors.
pub async fn cancel(state: &AppState, user_id: &str, reach: Reach) -> Result<u64, ApiError> {
    let now = state.jobs().clock().now_ms();
    if matches!(reach, Reach::All) {
        // Stop the running drain so it does not keep claiming.
        state
            .jobs()
            .cancel_all(user_id, super::AI_DRAIN_KIND)
            .await?;
    }
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let n = core_queue::cancel(tx, &reach, now)?;
        Ok(Change {
            value: n,
            keys: None,
        })
    })
    .await?;
    Ok(written.value)
}

/// Retries failed items (plan §2.9, G3-6): resets `error` posts to `pending`
/// and arms the drain. Returns how many changed.
///
/// # Errors
///
/// Database errors.
pub async fn retry(state: &AppState, user_id: &str, reach: Reach) -> Result<u64, ApiError> {
    let now = state.jobs().clock().now_ms();
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let n = core_queue::retry(tx, &reach, now)?;
        Ok(Change {
            value: n,
            keys: None,
        })
    })
    .await?;
    if written.value > 0 {
        super::super::jobs::ai_drain::enqueue(state.jobs(), user_id).await?;
    }
    Ok(written.value)
}

// ── Queue view ────────────────────────────────────────────────────────────────

/// The counts of the AI queue.
#[derive(Clone, Copy, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueCounts {
    /// Never analyzed.
    pub unanalyzed: u64,
    /// Queued.
    pub pending: u64,
    /// Being analyzed.
    pub analyzing: u64,
    /// Done.
    pub done: u64,
    /// Failed.
    pub error: u64,
}

impl From<StateCounts> for QueueCounts {
    fn from(c: StateCounts) -> Self {
        Self {
            unanalyzed: c.unanalyzed,
            pending: c.pending,
            analyzing: c.analyzing,
            done: c.done,
            error: c.error,
        }
    }
}

/// One item of the queue view.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    /// The post's key.
    pub post_key: String,
    /// Its AI state.
    pub status: String,
    /// Tries spent.
    pub attempts: i64,
    /// Next attempt time (a backed-off `pending` item), unix ms.
    #[schema(required = true)]
    pub next_at: Option<i64>,
    /// The last error code (an `error` item).
    #[schema(required = true)]
    pub error: Option<String>,
}

impl From<Item> for QueueItem {
    fn from(item: Item) -> Self {
        Self {
            post_key: item.key,
            status: item.status,
            attempts: item.attempts,
            next_at: item.next_at,
            error: item.error,
        }
    }
}

/// `GET /ai/queue`.
#[derive(Clone, Debug, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct QueueView {
    /// The counts per state.
    pub counts: QueueCounts,
    /// The items of the requested state, newest first.
    pub items: Vec<QueueItem>,
    /// The cursor for the next page; `null` at the end.
    #[schema(required = true)]
    pub cursor: Option<String>,
    /// An ETA for the queued and in-flight work, ms, from recent durations;
    /// `null` when nothing is queued.
    #[schema(required = true)]
    pub eta_ms: Option<u64>,
    /// The provider's state (`ok`, `offline`, …); an offline node means the
    /// queue is waiting, not stuck.
    #[schema(required = true)]
    pub provider_state: Option<ProviderState>,
    /// Whether the queue is paused (an invalid key, or a manual pause).
    pub paused: bool,
}

/// Reads the AI queue view for a page of `status` after `cursor`.
///
/// # Errors
///
/// 400 `invalid_cursor`; database errors.
pub async fn queue_view(
    state: &AppState,
    user_id: &str,
    status: Option<String>,
    cursor: Option<i64>,
) -> Result<QueueView, ApiError> {
    let (counts, items, next, web) = read(state, user_id, move |conn| {
        let counts = core_queue::state_counts(conn)?;
        let (items, next) = core_queue::list(conn, status.as_deref(), cursor, PAGE_SIZE)?;
        let web:i64=conn.query_row("SELECT count(*) FROM posts p WHERE deleted_at IS NULL AND ai_status IN ('pending','analyzing') AND (platform='web' OR media_type='website') AND EXISTS(SELECT 1 FROM web_captures c WHERE c.id=p.current_capture_id AND c.post_id=p.id)",[],|r|r.get(0)).map_err(shelfy_core::repo::RepoError::from)?;
        Ok::<_, ApiError>((counts, items, next, u64::try_from(web).unwrap_or(0)))
    })
    .await?;
    let outstanding = counts.pending + counts.analyzing;
    let eta_ms =
        Estimate::of_catalogs(outstanding.saturating_sub(web), web, measured_ms_per_post()).eta_ms;
    let owner = crate::jobs::ai_drain::is_owner(state, user_id).await?;
    let caller = super::Caller::new(user_id, owner);
    let provider_state =
        if let Ok(route) = state.ai().route(state, caller, super::Task::Catalog).await {
            state
                .ai()
                .list_providers(state, caller)
                .await?
                .into_iter()
                .find(|provider| provider.id == route.provider.id())
                .map(|provider| provider.status)
        } else {
            None
        };
    Ok(QueueView {
        counts: counts.into(),
        items: items.into_iter().map(QueueItem::from).collect(),
        cursor: next.map(|id| id.to_string()),
        eta_ms,
        provider_state,
        paused: state.jobs().is_paused(user_id, super::AI_DRAIN_KIND),
    })
}

// ── Confirmation tokens ────────────────────────────────────────────────────────

fn store_confirm(
    user_id: &str,
    selector: Selector,
    mode: Mode,
    deep: bool,
    ids: Vec<i64>,
    now: i64,
    route: &super::Route,
) -> String {
    let token = new_token();
    let mut map = confirms();
    map.retain(|_, c| c.expires > now);
    // Bound outstanding estimates; old estimates can simply be requested again.
    if map.len() >= 256 {
        map.clear();
    }
    map.insert(
        token.clone(),
        Confirm {
            user_id: user_id.to_owned(),
            selector,
            mode,
            deep,
            ids,
            provider: route.provider.id().to_owned(),
            model: route.model.clone(),
            expires: now + CONFIRM_TTL.as_millis() as i64,
        },
    );
    token
}

fn take_confirm(
    token: &str,
    user_id: &str,
    selector: &Selector,
    mode: Mode,
    deep: bool,
    now: i64,
) -> Option<Confirm> {
    let mut map = confirms();
    let c = map.get(token)?;
    if c.user_id != user_id
        || c.expires <= now
        || &c.selector != selector
        || c.mode != mode
        || c.deep != deep
    {
        return None;
    }
    map.remove(token)
}

async fn enqueue_ids(
    state: &AppState,
    user_id: &str,
    ids: Vec<i64>,
    mode: Mode,
    deep: bool,
    now: i64,
) -> Result<(u64, u64), ApiError> {
    let written = library::write(state, user_id, ChangeReason::Ai, move |tx| {
        let mut n = 0;
        let mut web = 0;
        // Recheck eligibility without widening the population the user confirmed.
        for id in ids {
            let key: Option<(String, bool)> = tx
                .query_row(
                    "SELECT key,(platform='web' OR media_type='website') FROM posts WHERE id=?1",
                    [id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((key, is_web)) = key {
                let selector = Selector::Keys(vec![key]);
                if core_queue::mark_pending(tx, &selector, mode, now)? > 0 {
                    core_queue::set_deep(tx, &[id], deep, now)?;
                    n += 1;
                    web += u64::from(is_web);
                }
            }
        }
        Ok(Change {
            value: (n, web),
            keys: None,
        })
    })
    .await?;
    if written.value.0 > 0 {
        crate::jobs::ai_drain::enqueue(state.jobs(), user_id).await?;
    }
    Ok(written.value)
}

fn new_token() -> String {
    let (a, b) = (
        getrandom::u64().expect("system entropy unavailable"),
        getrandom::u64().expect("system entropy unavailable"),
    );
    format!("{a:016x}{b:016x}")
}

/// Runs a read on `user_id`'s library off the async workers.
async fn read<T, F>(state: &AppState, user_id: &str, f: F) -> Result<T, ApiError>
where
    F: FnOnce(&rusqlite::Connection) -> Result<T, ApiError> + Send + 'static,
    T: Send + 'static,
{
    let db = state.user_db(user_id).await?;
    crate::state::blocking(move || db.read(|conn| f(conn))).await
}

#[cfg(test)]
mod estimate_tests {
    use super::*;
    use shelfy_ai::ProviderKind;

    #[test]
    fn priced_cloud_estimates_use_both_token_prices_and_operator_remains_unpriced() {
        let estimate = Estimate {
            input_tokens: 1_000_000,
            output_tokens: 500_000,
            eta_ms: Some(1000),
            ..Estimate::default()
        };
        let mut route = super::super::Route {
            provider: super::super::ProviderRef::User("cloud".into()),
            kind: ProviderKind::OpenAiCompatible,
            model: "synthetic".into(),
            task: super::super::Task::Catalog,
        };
        let prices = Some(AiPrices {
            input_per_million_usd: 2.0,
            output_per_million_usd: 8.0,
        });
        let cloud = AnalyzeEstimate::of(estimate, &route, prices);
        assert_eq!(cloud.cost_usd, Some(6.0));
        assert_eq!(cloud.eta_ms, None);
        assert_eq!(AnalyzeEstimate::of(estimate, &route, None).cost_usd, None);
        route.provider = super::super::ProviderRef::Operator;
        let operator = AnalyzeEstimate::of(estimate, &route, prices);
        assert_eq!(operator.cost_usd, None);
        assert_eq!(operator.eta_ms, Some(1000));
    }
}
