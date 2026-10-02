//! `Idempotency-Key` (plan §2.9): a job-creating request sent twice with the
//! same key acts once; the repeat gets the first response back.
//!
//! The middleware ([`layer`]) runs after the access gate, on the routes of
//! [`crate::routes::IDEMPOTENT_ROUTES`] only. For a request with the header:
//!
//! 1. the key must be 1–255 visible ASCII characters (else 400
//!    `bad_request`). Keys belong to the signed-in user;
//! 2. the request is fingerprinted: method, path and query, and body;
//! 3. the key is reserved in the control database before the handler runs.
//!    If it is already taken within the last 24 hours:
//!    - by a request with another fingerprint: 422 `validation_failed` on
//!      `Idempotency-Key`;
//!    - by a request that is still running: 409 `conflict`;
//!    - by a finished request: its response is sent again, with
//!      `Idempotent-Replayed: true`, and the handler does not run;
//! 4. the handler's response is stored with the key, except a server error,
//!    a 401, 403, 408 or 429, or a body over 64 KiB, which release the key
//!    so the request can be sent again.
//!
//! A reservation whose request died (a crash) is taken over after a minute.
//! Rows are pruned after 24 hours by the nightly schedule.

use std::sync::Arc;
use std::time::Duration;

use axum::body::{Body, Bytes};
use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderName, HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse as _, Response};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use utoipa::IntoParams;

use super::IDEMPOTENCY_TTL;
use crate::control::idempotency::{self, PENDING, Reservation};
use crate::current_user::CurrentUser;
use crate::error::{ApiError, ErrorCode};
use crate::state::{AppState, blocking};

/// The request header.
pub const IDEMPOTENCY_KEY: HeaderName = HeaderName::from_static("idempotency-key");

/// The response header of a replayed response.
pub const REPLAYED: HeaderName = HeaderName::from_static("idempotent-replayed");

/// Longest key, in bytes.
pub const MAX_KEY_BYTES: usize = 255;

/// A reservation older than this belongs to a request that died: handlers
/// have 30 s ([`crate::limits::REQUEST_TIMEOUT`]).
const PENDING_STALE: Duration = Duration::from_secs(60);

/// Largest response stored for a replay.
const MAX_STORED_BYTES: usize = 64 * 1024;

/// A route that takes `Idempotency-Key`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IdempotentRoute {
    /// The method.
    pub method: Method,
    /// The route template, as in `MatchedPath` (`/api/v1/jobs/{id}/retry`).
    pub path: &'static str,
    /// The route's body limit: the middleware reads the body before the
    /// route's own limit does.
    pub body_bytes: usize,
}

/// The request header, for the OpenAPI document: `params(IdempotencyHeader)`
/// on every route of [`crate::routes::IDEMPOTENT_ROUTES`].
#[derive(Clone, Debug, Default, IntoParams)]
#[into_params(parameter_in = Header)]
pub struct IdempotencyHeader {
    /// A key you choose for this request, 1–255 visible ASCII characters
    /// (a UUID works). Sending the same request again with the same key
    /// within 24 hours returns the first response, marked
    /// `Idempotent-Replayed: true`, instead of acting twice. Reusing a key
    /// for another request answers 422 `validation_failed`; while the first
    /// request is still running, 409 `conflict`.
    #[param(rename = "Idempotency-Key", nullable = false)]
    pub idempotency_key: Option<String>,
}

/// The state of [`layer`].
#[derive(Clone)]
pub struct Idempotency {
    state: AppState,
    routes: Arc<[IdempotentRoute]>,
}

impl Idempotency {
    /// The middleware's state for `routes`.
    #[must_use]
    pub fn new(state: AppState, routes: &[IdempotentRoute]) -> Self {
        Self {
            state,
            routes: routes.into(),
        }
    }

    fn route_of(&self, request: &Request) -> Option<&IdempotentRoute> {
        let path = request.extensions().get::<MatchedPath>()?.as_str();
        self.routes
            .iter()
            .find(|route| route.method == request.method() && route.path == path)
    }
}

/// What the `body` column holds: the fingerprint of the request, and, once
/// it finished, its response.
#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Envelope {
    fingerprint: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_type: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    body: String,
}

impl Envelope {
    fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("an envelope serializes")
    }

    fn decode(bytes: &[u8]) -> Self {
        serde_json::from_slice(bytes).unwrap_or_default()
    }
}

