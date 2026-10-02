//! The web app's files (plan §2.2 "SPA hosting", §3.2; P1-09).
//!
//! `shelfy-server serve` serves the built web app (`web/dist`, `/app/web` in
//! the image) from `SHELFY_WEB_DIR`. It answers through the router's fallback,
//! that is for requests no route matched, so the files stay public: the
//! access gate runs for routes only ([`crate::auth::access`]).
//!
//! | `GET` or `HEAD` of | Answer | `Cache-Control` |
//! |---|---|---|
//! | `/assets/<file>` | the file, or its `.br` or `.gz` sibling when the client accepts that encoding | [`IMMUTABLE`]: Vite puts a content hash in every asset name |
//! | `/assets/<missing>` | 404 problem, never `index.html`: a stale chunk must fail, not parse HTML | `no-store` |
//! | another file of the directory | the file, precompressed likewise | [`NO_CACHE`] |
//! | a path under [`SERVER_PREFIXES`] that no route matched | 404 problem | `no-store` |
//! | anything else: `/`, `/login/magic`, `/p/<key>`… | `index.html` with a strong `ETag`; the app routes on the client | [`NO_CACHE`] |
//!
//! Other methods answer 404, as before the web app existed. `index.html` is
//! read once, at start; the other files are read per request. The image never
//! changes them under a running server.
//!
//! The `.br` and `.gz` siblings are written when the image is built
//! (`deploy/docker/shelfy-api.Dockerfile`). Without them the files are sent
//! as they are, and the compression layer ([`crate::app`]) compresses them on
//! the fly.

use std::convert::Infallible;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use axum::body::{Body, Bytes};
use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse as _, Response};
use shelfy_media::Digest;
use tower::ServiceExt as _;
use tower::service_fn;
use tower::util::BoxCloneSyncService;
use tower_http::services::ServeDir;

use crate::error::ApiError;

/// `Cache-Control` of the hashed assets under `/assets/`.
pub const IMMUTABLE: &str = "public, max-age=31536000, immutable";

/// `Cache-Control` of `index.html` and the other unhashed files: the browser
/// revalidates before each use, so a deploy reaches it on the next load.
pub const NO_CACHE: &str = "no-cache";

/// The file every client route answers with.
pub const INDEX_HTML: &str = "index.html";

/// Where Vite writes the hashed assets, as a URL path prefix.
const ASSETS_PREFIX: &str = "/assets";

/// Path prefixes that belong to the server. A request under one of them that
/// no route matched is a 404, never the web app: a mistyped API call or probe
/// must not get an HTML page with a 200.
pub const SERVER_PREFIXES: &[&str] = &["/api", "/media", "/health", "/.well-known"];

/// The built web app: its directory and its `index.html`.
#[derive(Clone)]
pub struct WebApp {
    root: PathBuf,
    index: Index,
}

impl WebApp {
    /// Opens the built web app in `root`, which must hold `index.html`.
    ///
    /// # Errors
    ///
    /// `root` is not a directory, or its `index.html` cannot be read.
    pub fn load(root: impl Into<PathBuf>) -> io::Result<Self> {
        let root = std::path::absolute(root.into())?;
        if !root.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{} is not a directory", root.display()),
            ));
        }
        let index_path = root.join(INDEX_HTML);
        let index = Index::load(&index_path).map_err(|err| {
            io::Error::new(
                err.kind(),
                format!("cannot read {}: {err}", index_path.display()),
            )
        })?;
        Ok(Self { root, index })
    }

    /// The directory the files come from.
    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The router fallback that serves the app: see the module documentation.
    #[must_use]
    pub fn service(&self) -> BoxCloneSyncService<Request, Response, Infallible> {
        let assets = serve_dir(&self.root);
        let page = self.index.clone();
        let files = serve_dir(&self.root).fallback(service_fn(move |request: Request| {
            let page = page.clone();
            async move { Ok::<_, Infallible>(page.respond(request.method(), request.headers())) }
        }));
        let index = self.index.clone();
        BoxCloneSyncService::new(service_fn(move |request: Request| {
            let (assets, files, index) = (assets.clone(), files.clone(), index.clone());
            async move {
                let response = match Target::of(request.method(), request.uri().path()) {
                    Target::NotFound => ApiError::not_found().into_response(),
                    Target::Index => index.respond(request.method(), request.headers()),
                    Target::Asset => {
                        let Ok(response) = assets.oneshot(request).await;
                        with_cache_control(response.map(Body::new), IMMUTABLE)
                    }
                    Target::FileOrIndex => {
                        let Ok(response) = files.oneshot(request).await;
                        with_cache_control(response.map(Body::new), NO_CACHE)
                    }
                };
                Ok(response)
            }
        }))
    }
}

