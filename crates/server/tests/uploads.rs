//! Uploads for the web app (P4-08): tus with a session, an `uploads` token or
//! a `migrate` token. The purpose decides who may upload what, how large it
//! may be, whether the client declares the SHA-256 and what the bytes must
//! be; a complete web upload is used once; each purpose has its own expiry.
//! The migration CLI's uploads, end to end, are in `tests/migration.rs`.

mod support;

use std::io::Cursor;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, Response, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder as _, RgbImage};
use rusqlite::{Connection, params};
use sha2::{Digest as _, Sha256};
use shelfy_media::MediaKind;
use shelfy_server::control::uploads::{Found, UploadPurpose};
use shelfy_server::error::{ApiError, ErrorCode};
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::migrations::housekeeping;
use shelfy_server::routes::uploads::{self, ClaimError};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{add_member, from_spa, owner, sign_in, sign_in_as, spa, with_session};
use support::{TestState, get, json, post_json, problem, send};

const HOUR: i64 = 3_600_000;
const MIB: usize = 1024 * 1024;

fn control(t: &TestState) -> Connection {
    support::auth::control_db(t)
}

/// An API token of `user` with `scopes`, inserted directly; returns it.
fn token(t: &TestState, user: &str, kind: &str, scopes: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    control(t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                new_ulid(),
                user,
                kind,
                hash_token(&token).as_slice(),
                scopes,
                now_ms()
            ],
        )
        .unwrap();
    token
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `Upload-Metadata` of `pairs`.
fn metadata(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k} {}", STANDARD.encode(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// `Upload-Metadata` of a web upload of `purpose`.
fn purpose(name: &str) -> String {
    metadata(&[("purpose", name)])
}