/// The key, when well formed.
fn valid_key(value: &HeaderValue) -> Result<&str, ApiError> {
    let key = value.to_str().unwrap_or_default();
    if (1..=MAX_KEY_BYTES).contains(&key.len()) && key.bytes().all(|b| b.is_ascii_graphic()) {
        Ok(key)
    } else {
        Err(ApiError::new(ErrorCode::BadRequest)
            .with_detail("Idempotency-Key must be 1 to 255 visible ASCII characters"))
    }
}

/// SHA-256 of the method, the path and query, and the body, in hex.
fn fingerprint(method: &Method, target: &str, body: &[u8]) -> String {
    let mut hash = Sha256::new();
    for part in [method.as_str().as_bytes(), target.as_bytes(), body] {
        // Length-prefixed, so no two requests concatenate to the same bytes.
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part);
    }
    hash.finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Whether a response of this status is kept for replays: not when the
/// outcome depends on the moment (auth, rate limits, timeouts, failures)
/// rather than on the request.
fn storable(status: StatusCode) -> bool {
    !(status.is_server_error()
        || matches!(
            status,
            StatusCode::UNAUTHORIZED
                | StatusCode::FORBIDDEN
                | StatusCode::REQUEST_TIMEOUT
                | StatusCode::TOO_MANY_REQUESTS
        ))
}

/// Middleware: see the module docs.
pub async fn layer(
    State(idempotency): State<Idempotency>,
    request: Request,
    next: Next,
) -> Response {
    let Some(route) = idempotency.route_of(&request).cloned() else {
        return next.run(request).await;
    };
    let Some(value) = request.headers().get(&IDEMPOTENCY_KEY) else {
        return next.run(request).await;
    };
    let key = match valid_key(value) {
        Ok(key) => key.to_owned(),
        Err(err) => return err.into_response(),
    };
    // Without a user the gate refused already; leave it to the handler.
    let Some(user) = request.extensions().get::<CurrentUser>().cloned() else {
        return next.run(request).await;
    };
    let too_large = request
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<usize>().ok())
        .is_some_and(|length| length > route.body_bytes);
    if too_large {
        return ApiError::new(ErrorCode::PayloadTooLarge).into_response();
    }
    let (parts, body) = request.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, route.body_bytes).await else {
        return ApiError::new(ErrorCode::PayloadTooLarge).into_response();
    };
    let target = parts.uri.path_and_query().map_or("", |p| p.as_str());
    let fingerprint = fingerprint(&parts.method, target, &bytes);

    let state = &idempotency.state;
    let reserved = reserve(state, user.id(), &key, &fingerprint).await;
    match reserved {
        Err(err) => return err.into_response(),
        Ok(Reservation::Reserved) => {}
        Ok(Reservation::Taken(stored)) => {
            let envelope = Envelope::decode(&stored.body);
            if envelope.fingerprint != fingerprint {
                return ApiError::invalid_field("Idempotency-Key", "was used for another request")
                    .into_response();
            }
            if stored.status == PENDING {
                return ApiError::new(ErrorCode::Conflict)
                    .with_detail("a request with this Idempotency-Key is still running")
                    .into_response();
            }
            return replay(stored.status, &envelope);
        }
    }

    let response = next
        .run(Request::from_parts(parts, Body::from(bytes)))
        .await;
    if !storable(response.status()) {
        release(state, user.id(), &key).await;
        return response;
    }
    let (parts, body) = response.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, usize::MAX).await else {
        release(state, user.id(), &key).await;
        return ApiError::new(ErrorCode::Internal).into_response();
    };
    if bytes.len() > MAX_STORED_BYTES {
        tracing::warn!(
            bytes = bytes.len(),
            "a response too large to replay; its key is released"
        );
        release(state, user.id(), &key).await;
    } else {
        let envelope = Envelope {
            fingerprint,
            content_type: parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned),
            body: STANDARD.encode(&bytes),
        };
        complete(
            state,
            user.id(),
            &key,
            parts.status.as_u16(),
            envelope.encode(),
        )
        .await;
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn replay(status: u16, envelope: &Envelope) -> Response {
    let body = STANDARD.decode(&envelope.body).unwrap_or_default();
    let mut response = Response::new(Body::from(Bytes::from(body)));
    *response.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::OK);
    let headers = response.headers_mut();
    if let Some(value) = envelope
        .content_type
        .as_deref()
        .and_then(|v| HeaderValue::from_str(v).ok())
    {
        headers.insert(header::CONTENT_TYPE, value);
    }
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(REPLAYED, HeaderValue::from_static("true"));
    response
}

