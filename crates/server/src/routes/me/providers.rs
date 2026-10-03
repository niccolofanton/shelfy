//! `GET /api/v1/me/providers` (plan §2.15, §2.19; P3-09): the AI providers
//! the account may use, with no key.
//!
//! The owner sees the operator provider (the owner's node, `managed`: no edit
//! or delete, E4) and their own BYOK providers (P3-19); a member sees only
//! their BYOK providers. Each carries its models, whether it transcribes, and
//! its current status; never a key.

use std::sync::Arc;

use crate::ai::providers::{self, ConsentRequest, ProviderInput, ProviderTest};
use crate::auth::RecentAuth;
use axum::extract::{Path, State};
use axum::http::StatusCode;
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
    OpenApiRouter::new()
        .routes(routes!(get_providers))
        .routes(routes!(put_provider, delete_provider))
        .routes(routes!(test_provider))
        .routes(routes!(consent_provider))
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

#[utoipa::path(put, path = "/api/v1/me/providers/{id}", tag = "account", operation_id = "putProvider",
    params(("id" = String, Path)), request_body = ProviderInput,
    responses((status = NO_CONTENT, description = "Provider saved; credential is write-only.")))]
pub async fn put_provider(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
    Path(id): Path<String>,
    body: Result<Json<ProviderInput>, ApiError>,
) -> Result<Response, ApiError> {
    // Serde errors can quote rejected values. Credential-bearing requests use only the stable code.
    let Json(input) = body.map_err(|error| ApiError::new(error.code()))?;
    providers::put(&state, user.id(), &id, input).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
#[utoipa::path(delete, path = "/api/v1/me/providers/{id}", tag = "account", operation_id = "deleteProvider",
    params(("id" = String, Path)), responses((status = NO_CONTENT, description = "Provider, key, consent and routing references deleted.")))]
pub async fn delete_provider(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    providers::delete(&state, user.id(), &id).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
#[utoipa::path(post, path = "/api/v1/me/providers/{id}/test", tag = "account", operation_id = "testProvider",
    params(("id" = String, Path)), responses((status = OK, description = "Synthetic connectivity checks; no consent or library content needed.", body = ProviderTest)))]
pub async fn test_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    Ok(no_store(
        Json(providers::test(&state, user.id(), &id).await?).into_response(),
    ))
}
#[utoipa::path(post, path = "/api/v1/me/providers/{id}/consent", tag = "account", operation_id = "consentProvider",
    params(("id" = String, Path)), request_body = ConsentRequest,
    responses((status = NO_CONTENT, description = "Current provider consent recorded and audited.")))]
pub async fn consent_provider(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
    body: Result<Json<ConsentRequest>, ApiError>,
) -> Result<Response, ApiError> {
    let Json(input) = body.map_err(|error| ApiError::new(error.code()))?;
    providers::consent(&state, user.id(), &id, &input.version).await?;
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
