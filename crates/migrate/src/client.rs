//! The server's migration API, as `login`, `plan` and `run` call it (plan
//! §2.9, §4.1).
//!
//! Every request carries the `migrate` token as `Authorization: Bearer`,
//! except the device sign-in of `login`, which has none yet; the token is
//! never printed. Extra headers (`--header`: the Cloudflare Access service
//! token while Access guards the host, G2) go on every request and are never
//! printed either. Redirects are not followed: a redirect is Access asking
//! for a sign-in, and following it would carry the headers elsewhere.
//!
//! Uploads follow tus 1.0 (core, creation and termination): `POST
//! /api/v1/uploads` creates one, `PATCH` appends at an offset, `HEAD` tells
//! where an interrupted upload stopped, so a re-run continues it, and
//! `DELETE` drops one. Errors are the server's problem documents, reduced to
//! their `code` and developer `detail`.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::http::{HeaderName, HeaderValue, Response, StatusCode};

/// The tus protocol version.
pub const TUS_VERSION: &str = "1.0.0";
/// Content type of a tus `PATCH` body.
pub const OFFSET_OCTET_STREAM: &str = "application/offset+octet-stream";
/// Objects asked about per `POST /migrations/missing-objects`.
pub const MISSING_BATCH: usize = 500;
/// The header that makes `POST /migrations` act once.
pub const IDEMPOTENCY_KEY: &str = "Idempotency-Key";

/// Time limit of one request; uploads send at most one chunk per request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);
/// Times a request refused by the server's rate limit (429) is sent again,
/// each after its `Retry-After`. The limit per user (20 a second, 60 at
/// once) paces an upload of many small objects.
const RATE_LIMIT_RETRIES: u32 = 100;
/// The longest `Retry-After` honored at once.
const MAX_RETRY_AFTER: u64 = 60;

/// An extra request header (`--header "Name: value"`). Its value is never
/// printed.
#[derive(Clone, PartialEq, Eq)]
pub struct Header {
    pub name: String,
    pub value: String,
}

impl std::fmt::Debug for Header {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: (hidden)", self.name)
    }
}

impl Header {
    /// Parses `Name: value` (or `Name:value`).
    ///
    /// # Errors
    ///
    /// No colon, an invalid header name or value, or a header the client
    /// sets itself.
    pub fn parse(text: &str) -> anyhow::Result<Header> {
        let (name, value) = text.split_once(':').context("a header is `Name: value`")?;
        let (name, value) = (name.trim(), value.trim());
        HeaderName::from_bytes(name.as_bytes())
            .ok()
            .filter(|_| !name.is_empty())
            .with_context(|| format!("{name:?} is not a header name"))?;
        HeaderValue::from_str(value)
            .with_context(|| format!("the value of {name} is not a header value"))?;
        let reserved = [
            "authorization",
            "content-type",
            "content-length",
            "host",
            "idempotency-key",
            "tus-resumable",
            "upload-length",
            "upload-offset",
            "upload-metadata",
        ];
        anyhow::ensure!(
            !reserved.contains(&name.to_ascii_lowercase().as_str()),
            "{name} is set by shelfy-migrate itself"
        );
        Ok(Header {
            name: name.to_owned(),
            value: value.to_owned(),
        })
    }

    /// The headers of `--header` arguments: each one `Name: value`, or
    /// `@FILE` for a file of such lines (blank lines and `#` comments
    /// skipped), so secrets stay out of the shell history.
    ///
    /// # Errors
    ///
    /// A header does not parse, or a file cannot be read.
    pub fn parse_all(args: &[String]) -> anyhow::Result<Vec<Header>> {
        let mut out = Vec::new();
        for arg in args {
            match arg.strip_prefix('@') {
                Some(path) => {
                    let text = std::fs::read_to_string(path)
                        .with_context(|| format!("cannot read the header file {path}"))?;
                    for line in text.lines().map(str::trim) {
                        if !line.is_empty() && !line.starts_with('#') {
                            out.push(Header::parse(line)?);
                        }
                    }
                }
                None => out.push(Header::parse(arg)?),
            }
        }
        Ok(out)
    }
}

/// An object of the bundle, as the server checks it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObjectRef {
    /// SHA-256, lowercase hex.
    pub sha256: String,
    /// The store's extension for its type.
    pub ext: String,
    /// Size in bytes.
    pub bytes: u64,
}

