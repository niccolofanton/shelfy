//! The HTTP seam (L11).
//!
//! The adapters build requests and read answers; a [`Transport`] moves the
//! bytes. The server implements it on its one outbound client (P2-04's
//! `crates/server/src/outbound/`), which owns the egress proxy, the resolver
//! checks and the metrics; this crate builds no HTTP client of its own. Tests,
//! the stub and the SPIKE-6 harness use `crate::direct::DirectTransport`
//! (feature `direct`), which reaches only loopback and allowlisted endpoints.
//!
//! A transport must:
//!
//! - route by [`HttpRequest::egress`]: [`Egress::Allowlisted`] and
//!   [`Egress::Loopback`] directly, [`Egress::Public`] through
//!   `SHELFY_EGRESS_PROXY` when set, else only to addresses that pass
//!   [`crate::guard::check_answers`];
//! - never follow a redirect: the adapters treat a 3xx answer as an error;
//! - apply [`HttpRequest::connect_timeout`] to connecting, and report every
//!   failure before a connection exists as [`TransportError::Connect`] (the
//!   adapters map it to [`crate::ErrorKind::Offline`]);
//! - return as soon as the status and headers arrive, with the body as a
//!   stream: the adapters read streamed answers as they come and apply the
//!   call's own deadlines.

use std::fmt;
use std::time::Duration;

use bytes::Bytes;
use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;
use http::{HeaderMap, Method, StatusCode};
use url::Url;

use crate::guard::{Egress, GuardError};

/// One request. Its authorization headers are marked sensitive, so the
/// `Debug` of the headers prints `Sensitive` instead of a key.
pub struct HttpRequest {
    /// `GET` or `POST`.
    pub method: Method,
    /// The full URL: the base URL as configured plus the call's path suffix.
    pub url: Url,
    /// Headers, the key among them.
    pub headers: HeaderMap,
    /// The body: JSON or multipart, never streamed.
    pub body: Bytes,
    /// How the guard classified the URL.
    pub egress: Egress,
    /// The longest a connection may take to open.
    pub connect_timeout: Duration,
}

impl fmt::Debug for HttpRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The body holds prompts, captions and images: print its size only.
        f.debug_struct("HttpRequest")
            .field("method", &self.method)
            .field("url", &self.url.as_str())
            .field("headers", &self.headers)
            .field("body_bytes", &self.body.len())
            .field("egress", &self.egress)
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

/// The body of an answer, chunk by chunk.
pub type BodyStream = BoxStream<'static, Result<Bytes, TransportError>>;

/// An answer, as soon as its status and headers arrived.
pub struct HttpResponse {
    /// The status.
    pub status: StatusCode,
    /// The headers.
    pub headers: HeaderMap,
    /// The body, still arriving.
    pub body: BodyStream,
}

impl fmt::Debug for HttpResponse {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpResponse")
            .field("status", &self.status)
            .field("headers", &self.headers)
            .finish_non_exhaustive()
    }
}

/// Why no connection could be opened.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectFailure {
    /// Nothing listens on the port.
    Refused,
    /// No route to the host or its network.
    Unreachable,
    /// The name did not resolve.
    Dns,
    /// The connect timeout passed.
    Timeout,
    /// Any other failure before a connection existed.
    Other(String),
}

impl fmt::Display for ConnectFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused => f.write_str("connection refused"),
            Self::Unreachable => f.write_str("no route to the host"),
            Self::Dns => f.write_str("the host name did not resolve"),
            Self::Timeout => f.write_str("connect timeout"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

/// A failed exchange.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum TransportError {
    /// The guard refused the destination (a non-public answer, a non-loopback
    /// address on the loopback route).
    #[error("the destination was refused: {0}")]
    Blocked(#[from] GuardError),
    /// No connection could be opened.
    #[error("could not connect: {0}")]
    Connect(ConnectFailure),
    /// The exchange failed after the connection opened: a reset, a broken
    /// pipe, a protocol error, a read timeout.
    #[error("the exchange failed: {0}")]
    Io(String),
    /// The transport cannot reach this kind of destination.
    #[error("the transport cannot reach this destination: {0}")]
    Unsupported(String),
}

/// Moves one request and its answer.
pub trait Transport: Send + Sync {
    /// Sends `request` and returns once the status and headers arrived.
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>>;
}
