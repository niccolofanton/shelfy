//! `GET /media/{file}`: a user's stored objects and renditions (plan D5,
//! §2.5, §2.9), outside `/api` and outside the OpenAPI document.
//!
//! `{file}` is `<sha256>.<ext>` (a stored object) or `<sha256>.g480.webp` (its
//! grid rendition); any other spelling is 404. The file is read from the store
//! of the authenticated user only: no user id appears in the URL, so a user
//! can never address another user's objects (§7.1), even with the same digest.
//!
//! Every success carries:
//!
//! - `Cache-Control: private, max-age=31536000, immutable`: a name always
//!   designates the same bytes;
//! - a strong `ETag`: the digest for a stored object, the digest of the
//!   rendition's own bytes for a rendition (it can be re-rendered);
//! - `Content-Security-Policy: sandbox` (the security headers layer puts the
//!   app's policy before it, [`crate::security_headers`]),
//!   `X-Content-Type-Options: nosniff` and `Cross-Origin-Resource-Policy:
//!   same-origin`; `Content-Disposition: attachment` for anything that is not
//!   an image or a video (§7.1);
//! - `Accept-Ranges: bytes`. A single byte range is answered with 206 (videos
//!   seek with it); `If-None-Match` gives 304 ([`preconditions`]).
//!
//! Errors are problems with `Cache-Control: no-store`, so a rendition that does
//! not exist yet is fetched again later. A 416 keeps its `Content-Range`.
//!
//! # Authentication
//!
//! The handler takes a [`CurrentUser`]. The authentication layer
//! ([`crate::auth::session`]) inserts one for a valid session cookie and never
//! for an API token, so media stays cookie-only (D5, §2.9). Without one the
//! request answers 401 before any file is touched.

pub mod preconditions;

use std::io::{self, Read as _, SeekFrom};

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use shelfy_media::name::Variant;
use shelfy_media::store::MediaStore;
use shelfy_media::{Digest, ObjectName};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _};
use tokio_util::io::ReaderStream;
use utoipa_axum::router::OpenApiRouter;

use crate::current_user::CurrentUser;
use crate::error::ApiError;
use crate::extract::Path;
use crate::state::{AppState, blocking};
use preconditions::Outcome;

/// `Cache-Control` of every media success: names are content addresses.
pub const IMMUTABLE: &str = "private, max-age=31536000, immutable";

/// Largest rendition file served; renditions are read whole to hash them.
const MAX_RENDITION_BYTES: u64 = 8 * 1024 * 1024;
/// Read size of streamed objects.
const STREAM_CHUNK: usize = 64 * 1024;

/// The media routes. They take no body: the standard limits apply, and a
/// streamed video is not cut by the handler time limit, which ends when the
/// headers are sent.
pub fn router() -> OpenApiRouter<AppState> {
    OpenApiRouter::new().route("/media/{file}", get(media))
}

/// `GET /media/{file}` (and `HEAD`, which axum routes here without the body).
async fn media(
    user: CurrentUser,
    State(state): State<AppState>,
    Path(file): Path<String>,
    method: Method,
    headers: HeaderMap,
) -> Result<Response, ApiError> {
    let store = MediaStore::new(state.config().data_dir.users_dir());
    serve(&store, user.id(), &file, &method, &headers).await
}