#[derive(Serialize)]
struct MissingObjectsRequest<'a> {
    objects: &'a [ObjectRef],
}

#[derive(Deserialize)]
struct MissingObjectsResponse {
    missing: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StartMigration<'a> {
    db_upload_id: &'a str,
    merge: bool,
}

/// The web library before a migration (`GET /migrations/preflight`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Preflight {
    /// No posts and no collections: a bundle replaces it; otherwise it needs
    /// `--merge`.
    pub library_empty: bool,
    pub posts: u64,
    /// 0: unlimited.
    pub quota_bytes: i64,
    pub used_bytes: i64,
    pub max_object_bytes: u64,
    pub max_database_bytes: u64,
    /// An install already queued or running.
    pub active_job_id: Option<i64>,
}

/// An install, as `GET /migrations/{id}` describes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationStatus {
    pub id: String,
    #[serde(default)]
    pub job_id: i64,
    /// `running`, `succeeded` or `failed`.
    pub state: String,
    /// What the install is doing.
    pub stage: String,
    /// 0 to 1, of the whole install.
    pub progress: f64,
    #[serde(default)]
    pub merge: bool,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub max_attempts: u32,
    #[serde(default)]
    pub error: Option<MigrationFailure>,
    /// The reconciliation report, once the install succeeded.
    #[serde(default)]
    pub report: Option<InstallReport>,
}

/// Why an install failed.
#[derive(Debug, Clone, Deserialize)]
pub struct MigrationFailure {
    pub code: String,
    #[serde(default)]
    pub detail: Option<String>,
}

