//! The outbound HTTP clients, requests and responses (L11, §2.3). This file
//! is the only place in `crates/server` and `crates/media` that builds a
//! `reqwest` client: `tests/outbound.rs` fails the build otherwise.
//!
//! **Clients.** Up to three, with one configuration: rustls with `ring` and
//! the webpki roots, HTTP/2 by ALPN, no cookie store, no automatic
//! decompression (no `Accept-Encoding` is sent), no system proxy and no
//! automatic redirect.
//!
//! | Client | Reaches | Proxy | Resolver |
//! |---|---|---|---|
//! | public | the internet, for every [`Purpose`] | `SHELFY_EGRESS_PROXY` when set | without a proxy, [`PublicResolver`]: public addresses only; with one, the proxy's name only |
//! | operator | the exact origins of `SHELFY_EGRESS_ALLOW_ORIGINS` (L15), for [`Purpose::AiOperator`] only | never | those origins' names, any address |
//! | internal | the capture service's origin (`SHELFY_CAPTURE_URL`), for [`Purpose::Capture`] only | never | that name, any address |
//!
//! **Requests** ([`EgressRequest::send`]):
//! - every URL, the first and each redirect target, is checked before
//!   anything is sent ([`Refusal`]): http or https (https only when the
//!   purpose or the request says so), no credentials, ports 80 and 443, a
//!   host name with a dot that is not `localhost` and, when the request
//!   carries one, on its [`HostSet`]; a literal address must be public.
//!   Names are checked where they resolve: by [`PublicResolver`] in direct
//!   mode, by the proxy otherwise (D17). The operator and internal origins
//!   are checked by exact match instead;
//! - at most [`OutboundConfig::max_in_flight`] (64) requests in flight over
//!   every client: a response holds its slot until its body is read or
//!   dropped;
//! - one total timeout per request, every hop and the body included
//!   ([`Purpose::timeout`]);
//! - redirects by our own policy: 301, 302, 303, 307 and 308, at most
//!   [`MAX_REDIRECTS`] (5) hops, each target checked again and sent again
//!   through the proxy when there is one. A hop to another origin keeps only
//!   [`HEADERS_ACROSS_ORIGINS`]: no `Authorization` leaves its origin. With
//!   [`EgressRequest::max_redirects`] at 0 a redirect comes back as the
//!   response;
//! - a body is read with a cap only ([`EgressResponse::read_capped`],
//!   [`EgressResponse::stream_capped`], [`EgressResponse::reader_capped`]);
//! - `shelfy_egress_requests_total{purpose,outcome}` counts each request
//!   once, when its final headers arrive or it fails.
//!
//! Errors and logs never carry a URL: post URLs are user data (§3.7).

use std::error::Error as StdError;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime};

use axum::body::Bytes;
use axum::http::header::{
    CONTENT_ENCODING, CONTENT_LENGTH, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue, LOCATION,
    RETRY_AFTER,
};
use axum::http::{Method, StatusCode};
use futures_util::{Stream, StreamExt as _, TryStreamExt as _};
use rustls::pki_types::CertificateDer;
use serde::de::DeserializeOwned;
use tokio::io::AsyncRead;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use tokio_util::io::StreamReader;
use url::{Host, Url};

use super::resolve::{
    BlockedAddress, HostSet, Origin, OriginAllowlist, PinnedResolver, PublicResolver,
    is_localhost_name, is_public,
};
use super::{OutboundConfig, Purpose};
use crate::telemetry::metrics::{EGRESS_REQUESTS_TOTAL, egress_outcome};

/// Most redirects one request follows (L11, P4-01).
pub const MAX_REDIRECTS: u8 = 5;

/// How long connecting may take, within the request's timeout.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an idle pooled connection is kept.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);
/// Idle pooled connections kept per host.
const POOL_IDLE_PER_HOST: usize = 8;
/// The longest `Retry-After` taken into account.
const MAX_RETRY_AFTER: Duration = Duration::from_secs(24 * 3600);