/// Answers a media request of `user_id` for `file`: the work of the route
/// after authentication, callable by in-process benchmarks (`admin bench`,
/// P1-05).
///
/// # Errors
///
/// 404 for a name outside the allowlists or a missing file; 500 when the file
/// cannot be read or `user_id` cannot name a directory (an authentication bug).
pub async fn serve(
    store: &MediaStore,
    user_id: &str,
    file: &str,
    method: &Method,
    headers: &HeaderMap,
) -> Result<Response, ApiError> {
    let name = ObjectName::parse(file).ok_or_else(ApiError::not_found)?;
    let media = store.user(user_id).map_err(ApiError::internal)?;
    let path = media.path(&name);
    let found = blocking(move || open(&path, &name).map_err(ApiError::internal))
        .await?
        .ok_or_else(ApiError::not_found)?;
    let (size, etag) = (found.size, found.etag.clone());
    let mut response = match preconditions::evaluate(method, headers, &etag, size) {
        Outcome::NotModified => {
            let mut response = StatusCode::NOT_MODIFIED.into_response();
            add_media_headers(response.headers_mut(), &name, &etag);
            return Ok(response);
        }
        Outcome::PreconditionFailed => return Ok(StatusCode::PRECONDITION_FAILED.into_response()),
        Outcome::RangeNotSatisfiable => {
            // A bare error: the problem fallback adds the body and keeps the header.
            let unsatisfied = format!("bytes */{size}");
            return Ok((
                StatusCode::RANGE_NOT_SATISFIABLE,
                [(header::CONTENT_RANGE, unsatisfied)],
            )
                .into_response());
        }
        Outcome::Full => content(found, 0, size, method).await?,
        Outcome::Partial { start, end } => {
            let mut response = content(found, start, end - start + 1, method).await?;
            *response.status_mut() = StatusCode::PARTIAL_CONTENT;
            let range = format!("bytes {start}-{end}/{size}");
            response.headers_mut().insert(
                header::CONTENT_RANGE,
                HeaderValue::from_str(&range).expect("a range is a valid header value"),
            );
            response
        }
    };
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(name.kind().mime()),
    );
    add_media_headers(headers, &name, &etag);
    Ok(response)
}

/// An opened media file.
struct Found {
    size: u64,
    /// The strong entity tag, quotes included.
    etag: String,
    content: Content,
}

enum Content {
    /// A rendition, read whole: it is small, and its tag is the digest of
    /// its bytes because it may be rendered again.
    Bytes(Bytes),
    /// A stored object, streamed. Its name is its digest, hence its tag.
    File(std::fs::File),
}

/// Opens the file of `name` at `path`; `None` when it does not exist.
/// Blocking.
fn open(path: &std::path::Path, name: &ObjectName) -> io::Result<Option<Found>> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e),
    };
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Ok(None);
    }
    match name.variant {
        Variant::Original(_) => Ok(Some(Found {
            size: meta.len(),
            etag: format!("\"{}\"", name.digest),
            content: Content::File(file),
        })),
        Variant::Rendition(_) => {
            if meta.len() > MAX_RENDITION_BYTES {
                return Err(io::Error::other("rendition file over its size bound"));
            }
            let mut bytes = Vec::with_capacity(usize::try_from(meta.len()).unwrap_or(0));
            file.read_to_end(&mut bytes)?;
            Ok(Some(Found {
                size: bytes.len() as u64,
                etag: format!("\"{}\"", Digest::of(&bytes)),
                content: Content::Bytes(bytes.into()),
            }))
        }
    }
}

/// A 200 with `len` bytes from `start`; the body is left out for `HEAD`.
async fn content(
    found: Found,
    start: u64,
    len: u64,
    method: &Method,
) -> Result<Response, ApiError> {
    let body = if method == Method::HEAD {
        Body::empty()
    } else {
        match found.content {
            Content::Bytes(bytes) => {
                let from = usize::try_from(start).map_err(ApiError::internal)?;
                let to = usize::try_from(start + len).map_err(ApiError::internal)?;
                Body::from(bytes.slice(from..to))
            }
            Content::File(file) => {
                let mut file = tokio::fs::File::from_std(file);
                if start > 0 {
                    file.seek(SeekFrom::Start(start))
                        .await
                        .map_err(ApiError::internal)?;
                }
                Body::from_stream(ReaderStream::with_capacity(file.take(len), STREAM_CHUNK))
            }
        }
    };
    let mut response = Response::new(body);
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    Ok(response)
}

/// The headers of every media success (200, 206, 304), except the
/// `Content-*` fields of a body.
fn add_media_headers(headers: &mut HeaderMap, name: &ObjectName, etag: &str) {
    let kind = name.kind();
    headers.insert(
        header::ETAG,
        HeaderValue::from_str(etag).expect("an entity tag is a valid header value"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(IMMUTABLE));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("sandbox"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );
    if !kind.is_image() && !kind.is_video() {
        headers.insert(
            header::CONTENT_DISPOSITION,
            HeaderValue::from_static("attachment"),
        );
    }
}
