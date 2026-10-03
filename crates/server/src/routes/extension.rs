//! `/api/v1/extension/*`: the browser extension's own routes (P2-03; plan
//! §2.9 Extension, §2.11 device tokens, §2.16; contracts C1–C3 and C8 in
//! `docs/web-port/phases/P2.md`). The machinery is in [`crate::extension`].
//!
//! | Route | Access | Answer |
//! |---|---|---|
//! | `POST /extension/pair` `{code, installId, label?, version}` | public: the code is the credential; reads no cookie, needs no CSRF headers; counted by the sign-in limit per client (10 a minute) | 201 with an `extension` token (`ingest`, `tasks`, `uploads`, `lookup`), shown once; 400 `invalid_pairing_code`; 422; 426 `extension_outdated`; 409 at 50 working tokens |
//! | `GET /extension/config` | an API token with `ingest` (the extension's); conditional | the configuration: minimum version, kill switches, pacing, stop thresholds; 304 when unchanged. An outdated extension may still read it |
//! | `GET /extension/status` | session | whether the account's extension is connected, as the `extension.status` event says |
//!
//! The web app asks for the pairing code with `POST /me/tokens/pairing-code`
//! ([`super::me::tokens`]). Every other route an extension token reaches
//! answers 426 `extension_outdated` while its `X-Shelfy-Extension` is missing
//! or below `minVersion` ([`crate::extension::admit`]).

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse as _, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;

use super::auth::no_store;
use crate::auth::bearer::{Scope, TokenUser, scopes};
use crate::conditional::ConditionalHeaders;
use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::events::model::ExtensionStatusEvent;
use crate::extension::pairing::{self, PairRequest};
use crate::extension::{ExtensionConfig, ExtensionHeaders};
use crate::extract::Json;
use crate::state::AppState;

/// The routes of this module.
#[must_use]
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(pair_extension))
        .routes(routes!(get_extension_config))
        .routes(routes!(get_extension_status))
}

/// Body of `POST /api/v1/extension/pair` (contract C2). Fields a later
/// extension adds are ignored.
#[derive(Clone, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionPairRequest {
    /// The pairing code the web app handed over (43 characters).
    pub code: String,
    /// A random id the extension keeps while it is installed (16 to 128
    /// letters, digits, `-` or `_`). Pairing the same installation again
    /// revokes the token it held.
    pub install_id: String,
    /// A name for the account's token list, such as "Chrome on macOS"; up
    /// to 64 characters, blank is none.
    #[serde(default)]
    #[schema(nullable = false)]
    pub label: Option<String>,
    /// The extension's manifest version (`0.2.0`); below `minVersion`, 426.
    pub version: String,
}

/// A paired extension's token.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct PairedExtension {
    /// The `extension` token (`shx_…`) for `Authorization: Bearer`. Shown
    /// only now; keep it where page scripts cannot read it.
    pub token: String,
    /// Its id, as the account's token list names it.
    pub token_id: String,
    /// What it may do: `ingest`, `tasks`, `uploads`, `lookup`.
    pub scopes: Vec<Scope>,
}

/// Exchanges a pairing code for the extension's token.
///
/// The extension calls it with the code that the web app got from `POST
/// /me/tokens/pairing-code`, within 60 seconds, once. No cookie and no CSRF
/// headers are needed, and none is read. 400 `invalid_pairing_code` for a
/// code that is malformed, unknown, used or expired; 422 for a malformed
/// `installId`, `label` or `version`; 426 `extension_outdated` for a version
/// below `minVersion` (the code stays usable); 409 `conflict` when the
/// account holds 50 working tokens. Limit: the sign-in limit, 10 requests a
/// minute per client over this route and the `/auth` routes (429).
#[utoipa::path(
    post,
    path = "/api/v1/extension/pair",
    tag = "extension",
    operation_id = "pairExtension",
    security(()),
    request_body = ExtensionPairRequest,
    responses(
        (status = CREATED, description = "Paired: the token, shown once.", body = PairedExtension),
    )
)]
pub async fn pair_extension(
    State(state): State<AppState>,
    Json(request): Json<ExtensionPairRequest>,
) -> Result<Response, ApiError> {
    let pair = PairRequest {
        code: &request.code,
        install_id: &request.install_id,
        label: request.label.as_deref(),
        version: &request.version,
    };
    let paired = pairing::pair(&state, &pair).await?;
    let body = PairedExtension {
        token: paired.token.into_inner(),
        token_id: paired.token_id,
        scopes: paired.scopes,
    };
    Ok(no_store((StatusCode::CREATED, Json(body)).into_response()))
}

/// The extension's configuration: the minimum version, the kill switches of
/// each platform's capture modes, pacing and stop thresholds (contract C3).
///
/// Conditional: send the `ETag` back in `If-None-Match` and an unchanged
/// configuration answers 304. A change made by the operator (`admin flags`)
/// shows within 30 seconds. This is the one route an outdated extension
/// still reaches: every other answers it 426 `extension_outdated`.
#[utoipa::path(
    get,
    path = "/api/v1/extension/config",
    tag = "extension",
    operation_id = "getExtensionConfig",
    security(("bearer" = ["ingest"])),
    params(ConditionalHeaders, ExtensionHeaders),
    responses(
        (
            status = OK,
            description = "The configuration.",
            body = ExtensionConfig,
            headers(
                ("ETag" = String, description = "Weak ETag of this configuration."),
                ("Cache-Control" = String, description = "`private, no-cache`."),
            )
        ),
        (
            status = NOT_MODIFIED,
            description = "Unchanged since the ETag in `If-None-Match`; no body.",
            headers(("ETag" = String, description = "The same ETag."))
        ),
    )
)]
pub async fn get_extension_config(
    State(state): State<AppState>,
    _token: TokenUser<scopes::Ingest>,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let snapshot = state.extension().flags().get(state.control()).await?;
    let etag = snapshot.etag();
    if etag.matches(&headers) {
        return Ok(etag.not_modified());
    }
    Ok(etag.respond(Json(snapshot.config().clone())))
}

/// Whether the account's browser extension is connected: one of its
/// extension tokens made a request in the last 10 minutes, with the version
/// it sent. The same object as the `extension.status` event, which follows
/// every change. The server keeps it in memory: after a restart it is
/// disconnected until the extension's next request.
#[utoipa::path(
    get,
    path = "/api/v1/extension/status",
    tag = "extension",
    operation_id = "getExtensionStatus",
    responses(
        (status = OK, description = "The extension's status.", body = ExtensionStatusEvent),
    )
)]
pub async fn get_extension_status(
    State(state): State<AppState>,
    user: CurrentUser,
) -> Result<Response, ApiError> {
    let extension = state.extension();
    let status: ExtensionStatusEvent = extension.presence().status(user.id(), extension.now_ms());
    Ok(no_store(Json(status).into_response()))
}
