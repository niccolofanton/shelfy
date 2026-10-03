//! The test provider (feature `stub`, P3 lane rule 4): a local server that
//! speaks the three protocols, so that tests, e2e runs and local servers
//! never need the network or a real provider.
//!
//! | Path (suffix) | Endpoint |
//! |---|---|
//! | `…/chat/completions` | OpenAI-compatible chat, streamed or not, with `timings` like llama.cpp |
//! | `…/v1/messages` | Anthropic Messages, streamed or not, forced tools, `output_config.format` |
//! | `…/embeddings` | OpenAI-compatible embeddings: deterministic unit vectors |
//! | `…/audio/transcriptions` | OpenAI-compatible transcription |
//! | `…/inference` | whisper.cpp transcription |
//! | `…/models` | the model list, in Anthropic's shape when the request names `anthropic-version` |
//! | `/health` | `{"status": "ok"}`, without a key |
//! | `/_stub/…` | the admin routes: the request log, faults, latency, offline, canned answers |
//!
//! Answers are deterministic. For a chat request the stub looks, in order,
//! for a canned answer under the request's key ([`request_key`]), then for a
//! recording `<key>.json` in the recordings directory, then makes one: an
//! instance of the request's schema ([`schema_example`]), or a short text.
//! Faults ([`Fault`]) and a latency apply per request; [`Stub::set_offline`]
//! refuses connections. The request log ([`LoggedRequest`]) keeps bodies and
//! a few headers, never a key.

mod answer;
mod anthropic;
mod chat;
mod fault;
mod form;
mod log;
mod openai;

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, CONTENT_TYPE, LOCATION};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use bytes::Bytes;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};
use tokio::task::{JoinHandle, JoinSet};
use tokio_util::sync::CancellationToken;
use url::Url;

pub use answer::{Recording, key_of, request_key, schema_example, sha256_hex, write_recording};
pub use fault::{Endpoint, Fault, FaultRule};
pub use log::{AuthSeen, FileSeen, LoggedRequest};

use self::answer::Answers;
use self::chat::{Ask, Frame};

/// How the stub starts.
#[derive(Clone)]
pub struct StubConfig {
    /// The address to listen on; port 0 picks a free one.
    pub listen: SocketAddr,
    /// The key requests must send (`Authorization: Bearer` or `x-api-key`);
    /// none accepts any request.
    pub api_key: Option<SecretString>,
    /// The models `…/models` lists.
    pub models: Vec<String>,
    /// A delay before every answer (§6.3 scenario C uses 2 s).
    pub latency: Duration,
    /// A delay between streamed chunks.
    pub chunk_delay: Duration,
    /// The size of embedding vectors.
    pub embedding_dims: usize,
    /// A directory of recordings, `<key>.json` ([`Recording`]).
    pub recordings: Option<PathBuf>,
    /// Canned answers by request key.
    pub canned: HashMap<String, String>,
    /// Faults pending from the start.
    pub faults: Vec<FaultRule>,
    /// Whether chat requests may carry WebP images. Off, the stub answers
    /// them as llama.cpp does (the owner's node): 400 "Failed to load image or
    /// audio file".
    pub webp_images: bool,
}

impl Default for StubConfig {
    fn default() -> Self {
        Self {
            listen: SocketAddr::from(([127, 0, 0, 1], 0)),
            api_key: None,
            models: vec![
                "stub-text".into(),
                "stub-vision".into(),
                "stub-embed".into(),
            ],
            latency: Duration::ZERO,
            chunk_delay: Duration::ZERO,
            embedding_dims: 16,
            recordings: None,
            canned: HashMap::new(),
            faults: Vec::new(),
            webp_images: true,
        }
    }
}

impl std::fmt::Debug for StubConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StubConfig")
            .field("listen", &self.listen)
            .field("api_key", &self.api_key.as_ref().map(|_| "[redacted]"))
            .field("models", &self.models)
            .field("latency", &self.latency)
            .field("chunk_delay", &self.chunk_delay)
            .field("embedding_dims", &self.embedding_dims)
            .field("recordings", &self.recordings)
            .field("canned", &self.canned.len())
            .field("faults", &self.faults)
            .field("webp_images", &self.webp_images)
            .finish()
    }
}

