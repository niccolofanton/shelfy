//! `GET /api/v1/version`: the server build and API version (plan §2.9
//! Platform; App. A `app:getVersion`). The web app pairs it with its own
//! build constant; the `hello` event of the stream carries the same version.

use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::API_VERSION;
use crate::current_user::CurrentUser;
use crate::extract::Json;

/// Versions of the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct VersionInfo {
    /// Version of the server build.
    pub version: String,
    /// Major version of the API: the `1` of `/api/v1`.
    pub api_version: String,
}

/// The server's versions.
#[utoipa::path(
    get,
    path = "/api/v1/version",
    tag = "platform",
    operation_id = "getVersion",
    responses(
        (
            status = OK,
            description = "The versions.",
            body = VersionInfo,
            headers(("Cache-Control" = String, description = "`no-store`.")),
        ),
    )
)]
pub async fn get_version(_user: CurrentUser) -> Response {
    let info = VersionInfo {
        version: crate::VERSION.to_owned(),
        api_version: API_VERSION.to_owned(),
    };
    let mut response = Json(info).into_response();
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