/// The `User-Agent` of requests that set none.
pub const DEFAULT_USER_AGENT: &str = concat!("shelfy-server/", env!("CARGO_PKG_VERSION"));

/// The header Smokescreen adds to the answers it makes itself (a refused
/// destination). In proxy mode such an answer is a refusal, not the
/// destination's response.
pub const PROXY_ERROR_HEADER: HeaderName = HeaderName::from_static("x-smokescreen-error");

/// Headers a redirect to another origin keeps (lowercase names); every other
/// header is dropped, so credentials never follow a redirect.
pub const HEADERS_ACROSS_ORIGINS: &[&str] = &[
    "accept",
    "accept-language",
    "range",
    "referer",
    "user-agent",
    "sec-fetch-dest",
    "sec-fetch-mode",
    "sec-fetch-site",
];

type BoxError = Box<dyn StdError + Send + Sync>;

/// Why the egress policy refused a URL. Nothing was sent to it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum Refusal {
    /// Not http or https, or not https where https is required.
    #[error("the scheme is not allowed")]
    Scheme,
    /// The URL carries a user name or password.
    #[error("the URL carries credentials")]
    Credentials,
    /// A port other than 80 and 443.
    #[error("the port is not allowed")]
    Port,
    /// The host is not allowed: `localhost`, a single label, a trailing dot,
    /// or a name off the request's host allowlist.
    #[error("the host is not allowed")]
    Host,
    /// The address is not public: a literal, or every address of the name.
    #[error("the address is not public")]
    Address,
    /// The internal client reaches the capture service's origin only.
    #[error("not the internal origin")]
    Origin,
    /// The egress proxy refused the destination.
    #[error("the egress proxy refused the destination")]
    Proxy,
}

impl Refusal {
    /// A stable code for logs and tests.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::Scheme => "scheme",
            Self::Credentials => "credentials",
            Self::Port => "port",
            Self::Host => "host",
            Self::Address => "address",
            Self::Origin => "origin",
            Self::Proxy => "proxy",
        }
    }
}

/// Why an outbound request failed.
#[derive(Debug, thiserror::Error)]
pub enum EgressError {
    /// The URL, or a redirect's `Location`, is not an absolute URL.
    #[error("not a valid absolute URL")]
    InvalidUrl,
    /// The egress policy refused a URL; nothing was sent to it.
    #[error("refused by the egress policy: {0}")]
    Refused(Refusal),
    /// The redirects went past the request's limit.
    #[error("more than {0} redirects")]
    TooManyRedirects(u8),
    /// The request, a hop or the body took longer than the timeout.
    #[error("the request timed out")]
    Timeout,
    /// The body is larger than the cap the reader set.
    #[error("the response body is larger than {limit} bytes")]
    TooLarge {
        /// The cap.
        limit: u64,
    },
    /// No connection: DNS, TCP, TLS or the proxy's tunnel.
    #[error("the connection failed")]
    Connect(#[source] BoxError),
    /// The exchange failed after connecting.
    #[error("the exchange failed")]
    Network(#[source] BoxError),
    /// The body is not the JSON expected.
    #[error("the response body is not the expected JSON")]
    Decode(#[source] serde_json::Error),
}

impl EgressError {
    /// Maps a client error, without its URL.
    fn from_reqwest(err: reqwest::Error) -> Self {
        let err = err.without_url();
        if find_source::<BlockedAddress>(&err).is_some() {
            return Self::Refused(Refusal::Address);
        }
        if is_proxy_refusal(&err) {
            return Self::Refused(Refusal::Proxy);
        }
        if err.is_timeout() {
            return Self::Timeout;
        }
        if err.is_connect() {
            return Self::Connect(Box::new(err));
        }
        Self::Network(Box::new(err))
    }

    /// The `outcome` label of `shelfy_egress_requests_total`.
    fn outcome(&self) -> &'static str {
        match self {
            Self::InvalidUrl | Self::Refused(_) | Self::TooManyRedirects(_) => {
                egress_outcome::REFUSED
            }
            Self::Timeout => egress_outcome::TIMEOUT,
            Self::TooLarge { .. } | Self::Connect(_) | Self::Network(_) | Self::Decode(_) => {
                egress_outcome::FAILED
            }
        }
    }
}

/// The first error of type `T` in `err`'s chain, `err` included. An I/O
/// error's own source skips the error it wraps, so that one is looked at
/// too.
fn find_source<'a, T: StdError + 'static>(err: &'a (dyn StdError + 'static)) -> Option<&'a T> {
    let mut current = Some(err);
    while let Some(err) = current {
        if let Some(found) = err.downcast_ref::<T>() {
            return Some(found);
        }
        if let Some(inner) = err.downcast_ref::<io::Error>().and_then(io::Error::get_ref)
            && let Some(found) = find_source::<T>(inner)
        {
            return Some(found);
        }
        current = err.source();
    }
    None
}

