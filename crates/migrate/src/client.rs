//! The server's migration API, as `run` calls it (plan §2.9, §4.1).
//!
//! Every request carries the `migrate` token as `Authorization: Bearer`; the
//! token is never printed. Uploads follow tus 1.0 (core and creation):
//! `POST /api/v1/uploads` creates one, `PATCH` appends at an offset, and
//! `HEAD` tells where an interrupted upload stopped, so a re-run continues
//! it. Errors are the server's problem documents, reduced to their `code` and
//! developer `detail`.

use std::time::Duration;

use anyhow::Context as _;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use ureq::Agent;
use ureq::http::{Response, StatusCode};

/// The tus protocol version.
pub const TUS_VERSION: &str = "1.0.0";
/// Content type of a tus `PATCH` body.
pub const OFFSET_OCTET_STREAM: &str = "application/offset+octet-stream";
/// Objects asked about per `POST /migrations/missing-objects`.
pub const MISSING_BATCH: usize = 500;

/// Time limit of one request; uploads send at most one chunk per request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

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

/// An install, as `GET /migrations/{id}` describes it.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MigrationStatus {
    pub id: String,
    /// `running`, `succeeded` or `failed`.
    pub state: String,
    /// What the install is doing.
    pub stage: String,
    /// 0 to 1, within the stage.
    pub progress: f64,
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
    /// The bundle's own summary, as uploaded.
    pub bundle: serde_json::Value,
    pub installed: InstalledCounts,
    pub objects: InstalledObjects,
    pub renditions: Renditions,
    pub archive: Archive,
    /// How long the install took, ms.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct InstalledCounts {
    pub posts: std::collections::BTreeMap<String, u64>,
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
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Archive {
    pub by_state: std::collections::BTreeMap<String, u64>,
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
}

/// A client of one server, authenticated by a `migrate` token.
pub struct Client {
    agent: Agent,
    origin: String,
    authorization: String,
}

impl Client {
    /// A client of the server at `server` (its public origin, for example
    /// `https://refs.example.com`) with `token` (`shx_…`).
    ///
    /// # Errors
    ///
    /// `server` is not an http(s) URL.
    pub fn new(server: &str, token: &str) -> anyhow::Result<Client> {
        let origin = server.trim().trim_end_matches('/').to_owned();
        anyhow::ensure!(
            origin.starts_with("https://") || origin.starts_with("http://"),
            "the server URL must start with https:// or http://"
        );
        let agent: Agent = Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(REQUEST_TIMEOUT))
            .user_agent(format!("shelfy-migrate/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .into();
        Ok(Client {
            agent,
            origin,
            authorization: format!("Bearer {}", token.trim()),
        })
    }

    /// An absolute URL for a server path or a `Location`.
    #[must_use]
    pub fn url(&self, path: &str) -> String {
        if path.starts_with("http://") || path.starts_with("https://") {
            path.to_owned()
        } else {
            format!("{}{path}", self.origin)
        }
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
            )?;
            let answer: MissingObjectsResponse = json(response)?;
            missing.extend(answer.missing);
        }
        Ok(missing)
    }

    /// `POST`s `body` as compact JSON (ureq's `send_json` pretty-prints,
    /// which would put 500 objects over the server's 64 KiB JSON limit).
    fn post_json(&self, path: &str, body: &impl Serialize) -> anyhow::Result<Response<ureq::Body>> {
        let bytes = serde_json::to_vec(body)?;
        self.agent
            .post(self.url(path))
            .header("Authorization", &self.authorization)
            .content_type("application/json")
            .send(&bytes[..])
            .context("cannot reach the server")
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
        let response = self
            .agent
            .post(self.url("/api/v1/uploads"))
            .header("Authorization", &self.authorization)
            .header("Tus-Resumable", TUS_VERSION)
            .header("Upload-Length", length.to_string())
            .header("Upload-Metadata", metadata)
            .send_empty()
            .context("cannot reach the server")?;
        if response.status() != StatusCode::CREATED {
            return Err(problem(response).into());
        }
        let location = response
            .headers()
            .get("location")
            .and_then(|v| v.to_str().ok())
            .context("the server created an upload without a Location")?;
        Ok(self.url(location))
    }

    /// Where the upload at `url` stands; `None` when the server no longer
    /// knows it (expired, or completed and consumed).
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn upload_state(&self, url: &str) -> anyhow::Result<Option<UploadState>> {
        let response = self
            .agent
            .head(url)
            .header("Authorization", &self.authorization)
            .header("Tus-Resumable", TUS_VERSION)
            .call()
            .context("cannot reach the server")?;
        match response.status() {
            StatusCode::NOT_FOUND | StatusCode::GONE => Ok(None),
            status if status.is_success() => Ok(Some(UploadState {
                offset: header_u64(&response, "upload-offset")?,
                length: header_u64(&response, "upload-length")?,
            })),
            status => Err(ApiError {
                status: status.as_u16(),
                code: "unknown".to_owned(),
                detail: None,
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
        let response = self
            .agent
            .patch(url)
            .header("Authorization", &self.authorization)
            .header("Tus-Resumable", TUS_VERSION)
            .header("Upload-Offset", offset.to_string())
            .content_type(OFFSET_OCTET_STREAM)
            .send(chunk)
            .context("the upload was interrupted")?;
        if response.status() != StatusCode::NO_CONTENT {
            return Err(problem(response).into());
        }
        header_u64(&response, "upload-offset")
    }

    /// Starts installing the bundle whose database is the upload
    /// `db_upload_id`.
    ///
    /// # Errors
    ///
    /// The request failed or the server refused the install.
    pub fn start_migration(
        &self,
        db_upload_id: &str,
        merge: bool,
    ) -> anyhow::Result<MigrationStatus> {
        let response = self.post_json(
            "/api/v1/migrations",
            &StartMigration {
                db_upload_id,
                merge,
            },
        )?;
        json(response)
    }

    /// The state of the install `id`.
    ///
    /// # Errors
    ///
    /// The request failed.
    pub fn migration(&self, id: &str) -> anyhow::Result<MigrationStatus> {
        let response = self
            .agent
            .get(self.url(&format!("/api/v1/migrations/{id}")))
            .header("Authorization", &self.authorization)
            .call()
            .context("cannot reach the server")?;
        json(response)
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
    let status = response.status().as_u16();
    let parsed: Option<Problem> = response.body_mut().read_json().ok();
    ApiError {
        status,
        code: parsed
            .as_ref()
            .map(|p| p.code.clone())
            .filter(|c| !c.is_empty())
            .unwrap_or_else(|| "unknown".to_owned()),
        detail: parsed.and_then(|p| p.detail),
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

    #[test]
    fn urls_are_resolved_against_the_origin() {
        let client = Client::new("https://refs.example.test/", "shx_token").unwrap();
        assert_eq!(
            client.url("/api/v1/uploads/U1"),
            "https://refs.example.test/api/v1/uploads/U1"
        );
        assert_eq!(
            client.url("http://other.test/api/v1/uploads/U1"),
            "http://other.test/api/v1/uploads/U1"
        );
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
        };
        assert_eq!(
            err.to_string(),
            "the server answered 409 conflict: the web library is not empty"
        );
    }
}