fn jpeg(seed: u8) -> Vec<u8> {
    let image = RgbImage::from_fn(32, 24, |x, y| {
        image::Rgb([seed, (x % 256) as u8, (y % 256) as u8])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image)
        .unwrap();
    out
}

fn png() -> Vec<u8> {
    let image = RgbImage::from_fn(16, 16, |x, y| image::Rgb([x as u8, y as u8, 7]));
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new(&mut out)
        .write_image(image.as_raw(), 16, 16, ExtendedColorType::Rgb8)
        .unwrap();
    out.into_inner()
}

/// The tus requests, without credentials.
mod tus {
    use super::*;

    pub fn create(length: usize, meta: &str) -> Request<Body> {
        Request::post("/api/v1/uploads")
            .header("tus-resumable", "1.0.0")
            .header("upload-length", length.to_string())
            .header("upload-metadata", meta)
            .body(Body::empty())
            .unwrap()
    }

    pub fn head(location: &str) -> Request<Body> {
        Request::head(location)
            .header("tus-resumable", "1.0.0")
            .body(Body::empty())
            .unwrap()
    }

    pub fn patch(location: &str, offset: usize, chunk: &[u8]) -> Request<Body> {
        Request::patch(location)
            .header("tus-resumable", "1.0.0")
            .header("upload-offset", offset.to_string())
            .header(header::CONTENT_TYPE, "application/offset+octet-stream")
            .body(Body::from(chunk.to_vec()))
            .unwrap()
    }

    pub fn delete(location: &str) -> Request<Body> {
        Request::delete(location)
            .header("tus-resumable", "1.0.0")
            .body(Body::empty())
            .unwrap()
    }
}

fn bearer(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
}

fn location(response: &Response<Body>) -> String {
    response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned()
}

fn id_of(location: &str) -> String {
    location.rsplit('/').next().unwrap().to_owned()
}

fn offset_of(response: &Response<Body>) -> u64 {
    response.headers()["upload-offset"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

/// The stored row of upload `id`: purpose, `meta_json`, completed.
fn row(t: &TestState, id: &str) -> Option<(String, serde_json::Value, bool)> {
    control(t)
        .query_row(
            "SELECT purpose, meta_json, completed_at IS NOT NULL FROM uploads WHERE id = ?1",
            [id],
            |r| {
                let meta: String = r.get(1)?;
                Ok((r.get(0)?, serde_json::from_str(&meta).unwrap(), r.get(2)?))
            },
        )
        .ok()
}

/// A signed-in owner: the app, its session cookie and its id.
async fn signed_in(t: &TestState) -> (Router, String, String) {
    let app = t.app();
    let cookie = sign_in(&app, t).await;
    (app, cookie, owner(t))
}

/// Creates and completes a web upload of `bytes` as the session; returns
/// its location.
async fn upload_as(
    t: &TestState,
    app: &Router,
    cookie: &str,
    meta: &str,
    bytes: &[u8],
) -> Response<Body> {
    let response = send(app, spa(t, tus::create(bytes.len(), meta), cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED, "{meta}");
    let at = location(&response);
    send(app, spa(t, tus::patch(&at, 0, bytes), cookie)).await
}

#[tokio::test]
async fn a_session_uploads_a_bookmark_under_the_csrf_rules() {
    let t = TestState::new();
    let (app, cookie, _) = signed_in(&t).await;
    let bytes = jpeg(1);
    let meta = metadata(&[
        ("purpose", "bookmark-original"),
        ("filename", "../Holiday 2026.jpg"),
        ("filetype", "image/svg+xml"),
    ]);

    // Without the app's headers, every state-changing request is refused.
    let refused = problem(
        send(&app, with_session(tus::create(bytes.len(), &meta), &cookie)).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::CsrfFailed);
    let response = send(&app, spa(&t, tus::create(bytes.len(), &meta), &cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["tus-resumable"], "1.0.0");
    let at = location(&response);
    let created = json(response).await;
    assert_eq!(
        at,
        format!("/api/v1/uploads/{}", created["id"].as_str().unwrap())
    );

    // HEAD changes nothing: the session alone is enough (browsers send no
    // Origin on it).
    let response = send(&app, with_session(tus::head(&at), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(offset_of(&response), 0);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

    let half = bytes.len() / 2;
    for request in [
        with_session(tus::patch(&at, 0, &bytes[..half]), &cookie),
        {
            let mut cross = spa(&t, tus::patch(&at, 0, &bytes[..half]), &cookie);
            cross
                .headers_mut()
                .insert(header::ORIGIN, "https://evil.example.test".parse().unwrap());
            cross
        },
        with_session(tus::delete(&at), &cookie),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::CsrfFailed);
    }
    // PATCH bodies stay application/offset+octet-stream.
    let mut typed = spa(&t, tus::patch(&at, 0, &bytes[..half]), &cookie);
    typed
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    problem(send(&app, typed).await, StatusCode::UNSUPPORTED_MEDIA_TYPE).await;

    let response = send(&app, spa(&t, tus::patch(&at, 0, &bytes[..half]), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(offset_of(&response), half as u64);
    let response = send(
        &app,
        spa(&t, tus::patch(&at, half, &bytes[half..]), &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(offset_of(&response), bytes.len() as u64);

    // Complete: the server recorded the hash it computed and the type it
    // sniffed; the client's file name is a label.
    let id = id_of(&at);
    let (purpose, meta, complete) = row(&t, &id).unwrap();
    assert_eq!(purpose, "bookmark-original");
    assert!(complete);
    assert_eq!(meta["sha256"], sha256_hex(&bytes));
    assert_eq!(meta["ext"], "jpg", "sniffed, whatever the client said");
    assert_eq!(meta["filename"], "Holiday 2026.jpg");
    let stored = t.data_dir().uploads_dir().join(&id);
    assert_eq!(std::fs::read(&stored).unwrap(), bytes);

    // Termination, with the app's headers.
    let response = send(&app, spa(&t, tus::delete(&at), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(!stored.exists());
    let response = send(&app, with_session(tus::head(&at), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn the_purpose_decides_who_may_create_an_upload() {
    let t = TestState::new();
    let (app, cookie, owner_id) = signed_in(&t).await;
    let uploads_token = token(&t, &owner_id, "extension", "uploads");
    let migrate_token = token(&t, &owner_id, "migrate", "migrate");
    let extension_token = token(&t, &owner_id, "extension", "ingest tasks lookup");
    let shortcut_token = token(&t, &owner_id, "shortcut", "links:create");
    let migration = [
        metadata(&[
            ("purpose", "migration-object"),
            ("sha256", &sha256_hex(b"x")),
            ("ext", "jpg"),
        ]),
        metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(b"x"))]),
    ];
    let web = [
        purpose("bookmark-original"),
        purpose("bookmark-preview"),
        purpose("import"),
    ];

    let as_session = |meta: &str| spa(&t, tus::create(1, meta), &cookie);
    let as_token = |meta: &str, token: &str| bearer(tus::create(1, meta), token);
    // A session creating a migration upload gets 403; so does an `uploads`
    // token, and a `migrate` token creating a web upload.
    for meta in &migration {
        for request in [as_session(meta), as_token(meta, &uploads_token)] {
            let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
            assert_eq!(refused.code, ErrorCode::Forbidden);
            assert!(refused.detail.unwrap().contains("migrate scope"));
        }
        let response = send(&app, as_token(meta, &migrate_token)).await;
        assert_eq!(response.status(), StatusCode::CREATED, "{meta}");
    }
    for meta in &web {
        for request in [as_session(meta), as_token(meta, &uploads_token)] {
            assert_eq!(send(&app, request).await.status(), StatusCode::CREATED);
        }
        let refused = problem(
            send(&app, as_token(meta, &migrate_token)).await,
            StatusCode::FORBIDDEN,
        )
        .await;
        assert!(refused.detail.unwrap().contains("uploads scope"), "{meta}");
    }
    // Tokens without `uploads` or `migrate` never get past the gate.
    for token in [&extension_token, &shortcut_token] {
        let refused = problem(
            send(&app, as_token(&web[0], token)).await,
            StatusCode::FORBIDDEN,
        )
        .await;
        assert!(refused.detail.unwrap().ends_with("uploads, migrate"));
    }
    // An unknown purpose is malformed metadata, whoever asks.
    for request in [
        as_session(&purpose("archive-object")),
        as_token(&purpose("bookmark"), &uploads_token),
        as_token(&metadata(&[("sha256", &sha256_hex(b"x"))]), &migrate_token),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(refused.errors[0].field, "purpose");
    }
    // Nobody: 401 with the Bearer challenge, as on a token-only route.
    let response = send(&app, from_spa(&t, tus::create(1, &web[0]))).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn each_request_checks_the_caller_against_the_uploads_purpose() {
    let t = TestState::new();
    let (app, cookie, owner_id) = signed_in(&t).await;
    let uploads_token = token(&t, &owner_id, "extension", "uploads");
    let migrate_token = token(&t, &owner_id, "migrate", "migrate");
    let migration = metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(b"x"))]);
    let cli_upload =
        location(&send(&app, bearer(tus::create(1, &migration), &migrate_token)).await);
    let web_upload = location(
        &send(
            &app,
            spa(&t, tus::create(1, &purpose("bookmark-original")), &cookie),
        )
        .await,
    );

    // The session and the `uploads` token cannot touch the CLI's upload…
    for request in [
        spa(&t, tus::patch(&cli_upload, 0, b"S"), &cookie),
        spa(&t, tus::delete(&cli_upload), &cookie),
        bearer(tus::patch(&cli_upload, 0, b"S"), &uploads_token),
        bearer(tus::delete(&cli_upload), &uploads_token),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::Forbidden);
    }
    let response = send(&app, with_session(tus::head(&cli_upload), &cookie)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // …nor the `migrate` token the web's; the `uploads` token may.
    let response = send(&app, bearer(tus::head(&web_upload), &migrate_token)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = send(&app, bearer(tus::head(&web_upload), &uploads_token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    // Each still works for its own.
    let response = send(&app, bearer(tus::head(&cli_upload), &migrate_token)).await;
    assert_eq!(response.status(), StatusCode::OK);
    let response = send(&app, spa(&t, tus::delete(&web_upload), &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn another_users_upload_is_not_found() {
    let t = TestState::new();
    let (app, cookie, _) = signed_in(&t).await;
    let member = add_member(&t, "member@example.test");
    let member_cookie = sign_in_as(&app, &t, "member@example.test").await;
    let member_token = token(&t, &member, "extension", "uploads");
    let mine = location(&send(&app, spa(&t, tus::create(4, &purpose("import")), &cookie)).await);
    for request in [
        with_session(tus::head(&mine), &member_cookie),
        spa(&t, tus::patch(&mine, 0, b"[{}]"), &member_cookie),
        spa(&t, tus::delete(&mine), &member_cookie),
        bearer(tus::head(&mine), &member_token),
        bearer(tus::patch(&mine, 0, b"[{}]"), &member_token),
        bearer(tus::delete(&mine), &member_token),
    ] {
        let method = request.method().clone();
        let response = send(&app, request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method}");
    }
    // Still the owner's, untouched.
    let response = send(&app, with_session(tus::head(&mine), &cookie)).await;
    assert_eq!(offset_of(&response), 0);
    let claimed = uploads::claim(&t.state, &member, UploadPurpose::IMPORT, &[id_of(&mine)]).await;
    assert!(matches!(claimed, Err(ClaimError::Unusable { .. })));
}

#[tokio::test]
async fn a_token_never_reaches_a_cookie_only_route() {
    let t = TestState::new();
    let (app, cookie, owner_id) = signed_in(&t).await;
    let uploads_token = token(&t, &owner_id, "extension", "uploads");
    let migrate_token = token(&t, &owner_id, "migrate", "migrate");
    let requests = || {
        [
            get("/api/v1/me"),
            get("/api/v1/posts"),
            get("/api/v1/me/usage"),
            post_json("/api/v1/collections", r#"{"name":"x"}"#),
        ]
    };
    for token in [&uploads_token, &migrate_token] {
        // With the token alone, and beside a valid session cookie.
        for (alone, beside) in requests().into_iter().zip(requests()) {
            let uri = alone.uri().to_string();
            for request in [
                bearer(alone, token),
                bearer(with_session(beside, &cookie), token),
            ] {
                let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
                assert_eq!(refused.code, ErrorCode::Unauthorized, "{uri}");
            }
        }
    }
}

#[tokio::test]
async fn web_uploads_need_no_hash_but_a_declared_one_is_checked() {
    let t = TestState::new();
    let (app, cookie, _) = signed_in(&t).await;
    let bytes = jpeg(2);

    let lie = metadata(&[
        ("purpose", "bookmark-original"),
        ("sha256", &sha256_hex(b"something else")),
    ]);
    let response = send(&app, spa(&t, tus::create(bytes.len(), &lie), &cookie)).await;
    let at = location(&response);
    let refused = problem(
        send(&app, spa(&t, tus::patch(&at, 0, &bytes), &cookie)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.errors[0].field, "sha256");
    let response = send(&app, with_session(tus::head(&at), &cookie)).await;
    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "the upload is gone"
    );
    assert!(!t.data_dir().uploads_dir().join(id_of(&at)).exists());

    let truth = metadata(&[
        ("purpose", "bookmark-original"),
        ("sha256", &sha256_hex(&bytes)),
    ]);
    let response = upload_as(&t, &app, &cookie, &truth, &bytes).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);

    // The migration still has to declare it.
    let migrate_token = token(&t, &owner(&t), "migrate", "migrate");
    let refused = problem(
        send(
            &app,
            bearer(
                tus::create(1, &metadata(&[("purpose", "migration-db")])),
                &migrate_token,
            ),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.errors[0].field, "sha256");
}

#[tokio::test]
async fn each_purpose_has_its_size_cap() {
    let t = TestState::with_config(|config| config.import_max_bytes = MIB as u64);
    let (app, cookie, _) = signed_in(&t).await;
    for (meta, cap) in [
        (purpose("bookmark-original"), 200 * MIB),
        (purpose("bookmark-preview"), 2 * MIB),
        (purpose("import"), MIB),
    ] {
        let refused = problem(
            send(&app, spa(&t, tus::create(cap + 1, &meta), &cookie)).await,
            StatusCode::PAYLOAD_TOO_LARGE,
        )
        .await;
        assert!(refused.detail.unwrap().contains(&cap.to_string()), "{meta}");
        let response = send(&app, spa(&t, tus::create(cap, &meta), &cookie)).await;
        assert_eq!(response.status(), StatusCode::CREATED, "{meta}");
        let response = send(&app, spa(&t, tus::delete(&location(&response)), &cookie)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }
}

#[tokio::test]
async fn the_bytes_waiting_to_be_used_are_capped_per_user() {
    // The staging cap is the largest import plus 1 GiB: here 1 MiB + 1 GiB.
    let t = TestState::with_config(|config| config.import_max_bytes = MIB as u64);
    let (app, cookie, owner_id) = signed_in(&t).await;
    let original = purpose("bookmark-original");
    for _ in 0..5 {
        let response = send(&app, spa(&t, tus::create(200 * MIB, &original), &cookie)).await;
        assert_eq!(response.status(), StatusCode::CREATED);
    }
    let refused = problem(
        send(&app, spa(&t, tus::create(200 * MIB, &original), &cookie)).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(refused.detail.unwrap().contains("too many bytes"));
    let files = std::fs::read_dir(t.data_dir().uploads_dir())
        .unwrap()
        .count();
    assert_eq!(files, 5, "a refused upload leaves no file");
    // The migration's own uploads are bounded by the install, not by it.
    let migrate_token = token(&t, &owner_id, "migrate", "migrate");
    let meta = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha256_hex(b"x")),
        ("ext", "mp4"),
    ]);
    let response = send(&app, bearer(tus::create(300 * MIB, &meta), &migrate_token)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn the_bytes_must_be_what_the_purpose_takes() {
    let t = TestState::new();
    let (app, cookie, _) = signed_in(&t).await;
    let pdf = b"%PDF-1.7\n1 0 obj << >> endobj\n%%EOF\n".to_vec();
    let avif = b"\0\0\0\x1cftypavif\0\0\0\0mif1miafMA1B\0\0\0\x08free".to_vec();
    let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"/>"#.to_vec();
    let html = b"<!DOCTYPE html><html><script>alert(1)</script>".to_vec();
    let zip = b"PK\x03\x04\x14\0\0\0\x08\0rest of a zip".to_vec();
    let mut json_doc = b"\xEF\xBB\xBF\n  ".to_vec();
    json_doc.extend_from_slice(br#"{"posts": [], "collections": []}"#);

    for (name, bytes, found) in [
        ("bookmark-original", jpeg(3), Some("jpg")),
        ("bookmark-original", pdf.clone(), Some("pdf")),
        ("bookmark-original", avif.clone(), Some("avif")),
        ("bookmark-original", svg.clone(), None),
        ("bookmark-original", html.clone(), None),
        ("bookmark-original", zip.clone(), None),
        ("bookmark-preview", png(), Some("png")),
        ("bookmark-preview", pdf.clone(), None),
        ("bookmark-preview", avif.clone(), None),
        ("import", json_doc.clone(), Some("json")),
        ("import", br#"[{"id": "1"}]"#.to_vec(), Some("json")),
        ("import", zip.clone(), Some("zip")),
        ("import", html.clone(), None),
        ("import", jpeg(4), None),
    ] {
        let response = send(
            &app,
            spa(&t, tus::create(bytes.len(), &purpose(name)), &cookie),
        )
        .await;
        let at = location(&response);
        let response = send(&app, spa(&t, tus::patch(&at, 0, &bytes), &cookie)).await;
        let id = id_of(&at);
        match found {
            Some(ext) => {
                assert_eq!(response.status(), StatusCode::NO_CONTENT, "{name} {ext}");
                assert_eq!(row(&t, &id).unwrap().1["ext"], ext, "{name}");
            }
            None => {
                let refused = problem(response, StatusCode::UNSUPPORTED_MEDIA_TYPE).await;
                assert_eq!(refused.code, ErrorCode::UnsupportedMediaType, "{name}");
                assert_eq!(row(&t, &id), None, "{name}: a refused upload is deleted");
                assert!(!t.data_dir().uploads_dir().join(&id).exists());
                assert!(
                    !t.data_dir()
                        .uploads_dir()
                        .join(format!("{id}.part"))
                        .exists()
                );
            }
        }
    }
}

#[tokio::test]
async fn a_complete_web_upload_is_used_once() {
    let t = TestState::new();
    let (app, cookie, owner_id) = signed_in(&t).await;
    let bytes = jpeg(5);
    let meta = metadata(&[("purpose", "bookmark-original"), ("filename", "cat.jpg")]);
    let response = send(&app, spa(&t, tus::create(bytes.len(), &meta), &cookie)).await;
    let at = location(&response);
    let id = id_of(&at);

    // Unfinished, of another purpose, or unknown: not usable.
    let original = UploadPurpose::BOOKMARK_ORIGINAL;
    let claim = |purpose: UploadPurpose, id: &str| {
        let ids = vec![id.to_owned()];
        let state = t.state.clone();
        let user = owner_id.clone();
        async move { uploads::claim(&state, &user, purpose, &ids).await }
    };
    assert!(matches!(
        claim(original, &id).await,
        Err(ClaimError::Unusable { .. })
    ));
    send(&app, spa(&t, tus::patch(&at, 0, &bytes), &cookie)).await;
    for (purpose, id) in [
        (UploadPurpose::BOOKMARK_PREVIEW, id.as_str()),
        (original, "01NOSUCHUPLOAD000000000000"),
    ] {
        let refused = claim(purpose, id).await.unwrap_err();
        assert!(matches!(refused, ClaimError::Unusable { .. }));
        let problem = refused.into_problem("files[0].upload").problem();
        assert_eq!(problem.code, ErrorCode::ValidationFailed);
        assert_eq!(problem.errors[0].field, "files[0].upload");
    }

    // The consumer gets the bytes and what the server found in them.
    let claimed = claim(original, &id).await.unwrap();
    let [claimed] = claimed.as_slice() else {
        panic!("one upload")
    };
    assert_eq!(claimed.id, id);
    assert_eq!(std::fs::read(&claimed.path).unwrap(), bytes);
    assert_eq!(claimed.length, bytes.len() as u64);
    assert_eq!(claimed.sha256.to_string(), sha256_hex(&bytes));
    assert_eq!(claimed.found, Found::Media(MediaKind::Jpeg));
    assert_eq!(claimed.media_kind(), Some(MediaKind::Jpeg));
    assert_eq!(claimed.filename.as_deref(), Some("cat.jpg"));

    // Reuse answers upload_consumed, also to the tus requests.
    let reused = ApiError::from(claim(original, &id).await.unwrap_err());
    assert_eq!(
        (reused.code(), reused.status()),
        (ErrorCode::UploadConsumed, StatusCode::CONFLICT)
    );
    for request in [
        spa(&t, tus::patch(&at, bytes.len(), b""), &cookie),
        spa(&t, tus::delete(&at), &cookie),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::CONFLICT).await;
        assert_eq!(refused.code, ErrorCode::UploadConsumed);
    }
    let response = send(&app, with_session(tus::head(&at), &cookie)).await;
    assert_eq!(response.status(), StatusCode::CONFLICT);
    assert!(claimed.path.exists(), "the consumer's bytes are safe");

    // Given back, it can be claimed again; discarded, its bytes go and a
    // reuse still answers upload_consumed.
    uploads::release(&t.state, &owner_id, std::slice::from_ref(&id))
        .await
        .unwrap();
    let again = claim(original, &id).await.unwrap();
    uploads::discard(&t.state, &owner_id, std::slice::from_ref(&id))
        .await
        .unwrap();
    assert!(!again[0].path.exists());
    assert!(matches!(
        claim(original, &id).await,
        Err(ClaimError::Consumed { .. })
    ));
    let (_, meta, _) = row(&t, &id).unwrap();
    assert!(meta.get("filename").is_none(), "{meta}");
    assert!(meta["consumed_at"].is_i64());
}

#[tokio::test]
async fn each_purpose_expires_on_its_own_schedule() {
    let t = TestState::new();
    let (app, cookie, owner_id) = signed_in(&t).await;
    let migrate_token = token(&t, &owner_id, "migrate", "migrate");
    let bytes = jpeg(6);

    let web = upload_as(&t, &app, &cookie, &purpose("bookmark-original"), &bytes).await;
    assert_eq!(web.status(), StatusCode::NO_CONTENT);
    let web_id = only_upload(&t, "bookmark-original", true);
    let used = {
        let response = upload_as(&t, &app, &cookie, &purpose("bookmark-preview"), &png()).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let id = only_upload(&t, "bookmark-preview", true);
        uploads::claim(
            &t.state,
            &owner_id,
            UploadPurpose::BOOKMARK_PREVIEW,
            std::slice::from_ref(&id),
        )
        .await
        .unwrap();
        id
    };
    let open = location(&send(&app, spa(&t, tus::create(10, &purpose("import")), &cookie)).await);
    let cli = {
        let meta = metadata(&[
            ("purpose", "migration-object"),
            ("sha256", &sha256_hex(&bytes)),
            ("ext", "jpg"),
        ]);
        let at = location(
            &send(
                &app,
                bearer(tus::create(bytes.len(), &meta), &migrate_token),
            )
            .await,
        );
        let response = send(&app, bearer(tus::patch(&at, 0, &bytes), &migrate_token)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        id_of(&at)
    };

    let now = now_ms();
    let swept = housekeeping::sweep(&t.state, now + 23 * HOUR).await;
    assert_eq!(swept.uploads, 0, "{swept:?}");

    // A complete web upload past its day is no longer usable, swept or not.
    control(&t)
        .execute(
            "UPDATE uploads SET completed_at = completed_at - ?1 WHERE id = ?2",
            params![25 * HOUR, web_id],
        )
        .unwrap();
    let response = send(
        &app,
        with_session(tus::head(&format!("/api/v1/uploads/{web_id}")), &cookie),
    )
    .await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert!(matches!(
        uploads::claim(
            &t.state,
            &owner_id,
            UploadPurpose::BOOKMARK_ORIGINAL,
            std::slice::from_ref(&web_id)
        )
        .await,
        Err(ClaimError::Unusable { .. })
    ));

    // A day on: the complete web upload and the unfinished one go, files too.
    let swept = housekeeping::sweep(&t.state, now + 24 * HOUR + 1).await;
    assert_eq!(swept.uploads, 2, "{swept:?}");
    assert_eq!(row(&t, &web_id), None);
    assert_eq!(row(&t, &id_of(&open)), None);
    assert!(!t.data_dir().uploads_dir().join(&web_id).exists());
    // The migration's waits a week, as the used one's row does.
    assert!(row(&t, &cli).is_some() && row(&t, &used).is_some());
    let swept = housekeeping::sweep(&t.state, now + 7 * 24 * HOUR + HOUR).await;
    assert_eq!(swept.uploads, 2, "{swept:?}");
    let left: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    let files = std::fs::read_dir(t.data_dir().uploads_dir())
        .map(|d| d.count())
        .unwrap();
    assert_eq!(files, 0);
}

/// The id of the one upload with `purpose` and completion state.
fn only_upload(t: &TestState, purpose: &str, complete: bool) -> String {
    control(t)
        .query_row(
            "SELECT id FROM uploads WHERE purpose = ?1 AND (completed_at IS NOT NULL) = ?2",
            params![purpose, complete],
            |r| r.get(0),
        )
        .unwrap()
}

#[tokio::test]
async fn a_bookmark_that_could_not_fit_in_the_quota_is_refused_at_once() {
    let t = TestState::new();
    let app = t.app();
    owner(&t);
    let member = add_member(&t, "member@example.test");
    control(&t)
        .execute(
            "UPDATE users SET quota_bytes = 1000, usage_bytes = 900 WHERE id = ?1",
            [&member],
        )
        .unwrap();
    let cookie = sign_in_as(&app, &t, "member@example.test").await;
    for (name, length, status) in [
        ("bookmark-original", 101, StatusCode::FORBIDDEN),
        ("bookmark-preview", 101, StatusCode::FORBIDDEN),
        ("bookmark-original", 100, StatusCode::CREATED),
        // An import adds posts, not these bytes: its importer reserves.
        ("import", 5000, StatusCode::CREATED),
    ] {
        let response = send(&app, spa(&t, tus::create(length, &purpose(name)), &cookie)).await;
        assert_eq!(response.status(), status, "{name} {length}");
        if status == StatusCode::FORBIDDEN {
            assert_eq!(
                problem(response, status).await.code,
                ErrorCode::QuotaExceeded
            );
        }
    }
    // The owner's quota is unlimited.
    let owner_cookie = sign_in(&app, &t).await;
    let response = send(
        &app,
        spa(
            &t,
            tus::create(200 * MIB, &purpose("bookmark-original")),
            &owner_cookie,
        ),
    )
    .await;
    assert_eq!(response.status(), StatusCode::CREATED);
}