/// Whether the proxy answered the tunnel request with something else than
/// 200 (hyper-util's `TunnelError` is private: its message is the signal).
fn is_proxy_refusal(err: &(dyn StdError + 'static)) -> bool {
    let mut current = Some(err);
    while let Some(err) = current {
        let text = err.to_string();
        if text == "tunnel error: unsuccessful"
            || text == "tunnel error: proxy authorization required"
        {
            return true;
        }
        current = err.source();
    }
    false
}

/// Which client a URL goes through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Route {
    Public,
    Operator,
    Internal,
}

/// The `reqwest` clients.
struct Clients {
    public: reqwest::Client,
    operator: Option<reqwest::Client>,
    internal: Option<reqwest::Client>,
}

impl Clients {
    /// Builds the clients of `config`: the only `reqwest` client
    /// construction of the server.
    fn build(config: &OutboundConfig) -> anyhow::Result<Self> {
        let tls = tls_config(&config.extra_roots)?;
        let lookup = &config.lookup;
        let public = match &config.proxy {
            Some(proxy) => builder(&tls)
                .proxy(reqwest::Proxy::all(proxy.as_str())?)
                .dns_resolver(PinnedResolver::new(
                    lookup.clone(),
                    proxy.host_str().filter(|_| proxy_host_is_name(proxy)),
                )),
            None => builder(&tls).no_proxy().dns_resolver(PublicResolver::new(
                lookup.clone(),
                config.dev_hosts.clone(),
            )),
        }
        .build()?;
        let pinned = |names: Vec<&str>| {
            builder(&tls)
                .no_proxy()
                .dns_resolver(PinnedResolver::new(lookup.clone(), names))
                .build()
        };
        let operator = (!config.allow_origins.is_empty())
            .then(|| {
                pinned(
                    config
                        .allow_origins
                        .iter()
                        .filter_map(Origin::domain)
                        .collect(),
                )
            })
            .transpose()?;
        let internal = config
            .capture
            .as_ref()
            .map(|origin| pinned(origin.domain().into_iter().collect()))
            .transpose()?;
        Ok(Self {
            public,
            operator,
            internal,
        })
    }

    fn get(&self, route: Route) -> Option<&reqwest::Client> {
        match route {
            Route::Public => Some(&self.public),
            Route::Operator => self.operator.as_ref(),
            Route::Internal => self.internal.as_ref(),
        }
    }
}

fn proxy_host_is_name(proxy: &Url) -> bool {
    matches!(proxy.host(), Some(Host::Domain(_)))
}

/// What every client shares.
fn builder(tls: &rustls::ClientConfig) -> reqwest::ClientBuilder {
    reqwest::Client::builder()
        .tls_backend_preconfigured(tls.clone())
        .redirect(reqwest::redirect::Policy::none())
        .referer(false)
        .no_gzip()
        .no_brotli()
        .no_deflate()
        .no_zstd()
        .no_hickory_dns()
        .connect_timeout(CONNECT_TIMEOUT)
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_IDLE_PER_HOST)
        .user_agent(DEFAULT_USER_AGENT)
}

/// TLS: `ring`, the webpki roots plus `extra_roots` (the dev CA), TLS 1.2
/// and 1.3, HTTP/2 or HTTP/1.1 by ALPN.
fn tls_config(extra_roots: &[CertificateDer<'static>]) -> anyhow::Result<rustls::ClientConfig> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    for root in extra_roots {
        roots.add(root.clone())?;
    }
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
    Ok(config)
}