impl fmt::Debug for WebApp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WebApp")
            .field("root", &self.root)
            .field("index_etag", &self.index.etag)
            .finish_non_exhaustive()
    }
}

/// The files of `root`, with their `.br` and `.gz` siblings. A directory is
/// not a file: it falls through like a missing one.
fn serve_dir(root: &Path) -> ServeDir {
    ServeDir::new(root)
        .precompressed_br()
        .precompressed_gzip()
        .append_index_html_on_directories(false)
}

/// Whether the web app's files answer a request with `method` and `path`
/// that no route matched: a page, an asset or another file of the directory,
/// as opposed to the 404 problem of other methods and server paths. The
/// request metrics label these `spa` ([`crate::telemetry::http::SPA_ROUTE`]).
#[must_use]
pub fn serves(method: &Method, path: &str) -> bool {
    Target::of(method, path) != Target::NotFound
}

/// What a request that no route matched gets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// A 404 problem: another method, or a server path.
    NotFound,
    /// A hashed asset, or a 404 problem.
    Asset,
    /// `index.html`.
    Index,
    /// A file of the directory, or `index.html` when there is none.
    FileOrIndex,
}

impl Target {
    fn of(method: &Method, path: &str) -> Self {
        if !matches!(*method, Method::GET | Method::HEAD)
            || SERVER_PREFIXES.iter().any(|prefix| under(path, prefix))
        {
            Self::NotFound
        } else if under(path, ASSETS_PREFIX) {
            Self::Asset
        } else if path == "/" || path == "/index.html" {
            Self::Index
        } else {
            Self::FileOrIndex
        }
    }
}

/// Whether `path` is `prefix` or lies under it.
fn under(path: &str, prefix: &str) -> bool {
    path.strip_prefix(prefix)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

/// Sets `Cache-Control` on a file that was served (200, 206, 304). Errors
/// keep theirs: the problem fallback makes them `no-store` problems.
fn with_cache_control(mut response: Response, value: &'static str) -> Response {
    let status = response.status();
    if status.is_success() || status == StatusCode::NOT_MODIFIED {
        response
            .headers_mut()
            .entry(header::CACHE_CONTROL)
            .or_insert(HeaderValue::from_static(value));
    }
    response
}

/// `index.html`, in memory, with its entity tag: the digest of its bytes, so
/// any change to the file, even one that keeps its size and date, makes
/// browsers fetch it again.
#[derive(Clone)]
struct Index {
    body: Bytes,
    etag: HeaderValue,
}

impl Index {
    fn load(path: &Path) -> io::Result<Self> {
        let body = Bytes::from(std::fs::read(path)?);
        let etag = HeaderValue::from_str(&format!("\"{}\"", Digest::of(&body)))
            .expect("a quoted hex digest is a valid header value");
        Ok(Self { body, etag })
    }

    /// `index.html` for a `GET` or `HEAD` with these headers: 304 when
    /// `If-None-Match` names the current tag, otherwise the page.
    fn respond(&self, method: &Method, headers: &HeaderMap) -> Response {
        let etag = self.etag.to_str().expect("the tag is ASCII");
        let unchanged = headers
            .get_all(header::IF_NONE_MATCH)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .any(|value| none_match_fails(value, etag));
        let mut response = if unchanged {
            StatusCode::NOT_MODIFIED.into_response()
        } else {
            let body = if method == Method::HEAD {
                Body::empty()
            } else {
                Body::from(self.body.clone())
            };
            let mut response = Response::new(body);
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/html; charset=utf-8"),
            );
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(self.body.len()));
            response
        };
        let headers = response.headers_mut();
        headers.insert(header::ETAG, self.etag.clone());
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(NO_CACHE));
        response
    }
}