/// The protocol whose shapes an answer takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Protocol {
    OpenAi,
    Anthropic,
    Whisper,
}

enum Command {
    Offline(bool, Option<oneshot::Sender<io::Result<()>>>),
}

struct Shared {
    api_key: Option<SecretString>,
    models: Vec<String>,
    embedding_dims: usize,
    webp_images: bool,
    state: Mutex<MutableState>,
    commands: mpsc::UnboundedSender<Command>,
}

struct MutableState {
    faults: Vec<FaultRule>,
    latency: Duration,
    chunk_delay: Duration,
    log: Vec<LoggedRequest>,
    answers: Answers,
}

impl Shared {
    fn state(&self) -> std::sync::MutexGuard<'_, MutableState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// A running stub. Dropping it stops the server.
pub struct Stub {
    shared: Arc<Shared>,
    addr: SocketAddr,
    shutdown: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for Stub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Stub")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl Stub {
    /// Starts a stub with `config`.
    ///
    /// # Errors
    ///
    /// The listener's bind error.
    pub async fn start(config: StubConfig) -> io::Result<Self> {
        let listener = TcpListener::bind(config.listen).await?;
        let addr = listener.local_addr()?;
        let (commands, receiver) = mpsc::unbounded_channel();
        let shared = Arc::new(Shared {
            api_key: config.api_key,
            models: config.models,
            embedding_dims: config.embedding_dims.max(1),
            webp_images: config.webp_images,
            state: Mutex::new(MutableState {
                faults: config.faults,
                latency: config.latency,
                chunk_delay: config.chunk_delay,
                log: Vec::new(),
                answers: Answers {
                    canned: config.canned,
                    recordings: config.recordings,
                },
            }),
            commands,
        });
        let router = Router::new()
            .fallback(handle)
            .with_state(Arc::clone(&shared))
            .layer(DefaultBodyLimit::max(64 << 20));
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(serve(listener, addr, router, receiver, shutdown.clone()));
        Ok(Self {
            shared,
            addr,
            shutdown,
            task: Some(task),
        })
    }

    /// The address it listens on.
    #[must_use]
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// `http://127.0.0.1:<port>`, the base URL of an Anthropic provider.
    #[must_use]
    pub fn url(&self) -> Url {
        Url::parse(&format!("http://{}", self.addr)).expect("a socket address makes a URL")
    }

    /// `…/v1`, the base URL of an OpenAI-compatible provider.
    #[must_use]
    pub fn openai_base(&self) -> Url {
        self.url().join("/v1").expect("a valid path")
    }

    /// `…/inference`, the URL of a whisper.cpp provider.
    #[must_use]
    pub fn whisper_url(&self) -> Url {
        self.url().join("/inference").expect("a valid path")
    }

    /// `…/health`.
    #[must_use]
    pub fn health_url(&self) -> Url {
        self.url().join("/health").expect("a valid path")
    }

    /// Adds a fault rule after the pending ones.
    pub fn inject(&self, rule: FaultRule) {
        self.shared.state().faults.push(rule);
    }

    /// Drops every pending fault.
    pub fn clear_faults(&self) {
        self.shared.state().faults.clear();
    }

    /// Sets the delay before every answer.
    pub fn set_latency(&self, latency: Duration) {
        self.shared.state().latency = latency;
    }

    /// Sets the delay between streamed chunks.
    pub fn set_chunk_delay(&self, delay: Duration) {
        self.shared.state().chunk_delay = delay;
    }

    /// Answers `text` to requests whose key is `key`.
    pub fn add_canned(&self, key: impl Into<String>, text: impl Into<String>) {
        self.shared
            .state()
            .answers
            .canned
            .insert(key.into(), text.into());
    }

    /// The requests received so far.
    #[must_use]
    pub fn requests(&self) -> Vec<LoggedRequest> {
        self.shared.state().log.clone()
    }

    /// Forgets the requests received so far.
    pub fn clear_requests(&self) {
        self.shared.state().log.clear();
    }