/// The clients and the limits every [`Egress`] handle shares.
pub(crate) struct Shared {
    clients: Clients,
    in_flight: Arc<Semaphore>,
    allow_origins: OriginAllowlist,
    capture: Option<Origin>,
    proxied: bool,
}

impl Shared {
    pub(crate) fn new(config: &OutboundConfig) -> anyhow::Result<Self> {
        anyhow::ensure!(config.max_in_flight > 0, "max_in_flight must be positive");
        Ok(Self {
            clients: Clients::build(config)?,
            in_flight: Arc::new(Semaphore::new(config.max_in_flight)),
            allow_origins: config.allow_origins.clone(),
            capture: config.capture.clone(),
            proxied: config.proxy.is_some(),
        })
    }

    /// The route of `url` under `policy`, or why it is refused.
    fn route(&self, url: &Url, policy: &Policy<'_>) -> Result<Route, Refusal> {
        let origin = Origin::of(url).ok_or(Refusal::Scheme)?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(Refusal::Credentials);
        }
        if policy.purpose == Purpose::Capture {
            return match &self.capture {
                Some(capture) if *capture == origin => Ok(Route::Internal),
                _ => Err(Refusal::Origin),
            };
        }
        if policy.purpose.reaches_operator_origins() && self.allow_origins.contains(&origin) {
            return Ok(Route::Operator);
        }
        if policy.https_only && url.scheme() != "https" {
            return Err(Refusal::Scheme);
        }
        if !matches!(origin.port(), 80 | 443) {
            return Err(Refusal::Port);
        }
        match url.host() {
            Some(Host::Domain(name)) => {
                let named_ok = name.contains('.')
                    && !name.ends_with('.')
                    && !is_localhost_name(name)
                    && policy.hosts.is_none_or(|hosts| hosts.matches(name));
                if !named_ok {
                    return Err(Refusal::Host);
                }
            }
            Some(Host::Ipv4(ip)) => check_literal(ip.into(), policy)?,
            Some(Host::Ipv6(ip)) => check_literal(ip.into(), policy)?,
            None => return Err(Refusal::Host),
        }
        Ok(Route::Public)
    }
}

/// A literal address: never on a host allowlist (names only), and public.
fn check_literal(ip: std::net::IpAddr, policy: &Policy<'_>) -> Result<(), Refusal> {
    if policy.hosts.is_some() {
        return Err(Refusal::Host);
    }
    if !is_public(ip) {
        return Err(Refusal::Address);
    }
    Ok(())
}

/// The checks a request applies to each URL.
struct Policy<'a> {
    purpose: Purpose,
    https_only: bool,
    hosts: Option<&'a HostSet>,
}

/// A handle that sends requests for one [`Purpose`]. Cheap to clone.
#[derive(Clone)]
pub struct Egress {
    shared: Arc<Shared>,
    purpose: Purpose,
}

impl Egress {
    pub(crate) fn new(shared: Arc<Shared>, purpose: Purpose) -> Self {
        Self { shared, purpose }
    }

    /// The purpose its requests are counted under.
    #[must_use]
    pub fn purpose(&self) -> Purpose {
        self.purpose
    }

    /// A `GET` of `url`.
    pub fn get(&self, url: &str) -> EgressRequest {
        self.request(Method::GET, url)
    }

    /// A `POST` to `url`.
    pub fn post(&self, url: &str) -> EgressRequest {
        self.request(Method::POST, url)
    }

    /// A request. An invalid `url` fails at [`EgressRequest::send`].
    pub fn request(&self, method: Method, url: &str) -> EgressRequest {
        EgressRequest {
            egress: self.clone(),
            method,
            url: Url::parse(url).ok(),
            headers: HeaderMap::new(),
            body: None,
            timeout: self.purpose.timeout(),
            max_redirects: self.purpose.max_redirects(),
            https_only: self.purpose.https_only(),
            hosts: None,
        }
    }
}

