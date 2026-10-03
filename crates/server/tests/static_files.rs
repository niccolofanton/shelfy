//! The web app's files through the real stack (P1-09): `index.html` for
//! client routes, hashed assets with their precompressed siblings, the cache
//! headers, and the server paths that never fall back to the page.

mod support;

use axum::Router;
use axum::body::Body;
use axum::http::{HeaderValue, Method, Request, StatusCode, header};
use shelfy_media::Digest;
use shelfy_server::static_files::{IMMUTABLE, NO_CACHE, WebApp};
use support::{TestState, body, from_app, get, json, problem, send};
use tempfile::TempDir;

const INDEX: &str = "<!doctype html><title>Shelfy</title><div id=\"root\"></div>";
const SCRIPT: &str = "export const answer = 42;";
/// Stand-ins for the compressed siblings: the server picks the file by the
/// client's `Accept-Encoding` and never decodes it.
const SCRIPT_BR: &[u8] = b"brotli bytes of the script";
const SCRIPT_GZ: &[u8] = b"gzip bytes of the script";
const STYLE: &str = "body{margin:0}";
const MANIFEST: &str = r#"{"name":"Shelfy"}"#;
const SECRET: &str = "outside the web app";

/// A built web app, as `pnpm run web:build` and the image's compression step
/// leave it, plus a file next to it that must never be served.
struct Site {
    dir: TempDir,
}

impl Site {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let web = dir.path().join("dist");
        let assets = web.join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(web.join("index.html"), INDEX).unwrap();
        std::fs::write(web.join("manifest.webmanifest"), MANIFEST).unwrap();
        std::fs::write(assets.join("index-Abc123.js"), SCRIPT).unwrap();
        std::fs::write(assets.join("index-Abc123.js.br"), SCRIPT_BR).unwrap();
        std::fs::write(assets.join("index-Abc123.js.gz"), SCRIPT_GZ).unwrap();
        std::fs::write(assets.join("index-Def456.css"), STYLE).unwrap();
        std::fs::write(dir.path().join("secret.txt"), SECRET).unwrap();
        Self { dir }
    }

    fn web_dir(&self) -> std::path::PathBuf {
        self.dir.path().join("dist")
    }

    /// A server that serves this web app.
    fn serve(&self) -> TestState {
        let web = WebApp::load(self.web_dir()).unwrap();
        TestState::with_config(|config| config.web = Some(web))
    }
}

fn index_etag() -> String {
    format!("\"{}\"", Digest::of(INDEX.as_bytes()))
}

fn with_header(mut request: Request<Body>, name: header::HeaderName, value: &str) -> Request<Body> {
    request
        .headers_mut()
        .insert(name, HeaderValue::from_str(value).unwrap());
    request
}

fn head(uri: &str) -> Request<Body> {
    Request::head(uri).body(Body::empty()).unwrap()
}

async fn assert_index(app: &Router, uri: &str) {
    let response = send(app, get(uri)).await;
    assert_eq!(response.status(), StatusCode::OK, "{uri}");
    let headers = response.headers();
    assert_eq!(headers[header::CONTENT_TYPE], "text/html; charset=utf-8");
    assert_eq!(headers[header::CACHE_CONTROL], NO_CACHE, "{uri}");
    assert_eq!(headers[header::ETAG], index_etag().as_str());
    assert_eq!(body(response).await, INDEX, "{uri}");
}