/// The server's reconciliation report (counts only).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstallReport {
    /// `replace` or `merge`.
    pub mode: String,
    /// The bundle's own summary, as uploaded.
    pub bundle: serde_json::Value,
    /// Rows of the web library after the install.
    pub installed: InstalledCounts,
    /// What a merge did.
    pub merge: Option<MergeCounts>,
    pub objects: InstalledObjects,
    pub renditions: Renditions,
    pub archive: Archive,
    /// The desktop settings the library took.
    pub settings: Vec<String>,
    /// The file the previous library is kept in for 7 days.
    pub previous: Option<String>,
    /// How long the install took, ms.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstalledCounts {
    pub posts: BTreeMap<String, u64>,
    pub slides: u64,
    pub collections: u64,
    pub memberships: u64,
    pub post_tags: u64,
    pub post_entities: u64,
    pub tag_aliases: u64,
    pub tag_clusters: u64,
    pub web_captures: u64,
    pub web_capture_assets: u64,
    pub media_objects: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MergeCounts {
    pub posts: BTreeMap<String, MergedPosts>,
    pub replaced: u64,
    pub unchanged: u64,
    pub ai_filled: u64,
    pub dates_filled: u64,
    pub notes_joined: u64,
    pub tags_added: u64,
    pub collections_inserted: u64,
    pub collections_matched: u64,
    pub memberships_added: u64,
    pub memberships_present: u64,
    pub captures_added: u64,
    pub captures_present: u64,
    pub aliases_added: u64,
    pub clusters_added: u64,
    pub cluster_memberships_added: u64,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct MergedPosts {
    pub inserted: u64,
    pub merged: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstalledObjects {
    pub total: u64,
    pub bytes: u64,
    pub from_uploads: u64,
    pub already_stored: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Renditions {
    pub wanted: u64,
    pub rendered: u64,
    pub existing: u64,
    pub failed: u64,
    pub not_renderable: u64,
    pub thumbhashes: u64,
    /// Sizes of the covers' `g480` renditions.
    pub cover_bytes: Option<SizeStats>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SizeStats {
    pub count: u64,
    pub p50: u64,
    pub p95: u64,
    pub max: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Archive {
    pub by_state: BTreeMap<String, u64>,
    pub ig_cover_valid: u64,
    pub ig_cover_expired: u64,
    pub ig_cover_no_expiry: u64,
    pub x_cover: u64,
    pub pinterest_cover: u64,
    pub other_cover: u64,
    pub image_slides_pending: u64,
}

/// Where a tus upload stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UploadState {
    pub offset: u64,
    pub length: u64,
}

/// What `POST /auth/device/start` answers.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorization {
    /// The secret the CLI polls with. Never printed.
    pub device_code: String,
    /// What the user approves: `XXXX-XXXX`.
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: String,
    /// Seconds both codes stay valid.
    pub expires_in: u64,
    /// Seconds between polls.
    pub interval: u64,
}

impl std::fmt::Debug for DeviceAuthorization {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceAuthorization")
            .field("user_code", &self.user_code)
            .field("verification_uri", &self.verification_uri)
            .field("expires_in", &self.expires_in)
            .field("interval", &self.interval)
            .finish_non_exhaustive()
    }
}

/// What a device poll found.
#[derive(Clone, PartialEq, Eq)]
pub enum DevicePoll {
    /// Not approved yet: poll again after `interval` seconds.
    Pending { interval: u64 },
    /// Polled too soon: wait `interval` seconds from now on.
    SlowDown { interval: u64 },
    /// Approved: the token (never printed), valid until `expires_at` (unix
    /// ms).
    Approved { token: String, expires_at: i64 },
    /// 429: wait this long.
    Limited { retry_after: Duration },
    /// 400 `invalid_device_code`: expired, used or unknown; start over.
    Invalid,
}

impl std::fmt::Debug for DevicePoll {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DevicePoll::Pending { interval } => write!(f, "Pending({interval})"),
            DevicePoll::SlowDown { interval } => write!(f, "SlowDown({interval})"),
            DevicePoll::Approved { expires_at, .. } => write!(f, "Approved(until {expires_at})"),
            DevicePoll::Limited { retry_after } => write!(f, "Limited({retry_after:?})"),
            DevicePoll::Invalid => write!(f, "Invalid"),
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum PollAnswer {
    Pending {
        interval: u64,
    },
    SlowDown {
        interval: u64,
    },
    Approved {
        token: String,
        #[serde(rename = "expiresAt", default)]
        expires_at: i64,
    },
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PollRequest<'a> {
    device_code: &'a str,
}

#[derive(Deserialize)]
struct Problem {
    #[serde(default)]
    code: String,
    #[serde(default)]
    detail: Option<String>,
}

/// An error answer of the server.
#[derive(Debug, Clone, thiserror::Error)]
#[error("the server answered {status} {code}{}", detail.as_deref().map(|d| format!(": {d}")).unwrap_or_default())]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub detail: Option<String>,
    /// `Retry-After`, in seconds.
    pub retry_after: Option<u64>,
}

/// A client of one server.
pub struct Client {
    agent: Agent,
    origin: String,
    authorization: Option<String>,
    headers: Vec<Header>,
}

impl Client {
    /// A client of the server at `server` (its public origin, for example
    /// `https://refs.example.com`) with `token` (`shx_…`).
    ///
    /// # Errors
    ///
    /// `server` is not an http(s) URL.
    pub fn new(server: &str, token: &str) -> anyhow::Result<Client> {
        Client::with_headers(server, Some(token), &[])
    }

    /// A client of `server` that sends `token`, if any, and `headers` with
    /// every request.
    ///
    /// # Errors
    ///
    /// `server` is not an http(s) URL.
    pub fn with_headers(
        server: &str,
        token: Option<&str>,
        headers: &[Header],
    ) -> anyhow::Result<Client> {
        let origin = server.trim().trim_end_matches('/').to_owned();
        anyhow::ensure!(
            origin.starts_with("https://") || origin.starts_with("http://"),
            "the server URL must start with https:// or http://"
        );
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .max_redirects(0)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(format!("shelfy-migrate/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Client {
            agent,
            origin,
            authorization: token.map(|t| format!("Bearer {}", t.trim())),
            headers: headers.to_vec(),
        })
    }

    /// The server's origin.
    #[must_use]
    pub fn origin(&self) -> &str {
        &self.origin
    }

    /// An absolute URL for a server path or a `Location` of this server.
    ///
    /// # Errors
    ///
    /// `path` is a URL of another origin: the token and the headers never
    /// leave the server they were given for.
    pub fn url(&self, path: &str) -> anyhow::Result<String> {
        if path.starts_with("http://") || path.starts_with("https://") {
            let same = path
                .strip_prefix(&self.origin)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'));
            anyhow::ensure!(same, "the server pointed to another origin");
            Ok(path.to_owned())
        } else {
            Ok(format!("{}{path}", self.origin))
        }
    }

    /// Adds the authorization and the extra headers.
    fn authorized<B>(&self, mut request: ureq::RequestBuilder<B>) -> ureq::RequestBuilder<B> {
        for header in &self.headers {
            request = request.header(header.name.as_str(), header.value.as_str());
        }
        if let Some(authorization) = &self.authorization {
            request = request.header("Authorization", authorization.as_str());
        }
        request
    }

    /// Sends the request that `build` makes; one the server's rate limit
    /// refuses (429) is sent again after its `Retry-After`, up to
    /// [`RATE_LIMIT_RETRIES`] times. The limit answers before the handler
    /// runs, so the request did nothing and sending it again is safe.
    fn send(
        &self,
        build: impl Fn() -> Result<Response<ureq::Body>, ureq::Error>,
    ) -> anyhow::Result<Response<ureq::Body>> {
        let mut tries = 0;
        loop {
            let response = build().context("cannot reach the server")?;
            if response.status() != StatusCode::TOO_MANY_REQUESTS || tries >= RATE_LIMIT_RETRIES {
                return Ok(response);
            }
            tries += 1;
            let wait = response
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.trim().parse::<u64>().ok())
                .unwrap_or(1)
                .clamp(1, MAX_RETRY_AFTER);
            std::thread::sleep(Duration::from_secs(wait));
        }
    }

    /// What the web library looks like: empty or not, and its quota.
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn preflight(&self) -> anyhow::Result<Preflight> {
        let url = self.url("/api/v1/migrations/preflight")?;
        let response = self.send(|| self.authorized(self.agent.get(&url)).call())?;
        json(response)
    }

    /// The hashes of `objects` the server does not have yet.
    ///
    /// # Errors
    ///
    /// A request failed.
    pub fn missing_objects(&self, objects: &[ObjectRef]) -> anyhow::Result<Vec<String>> {
        let mut missing = Vec::new();
        for batch in objects.chunks(MISSING_BATCH) {
            let response = self.post_json(
                "/api/v1/migrations/missing-objects",
                &MissingObjectsRequest { objects: batch },
                None,
            )?;
            let answer: MissingObjectsResponse = json(response)?;
            missing.extend(answer.missing);
        }
        Ok(missing)
    }

    /// `POST`s `body` as compact JSON (ureq's `send_json` pretty-prints,
    /// which would put 500 objects over the server's 64 KiB JSON limit).
    fn post_json(
        &self,
        path: &str,
        body: &impl Serialize,
        idempotency_key: Option<&str>,
    ) -> anyhow::Result<Response<ureq::Body>> {
        let bytes = serde_json::to_vec(body)?;
        let url = self.url(path)?;
        self.send(|| {
            let mut request = self
                .authorized(self.agent.post(&url))
                .content_type("application/json");
            if let Some(key) = idempotency_key {
                request = request.header(IDEMPOTENCY_KEY, key);
            }
            request.send(&bytes[..])
        })
    }

    /// Creates an upload of `length` bytes; returns its URL.
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn create_upload(&self, length: u64, metadata: &[(&str, &str)]) -> anyhow::Result<String> {
        let metadata = metadata
            .iter()
            .map(|(key, value)| format!("{key} {}", STANDARD.encode(value)))
            .collect::<Vec<_>>()
            .join(",");
        let url = self.url("/api/v1/uploads")?;
        let response = self.send(|| {
            self.authorized(self.agent.post(&url))
                .header("Tus-Resumable", TUS_VERSION)
                .header("Upload-Length", length.to_string())
                .header("Upload-Metadata", metadata.as_str())
                .send_empty()
        })?;
        if response.status() != StatusCode::CREATED {
            return Err(problem(response).into());
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .context("the server created an upload without a Location")?;
        self.url(location)
    }

    /// Where the upload at `url` stands; `None` when the server no longer
    /// knows it (expired, or completed and consumed).
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn upload_state(&self, url: &str) -> anyhow::Result<Option<UploadState>> {
        let url = self.url(url)?;
        let response = self.send(|| {
            self.authorized(self.agent.head(&url))
                .header("Tus-Resumable", TUS_VERSION)
                .call()
        })?;
        match response.status() {
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            status if status.is_success() => Ok(Some(UploadState {
                offset: header_u64(&response, "upload-offset")?,
                length: header_u64(&response, "upload-length")?,
            })),
            status => Err(ApiError {
                status: status.as_u16(),
                code: if status.is_redirection() {
                    "redirect".to_owned()
                } else {
                    "unknown".to_owned()
                },
                detail: None,
                retry_after: None,
            }
            .into()),
        }
    }

    /// Appends `chunk` at `offset` to the upload at `url`; returns the new
    /// offset.
    ///
    /// # Errors
    ///
    /// The request failed or the server refused the chunk.
    pub fn append(&self, url: &str, offset: u64, chunk: &[u8]) -> anyhow::Result<u64> {
        let url = self.url(url)?;
        let response = self
            .send(|| {
                self.authorized(self.agent.patch(&url))
                    .header("Tus-Resumable", TUS_VERSION)
                    .header("Upload-Offset", offset.to_string())
                    .content_type(OFFSET_OCTET_STREAM)
                    .send(chunk)
            })
            .context("the upload was interrupted")?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(problem(response).into());
        }
        header_u64(&response, "upload-offset")
    }

    /// Drops the upload at `url` (tus termination). Returns whether the
    /// server had it.
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn delete_upload(&self, url: &str) -> anyhow::Result<bool> {
        let url = self.url(url)?;
        let response = self.send(|| {
            self.authorized(self.agent.delete(&url))
                .header("Tus-Resumable", TUS_VERSION)
                .call()
        })?;
        match response.status() {
            StatusCode::NO_CONTENT => Ok(true),
            StatusCode::NOT_FOUND => Ok(false),
            _ => Err(problem(response).into()),
        }
    }

    /// Starts installing the bundle whose database is the upload
    /// `db_upload_id`; `idempotency_key` makes a repeat return the same
    /// install.
    ///
    /// # Errors
    ///
    /// The request failed or the server refused the install.
    pub fn start_migration(
        &self,
        db_upload_id: &str,
        merge: bool,
        idempotency_key: &str,
    ) -> anyhow::Result<MigrationStatus> {
        let response = self.post_json(
            "/api/v1/migrations",
            &StartMigration {
                db_upload_id,
                merge,
            },
            Some(idempotency_key),
        )?;
        json(response)
    }

    /// The state of the install `id`.
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn migration(&self, id: &str) -> anyhow::Result<MigrationStatus> {
        let url = self.url(&format!("/api/v1/migrations/{id}"))?;
        let response = self.send(|| self.authorized(self.agent.get(&url)).call())?;
        json(response)
    }

    /// Starts a device sign-in (`login`).
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn device_start(&self) -> anyhow::Result<DeviceAuthorization> {
        let url = self.url("/api/v1/auth/device/start")?;
        let response = self.send(|| self.authorized(self.agent.post(&url)).send_empty())?;
        json(response)
    }

    /// Polls the device sign-in of `device_code`. A 429 is an answer here
    /// ([`DevicePoll::Limited`]): `login` waits and tells the user.
    ///
    /// # Errors
    ///
    /// The request failed, or the server answered an unexpected error.
    pub fn device_poll(&self, device_code: &str) -> anyhow::Result<DevicePoll> {
        let body = serde_json::to_vec(&PollRequest { device_code })?;
        let response = self
            .authorized(self.agent.post(self.url("/api/v1/auth/device/poll")?))
            .content_type("application/json")
            .send(&body[..])
            .context("cannot reach the server")?;
        if response.status().is_success() {
            return Ok(match json::<PollAnswer>(response)? {
                PollAnswer::Pending { interval } => DevicePoll::Pending { interval },
                PollAnswer::SlowDown { interval } => DevicePoll::SlowDown { interval },
                PollAnswer::Approved { token, expires_at } => {
                    DevicePoll::Approved { token, expires_at }
                }
            });
        }
        let err = problem(response);
        match (err.status, err.code.as_str()) {
            (429, _) => Ok(DevicePoll::Limited {
                retry_after: Duration::from_secs(err.retry_after.unwrap_or(5).max(1)),
            }),
            (400, "invalid_device_code") => Ok(DevicePoll::Invalid),
            _ => Err(err.into()),
        }
    }
}

/// The JSON body of a success, or the problem of an error.
fn json<T: DeserializeOwned>(mut response: Response<ureq::Body>) -> anyhow::Result<T> {
    if !response.status().is_success() {
        return Err(problem(response).into());
    }
    response
        .body_mut()
        .read_json()
        .context("the server sent an unexpected answer")
}

fn problem(mut response: Response<ureq::Body>) -> ApiError {
    let status = response.status();
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok());
    if status.is_redirection() {
        return ApiError {
            status: status.as_u16(),
            code: "redirect".to_owned(),
            detail: Some(
                "the server redirected the request: behind Cloudflare Access, pass the \
                 service token with --header"
                    .to_owned(),
            ),
            retry_after,
        };
    }
    let parsed: Option<Problem> = response.body_mut().read_json().ok();
    ApiError {
        status: status.as_u16(),
        code: parsed
            .as_ref()
            .map(|p| p.code.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        detail: parsed.and_then(|p| p.detail),
        retry_after,
    }
}

fn header_u64(response: &Response<ureq::Body>, name: &str) -> anyhow::Result<u64> {
    response
        .headers()
        .get(name)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .with_context(|| format!("the server answered without a valid {name}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::scripted;

    #[test]
    fn a_rate_limited_request_is_sent_again_after_its_retry_after() {
        let limited = r#"{"code":"rate_limited","status":429}"#.to_owned();
        let (origin, server) = scripted(vec![
            (429, "Retry-After: 1\r\n", limited.clone()),
            (429, "Retry-After: 1\r\n", limited),
            (
                200,
                "",
                r#"{"libraryEmpty":true,"posts":0,"quotaBytes":0,"usedBytes":4096,
                   "maxObjectBytes":1,"maxDatabaseBytes":1}"#
                    .to_owned(),
            ),
        ]);
        let started = std::time::Instant::now();
        let headers = [Header::parse("CF-Access-Client-Id: id").unwrap()];
        let client = Client::with_headers(&origin, Some("shx_token"), &headers).unwrap();
        let preflight = client.preflight().unwrap();
        assert!(preflight.library_empty);
        assert_eq!(preflight.used_bytes, 4096);
        assert!(started.elapsed() >= Duration::from_secs(2), "it waited");
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 3);
        for request in &seen {
            assert_eq!(request.line, "GET /api/v1/migrations/preflight HTTP/1.1");
            assert!(request.has("authorization") && request.has("cf-access-client-id"));
        }
    }

    #[test]
    fn urls_stay_on_the_server() {
        let client = Client::new("https://refs.example.test/", "shx_token").unwrap();
        assert_eq!(
            client.url("/api/v1/uploads/U1").unwrap(),
            "https://refs.example.test/api/v1/uploads/U1"
        );
        assert_eq!(
            client
                .url("https://refs.example.test/api/v1/uploads/U1")
                .unwrap(),
            "https://refs.example.test/api/v1/uploads/U1"
        );
        for foreign in [
            "http://other.test/api/v1/uploads/U1",
            "https://refs.example.test.evil/x",
        ] {
            assert!(client.url(foreign).is_err(), "{foreign}");
        }
        assert!(Client::new("refs.example.test", "shx_token").is_err());
    }

    #[test]
    fn a_full_batch_of_objects_fits_the_servers_json_limit() {
        let objects = vec![
            ObjectRef {
                sha256: "f".repeat(64),
                ext: "webp".into(),
                bytes: 314_572_800,
            };
            MISSING_BATCH
        ];
        let body = serde_json::to_vec(&MissingObjectsRequest { objects: &objects }).unwrap();
        assert!(body.len() < 64 * 1024, "{} bytes", body.len());
    }

    #[test]
    fn errors_show_the_code_and_the_detail() {
        let err = ApiError {
            status: 409,
            code: "conflict".into(),
            detail: Some("the web library is not empty".into()),
            retry_after: None,
        };
        assert_eq!(
            err.to_string(),
            "the server answered 409 conflict: the web library is not empty"
        );
    }

    #[test]
    fn headers_parse_and_never_show_their_value() {
        let header = Header::parse("CF-Access-Client-Id: abc.access").unwrap();
        assert_eq!(header.name, "CF-Access-Client-Id");
        assert_eq!(header.value, "abc.access");
        assert_eq!(format!("{header:?}"), "CF-Access-Client-Id: (hidden)");
        assert_eq!(Header::parse("X-A:b").unwrap().value, "b");
        for bad in [
            "no colon",
            ": value",
            "Bad Name: v",
            "Authorization: Bearer x",
        ] {
            assert!(Header::parse(bad).is_err(), "{bad}");
        }
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("access.headers");
        std::fs::write(
            &file,
            "# Access service token\nCF-Access-Client-Id: id\n\nCF-Access-Client-Secret: s\n",
        )
        .unwrap();
        let all =
            Header::parse_all(&[format!("@{}", file.display()), "X-Extra: 1".to_owned()]).unwrap();
        let names: Vec<&str> = all.iter().map(|h| h.name.as_str()).collect();
        assert_eq!(
            names,
            ["CF-Access-Client-Id", "CF-Access-Client-Secret", "X-Extra"]
        );
    }
}