/// An outbound request being built; [`EgressRequest::send`] sends it.
#[must_use = "a request does nothing until it is sent"]
pub struct EgressRequest {
    egress: Egress,
    method: Method,
    url: Option<Url>,
    headers: HeaderMap,
    body: Option<Bytes>,
    timeout: Duration,
    max_redirects: u8,
    https_only: bool,
    hosts: Option<Arc<HostSet>>,
}

impl EgressRequest {
    /// Sets a header, replacing any value it had.
    pub fn header(mut self, name: HeaderName, value: HeaderValue) -> Self {
        self.headers.insert(name, value);
        self
    }

    /// Sets several headers, replacing the values they had.
    pub fn headers(mut self, headers: HeaderMap) -> Self {
        for (name, value) in &headers {
            self.headers.insert(name.clone(), value.clone());
        }
        self
    }

    /// The request body. Set `Content-Type` with [`EgressRequest::header`].
    pub fn body(mut self, body: impl Into<Bytes>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// The total time: every hop, and the body of the last one.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Redirects to follow, at most [`MAX_REDIRECTS`]. At 0 a redirect comes
    /// back as the response.
    pub fn max_redirects(mut self, max: u8) -> Self {
        self.max_redirects = max.min(MAX_REDIRECTS);
        self
    }

    /// https only, for the URL and every redirect target.
    pub fn https_only(mut self) -> Self {
        self.https_only = true;
        self
    }

    /// The hosts the URL and every redirect target must be on (P2-G4).
    pub fn hosts(mut self, hosts: Arc<HostSet>) -> Self {
        self.hosts = Some(hosts);
        self
    }

    /// Sends the request and follows its redirects. A refused URL is
    /// refused before the request waits for a slot.
    ///
    /// # Errors
    ///
    /// [`EgressError`]: a refusal, a timeout, too many redirects or a
    /// network failure. A response of any status is `Ok`.
    pub async fn send(self) -> Result<EgressResponse, EgressError> {
        let purpose = self.egress.purpose;
        let result = self.exchange().await;
        let outcome = match &result {
            Ok(response) => match response.status().as_u16() {
                400..=499 => egress_outcome::CLIENT_ERROR,
                500..=599 => egress_outcome::SERVER_ERROR,
                _ => egress_outcome::OK,
            },
            Err(err) => {
                tracing::debug!(purpose = purpose.label(), error = %err, "outbound request failed");
                err.outcome()
            }
        };
        metrics::counter!(EGRESS_REQUESTS_TOTAL, "purpose" => purpose.label(), "outcome" => outcome)
            .increment(1);
        result
    }

    async fn exchange(self) -> Result<EgressResponse, EgressError> {
        let Self {
            egress,
            mut method,
            url,
            mut headers,
            mut body,
            timeout,
            max_redirects,
            https_only,
            hosts,
        } = self;
        let shared = &egress.shared;
        let policy = Policy {
            purpose: egress.purpose,
            https_only,
            hosts: hosts.as_deref(),
        };
        let mut url = url.ok_or(EgressError::InvalidUrl)?;
        let mut route = shared.route(&url, &policy).map_err(EgressError::Refused)?;
        let permit = Arc::clone(&shared.in_flight)
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");
        let deadline = Instant::now() + timeout;
        let mut redirects = 0_u8;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(EgressError::Timeout);
            }
            let client = shared
                .clients
                .get(route)
                .ok_or(EgressError::Refused(Refusal::Origin))?;
            let mut request = client
                .request(method.clone(), url.clone())
                .headers(headers.clone())
                .timeout(remaining);
            if let Some(body) = &body {
                request = request.body(body.clone());
            }
            let response = request.send().await.map_err(EgressError::from_reqwest)?;
            if shared.proxied
                && route == Route::Public
                && response.headers().contains_key(PROXY_ERROR_HEADER)
            {
                return Err(EgressError::Refused(Refusal::Proxy));
            }
            let status = response.status();
            let next = redirect_target(status, response.headers(), &url);
            let Some(next) = next.filter(|_| max_redirects > 0) else {
                return Ok(EgressResponse {
                    response,
                    url,
                    redirects,
                    _permit: permit,
                });
            };
            if redirects >= max_redirects {
                return Err(EgressError::TooManyRedirects(max_redirects));
            }
            let next = next?;
            route = shared.route(&next, &policy).map_err(EgressError::Refused)?;
            drop(response);
            let to_get = match status.as_u16() {
                303 => method != Method::HEAD,
                301 | 302 => method == Method::POST,
                _ => false,
            };
            if to_get {
                method = Method::GET;
                body = None;
                for name in [CONTENT_TYPE, CONTENT_LENGTH, CONTENT_ENCODING] {
                    headers.remove(name);
                }
            }
            if Origin::of(&next) != Origin::of(&url) {
                headers = keep_across_origins(&headers);
            }
            redirects += 1;
            url = next;
        }
    }
}