fn ttl_ms(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

async fn reserve(
    state: &AppState,
    user_id: &str,
    key: &str,
    fingerprint: &str,
) -> Result<Reservation, ApiError> {
    let control = Arc::clone(state.control());
    let now = state.jobs().clock().now_ms();
    let (user, key) = (user_id.to_owned(), key.to_owned());
    let pending = Envelope {
        fingerprint: fingerprint.to_owned(),
        ..Envelope::default()
    }
    .encode();
    blocking(move || {
        control.write(|tx| {
            idempotency::reserve(
                tx,
                &user,
                &key,
                &pending,
                now,
                now.saturating_sub(ttl_ms(IDEMPOTENCY_TTL)),
                now.saturating_sub(ttl_ms(PENDING_STALE)),
            )
        })
    })
    .await
}

async fn complete(state: &AppState, user_id: &str, key: &str, status: u16, body: Vec<u8>) {
    let control = Arc::clone(state.control());
    let (user, key) = (user_id.to_owned(), key.to_owned());
    let stored =
        blocking(move || control.write(|tx| idempotency::complete(tx, &user, &key, status, &body)))
            .await;
    if let Err(err) = stored {
        // The response still goes out; a repeat runs the request again.
        tracing::warn!(error = %err, "storing a response for its Idempotency-Key failed");
    }
}

async fn release(state: &AppState, user_id: &str, key: &str) {
    let control = Arc::clone(state.control());
    let (user, key) = (user_id.to_owned(), key.to_owned());
    if let Err(err) =
        blocking(move || control.write(|tx| idempotency::release(tx, &user, &key))).await
    {
        tracing::warn!(error = %err, "releasing an Idempotency-Key failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_visible_ascii_up_to_255_bytes() {
        for good in [
            "k",
            "8e03978e-40d5-43e8-bc93-6894a57f9324",
            "\"quoted\"",
            &"a".repeat(255),
        ] {
            assert!(
                valid_key(&HeaderValue::from_str(good).unwrap()).is_ok(),
                "{good}"
            );
        }
        for bad in ["", "with space", &"a".repeat(256)] {
            let err = valid_key(&HeaderValue::from_str(bad).unwrap()).unwrap_err();
            assert_eq!(err.code(), ErrorCode::BadRequest, "{bad:?}");
        }
        let latin = HeaderValue::from_bytes(b"caf\xe9").unwrap();
        assert!(valid_key(&latin).is_err());
    }

    #[test]
    fn fingerprints_cover_method_target_and_body() {
        let base = fingerprint(&Method::POST, "/a?x=1", b"{}");
        assert_eq!(base.len(), 64);
        assert_eq!(base, fingerprint(&Method::POST, "/a?x=1", b"{}"));
        for other in [
            fingerprint(&Method::PUT, "/a?x=1", b"{}"),
            fingerprint(&Method::POST, "/a?x=2", b"{}"),
            fingerprint(&Method::POST, "/a?x=1", b"{ }"),
            fingerprint(&Method::POST, "/a?x=1{", b"}"),
        ] {
            assert_ne!(base, other);
        }
    }

    #[test]
    fn envelopes_round_trip_and_tolerate_garbage() {
        let envelope = Envelope {
            fingerprint: "ab".into(),
            content_type: Some("application/json".into()),
            body: STANDARD.encode(b"{\"id\":1}"),
        };
        let back = Envelope::decode(&envelope.encode());
        assert_eq!(back.fingerprint, "ab");
        assert_eq!(back.content_type.as_deref(), Some("application/json"));
        let replayed = replay(200, &back);
        assert_eq!(replayed.headers()[&REPLAYED], "true");
        assert_eq!(replayed.headers()[header::CONTENT_TYPE], "application/json");
        assert_eq!(Envelope::decode(b"not json").fingerprint, "");
    }

    #[test]
    fn only_outcomes_of_the_request_itself_are_stored() {
        for status in [200, 201, 400, 404, 409, 422] {
            assert!(storable(StatusCode::from_u16(status).unwrap()), "{status}");
        }
        for status in [401, 403, 408, 429, 500, 503, 504] {
            assert!(!storable(StatusCode::from_u16(status).unwrap()), "{status}");
        }
    }
}