#[tokio::test]
async fn client_routes_get_the_page() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    for uri in [
        "/",
        "/index.html",
        "/login",
        "/login/magic",
        "/p/ig_3141592653",
        "/p/web_example.com%2Fpricing",
        "/c/01J9ZQ3K8M2N4P6R8T0V2X4Z6B",
        "/settings/account?tab=sessions",
        "/deep/client/route/",
    ] {
        assert_index(&app, uri).await;
    }

    // HEAD: the same headers, no body.
    let response = send(&app, head("/login/magic")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_LENGTH],
        INDEX.len().to_string().as_str()
    );
    assert_eq!(response.headers()[header::ETAG], index_etag().as_str());
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn the_page_revalidates_with_its_etag() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    let etag = index_etag();
    for tag in [
        etag.clone(),
        format!("W/{etag}"),
        format!("\"other\", {etag}"),
        "*".to_owned(),
    ] {
        let request = with_header(get("/login"), header::IF_NONE_MATCH, &tag);
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{tag}");
        assert_eq!(response.headers()[header::ETAG], etag.as_str());
        assert_eq!(response.headers()[header::CACHE_CONTROL], NO_CACHE);
        assert!(body(response).await.is_empty());
    }
    let request = with_header(get("/"), header::IF_NONE_MATCH, "\"stale\"");
    assert_eq!(send(&app, request).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn assets_are_immutable_and_precompressed() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    let script = "/assets/index-Abc123.js";

    let cases: [(Option<&str>, Option<&str>, &[u8]); 5] = [
        (None, None, SCRIPT.as_bytes()),
        (Some("gzip, deflate, br, zstd"), Some("br"), SCRIPT_BR),
        (Some("br;q=1.0, gzip;q=0.8"), Some("br"), SCRIPT_BR),
        (Some("gzip"), Some("gzip"), SCRIPT_GZ),
        (Some("identity"), None, SCRIPT.as_bytes()),
    ];
    for (accept, encoding, expected) in cases {
        let mut request = get(script);
        if let Some(accept) = accept {
            request = with_header(request, header::ACCEPT_ENCODING, accept);
        }
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::OK, "{accept:?}");
        let headers = response.headers();
        assert_eq!(headers[header::CACHE_CONTROL], IMMUTABLE);
        assert_eq!(IMMUTABLE, "public, max-age=31536000, immutable");
        assert!(
            headers[header::CONTENT_TYPE]
                .to_str()
                .unwrap()
                .contains("javascript"),
            "a module script needs a JavaScript type under nosniff"
        );
        assert_eq!(headers[header::VARY], "accept-encoding");
        assert_eq!(
            headers
                .get(header::CONTENT_ENCODING)
                .map(|v| v.to_str().unwrap()),
            encoding,
            "{accept:?}"
        );
        assert_eq!(body(response).await, expected, "{accept:?}");
    }

    // A file without siblings goes out as it is, still immutable.
    let request = with_header(
        get("/assets/index-Def456.css"),
        header::ACCEPT_ENCODING,
        "br",
    );
    let response = send(&app, request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], IMMUTABLE);
    assert!(response.headers().get(header::CONTENT_ENCODING).is_none());
    assert_eq!(body(response).await, STYLE);

    let response = send(&app, head(script)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], IMMUTABLE);
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn a_missing_asset_is_a_404_and_never_the_page() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    for uri in [
        "/assets/index-Gone99.js",
        "/assets/",
        "/assets",
        "/assets/nested/missing.css",
    ] {
        problem(send(&app, get(uri)).await, StatusCode::NOT_FOUND).await;
    }
}

#[tokio::test]
async fn server_paths_never_get_the_page() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    for uri in [
        "/api",
        "/api/v1/no-such-route",
        "/media/abc/def",
        "/health/no-such-service",
        "/.well-known/security.txt",
    ] {
        problem(send(&app, get(uri)).await, StatusCode::NOT_FOUND).await;
    }

    // The routes themselves are untouched.
    let response = send(&app, get("/health")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json(response).await["status"], "ok");
    // Capture has a real status-only health route; an unconfigured service
    // must report unavailable without falling back to the web page.
    let response = send(&app, get("/health/capture")).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/octet-stream"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert!(body(response).await.is_empty());
    let response = send(&app, get("/api/v1/openapi.json")).await;
    assert_eq!(response.status(), StatusCode::OK);
    problem(
        send(&app, get("/api/v1/posts")).await,
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[tokio::test]
async fn other_files_are_served_and_revalidated() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    let response = send(&app, get("/manifest.webmanifest")).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], NO_CACHE);
    assert_eq!(body(response).await, MANIFEST);
}

#[tokio::test]
async fn nothing_outside_the_directory_is_served() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    assert!(site.dir.path().join("secret.txt").is_file());
    for uri in [
        "/../secret.txt",
        "/..%2fsecret.txt",
        "/%2e%2e/secret.txt",
        "/assets/../../secret.txt",
    ] {
        let response = send(&app, get(uri)).await;
        let status = response.status();
        let bytes = body(response).await;
        assert!(
            !String::from_utf8_lossy(&bytes).contains(SECRET),
            "{uri} leaked a file outside the web app ({status})"
        );
    }
}

#[tokio::test]
async fn other_methods_find_nothing() {
    let site = Site::new();
    let t = site.serve();
    let app = t.app();
    for (method, uri) in [
        (Method::POST, "/"),
        (Method::PUT, "/login"),
        (Method::DELETE, "/assets/index-Abc123.js"),
    ] {
        let request = from_app(
            Request::builder()
                .method(method.clone())
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        );
        problem(send(&app, request).await, StatusCode::NOT_FOUND).await;
    }
}

#[tokio::test]
async fn without_a_web_app_the_api_stands_alone() {
    let t = TestState::new();
    let app = t.app();
    problem(send(&app, get("/")).await, StatusCode::NOT_FOUND).await;
    problem(send(&app, get("/login/magic")).await, StatusCode::NOT_FOUND).await;
}