/// The target of a redirect response; `None` when the response is not a
/// followed redirect (another status, or no usable `Location`).
fn redirect_target(
    status: StatusCode,
    headers: &HeaderMap,
    base: &Url,
) -> Option<Result<Url, EgressError>> {
    if !matches!(status.as_u16(), 301 | 302 | 303 | 307 | 308) {
        return None;
    }
    let location = headers.get(LOCATION)?.to_str().ok()?;
    let mut next = match base.join(location.trim()) {
        Ok(next) => next,
        Err(_) => return Some(Err(EgressError::InvalidUrl)),
    };
    next.set_fragment(None);
    Some(Ok(next))
}

fn keep_across_origins(headers: &HeaderMap) -> HeaderMap {
    let mut kept = HeaderMap::new();
    for (name, value) in headers {
        if HEADERS_ACROSS_ORIGINS.contains(&name.as_str()) {
            kept.append(name.clone(), value.clone());
        }
    }
    kept
}

/// A response. It holds its in-flight slot until it is dropped or its body
/// is read.
#[derive(Debug)]
pub struct EgressResponse {
    response: reqwest::Response,
    url: Url,
    redirects: u8,
    _permit: OwnedSemaphorePermit,
}

impl EgressResponse {
    /// The status.
    #[must_use]
    pub fn status(&self) -> StatusCode {
        self.response.status()
    }

    /// The headers.
    #[must_use]
    pub fn headers(&self) -> &HeaderMap {
        self.response.headers()
    }

    /// The URL that answered, after the redirects.
    #[must_use]
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Redirects followed.
    #[must_use]
    pub fn redirects(&self) -> u8 {
        self.redirects
    }

    /// The body's length, when the response declares it.
    #[must_use]
    pub fn content_length(&self) -> Option<u64> {
        self.response.content_length()
    }

    /// The `Content-Type`, when it is text.
    #[must_use]
    pub fn content_type(&self) -> Option<&str> {
        self.headers().get(CONTENT_TYPE)?.to_str().ok()
    }