    /// Goes offline (the listener closes and open connections drop: clients
    /// get "connection refused") or back online on the same port. Returns
    /// once done.
    ///
    /// # Errors
    ///
    /// The port could not be bound again (another process took it while the
    /// stub was offline), or the stub has stopped.
    pub async fn set_offline(&self, offline: bool) -> io::Result<()> {
        let (ack, done) = oneshot::channel();
        let stopped = || io::Error::new(io::ErrorKind::NotConnected, "the stub has stopped");
        self.shared
            .commands
            .send(Command::Offline(offline, Some(ack)))
            .map_err(|_| stopped())?;
        done.await.map_err(|_| stopped())?
    }

    /// Stops the server.
    pub async fn shutdown(mut self) {
        self.shutdown.cancel();
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

enum Event {
    Stop,
    Command(Command),
    Accepted(TcpStream),
    AcceptFailed,
    Nothing,
}

/// Accepts connections until shut down; closes and reopens the listener on
/// command.
async fn serve(
    listener: TcpListener,
    addr: SocketAddr,
    router: Router,
    mut commands: mpsc::UnboundedReceiver<Command>,
    shutdown: CancellationToken,
) {
    let mut listener = Some(listener);
    let mut connections = JoinSet::new();
    loop {
        let event = match &listener {
            Some(open) => tokio::select! {
                () = shutdown.cancelled() => Event::Stop,
                command = commands.recv() => command.map_or(Event::Stop, Event::Command),
                accepted = open.accept() => accepted.map_or(Event::AcceptFailed, |(stream, _)| Event::Accepted(stream)),
                Some(_) = connections.join_next(), if !connections.is_empty() => Event::Nothing,
            },
            None => tokio::select! {
                () = shutdown.cancelled() => Event::Stop,
                command = commands.recv() => command.map_or(Event::Stop, Event::Command),
            },
        };
        match event {
            Event::Stop => break,
            Event::Nothing => {}
            // Out of file descriptors and the like: do not spin.
            Event::AcceptFailed => tokio::time::sleep(Duration::from_millis(10)).await,
            Event::Accepted(stream) => {
                connections.spawn(connection(stream, router.clone()));
            }
            Event::Command(Command::Offline(offline, ack)) => {
                let mut result = Ok(());
                if offline {
                    listener = None;
                    connections.abort_all();
                    while connections.join_next().await.is_some() {}
                } else if listener.is_none() {
                    match rebind(addr).await {
                        Ok(bound) => listener = Some(bound),
                        Err(error) => result = Err(error),
                    }
                }
                if let Some(ack) = ack {
                    let _ = ack.send(result);
                }
            }
        }
    }
    connections.abort_all();
}

/// Binds `addr` again, retrying for a moment.
async fn rebind(addr: SocketAddr) -> io::Result<TcpListener> {
    let mut last = None;
    for _ in 0..40 {
        match TcpListener::bind(addr).await {
            Ok(listener) => return Ok(listener),
            Err(error) => last = Some(error),
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    tracing::warn!(%addr, "the stub could not listen again");
    Err(last.unwrap_or_else(|| io::Error::other("the stub could not listen again")))
}

async fn connection(stream: TcpStream, router: Router) {
    let service = TowerToHyperService::new(router);
    let _ = hyper::server::conn::http1::Builder::new()
        .serve_connection(TokioIo::new(stream), service)
        .await;
}

/// The endpoint a path names, by its suffix.
fn endpoint_of(path: &str) -> Endpoint {
    let path = path.trim_end_matches('/');
    if path.ends_with("/health") {
        Endpoint::Health
    } else if path.ends_with("/chat/completions") {
        Endpoint::Chat
    } else if path.ends_with("/messages") {
        Endpoint::Messages
    } else if path.ends_with("/embeddings") {
        Endpoint::Embeddings
    } else if path.ends_with("/audio/transcriptions") {
        Endpoint::Transcriptions
    } else if path.ends_with("/inference") {
        Endpoint::Inference
    } else if path.ends_with("/models") {
        Endpoint::Models
    } else {
        Endpoint::Unknown
    }
}

fn auth_seen(shared: &Shared, protocol: Protocol, headers: &HeaderMap) -> AuthSeen {
    let sent = match protocol {
        Protocol::Anthropic => headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok()),
        _ => headers
            .get(AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer ")),
    };
    match (sent, &shared.api_key) {
        (None, _) => AuthSeen::None,
        (Some(sent), Some(key)) if sent != key.expose_secret() => AuthSeen::Invalid,
        (Some(_), _) => AuthSeen::Valid,
    }
}

async fn handle(
    State(shared): State<Arc<Shared>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let path = uri.path().to_owned();
    if let Some(route) = path.strip_prefix("/_stub/") {
        return admin(&shared, &method, route, &body);
    }
    let endpoint = endpoint_of(&path);
    let protocol = match endpoint {
        Endpoint::Messages => Protocol::Anthropic,
        Endpoint::Models if headers.contains_key("anthropic-version") => Protocol::Anthropic,
        Endpoint::Inference => Protocol::Whisper,
        _ => Protocol::OpenAi,
    };
    let auth = auth_seen(&shared, protocol, &headers);
    let mut entry = LoggedRequest::new(
        endpoint,
        method.as_str(),
        &path,
        uri.query(),
        &headers,
        auth,
    );
    let (fault, latency, chunk_delay) = {
        let mut state = shared.state();
        let fault = fault::take(&mut state.faults, endpoint);
        (fault, state.latency, state.chunk_delay)
    };
    entry.fault = fault.as_ref().map(|fault| fault.name().to_owned());

    let json: Option<Value> = serde_json::from_slice(&body).ok();
    let mut ask = None;
    let mut parts = None;
    match endpoint {
        Endpoint::Chat | Endpoint::Messages => {
            if let Some(json) = &json {
                let read = if endpoint == Endpoint::Chat {
                    openai::ask(json)
                } else {
                    anthropic::ask(json)
                };
                entry.key = Some(read.key.clone());
                ask = Some(read);
            }
            entry.body.clone_from(&json);
        }
        Endpoint::Embeddings => entry.body.clone_from(&json),
        Endpoint::Transcriptions | Endpoint::Inference => {
            let read = headers
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .and_then(form::boundary)
                .and_then(|boundary| form::parse(&body, &boundary));
            if let Some(read) = &read {
                entry.form = Some(
                    read.iter()
                        .filter(|part| part.filename.is_none())
                        .map(|part| {
                            (
                                part.name.clone(),
                                String::from_utf8_lossy(&part.data).into_owned(),
                            )
                        })
                        .collect::<BTreeMap<_, _>>(),
                );
                if let Some(file) = read.iter().find(|part| part.filename.is_some()) {
                    let sha256 = sha256_hex(&file.data);
                    entry.key = Some(sha256.clone());
                    entry.file = Some(FileSeen {
                        field: file.name.clone(),
                        filename: file.filename.clone(),
                        content_type: file.content_type.clone(),
                        bytes: file.data.len(),
                        sha256,
                    });
                }
            }
            parts = read;
        }
        _ => {}
    }
    shared.state().log.push(entry);

    if !latency.is_zero() {
        tokio::time::sleep(latency).await;
    }
    let needs_key = !matches!(endpoint, Endpoint::Health | Endpoint::Unknown);
    if needs_key && shared.api_key.is_some() && auth != AuthSeen::Valid {
        return error(
            protocol,
            StatusCode::UNAUTHORIZED,
            "invalid api key",
            "authentication_error",
        );
    }
    if let Some(fault) = &fault
        && let Some(response) = status_fault(fault, protocol, &path).await
    {
        return response;
    }
    let expected = match endpoint {
        Endpoint::Models | Endpoint::Health => Method::GET,
        _ => Method::POST,
    };
    if method != expected && endpoint != Endpoint::Unknown {
        return error(
            protocol,
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
            "invalid_request_error",
        );
    }
    match endpoint {
        Endpoint::Unknown => error(
            protocol,
            StatusCode::NOT_FOUND,
            "not found",
            "not_found_error",
        ),
        Endpoint::Health => simple(fault.as_ref(), json!({"status": "ok"})).await,
        Endpoint::Models => {
            let list = match protocol {
                Protocol::Anthropic => anthropic::models(&shared.models),
                _ => openai::models(&shared.models),
            };
            simple(fault.as_ref(), list).await
        }
        Endpoint::Embeddings => match json
            .as_ref()
            .map(|json| openai::embeddings(json, shared.embedding_dims))
        {
            Some(Ok(answer)) => simple(fault.as_ref(), answer).await,
            Some(Err(message)) => error(
                protocol,
                StatusCode::BAD_REQUEST,
                message,
                "invalid_request_error",
            ),
            None => error(
                protocol,
                StatusCode::BAD_REQUEST,
                "the body is not JSON",
                "invalid_request_error",
            ),
        },
        Endpoint::Transcriptions | Endpoint::Inference => {
            transcription(&shared, protocol, parts, fault.as_ref()).await
        }
        Endpoint::Chat | Endpoint::Messages => match ask {
            Some(_) if !shared.webp_images && json.as_ref().is_some_and(carries_webp) => error(
                protocol,
                StatusCode::BAD_REQUEST,
                "Failed to load image or audio file",
                "invalid_request_error",
            ),
            Some(ask) => chat(&shared, protocol, &ask, fault.as_ref(), chunk_delay).await,
            None => error(
                protocol,
                StatusCode::BAD_REQUEST,
                "the body is not JSON",
                "invalid_request_error",
            ),
        },
    }
}

/// Whether a chat body carries a WebP image: an `image_url` data URL or an
/// Anthropic base64 block.
fn carries_webp(body: &Value) -> bool {
    let Some(messages) = body.get("messages").and_then(Value::as_array) else {
        return false;
    };
    messages
        .iter()
        .filter_map(|message| message.get("content")?.as_array())
        .flatten()
        .any(|part| {
            let url = part
                .get("image_url")
                .and_then(|image| image.get("url"))
                .and_then(Value::as_str);
            let media_type = part
                .get("source")
                .and_then(|source| source.get("media_type"))
                .and_then(Value::as_str);
            url.is_some_and(|url| url.starts_with("data:image/webp"))
                || media_type == Some("image/webp")
        })
}

/// The answer of a status fault, or `None` for faults the endpoint plays.
async fn status_fault(fault: &Fault, protocol: Protocol, path: &str) -> Option<Response> {
    Some(match fault {
        Fault::RateLimited { retry_after_ms } => {
            let mut response = error(
                protocol,
                StatusCode::TOO_MANY_REQUESTS,
                "rate limit reached",
                "rate_limit_error",
            );
            let headers = response.headers_mut();
            headers.insert(
                "retry-after",
                HeaderValue::from(retry_after_ms.div_ceil(1000)),
            );
            headers.insert("retry-after-ms", HeaderValue::from(*retry_after_ms));
            response
        }
        Fault::ServerError => error(
            protocol,
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal error",
            "api_error",
        ),
        Fault::Overloaded => match protocol {
            Protocol::Anthropic => error(
                protocol,
                StatusCode::from_u16(529).unwrap_or(StatusCode::SERVICE_UNAVAILABLE),
                "Overloaded",
                "overloaded_error",
            ),
            _ => error(
                protocol,
                StatusCode::SERVICE_UNAVAILABLE,
                "overloaded",
                "overloaded_error",
            ),
        },
        Fault::Unauthorized => error(
            protocol,
            StatusCode::UNAUTHORIZED,
            "invalid api key",
            "authentication_error",
        ),
        Fault::Forbidden => error(
            protocol,
            StatusCode::FORBIDDEN,
            "not allowed",
            "permission_error",
        ),
        Fault::QuotaExhausted => match protocol {
            Protocol::OpenAi => json_response(
                StatusCode::TOO_MANY_REQUESTS,
                &openai::error(
                    "You exceeded your current quota, please check your plan and billing details.",
                    "insufficient_quota",
                    "insufficient_quota",
                ),
            ),
            _ => error(
                protocol,
                StatusCode::PAYMENT_REQUIRED,
                "credit balance too low",
                "billing_error",
            ),
        },
        Fault::BadRequest => error(
            protocol,
            StatusCode::BAD_REQUEST,
            "bad request",
            "invalid_request_error",
        ),
        Fault::Redirect => {
            let mut response = (StatusCode::FOUND, "").into_response();
            if let Ok(location) = HeaderValue::from_str(&format!("{path}/moved")) {
                response.headers_mut().insert(LOCATION, location);
            }
            response
        }
        Fault::Timeout => {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            error(
                protocol,
                StatusCode::GATEWAY_TIMEOUT,
                "timeout",
                "api_error",
            )
        }
        Fault::SlowFirstToken { .. }
        | Fault::MalformedJson
        | Fault::NonConformingJson
        | Fault::EmptyStream
        | Fault::Refusal
        | Fault::Reset => return None,
    })
}

/// An error answer in `protocol`'s shape.
fn error(protocol: Protocol, status: StatusCode, message: &str, kind: &str) -> Response {
    let body = match protocol {
        Protocol::OpenAi => openai::error(message, kind, kind),
        Protocol::Anthropic => anthropic::error(message, kind),
        Protocol::Whisper => json!({"error": message}),
    };
    json_response(status, &body)
}

fn json_response(status: StatusCode, body: &Value) -> Response {
    (
        status,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        body.to_string(),
    )
        .into_response()
}

/// A JSON answer, under the faults every endpoint plays: slow, malformed,
/// reset.
async fn simple(fault: Option<&Fault>, body: Value) -> Response {
    match fault {
        Some(Fault::SlowFirstToken { delay_ms }) => {
            tokio::time::sleep(Duration::from_millis(*delay_ms)).await;
            json_response(StatusCode::OK, &body)
        }
        Some(Fault::MalformedJson) => raw_json("{\"not json"),
        Some(Fault::Reset) => broken(body.to_string()),
        _ => json_response(StatusCode::OK, &body),
    }
}

fn raw_json(text: &'static str) -> Response {
    (
        StatusCode::OK,
        [(CONTENT_TYPE, HeaderValue::from_static("application/json"))],
        text,
    )
        .into_response()
}

/// A 200 whose body breaks off halfway: the connection drops.
fn broken(text: String) -> Response {
    let half = Bytes::from(text).slice(..);
    let cut = half.len() / 2;
    frames_response(
        vec![
            Frame::now(String::from_utf8_lossy(&half[..cut]).into_owned()),
            Frame::now(String::new()),
        ],
        Some(1),
        "application/json",
    )
}

/// A body of `frames`, dropping the connection at frame `reset_at`.
fn frames_response(
    frames: Vec<Frame>,
    reset_at: Option<usize>,
    content_type: &'static str,
) -> Response {
    let stream = futures_util::stream::unfold(
        (frames.into_iter().enumerate(), false),
        move |(mut frames, ended)| async move {
            if ended {
                return None;
            }
            let (index, frame) = frames.next()?;
            if Some(index) == reset_at {
                // Pause first: hyper flushes what it holds while the body is
                // pending, so the client sees the start of the answer before
                // the connection drops.
                tokio::time::sleep(Duration::from_millis(50)).await;
                let error = io::Error::new(
                    io::ErrorKind::ConnectionReset,
                    "the stub dropped the connection",
                );
                return Some((Err(error), (frames, true)));
            }
            if !frame.delay.is_zero() {
                tokio::time::sleep(frame.delay).await;
            }
            Some((Ok(frame.bytes), (frames, false)))
        },
    );
    Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, content_type)
        .header(CACHE_CONTROL, "no-cache")
        .body(Body::from_stream(stream))
        .unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// A chat answer, streamed or not.
async fn chat(
    shared: &Shared,
    protocol: Protocol,
    ask: &Ask,
    fault: Option<&Fault>,
    chunk_delay: Duration,
) -> Response {
    let reply = chat::reply(&shared.state().answers, ask, fault);
    let malformed = matches!(fault, Some(Fault::MalformedJson));
    let slow = match fault {
        Some(Fault::SlowFirstToken { delay_ms }) => Duration::from_millis(*delay_ms),
        _ => Duration::ZERO,
    };
    let reset = matches!(fault, Some(Fault::Reset));
    // The first token arrives late; like llama.cpp, a stream's head waits
    // for it too.
    if !slow.is_zero() {
        tokio::time::sleep(slow).await;
    }
    if ask.stream {
        let mut frames = match protocol {
            Protocol::Anthropic => anthropic::message_stream(ask, &reply, malformed),
            _ => openai::completion_stream(ask, &reply, malformed),
        };
        for frame in frames.iter_mut().skip(1) {
            frame.delay = chunk_delay;
        }
        let reset_at = reset.then_some(frames.len() / 2);
        return frames_response(frames, reset_at, "text/event-stream");
    }
    if malformed {
        return raw_json("{\"choices\": [");
    }
    let body = match protocol {
        Protocol::Anthropic => anthropic::message(ask, &reply),
        _ => openai::completion(ask, &reply),
    };
    if reset {
        return broken(body.to_string());
    }
    json_response(StatusCode::OK, &body)
}

/// A transcription: a canned answer for the file's hash, or a description of
/// what arrived.
async fn transcription(
    shared: &Shared,
    protocol: Protocol,
    parts: Option<Vec<form::FormPart>>,
    fault: Option<&Fault>,
) -> Response {
    let Some(parts) = parts else {
        return error(
            protocol,
            StatusCode::BAD_REQUEST,
            "the body is not a multipart form",
            "invalid_request_error",
        );
    };
    let field = |name: &str| {
        parts
            .iter()
            .find(|part| part.name == name && part.filename.is_none())
            .map(|part| String::from_utf8_lossy(&part.data).into_owned())
    };
    let Some(file) = parts.iter().find(|part| part.name == "file") else {
        // whisper.cpp answers this with 200 and an error object.
        return match protocol {
            Protocol::Whisper => json_response(
                StatusCode::OK,
                &json!({"error": "no 'file' field in the request"}),
            ),
            _ => error(
                protocol,
                StatusCode::BAD_REQUEST,
                "file is required",
                "invalid_request_error",
            ),
        };
    };
    if protocol == Protocol::OpenAi && field("model").is_none_or(|model| model.is_empty()) {
        return error(
            protocol,
            StatusCode::BAD_REQUEST,
            "model is required",
            "invalid_request_error",
        );
    }
    let key = sha256_hex(&file.data);
    let text = match fault {
        Some(Fault::EmptyStream) => String::new(),
        _ => shared.state().answers.known(&key).unwrap_or_else(|| {
            let language = field("language")
                .map(|language| format!(" in {language}"))
                .unwrap_or_default();
            format!("stub transcript of {} bytes{language}", file.data.len())
        }),
    };
    simple(fault, json!({"text": text})).await
}

#[derive(Deserialize)]
struct LatencyBody {
    ms: u64,
}

#[derive(Deserialize)]
struct OfflineBody {
    seconds: f64,
}

#[derive(Deserialize)]
struct CannedBody {
    key: String,
    text: String,
}

/// The admin routes.
fn admin(shared: &Arc<Shared>, method: &Method, route: &str, body: &[u8]) -> Response {
    let done = || StatusCode::NO_CONTENT.into_response();
    let bad = || (StatusCode::BAD_REQUEST, "bad admin request").into_response();
    match (method.as_str(), route.trim_end_matches('/')) {
        ("GET", "requests") => json_response(StatusCode::OK, &json!(shared.state().log)),
        ("DELETE", "requests") => {
            shared.state().log.clear();
            done()
        }
        ("POST", "faults") => match serde_json::from_slice::<FaultRule>(body) {
            Ok(rule) => {
                shared.state().faults.push(rule);
                done()
            }
            Err(_) => bad(),
        },
        ("DELETE", "faults") => {
            shared.state().faults.clear();
            done()
        }
        ("PUT", "latency") => match serde_json::from_slice::<LatencyBody>(body) {
            Ok(latency) => {
                shared.state().latency = Duration::from_millis(latency.ms);
                done()
            }
            Err(_) => bad(),
        },
        ("POST", "canned") => match serde_json::from_slice::<CannedBody>(body) {
            Ok(canned) => {
                shared
                    .state()
                    .answers
                    .canned
                    .insert(canned.key, canned.text);
                done()
            }
            Err(_) => bad(),
        },
        ("POST", "offline") => match serde_json::from_slice::<OfflineBody>(body) {
            Ok(offline) if offline.seconds.is_finite() && offline.seconds >= 0.0 => {
                let commands = shared.commands.clone();
                let seconds = offline.seconds.min(86_400.0);
                tokio::spawn(async move {
                    // Let this answer leave before the connections drop.
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    let _ = commands.send(Command::Offline(true, None));
                    tokio::time::sleep(Duration::from_secs_f64(seconds)).await;
                    let _ = commands.send(Command::Offline(false, None));
                });
                StatusCode::ACCEPTED.into_response()
            }
            _ => bad(),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}
