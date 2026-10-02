//! Request extractors whose rejections are [`ApiError`]s, so a malformed body,
//! query or path answers with a problem like every other error.
//!
//! Handlers use these instead of `axum::Json`, `axum::extract::Query` and
//! `axum::extract::Path`. Mapping, by the status axum gives each rejection:
//!
//! | Rejection | Code |
//! |---|---|
//! | body is not JSON, bad query string or path | 400 `bad_request` |
//! | body over the route's limit | 413 `payload_too_large` |
//! | `Content-Type` is not JSON | 415 `unsupported_media_type` |
//! | valid JSON of the wrong shape | 422 `validation_failed` |

use axum::extract::rejection::{JsonRejection, PathRejection, QueryRejection};
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;

/// A JSON request body (or response): `axum::Json` with problem rejections.
#[derive(Clone, Copy, Debug, Default, FromRequest)]
#[from_request(via(axum::Json), rejection(ApiError))]
pub struct Json<T>(pub T);

impl<T: Serialize> IntoResponse for Json<T> {
    fn into_response(self) -> Response {
        axum::Json(self.0).into_response()
    }
}

/// A query string: `axum::extract::Query` with problem rejections.
#[derive(Clone, Copy, Debug, Default, FromRequestParts)]
#[from_request(via(axum::extract::Query), rejection(ApiError))]
pub struct Query<T>(pub T);

/// Path parameters: `axum::extract::Path` with problem rejections.
#[derive(Clone, Copy, Debug, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
        from_rejection(rejection.status(), rejection.body_text())
    }
}

impl From<QueryRejection> for ApiError {
    fn from(rejection: QueryRejection) -> Self {
        // axum answers 400 for a query string of the wrong shape, too.
        from_rejection(rejection.status(), rejection.body_text())
    }
}

impl From<PathRejection> for ApiError {
    fn from(rejection: PathRejection) -> Self {
        from_rejection(rejection.status(), rejection.body_text())
    }
}

fn from_rejection(status: StatusCode, detail: String) -> ApiError {
    // `problem()` drops the detail of 5xx (axum reports its own misuse there).
    ApiError::from_status(status).with_detail(detail)
}