    /// `Retry-After`, in seconds or as an HTTP date, at most a day.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        let value = self.headers().get(RETRY_AFTER)?.to_str().ok()?;
        parse_retry_after(value, SystemTime::now())
    }

    /// The whole body, refused when longer than `cap` bytes.
    ///
    /// # Errors
    ///
    /// [`EgressError::TooLarge`] (by the declared length, before reading, or
    /// while reading), a timeout or a network failure.
    pub async fn read_capped(self, cap: u64) -> Result<Bytes, EgressError> {
        if self.content_length().is_some_and(|len| len > cap) {
            return Err(EgressError::TooLarge { limit: cap });
        }
        let mut body = Vec::new();
        let mut stream = self.stream_capped(cap);
        while let Some(chunk) = stream.next().await {
            body.extend_from_slice(&chunk?);
        }
        Ok(body.into())
    }

    /// At most the first `max` bytes of the body; the rest is not read.
    ///
    /// # Errors
    ///
    /// A timeout or a network failure.
    pub async fn read_prefix(mut self, max: usize) -> Result<Bytes, EgressError> {
        let mut head = Vec::new();
        while head.len() < max {
            let Some(chunk) = self
                .response
                .chunk()
                .await
                .map_err(EgressError::from_reqwest)?
            else {
                break;
            };
            let take = chunk.len().min(max - head.len());
            head.extend_from_slice(&chunk[..take]);
        }
        Ok(head.into())
    }

    /// The body as text (invalid UTF-8 replaced), refused past `cap` bytes.
    ///
    /// # Errors
    ///
    /// Like [`EgressResponse::read_capped`].
    pub async fn text_capped(self, cap: u64) -> Result<String, EgressError> {
        let body = self.read_capped(cap).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// The body as JSON, refused past `cap` bytes.
    ///
    /// # Errors
    ///
    /// Like [`EgressResponse::read_capped`], or [`EgressError::Decode`].
    pub async fn json_capped<T: DeserializeOwned>(self, cap: u64) -> Result<T, EgressError> {
        let body = self.read_capped(cap).await?;
        serde_json::from_slice(&body).map_err(EgressError::Decode)
    }

    /// The body as a stream of chunks that fails with
    /// [`EgressError::TooLarge`] past `cap` bytes.
    #[must_use]
    pub fn stream_capped(self, cap: u64) -> CappedBody {
        CappedBody {
            inner: Box::pin(self.response.bytes_stream()),
            cap,
            read: 0,
            done: false,
            _permit: self._permit,
        }
    }

    /// The body as an [`AsyncRead`] that fails past `cap` bytes; its errors
    /// wrap an [`EgressError`] ([`EgressError::in_io`] finds it).
    pub fn reader_capped(self, cap: u64) -> impl AsyncRead + Send + Unpin {
        StreamReader::new(self.stream_capped(cap).map_err(io::Error::other))
    }
}

impl EgressError {
    /// The egress error inside an I/O error of
    /// [`EgressResponse::reader_capped`].
    #[must_use]
    pub fn in_io(err: &io::Error) -> Option<&Self> {
        err.get_ref()?.downcast_ref::<Self>()
    }
}

/// A capped response body ([`EgressResponse::stream_capped`]).
pub struct CappedBody {
    inner: Pin<Box<dyn Stream<Item = reqwest::Result<Bytes>> + Send>>,
    cap: u64,
    read: u64,
    done: bool,
    _permit: OwnedSemaphorePermit,
}

impl Stream for CappedBody {
    type Item = Result<Bytes, EgressError>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.done {
            return Poll::Ready(None);
        }
        match self.inner.as_mut().poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(None) => {
                self.done = true;
                Poll::Ready(None)
            }
            Poll::Ready(Some(Err(err))) => {
                self.done = true;
                Poll::Ready(Some(Err(EgressError::from_reqwest(err))))
            }
            Poll::Ready(Some(Ok(chunk))) => {
                let read = self.read.saturating_add(chunk.len() as u64);
                if read > self.cap {
                    self.done = true;
                    let cap = self.cap;
                    return Poll::Ready(Some(Err(EgressError::TooLarge { limit: cap })));
                }
                self.read = read;
                Poll::Ready(Some(Ok(chunk)))
            }
        }
    }
}

/// `Retry-After` as seconds or an HTTP date, relative to `now`, at most a
/// day; `None` when it is neither.
#[must_use]
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<Duration> {
    let value = value.trim();
    let wait = match value.parse::<u64>() {
        Ok(seconds) => Duration::from_secs(seconds),
        Err(_) => httpdate::parse_http_date(value)
            .ok()?
            .duration_since(now)
            .unwrap_or(Duration::ZERO),
    };
    Some(wait.min(MAX_RETRY_AFTER))
}

#[cfg(test)]
mod tests {
    use axum::http::header;

    use super::super::resolve::Lookup;
    use super::*;

