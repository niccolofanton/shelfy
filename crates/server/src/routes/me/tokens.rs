//! `GET,POST,DELETE /api/v1/me/tokens` (plan §2.9 Account, §2.11 device
//! tokens, §7.1): the account's API tokens. Minting is in
//! [`crate::auth::api_tokens`], the bearer check in [`crate::auth::bearer`].
//!
//! The list shows the tokens that still work, with their last use. Creating
//! one needs a sign-in or a re-authentication from the last 5 minutes, and
//! the token's value appears in that answer only. A `migrate` token cannot be
//! created here: the migration CLI gets its own through the device flow.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use shelfy_core::repo::RepoError;
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use crate::auth::RecentAuth;
use crate::auth::api_tokens::{self, MAX_ACTIVE_TOKENS, Mint, Minted, Via, allowed_scopes};
use crate::auth::bearer::Scope;
use crate::auth::passkeys::normalize_label;
use crate::control::api_tokens::TokenKind as StoredKind;
use crate::control::api_tokens::{self as rows, TokenRow};
use crate::control::audit::{self, Entry};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::extract::{Json, Path};
use crate::ids::now_ms;
use crate::routes::auth::no_store;
use crate::state::{AppState, blocking};

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(list_tokens, create_token))
        .routes(routes!(revoke_token))
}

/// Who holds a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum TokenKind {
    /// The browser extension: scopes `ingest`, `tasks`, `uploads`,
    /// `lookup`.
    Extension,
    /// The iOS Shortcut: scope `links:create`.
    Shortcut,
    /// The migration CLI: scope `migrate`, 7 days; from the device flow
    /// only.
    Migrate,
}

impl From<StoredKind> for TokenKind {
    fn from(kind: StoredKind) -> Self {
        match kind {
            StoredKind::Extension => Self::Extension,
            StoredKind::Shortcut => Self::Shortcut,
            StoredKind::Migrate => Self::Migrate,
        }
    }
}

impl From<TokenKind> for StoredKind {
    fn from(kind: TokenKind) -> Self {
        match kind {
            TokenKind::Extension => Self::Extension,
            TokenKind::Shortcut => Self::Shortcut,
            TokenKind::Migrate => Self::Migrate,
        }
    }
}

/// An API token of the account. Its value is never shown again.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ApiToken {
    /// Its id (ULID), for `DELETE /me/tokens/{id}`.
    pub id: String,
    /// Who holds it.
    pub kind: TokenKind,
    /// The user's name for it.
    #[schema(required = true)]
    pub label: Option<String>,
    /// What it may do.
    pub scopes: Vec<Scope>,
    /// Creation time, unix ms.
    pub created_at: i64,
    /// Last request it authenticated, unix ms (recorded at most once a
    /// minute).
    #[schema(required = true)]
    pub last_used_at: Option<i64>,
    /// When it stops working, unix ms; `null` until revoked.
    #[schema(required = true)]
    pub expires_at: Option<i64>,
}

impl From<TokenRow> for ApiToken {
    fn from(row: TokenRow) -> Self {
        Self {
            scopes: Scope::parse_list(&row.scopes),
            id: row.id,
            kind: row.kind.into(),
            label: row.label,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
            expires_at: row.expires_at,
        }
    }
}

/// The account's working API tokens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ApiTokenList {
    /// Newest first.
    pub items: Vec<ApiToken>,
}

/// Body of `POST /api/v1/me/tokens`.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ApiTokenRequest {
    /// Who will hold it: `extension` or `shortcut`.
    pub kind: TokenKind,
    /// A name for the list, up to 64 characters; trimmed, and blank is none.
    #[serde(default)]
    #[schema(nullable = false)]
    pub label: Option<String>,
    /// What it may do: some of the kind's scopes; all of them when left out.
    #[serde(default)]
    #[schema(nullable = false)]
    pub scopes: Option<Vec<Scope>>,
}