/// Whether an `If-None-Match` value matches `etag`, so the condition fails
/// and the answer is 304: `*`, or one of the listed tags under the weak
/// comparison of RFC 9110 §8.8.3.2 (a `W/` prefix is ignored, as a proxy
/// that compresses on the way weakens the tag).
fn none_match_fails(value: &str, etag: &str) -> bool {
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*" || candidate.strip_prefix("W/").unwrap_or(candidate) == etag
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_go_to_the_asset_the_page_or_a_404() {
        let get = &Method::GET;
        assert_eq!(Target::of(get, "/"), Target::Index);
        assert_eq!(Target::of(&Method::HEAD, "/index.html"), Target::Index);
        assert_eq!(Target::of(get, "/assets/index-abc.js"), Target::Asset);
        assert_eq!(Target::of(get, "/assets"), Target::Asset);
        assert_eq!(Target::of(get, "/login/magic"), Target::FileOrIndex);
        assert_eq!(Target::of(get, "/p/web_example.com"), Target::FileOrIndex);
        assert_eq!(Target::of(get, "/assetsx"), Target::FileOrIndex);
        for path in [
            "/api",
            "/api/v1/nope",
            "/media/x/y",
            "/health/capture",
            "/.well-known/x",
        ] {
            assert_eq!(Target::of(get, path), Target::NotFound, "{path}");
        }
        assert_eq!(Target::of(&Method::POST, "/"), Target::NotFound);
        assert_eq!(Target::of(&Method::OPTIONS, "/login"), Target::NotFound);
    }

    #[test]
    fn the_app_serves_its_pages_and_files_only() {
        assert!(serves(&Method::GET, "/"));
        assert!(serves(&Method::HEAD, "/p/web_example.com"));
        assert!(serves(&Method::GET, "/assets/index-abc.js"));
        assert!(serves(&Method::GET, "/favicon.svg"));
        assert!(!serves(&Method::GET, "/api/v1/nope"));
        assert!(!serves(&Method::GET, "/media/x"));
        assert!(!serves(&Method::POST, "/"));
    }

    #[test]
    fn prefixes_match_whole_segments() {
        assert!(under("/assets", "/assets"));
        assert!(under("/assets/index-abc.js", "/assets"));
        assert!(!under("/assetsx", "/assets"));
        assert!(!under("/p/assets", "/assets"));
        assert!(under("/api/v1/nope", "/api"));
        assert!(!under("/apiary", "/api"));
    }

    #[test]
    fn if_none_match_uses_the_weak_comparison() {
        let etag = "\"abc\"";
        assert!(none_match_fails("\"abc\"", etag));
        assert!(none_match_fails("W/\"abc\"", etag));
        assert!(none_match_fails("\"x\", W/\"abc\"", etag));
        assert!(none_match_fails("*", etag));
        assert!(!none_match_fails("\"abd\"", etag));
        assert!(!none_match_fails("abc", etag));
        assert!(!none_match_fails("", etag));
    }

    #[test]
    fn loading_needs_an_index() {
        let dir = tempfile::tempdir().unwrap();
        let err = WebApp::load(dir.path()).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
        assert!(err.to_string().contains(INDEX_HTML), "{err}");
        let err = WebApp::load(dir.path().join("missing")).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");

        std::fs::write(dir.path().join(INDEX_HTML), "<!doctype html>").unwrap();
        let web = WebApp::load(dir.path()).unwrap();
        assert!(web.root().is_absolute());
        assert_eq!(
            web.index.etag.to_str().unwrap(),
            format!("\"{}\"", Digest::of(b"<!doctype html>"))
        );
    }
}