    #[test]
    fn retry_after_takes_seconds_and_dates() {
        let now = httpdate::parse_http_date("Sat, 03 Oct 2026 10:00:00 GMT").unwrap();
        assert_eq!(
            parse_retry_after("120", now),
            Some(Duration::from_secs(120))
        );
        assert_eq!(parse_retry_after(" 0 ", now), Some(Duration::ZERO));
        assert_eq!(
            parse_retry_after("Sat, 03 Oct 2026 10:01:30 GMT", now),
            Some(Duration::from_secs(90))
        );
        assert_eq!(
            parse_retry_after("Sat, 03 Oct 2026 09:00:00 GMT", now),
            Some(Duration::ZERO),
            "a past date is now"
        );
        assert_eq!(parse_retry_after("999999999", now), Some(MAX_RETRY_AFTER));
        assert_eq!(parse_retry_after("soon", now), None);
        assert_eq!(parse_retry_after("-5", now), None);
    }

    #[test]
    fn redirect_targets() {
        let base = Url::parse("https://pbs.twimg.com/media/a.jpg?x=1").unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(LOCATION, HeaderValue::from_static("/media/b.jpg#frag"));
        let target = |status: u16, headers: &HeaderMap| {
            redirect_target(StatusCode::from_u16(status).unwrap(), headers, &base)
                .map(|r| r.map(|u| u.to_string()).map_err(|e| e.to_string()))
        };
        for status in [301, 302, 303, 307, 308] {
            assert_eq!(
                target(status, &headers),
                Some(Ok("https://pbs.twimg.com/media/b.jpg".to_owned()))
            );
        }
        for status in [200, 300, 304, 305, 306, 404] {
            assert_eq!(target(status, &headers), None, "{status}");
        }
        assert_eq!(target(302, &HeaderMap::new()), None, "no location");
        headers.insert(LOCATION, HeaderValue::from_static("http://[::1"));
        assert!(matches!(target(302, &headers), Some(Err(_))));
    }

    #[test]
    fn only_safe_headers_cross_origins() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer secret"),
        );
        headers.insert(header::COOKIE, HeaderValue::from_static("a=b"));
        headers.insert("x-api-key", HeaderValue::from_static("secret"));
        headers.insert(header::ACCEPT, HeaderValue::from_static("image/*"));
        headers.insert(header::USER_AGENT, HeaderValue::from_static("ua"));
        headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
        let kept = keep_across_origins(&headers);
        let names: Vec<&str> = kept.keys().map(HeaderName::as_str).collect();
        assert_eq!(names.len(), 3, "{names:?}");
        for name in ["accept", "user-agent", "sec-fetch-dest"] {
            assert!(kept.contains_key(name), "{name}");
        }
    }

    #[test]
    fn errors_are_found_in_chains() {
        let blocked = BlockedAddress { reason: "loopback" };
        let wrapped = io::Error::other(blocked);
        assert_eq!(
            find_source::<BlockedAddress>(&wrapped).map(|b| b.reason),
            Some("loopback")
        );
        assert!(find_source::<BlockedAddress>(&io::Error::other("x")).is_none());

        let inner = io::Error::other(EgressError::TooLarge { limit: 7 });
        assert!(matches!(
            EgressError::in_io(&inner),
            Some(EgressError::TooLarge { limit: 7 })
        ));
        assert!(EgressError::in_io(&io::Error::other("x")).is_none());
    }

    #[test]
    fn refusal_codes_are_stable() {
        let codes = [
            Refusal::Scheme,
            Refusal::Credentials,
            Refusal::Port,
            Refusal::Host,
            Refusal::Address,
            Refusal::Origin,
            Refusal::Proxy,
        ]
        .map(Refusal::code);
        assert_eq!(
            codes,
            [
                "scheme",
                "credentials",
                "port",
                "host",
                "address",
                "origin",
                "proxy"
            ]
        );
    }

    #[test]
    fn the_tls_configuration_offers_http2() {
        let config = tls_config(&[]).unwrap();
        assert_eq!(
            config.alpn_protocols,
            [b"h2".to_vec(), b"http/1.1".to_vec()]
        );
        assert!(tls_config(&[CertificateDer::from(vec![1, 2, 3])]).is_err());
    }

    #[test]
    fn lookup_defaults_to_the_system() {
        assert_eq!(format!("{:?}", Lookup::default()), "Lookup");
    }
}