/// A new API token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct CreatedApiToken {
    /// The value (`shx_…`), for `Authorization: Bearer`. Shown only now:
    /// the server keeps its hash.
    pub token: String,
    /// The token as the list shows it.
    pub api_token: ApiToken,
}

/// The account's working API tokens (not revoked, not expired), with their
/// last use.
#[utoipa::path(
    get,
    path = "/api/v1/me/tokens",
    tag = "account",
    operation_id = "listApiTokens",
    responses(
        (status = OK, description = "The tokens.", body = ApiTokenList),
    )
)]
pub async fn list_tokens(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let now = now_ms();
    let found =
        blocking(move || control.read(|conn| rows::list_active(conn, &user_id, now))).await?;
    let list = ApiTokenList {
        items: found.into_iter().map(ApiToken::from).collect(),
    };
    Ok(no_store(Json(list).into_response()))
}

/// Creates an API token for the extension or the iOS Shortcut; the answer
/// is the only time its value is shown.
///
/// Needs a sign-in or a re-authentication from the last 5 minutes (403
/// `reauth_required` otherwise). 422 for the kind `migrate` (the migration
/// CLI signs in with the device flow), for scopes that are not the kind's,
/// or for a label over 64 characters; 409 `conflict` when the account holds
/// 50 working tokens already.
#[utoipa::path(
    post,
    path = "/api/v1/me/tokens",
    tag = "account",
    operation_id = "createApiToken",
    request_body = ApiTokenRequest,
    responses(
        (status = CREATED, description = "The new token, with its value.", body = CreatedApiToken),
    )
)]
pub async fn create_token(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
    Json(request): Json<ApiTokenRequest>,
) -> Result<Response, ApiError> {
    let kind = StoredKind::from(request.kind);
    if kind == StoredKind::Migrate {
        return Err(ApiError::invalid_field(
            "kind",
            "a migrate token comes from the device flow (shelfy-migrate login)",
        ));
    }
    let label = normalize_label(request.label.as_deref())?;
    let scopes = match request.scopes {
        Some(scopes) => scopes,
        None => allowed_scopes(kind).to_vec(),
    };
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let now = now_ms();
    let minted: Minted = blocking(move || {
        control.write(|tx| {
            if rows::count_active(tx, &user_id, now)? >= MAX_ACTIVE_TOKENS {
                return Err(RepoError::Conflict("too many tokens"));
            }
            let mint = Mint {
                user_id: &user_id,
                kind,
                scopes: &scopes,
                label: label.as_deref(),
                ttl: None,
                via: Via::Account,
                actor: Some(&user_id),
            };
            api_tokens::mint(tx, &mint, now)
        })
    })
    .await?;
    let created = CreatedApiToken {
        token: minted.token.into_inner(),
        api_token: minted.row.into(),
    };
    Ok(no_store(
        (StatusCode::CREATED, Json(created)).into_response(),
    ))
}

/// Revokes one of the account's tokens: it stops working at once. Another
/// account's token, or one that no longer works, is a 404.
#[utoipa::path(
    delete,
    path = "/api/v1/me/tokens/{id}",
    tag = "account",
    operation_id = "revokeApiToken",
    params(("id" = String, Path, description = "The token's id.")),
    responses(
        (status = NO_CONTENT, description = "Revoked."),
    )
)]
pub async fn revoke_token(
    State(state): State<AppState>,
    user: CurrentUser,
    Path(id): Path<String>,
) -> Result<Response, ApiError> {
    let control = Arc::clone(state.control());
    let user_id = user.id().to_owned();
    let now = now_ms();
    let revoked = blocking(move || {
        control.write(|tx| {
            let Some(row) = rows::revoke(tx, &user_id, &id, now)? else {
                return Ok(false);
            };
            let meta = serde_json::json!({ "id": row.id, "kind": row.kind.as_str() });
            let entry = Entry {
                action: audit::API_TOKEN_REVOKE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(true)
        })
    })
    .await?;
    if !revoked {
        return Err(ApiError::new(ErrorCode::NotFound));
    }
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}
