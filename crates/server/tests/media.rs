//! `GET /media/{file}` through the real middleware stack: headers, ETag and
//! 304, byte ranges, HEAD, streaming, names, authentication and the isolation
//! between users.
//!
//! Most tests authenticate with a test-only layer that turns an `x-test-user`
//! header into the [`CurrentUser`] the route reads, so one application can act
//! for several users. The application itself has no such layer:
//! `a_signed_in_session_reads_its_own_media_and_tokens_get_none` goes through
//! the real sign-in and session cookie.

mod support;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderName, Method, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::Response;
use shelfy_media::store::{IngestLimits, MediaStore, StoredObject, UserMedia};
use shelfy_media::{Digest, Rendition};
use shelfy_server::current_user::CurrentUser;
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::routes::media::IMMUTABLE;
use shelfy_server::security_headers::CONTENT_SECURITY_POLICY;
use shelfy_server::tokens::{SecretToken, hash_token};
use shelfy_server::{app, routes};
use support::auth::{owner, sign_in, with_session};
use support::library::{ALICE, BOB};
use support::{TestState, body, from_app, get as plain_get, problem, send};

const TEST_USER: &str = "x-test-user";

/// Test-only authentication: the user named by `x-test-user`.
async fn test_auth(mut request: Request, next: Next) -> Response {
    let user = request
        .headers()
        .get(TEST_USER)
        .and_then(|v| v.to_str().ok())
        .map(CurrentUser::new);
    if let Some(user) = user {
        request.extensions_mut().insert(user);
    }
    next.run(request).await
}

/// The real routes and stack, wrapped in the test authentication: it must
/// sit outside the application, because the access gate runs before any
/// layer inside the router.
fn app(t: &TestState) -> Router {
    app::build(t.state.clone(), routes::router()).layer(middleware::from_fn(test_auth))
}

fn media_of(t: &TestState, user: &str) -> UserMedia {
    MediaStore::new(t.data_dir().users_dir())
        .user(user)
        .unwrap()
}

/// `len` deterministic bytes after `magic`.
fn content(magic: &[u8], len: usize, seed: u8) -> Vec<u8> {
    let mut bytes = magic.to_vec();
    let mut x = u32::from(seed) | 1;
    while bytes.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        bytes.push(x.to_le_bytes()[0]);
    }
    bytes
}

fn jpeg_like(seed: u8) -> Vec<u8> {
    content(&[0xFF, 0xD8, 0xFF, 0xE0], 3_000, seed)
}

/// Stores `bytes` for `user`; returns the object and its URL.
fn store(t: &TestState, user: &str, bytes: &[u8]) -> (StoredObject, String) {
    let stored = media_of(t, user)
        .ingest(bytes, IngestLimits::UPLOAD)
        .unwrap()
        .publish()
        .unwrap();
    let url = format!("/media/{}", stored.name());
    (stored, url)
}

fn request(
    method: Method,
    uri: &str,
    user: Option<&str>,
    headers: &[(HeaderName, &str)],
) -> Request<Body> {
    let mut builder = Request::builder().method(method).uri(uri);
    if let Some(user) = user {
        builder = builder.header(TEST_USER, user);
    }
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    builder.body(Body::empty()).unwrap()
}

fn get(uri: &str, user: &str, headers: &[(HeaderName, &str)]) -> Request<Body> {
    request(Method::GET, uri, Some(user), headers)
}

fn header_str<'a>(response: &'a Response<Body>, name: &HeaderName) -> &'a str {
    response.headers()[name].to_str().unwrap()
}

