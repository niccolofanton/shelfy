//! `GET /api/v1/me/providers` (plan §2.15, §2.19; P3-09): the AI providers
//! the account may use, with no key.
//!
//! The owner sees the operator provider (the owner's node, `managed`: no edit
//! or delete, E4) and their own BYOK providers (P3-19); a member sees only
//! their BYOK providers. Each carries its models, whether it transcribes, and
//! its current status; never a key.

use std::sync::Arc;

use axum::extract::State;
use axum::response::{IntoResponse as _, Response};
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::ai::{Caller, ProviderSummary};
use crate::control::users::{self, Role};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::Json;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().routes(routes!(get_providers))
}

/// Whether `user` is the instance owner, for the operator provider (E4).
async fn is_owner(state: &AppState, user: &CurrentUser) -> Result<bool, ApiError> {
    let control = Arc::clone(state.control());
    let id = user.id().to_owned();
    let found = blocking(move || control.read(|conn| users::get(conn, &id))).await?;
    let role = found
        .ok_or_else(|| ApiError::new(ErrorCode::Unauthorized))?
        .role;
    Ok(role == Role::Owner)
}

/// The AI providers the account may use. No keys: the operator provider's key
/// is the server's, and a BYOK key is only ever shown as `last4` by P3-19.
#[utoipa::path(
    get,
    path = "/api/v1/me/providers",
    tag = "account",
    operation_id = "getProviders",
    responses(
        (status = OK, description = "The AI providers, with no keys.", body = Vec<ProviderSummary>),
    )
)]
pub async fn get_providers(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let caller = Caller::new(user.id(), is_owner(&state, &user).await?);
    let providers = state.ai().list_providers(&state, caller).await?;
    Ok(no_store(Json(providers).into_response()))
}
