//! `GET /api/v1/openapi.json`: the OpenAPI 3.1 document of the API.

use std::sync::LazyLock;

use axum::body::Bytes;
use axum::http::header;
use axum::response::IntoResponse;

/// The document, rendered once.
static DOCUMENT: LazyLock<Bytes> = LazyLock::new(|| Bytes::from(super::openapi_json()));

/// The OpenAPI document of this API.
#[utoipa::path(
    get,
    path = "/api/v1/openapi.json",
    tag = "platform",
    operation_id = "getOpenApi",
    responses(
        (
            status = OK,
            description = "The OpenAPI 3.1 description of this API.",
            content_type = "application/json",
            body = Object
        ),
    )
)]
pub async fn openapi_json() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/json")],
        DOCUMENT.clone(),
    )
}