/// Checks the headers every media success carries.
fn assert_media_headers(response: &Response<Body>, etag: &str) {
    let h = response.headers();
    assert_eq!(h[header::ETAG], etag, "ETag");
    assert_eq!(h[header::CACHE_CONTROL], IMMUTABLE);
    assert_eq!(
        h[header::CACHE_CONTROL],
        "private, max-age=31536000, immutable"
    );
    assert_eq!(h[header::ACCEPT_RANGES], "bytes");
    assert_eq!(h[header::CONTENT_SECURITY_POLICY], sandboxed_policy());
    assert_eq!(h[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
    assert_eq!(h["cross-origin-resource-policy"], "same-origin");
}

/// The policy of media answers: the app's (P1-09), then the route's
/// `sandbox`.
fn sandboxed_policy() -> String {
    format!("{CONTENT_SECURITY_POLICY}; sandbox")
}

fn quoted(digest: &Digest) -> String {
    format!("\"{digest}\"")
}

#[tokio::test]
async fn a_stored_object_is_served_with_every_header() {
    let t = TestState::new();
    let bytes = jpeg_like(1);
    let (stored, url) = store(&t, ALICE, &bytes);

    let response = send(
        &app(&t),
        get(&url, ALICE, &[(header::ACCEPT_ENCODING, "gzip, br")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_media_headers(&response, &quoted(&stored.digest));
    assert_eq!(header_str(&response, &header::CONTENT_TYPE), "image/jpeg");
    assert_eq!(
        header_str(&response, &header::CONTENT_LENGTH),
        bytes.len().to_string()
    );
    assert!(!response.headers().contains_key(header::CONTENT_DISPOSITION));
    assert!(
        !response.headers().contains_key(header::CONTENT_ENCODING),
        "never compressed"
    );
    assert_eq!(body(response).await, bytes);
}

#[tokio::test]
async fn renditions_are_served_with_the_digest_of_their_own_bytes() {
    let t = TestState::new();
    let (stored, _) = store(&t, ALICE, &jpeg_like(2));
    let media = media_of(&t, ALICE);
    let webp = content(b"RIFF\x20\0\0\0WEBPVP8 ", 900, 3);
    media
        .store_rendition(&stored.digest, Rendition::G480, &webp)
        .unwrap();
    let url = format!("/media/{}.g480.webp", stored.digest);

    let response = send(&app(&t), get(&url, ALICE, &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    let first_tag = quoted(&Digest::of(&webp));
    assert_media_headers(&response, &first_tag);
    assert_eq!(header_str(&response, &header::CONTENT_TYPE), "image/webp");
    assert_eq!(body(response).await, webp);

    // Rendered again (another encoder version): new bytes, new tag, no 304.
    let again = content(b"RIFF\x20\0\0\0WEBPVP8 ", 800, 4);
    media
        .store_rendition(&stored.digest, Rendition::G480, &again)
        .unwrap();
    let response = send(
        &app(&t),
        get(&url, ALICE, &[(header::IF_NONE_MATCH, &first_tag)]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_str(&response, &header::ETAG),
        quoted(&Digest::of(&again))
    );
    assert_eq!(body(response).await, again);
}

#[tokio::test]
async fn a_missing_rendition_is_a_not_found_that_is_never_cached() {
    let t = TestState::new();
    let (stored, _) = store(&t, ALICE, &jpeg_like(5));
    let url = format!("/media/{}.g480.webp", stored.digest);
    let response = send(&app(&t), get(&url, ALICE, &[])).await;
    // `problem` also checks `Cache-Control: no-store`.
    let problem = problem(response, StatusCode::NOT_FOUND).await;
    assert_eq!(problem.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn head_answers_the_headers_without_the_body() {
    let t = TestState::new();
    let bytes = jpeg_like(6);
    let (stored, url) = store(&t, ALICE, &bytes);
    let response = send(
        &app(&t),
        request(
            Method::HEAD,
            &url,
            Some(ALICE),
            &[(header::RANGE, "bytes=0-9")],
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "HEAD ignores Range");
    assert_media_headers(&response, &quoted(&stored.digest));
    assert_eq!(
        header_str(&response, &header::CONTENT_LENGTH),
        bytes.len().to_string()
    );
    assert!(body(response).await.is_empty());
}

#[tokio::test]
async fn a_current_copy_gets_304() {
    let t = TestState::new();
    let (stored, url) = store(&t, ALICE, &jpeg_like(7));
    let tag = quoted(&stored.digest);
    let weak = format!("W/{tag}");
    let list = format!("\"other\", {tag}");
    for value in [tag.as_str(), weak.as_str(), list.as_str(), "*"] {
        let response = send(
            &app(&t),
            get(&url, ALICE, &[(header::IF_NONE_MATCH, value)]),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{value}");
        assert_media_headers(&response, &tag);
        assert!(!response.headers().contains_key(header::CONTENT_TYPE));
        assert!(body(response).await.is_empty());
    }
    let head = request(
        Method::HEAD,
        &url,
        Some(ALICE),
        &[(header::IF_NONE_MATCH, &tag)],
    );
    assert_eq!(
        send(&app(&t), head).await.status(),
        StatusCode::NOT_MODIFIED
    );

    let stale = send(
        &app(&t),
        get(&url, ALICE, &[(header::IF_NONE_MATCH, "\"other\"")]),
    )
    .await;
    assert_eq!(stale.status(), StatusCode::OK);

    let response = send(
        &app(&t),
        get(&url, ALICE, &[(header::IF_MATCH, "\"other\"")]),
    )
    .await;
    problem(response, StatusCode::PRECONDITION_FAILED).await;
    let response = send(&app(&t), get(&url, ALICE, &[(header::IF_MATCH, &tag)])).await;
    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn byte_ranges_are_served_with_206() {
    let t = TestState::new();
    let bytes = jpeg_like(8);
    let size = bytes.len();
    let (stored, url) = store(&t, ALICE, &bytes);
    let tag = quoted(&stored.digest);
    let cases: [(&str, usize, usize); 6] = [
        ("bytes=0-0", 0, 0),
        ("bytes=0-", 0, size - 1),
        ("bytes=100-199", 100, 199),
        ("bytes=2900-9999", 2900, size - 1),
        ("bytes=-10", size - 10, size - 1),
        ("bytes=-99999", 0, size - 1),
    ];
    for (range, start, end) in cases {
        let response = send(&app(&t), get(&url, ALICE, &[(header::RANGE, range)])).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT, "{range}");
        assert_media_headers(&response, &tag);
        assert_eq!(
            header_str(&response, &header::CONTENT_RANGE),
            format!("bytes {start}-{end}/{size}"),
            "{range}"
        );
        assert_eq!(header_str(&response, &header::CONTENT_TYPE), "image/jpeg");
        assert_eq!(
            header_str(&response, &header::CONTENT_LENGTH),
            (end - start + 1).to_string()
        );
        assert_eq!(body(response).await, bytes[start..=end], "{range}");
    }

    // Ignored: several ranges, other units, malformed values; a stale If-Range.
    for headers in [
        vec![(header::RANGE, "bytes=0-1,5-6")],
        vec![(header::RANGE, "items=0-1")],
        vec![(header::RANGE, "bytes=9-1")],
        vec![
            (header::RANGE, "bytes=0-9"),
            (header::IF_RANGE, "\"other\""),
        ],
    ] {
        let response = send(&app(&t), get(&url, ALICE, &headers)).await;
        assert_eq!(response.status(), StatusCode::OK, "{headers:?}");
        assert!(!response.headers().contains_key(header::CONTENT_RANGE));
        assert_eq!(body(response).await, bytes);
    }
    let response = send(
        &app(&t),
        get(
            &url,
            ALICE,
            &[(header::RANGE, "bytes=0-9"), (header::IF_RANGE, &tag)],
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
}

#[tokio::test]
async fn an_unsatisfiable_range_is_a_416_problem_with_content_range() {
    let t = TestState::new();
    let bytes = jpeg_like(9);
    let (_, url) = store(&t, ALICE, &bytes);
    for range in [format!("bytes={}-", bytes.len()), "bytes=-0".to_owned()] {
        let response = send(&app(&t), get(&url, ALICE, &[(header::RANGE, &range)])).await;
        assert_eq!(
            header_str(&response, &header::CONTENT_RANGE),
            format!("bytes */{}", bytes.len())
        );
        let problem = problem(response, StatusCode::RANGE_NOT_SATISFIABLE).await;
        assert_eq!(problem.status, 416);
    }
}

#[tokio::test]
async fn large_objects_stream_and_seek() {
    let t = TestState::new();
    // An MP4 several read chunks long, the way videos are played.
    let bytes = content(b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2", 300_000, 10);
    let (stored, url) = store(&t, ALICE, &bytes);
    assert_eq!(stored.kind, shelfy_media::MediaKind::Mp4);

    let response = send(&app(&t), get(&url, ALICE, &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(header_str(&response, &header::CONTENT_TYPE), "video/mp4");
    assert_eq!(header_str(&response, &header::CONTENT_LENGTH), "300000");
    assert_eq!(body(response).await, bytes);

    let response = send(
        &app(&t),
        get(&url, ALICE, &[(header::RANGE, "bytes=70000-200000")]),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(
        header_str(&response, &header::CONTENT_RANGE),
        "bytes 70000-200000/300000"
    );
    assert_eq!(body(response).await, bytes[70_000..=200_000]);
}

#[tokio::test]
async fn documents_are_attachments() {
    let t = TestState::new();
    let (_, url) = store(&t, ALICE, &content(b"%PDF-1.7\n", 2_000, 11));
    let response = send(&app(&t), get(&url, ALICE, &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        header_str(&response, &header::CONTENT_TYPE),
        "application/pdf"
    );
    assert_eq!(
        header_str(&response, &header::CONTENT_DISPOSITION),
        "attachment"
    );
    assert_eq!(
        header_str(&response, &header::CONTENT_SECURITY_POLICY),
        sandboxed_policy()
    );
}

#[tokio::test]
async fn users_never_read_each_others_objects() {
    let t = TestState::new();
    let app = app(&t);
    let bytes = jpeg_like(12);
    let (stored, url) = store(&t, ALICE, &bytes);
    media_of(&t, ALICE)
        .store_rendition(&stored.digest, Rendition::G480, b"RIFF\0\0\0\0WEBPVP8 ")
        .unwrap();
    let rendition = format!("/media/{}.g480.webp", stored.digest);

    assert_eq!(
        send(&app, get(&url, ALICE, &[])).await.status(),
        StatusCode::OK
    );
    for uri in [&url, &rendition] {
        let response = send(&app, get(uri, BOB, &[])).await;
        problem(response, StatusCode::NOT_FOUND).await;
        // Conditional and range requests do not leak existence either.
        let tag = quoted(&stored.digest);
        let response = send(&app, get(uri, BOB, &[(header::IF_NONE_MATCH, &tag)])).await;
        problem(response, StatusCode::NOT_FOUND).await;
        let response = send(&app, get(uri, BOB, &[(header::RANGE, "bytes=0-0")])).await;
        problem(response, StatusCode::NOT_FOUND).await;
    }

    // The same bytes saved by Bob are Bob's own copy: removing Alice's
    // leaves Bob's, and the other way round.
    store(&t, BOB, &bytes);
    assert_eq!(
        send(&app, get(&url, BOB, &[])).await.status(),
        StatusCode::OK
    );
    media_of(&t, ALICE)
        .remove(&stored.digest, stored.kind)
        .unwrap();
    problem(
        send(&app, get(&url, ALICE, &[])).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let response = send(&app, get(&url, BOB, &[])).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(body(response).await, bytes);
}

#[tokio::test]
async fn only_canonical_names_are_served() {
    let t = TestState::new();
    let (stored, url) = store(&t, ALICE, &jpeg_like(13));
    let hex = stored.digest.to_string();
    let bob_dir = format!("..%2F..%2F{BOB}%2Fmedia%2F{}%2F{hex}.jpg", &hex[..2]);
    let names = [
        format!("{}.jpg", hex.to_uppercase()),
        format!("{}.jpg", &hex[1..]),
        format!("{hex}.jpeg"),
        format!("{hex}.JPG"),
        format!("{hex}.png"), // stored as JPEG
        format!("{hex}.svg"),
        format!("{hex}.g480.jpg"),
        format!("{hex}.g480.webp"), // not rendered
        format!("{hex}.jpg%00"),
        format!("{hex}.jpg%2F"),
        format!("{}%2F{hex}.jpg", &hex[..2]),
        bob_dir,
        "..%2Flibrary.sqlite".to_owned(),
        "%2E%2E".to_owned(),
    ];
    let app = app(&t);
    assert_eq!(
        send(&app, get(&url, ALICE, &[])).await.status(),
        StatusCode::OK
    );
    for name in names {
        let response = send(&app, get(&format!("/media/{name}"), ALICE, &[])).await;
        let problem = problem(response, StatusCode::NOT_FOUND).await;
        assert_eq!(problem.code, ErrorCode::NotFound, "{name}");
    }
    // A second path segment matches no route.
    let nested = format!("/media/{}/{hex}.jpg", &hex[..2]);
    problem(
        send(&app, get(&nested, ALICE, &[])).await,
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn media_needs_an_authenticated_user() {
    let t = TestState::new();
    let (_, url) = store(&t, ALICE, &jpeg_like(14));
    let response = send(&app(&t), request(Method::GET, &url, None, &[])).await;
    let unauthorized = problem(response, StatusCode::UNAUTHORIZED).await;
    assert_eq!(unauthorized.code, ErrorCode::Unauthorized);

    // A user id that cannot name a directory never reaches the file system.
    let response = send(&app(&t), get(&url, "..", &[])).await;
    problem(response, StatusCode::INTERNAL_SERVER_ERROR).await;
}

#[tokio::test]
async fn the_real_application_refuses_unauthenticated_media() {
    // No test layer and no session cookie.
    let t = TestState::new();
    let (_, url) = store(&t, ALICE, &jpeg_like(15));
    let response = send(&t.app(), request(Method::GET, &url, Some(ALICE), &[])).await;
    problem(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn other_methods_are_refused() {
    let t = TestState::new();
    let (_, url) = store(&t, ALICE, &jpeg_like(16));
    for method in [Method::POST, Method::PUT, Method::DELETE] {
        // With the web app's headers, so the CSRF guard lets them through.
        let unsafe_request = from_app(request(method.clone(), &url, Some(ALICE), &[]));
        let response = send(&app(&t), unsafe_request).await;
        let allow = header_str(&response, &header::ALLOW).to_owned();
        assert!(
            allow.contains("GET") && allow.contains("HEAD"),
            "{method}: {allow}"
        );
        problem(response, StatusCode::METHOD_NOT_ALLOWED).await;
    }
}

#[tokio::test]
async fn a_signed_in_session_reads_its_own_media_and_tokens_get_none() {
    // The real stack and T10's sign-in: no test layer.
    let t = TestState::new();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner = owner(&t);
    let bytes = jpeg_like(17);
    let (stored, url) = store(&t, &owner, &bytes);
    let (_, someone_elses) = store(&t, ALICE, &jpeg_like(18));

    let response = send(&app, with_session(plain_get(&url), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_media_headers(&response, &quoted(&stored.digest));
    assert_eq!(body(response).await, bytes);
    let response = send(&app, with_session(plain_get(&someone_elses), &cookie)).await;
    problem(response, StatusCode::NOT_FOUND).await;

    // Media is cookie-only: an API token of the same user is not a session.
    let token = api_token(&t, &owner);
    let mut request = plain_get(&url);
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
}

/// Inserts an API token for `user_id` (as `tests/auth.rs` does); returns it.
fn api_token(t: &TestState, user_id: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    rusqlite::Connection::open(t.data_dir().control_db())
        .unwrap()
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, 'extension', ?3, 'ingest tasks uploads lookup', ?4)",
            rusqlite::params![new_ulid(), user_id, hash_token(&token).as_slice(), now_ms()],
        )
        .unwrap();
    token
}
