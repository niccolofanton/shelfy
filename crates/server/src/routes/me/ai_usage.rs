//! `GET /api/v1/me/usage/ai?days=` (plan §2.15, §3.6; P3-09): the account's
//! AI usage, one row per UTC day with calls and tokens.
//!
//! A row's `cost` is filled only for a priced BYOK provider (P3-19 adds the
//! prices); the operator provider has no cost. Days with no AI activity are
//! left out.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::{IntoParams, ToSchema};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::control::usage_daily;
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::Json;
use crate::ids::now_ms;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// The default and the bounds of `days`.
const DEFAULT_DAYS: u32 = 30;
const MAX_DAYS: u32 = 365;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_ai_usage))
}

/// How many days back to report.
#[derive(Clone, Copy, Debug, Deserialize, IntoParams)]
#[into_params(parameter_in = Query)]
pub struct DaysQuery {
    /// Days up to today (UTC), 1–365; default 30.
    #[serde(default)]
    pub days: Option<u32>,
}

/// One day's AI usage.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiUsageDay {
    /// The UTC day, `YYYY-MM-DD`.
    pub day: String,
    /// AI calls that day.
    pub calls: i64,
    /// Prompt tokens providers reported.
    pub input_tokens: i64,
    /// Answer tokens providers reported.
    pub output_tokens: i64,
    /// The cost in US dollars, for a priced BYOK provider; `null` otherwise.
    #[schema(required = true)]
    pub cost: Option<f64>,
}

/// The account's AI usage.
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct AiUsage {
    /// The daily rows, newest day first.
    pub days: Vec<AiUsageDay>,
}

/// The account's AI usage over the last `days` days (default 30).
#[utoipa::path(
    get,
    path = "/api/v1/me/usage/ai",
    tag = "account",
    operation_id = "getAiUsage",
    params(DaysQuery),
    responses(
        (status = OK, description = "The daily AI usage.", body = AiUsage),
    )
)]
pub async fn get_ai_usage(
    State(state): State<AppState>,
    user: CurrentUser,
    Query(query): Query<DaysQuery>,
) -> Result<Response, ApiError> {
    let days = query.days.unwrap_or(DEFAULT_DAYS).clamp(1, MAX_DAYS);
    let control = Arc::clone(state.control());
    let id = user.id().to_owned();
    let now = now_ms();
    let rows =
        blocking(move || control.read(|conn| usage_daily::ai_recent(conn, &id, days, now))).await?;
    let days = rows
        .into_iter()
        .map(|row| AiUsageDay {
            day: row.day,
            calls: row.calls,
            input_tokens: row.in_tokens,
            output_tokens: row.out_tokens,
            cost: None,
        })
        .collect();
    Ok(no_store(Json(AiUsage { days }).into_response()))
}
