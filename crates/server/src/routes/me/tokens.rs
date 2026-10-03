//! `GET,POST,DELETE /api/v1/me/tokens` (plan §2.9 Account, §2.11 device
//! tokens, §7.1): the account's API tokens. Minting is in
//! [`crate::auth::api_tokens`], the bearer check in [`crate::auth::bearer`].
//!
//! The list shows the tokens that still work, with their last use. Creating
//! one needs a sign-in or a re-authentication from the last 5 minutes, and
//! the token's value appears in that answer only. A `migrate` token cannot be
//! created here: the migration CLI gets its own through the device flow.
//!
//! `POST /me/tokens/pairing-code` (P2-03, contract C2) gives the web app a
//! 60-second code to hand to the browser extension, which exchanges it for
//! its own token at `POST /extension/pair` ([`crate::extension::pairing`]).
//! It needs the same recent sign-in as creating a token.

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
use crate::extension::{self, pairing};
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
        .routes(routes!(create_pairing_code))
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

/// A code that pairs the browser extension (contract C2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairingCode {
    /// The code (43 characters), for the extension's `POST /extension/pair`.
    /// Hand it to the extension only; it works once.
    pub code: String,
    /// When it stops working, unix ms: 60 seconds from now.
    pub expires_at: i64,
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
                return Ok(None);
            };
            let meta = serde_json::json!({ "id": row.id, "kind": row.kind.as_str() });
            let entry = Entry {
                action: audit::API_TOKEN_REVOKE,
                actor_user_id: Some(&user_id),
                target: Some(&user_id),
                meta: Some(&meta),
            };
            audit::record(tx, &entry, now)?;
            Ok::<_, RepoError>(Some(row.id))
        })
    })
    .await?;
    let Some(token_id) = revoked else {
        return Err(ApiError::new(ErrorCode::NotFound));
    };
    // A revoked extension token no longer keeps the extension connected.
    extension::token_revoked(&state, user.id(), &token_id);
    Ok(no_store(StatusCode::NO_CONTENT.into_response()))
}

/// Creates a code that pairs the browser extension with this account.
///
/// The web app hands the code to the extension (`chrome.runtime.sendMessage`),
/// which exchanges it for its token at `POST /extension/pair` within 60
/// seconds, once. Needs a sign-in or a re-authentication from the last 5
/// minutes (403 `reauth_required` otherwise). 429 `rate_limited` while the
/// account holds 10 unused codes that have not expired.
#[utoipa::path(
    post,
    path = "/api/v1/me/tokens/pairing-code",
    tag = "account",
    operation_id = "createPairingCode",
    responses(
        (status = CREATED, description = "The code and its expiry.", body = PairingCode),
    )
)]
pub async fn create_pairing_code(
    State(state): State<AppState>,
    RecentAuth(user): RecentAuth,
) -> Result<Response, ApiError> {
    let created = pairing::create_code(&state, user.id()).await?;
    let code = PairingCode {
        code: created.code.expose().to_owned(),
        expires_at: created.expires_at,
    };
    Ok(no_store((StatusCode::CREATED, Json(code)).into_response()))
}
