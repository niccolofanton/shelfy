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
//!
//! [`Query`] parses with `serde_html_form` instead of axum's
//! `serde_urlencoded`, so repeatable parameters work (plan §2.9:
//! `?mediaType=image&mediaType=video` fills a `Vec` field) and an empty value
//! (`?limit=`) reads as `None` for an `Option` field. A repeated key for a
//! single-valued field is a 400.

use axum::extract::rejection::{JsonRejection, PathRejection};
use axum::extract::{FromRequest, FromRequestParts};
use axum::http::StatusCode;
use axum::http::request::Parts;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use serde::de::DeserializeOwned;

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

/// A query string, with problem rejections. Repeated keys fill `Vec` fields.
#[derive(Clone, Copy, Debug, Default)]
pub struct Query<T>(pub T);

impl<T, S> FromRequestParts<S> for Query<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _state: &S) -> Result<Self, Self::Rejection> {
        let query = parts.uri.query().unwrap_or_default();
        serde_html_form::from_str(query).map(Self).map_err(|err| {
            from_rejection(
                StatusCode::BAD_REQUEST,
                format!("Failed to deserialize query string: {err}"),
            )
        })
    }
}

/// Path parameters: `axum::extract::Path` with problem rejections.
#[derive(Clone, Copy, Debug, FromRequestParts)]
#[from_request(via(axum::extract::Path), rejection(ApiError))]
pub struct Path<T>(pub T);

impl From<JsonRejection> for ApiError {
    fn from(rejection: JsonRejection) -> Self {
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

#[cfg(test)]
mod tests {
    use axum::http::Request;
    use serde::Deserialize;

    use super::*;
    use crate::error::ErrorCode;

    #[derive(Debug, Default, Deserialize, PartialEq)]
    #[serde(default, rename_all = "camelCase")]
    struct Params {
        limit: Option<u32>,
        media_type: Vec<String>,
    }

    async fn parse(uri: &str) -> Result<Params, ApiError> {
        let (mut parts, ()) = Request::get(uri).body(()).unwrap().into_parts();
        Query::<Params>::from_request_parts(&mut parts, &())
            .await
            .map(|Query(p)| p)
    }

    #[tokio::test]
    async fn repeated_keys_fill_lists_and_empty_values_read_as_none() {
        let p = parse("/x?mediaType=image&limit=5&mediaType=video")
            .await
            .unwrap();
        assert_eq!(p.limit, Some(5));
        assert_eq!(p.media_type, ["image", "video"]);
        let p = parse("/x?mediaType=image&limit=").await.unwrap();
        assert_eq!(p.limit, None);
        assert_eq!(p.media_type, ["image"]);
        assert_eq!(parse("/x").await.unwrap(), Params::default());
    }

    #[tokio::test]
    async fn malformed_query_strings_are_bad_requests() {
        for uri in ["/x?limit=lots", "/x?limit=1&limit=2", "/x?limit=-1"] {
            let err = parse(uri).await.unwrap_err();
            assert_eq!(err.code(), ErrorCode::BadRequest, "{uri}");
            assert!(err.problem().detail.is_some(), "{uri}");
        }
    }
}
