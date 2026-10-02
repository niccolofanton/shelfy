//! The migration (T9, P1-19): `admin migrate-token`, the device sign-in of
//! `shelfy-migrate login`, the tus upload routes, the preflight and
//! missing-objects checks, and the `migrate` job, end to end with
//! `shelfy-migrate run` against a server on a local port: a replace, a merge
//! into a library that is not empty, an upload whose process is killed and
//! continued, a locked library, and the housekeeping of what installs leave
//! behind. Every library here is synthetic.

mod support;

use std::io::Cursor;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::{Request as AxumRequest, State};
use axum::http::{Method, Request, StatusCode, header};
use axum::middleware::Next;
use axum::response::Response;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::codecs::jpeg::JpegEncoder;
use image::codecs::png::PngEncoder;
use image::{ExtendedColorType, ImageEncoder as _, RgbImage};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use shelfy_core::legacy::fixture::DESKTOP_SCHEMA_CURRENT;
use shelfy_migrate::bundle::sha256_file;
use shelfy_migrate::client::Client;
use shelfy_migrate::login::{self, LoginEvent, LoginOptions};
use shelfy_migrate::run::{RunOptions, RunOutcome, STATE_FILE};
use shelfy_migrate::settings::fixture as local_storage;
use shelfy_server::admin::migrate_token::{MIGRATE_TOKEN_TTL, create_migrate_token};
use shelfy_server::control::jobs::JobRow;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::ids::now_ms;
use shelfy_server::jobs::migrate::KIND as MIGRATE;
use shelfy_server::migrations::housekeeping;
use shelfy_server::tokens::hash_token;
use support::auth::{OWNER_EMAIL, owner, sign_in, spa, with_session};
use support::{TestState, body, get, json, problem, send};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

/// A migrate token for the owner, as `admin migrate-token` mints it.
fn migrate_token(t: &TestState) -> String {
    owner(t);
    create_migrate_token(&t.data_dir(), OWNER_EMAIL, MIGRATE_TOKEN_TTL)
        .expect("mint a migrate token")
        .token
        .expose()
        .clone()
}

fn control(t: &TestState) -> Connection {
    let conn = Connection::open(t.data_dir().control_db()).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn metadata(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{k} {}", STANDARD.encode(v)))
        .collect::<Vec<_>>()
        .join(",")
}

/// `POST /api/v1/uploads` with tus headers.
fn create(token: &str, length: usize, meta: &str) -> Request<Body> {
    Request::post("/api/v1/uploads")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("tus-resumable", "1.0.0")
        .header("upload-length", length.to_string())
        .header("upload-metadata", meta)
        .body(Body::empty())
        .unwrap()
}

fn head(token: &str, location: &str) -> Request<Body> {
    Request::head(location)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("tus-resumable", "1.0.0")
        .body(Body::empty())
        .unwrap()
}

fn patch(token: &str, location: &str, offset: usize, chunk: &[u8]) -> Request<Body> {
    Request::patch(location)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("tus-resumable", "1.0.0")
        .header("upload-offset", offset.to_string())
        .header(header::CONTENT_TYPE, "application/offset+octet-stream")
        .body(Body::from(chunk.to_vec()))
        .unwrap()
}

fn delete(token: &str, location: &str) -> Request<Body> {
    Request::delete(location)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .header("tus-resumable", "1.0.0")
        .body(Body::empty())
        .unwrap()
}

fn bearer_get(token: &str, uri: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap()
}

fn offset_of(response: &axum::http::Response<Body>) -> u64 {
    response.headers()["upload-offset"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn jpeg(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([seed, (x % 256) as u8, (y % 256) as u8])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image)
        .unwrap();
    out
}

fn png(width: u32, height: u32, seed: u8) -> Vec<u8> {
    let image = RgbImage::from_fn(width, height, |x, y| {
        image::Rgb([(x % 256) as u8, seed, (y % 256) as u8])
    });
    let mut out = Cursor::new(Vec::new());
    PngEncoder::new(&mut out)
        .write_image(image.as_raw(), width, height, ExtendedColorType::Rgb8)
        .unwrap();
    out.into_inner()
}

/// Adds an active member; returns its id.
fn add_member(t: &TestState, id: &str) -> String {
    control(t)
        .execute(
            "INSERT INTO users (id, email, role, quota_bytes, created_at)
             VALUES (?1, ?2, 'member', 0, 1)",
            params![id, format!("{}@example.test", id.to_lowercase())],
        )
        .unwrap();
    id.to_owned()
}

/// A migrate token for `user`, inserted directly.
fn member_token(t: &TestState, user: &str) -> String {
    let token = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    control(t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at)
             VALUES (?1, ?2, 'migrate', ?3, 'migrate', 1)",
            params![format!("T-{user}"), user, hash_token(&token).as_slice()],
        )
        .unwrap();
    token
}

#[tokio::test]
async fn admin_mints_a_migrate_token_that_expires() {
    let t = TestState::new();
    owner(&t);
    let minted = create_migrate_token(&t.data_dir(), OWNER_EMAIL, MIGRATE_TOKEN_TTL).unwrap();
    let token = minted.token.expose();
    assert!(token.starts_with("shx_"));
    let (kind, scopes, expires_at): (String, String, i64) = control(&t)
        .query_row(
            "SELECT kind, scopes, expires_at FROM api_tokens WHERE token_hash = ?1",
            [hash_token(token).as_slice()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!((kind.as_str(), scopes.as_str()), ("migrate", "migrate"));
    let week = i64::try_from(MIGRATE_TOKEN_TTL.as_millis()).unwrap();
    assert!((expires_at - now_ms() - week).abs() < 60_000);
    assert_eq!(minted.expires_at, expires_at);
    let audited: i64 = control(&t)
        .query_row(
            "SELECT count(*) FROM audit_log WHERE action = 'api_token.create'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(audited, 1);
    assert!(create_migrate_token(&t.data_dir(), "nobody@example.test", MIGRATE_TOKEN_TTL).is_err());
}

#[tokio::test]
async fn migration_routes_take_a_migrate_token_and_nothing_else() {
    let t = TestState::new();
    let app = t.app();
    let token = migrate_token(&t);
    let meta = metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(b"x"))]);

    // A token with the scope gets in.
    let response = send(&app, create(&token, 1, &meta)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let response = send(&app, bearer_get(&token, "/api/v1/migrations/preflight")).await;
    assert_eq!(response.status(), StatusCode::OK);

    // No token: 401 with the Bearer challenge, on every migration route (sent
    // as the web app would, so the CSRF guard lets it reach the access gate).
    for request in [
        create(&token, 1, &meta),
        bearer_get(&token, "/api/v1/migrations/preflight"),
        delete(&token, &location),
        head(&token, &location),
    ] {
        let mut anonymous = request;
        anonymous.headers_mut().remove(header::AUTHORIZATION);
        let uri = anonymous.uri().to_string();
        let response = send(&app, support::from_app(anonymous)).await;
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{uri}");
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }

    // A signed-in session is not enough: the routes are token-only.
    let cookie = sign_in(&app, &t).await;
    for (method, uri) in [
        (Method::GET, "/api/v1/migrations/1"),
        (Method::GET, "/api/v1/migrations/preflight"),
        (Method::POST, "/api/v1/migrations/missing-objects"),
        (Method::DELETE, location.as_str()),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header("tus-resumable", "1.0.0")
            .body(Body::from(r#"{"objects":[]}"#))
            .unwrap();
        let request = support::auth::from_spa(&t, with_session(request, &cookie));
        let refused = problem(send(&app, request).await, StatusCode::UNAUTHORIZED).await;
        assert_eq!(refused.code, ErrorCode::Unauthorized, "{uri}");
    }

    // A token without the scope: 403.
    let other = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    let owner_id = owner(&t);
    control(&t)
        .execute(
            "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at)
             VALUES ('T2', ?1, 'extension', ?2, 'ingest lookup', ?3)",
            params![owner_id, hash_token(&other).as_slice(), now_ms()],
        )
        .unwrap();
    for request in [
        create(&other, 1, &meta),
        bearer_get(&other, "/api/v1/migrations/preflight"),
    ] {
        let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
        assert_eq!(refused.code, ErrorCode::Forbidden);
    }

    // Another user's upload and install do not exist for this one.
    let member = add_member(&t, "01J9Z3B8K4QW6TFX0V7G2N5RCZ");
    let member_token = member_token(&t, &member);
    problem(
        send(&app, delete(&member_token, &location)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    let job = shelfy_server::jobs::migrate::enqueue(
        t.state.jobs(),
        &owner_id,
        &shelfy_server::jobs::migrate::Payload {
            db_upload_id: "U".into(),
            merge: false,
        },
    )
    .await
    .unwrap()
    .job;
    let mine = send(
        &app,
        bearer_get(&token, &format!("/api/v1/migrations/{}", job.id)),
    )
    .await;
    assert_eq!(mine.status(), StatusCode::OK);
    problem(
        send(
            &app,
            bearer_get(&member_token, &format!("/api/v1/migrations/{}", job.id)),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    // A job of another kind is no install.
    let usage = shelfy_server::jobs::usage::enqueue(t.state.jobs(), &owner_id)
        .await
        .unwrap()
        .job;
    problem(
        send(
            &app,
            bearer_get(&token, &format!("/api/v1/migrations/{}", usage.id)),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;

    // An expired token: 401.
    control(&t)
        .execute(
            "UPDATE api_tokens SET expires_at = ?1 WHERE kind = 'migrate' AND user_id = ?2",
            params![now_ms() - 1, owner_id],
        )
        .unwrap();
    let response = send(&app, create(&token, 1, &meta)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn uploads_follow_tus_resume_where_they_stopped_and_can_be_terminated() {
    let t = TestState::new();
    let app = t.app();
    let token = migrate_token(&t);
    let bytes = jpeg(64, 48, 7);
    let sha = sha256_hex(&bytes);
    let meta = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha),
        ("ext", "jpg"),
    ]);

    // Creation: tus version, length and metadata are checked.
    let mut no_tus = create(&token, bytes.len(), &meta);
    no_tus.headers_mut().remove("tus-resumable");
    problem(send(&app, no_tus).await, StatusCode::PRECONDITION_FAILED).await;
    let bad = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha),
        ("ext", "svg"),
    ]);
    problem(
        send(&app, create(&token, bytes.len(), &bad)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    problem(
        send(&app, create(&token, 301 * 1024 * 1024, &meta)).await,
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;
    let response = send(&app, create(&token, bytes.len(), &meta)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    assert_eq!(response.headers()["tus-resumable"], "1.0.0");
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let created = json(response).await;
    assert_eq!(
        location,
        format!("/api/v1/uploads/{}", created["id"].as_str().unwrap())
    );

    let response = send(&app, head(&token, &location)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(offset_of(&response), 0);
    assert_eq!(
        response.headers()["upload-length"],
        bytes.len().to_string().as_str()
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");

    // The first part, then a wrong offset, a wrong body type.
    let half = bytes.len() / 2;
    let response = send(&app, patch(&token, &location, 0, &bytes[..half])).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(offset_of(&response), half as u64);
    let conflict = problem(
        send(&app, patch(&token, &location, 0, &bytes[..half])).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(conflict.detail.unwrap().contains(&half.to_string()));
    let mut typed = patch(&token, &location, half, &bytes[half..]);
    typed
        .headers_mut()
        .insert(header::CONTENT_TYPE, "application/json".parse().unwrap());
    problem(send(&app, typed).await, StatusCode::UNSUPPORTED_MEDIA_TYPE).await;
    problem(
        send(
            &app,
            patch(
                &token,
                &location,
                half,
                &[bytes.clone(), bytes.clone()].concat(),
            ),
        )
        .await,
        StatusCode::PAYLOAD_TOO_LARGE,
    )
    .await;

    // HEAD tells where to resume; the rest completes the upload.
    let response = send(&app, head(&token, &location)).await;
    let resume = usize::try_from(offset_of(&response)).unwrap();
    assert_eq!(resume, half, "a refused chunk stores nothing");
    let response = send(&app, patch(&token, &location, resume, &bytes[resume..])).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert_eq!(offset_of(&response), bytes.len() as u64);
    let response = send(&app, head(&token, &location)).await;
    assert_eq!(offset_of(&response), bytes.len() as u64);
    let id = location.rsplit('/').next().unwrap();
    let done = t.data_dir().uploads_dir().join(id);
    assert_eq!(std::fs::read(&done).unwrap(), bytes);

    // Bytes that do not match the declared hash: the upload is dropped.
    let lie = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha256_hex(b"something else")),
        ("ext", "jpg"),
    ]);
    let response = send(&app, create(&token, bytes.len(), &lie)).await;
    let liar = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let refused = problem(
        send(&app, patch(&token, &liar, 0, &bytes)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.errors[0].field, "sha256");
    let response = send(&app, head(&token, &liar)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    // Termination: an unfinished upload and a complete one go, bytes too.
    let response = send(&app, create(&token, bytes.len(), &meta)).await;
    let unfinished = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    send(&app, patch(&token, &unfinished, 0, &bytes[..10])).await;
    for target in [&unfinished, &location] {
        let mut terminate = delete(&token, target);
        terminate.headers_mut().remove("tus-resumable");
        problem(send(&app, terminate).await, StatusCode::PRECONDITION_FAILED).await;
        let response = send(&app, delete(&token, target)).await;
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(response.headers()["tus-resumable"], "1.0.0");
        let response = send(&app, head(&token, target)).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        problem(
            send(&app, delete(&token, target)).await,
            StatusCode::NOT_FOUND,
        )
        .await;
    }
    assert!(!done.exists(), "the bytes are gone");
    let left: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);

    // Another user's upload does not exist for this one.
    let member = add_member(&t, "01J9Z3B8K4QW6TFX0V7G2N5RCZ");
    let member_token = member_token(&t, &member);
    let response = send(&app, create(&token, bytes.len(), &meta)).await;
    let mine = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let response = send(&app, head(&member_token, &mine)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    problem(
        send(&app, patch(&member_token, &mine, 0, &bytes)).await,
        StatusCode::NOT_FOUND,
    )
    .await;
}

#[tokio::test]
async fn missing_objects_are_those_neither_stored_nor_uploaded() {
    let t = TestState::new();
    let app = t.app();
    let token = migrate_token(&t);
    let (a, b) = (jpeg(16, 16, 1), jpeg(16, 16, 2));
    let refs = json!({"objects": [
        {"sha256": sha256_hex(&a), "ext": "jpg", "bytes": a.len()},
        {"sha256": sha256_hex(&b), "ext": "jpg", "bytes": b.len()},
    ]});
    let ask = |body: Value| {
        Request::post("/api/v1/migrations/missing-objects")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let missing = json(send(&app, ask(refs.clone())).await).await;
    assert_eq!(missing["missing"], json!([sha256_hex(&a), sha256_hex(&b)]));

    let meta = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha256_hex(&a)),
        ("ext", "jpg"),
    ]);
    let response = send(&app, create(&token, a.len(), &meta)).await;
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    send(&app, patch(&token, &location, 0, &a)).await;
    let missing = json(send(&app, ask(refs)).await).await;
    assert_eq!(missing["missing"], json!([sha256_hex(&b)]));

    let too_many: Vec<Value> = (0..501)
        .map(|_| json!({"sha256": sha256_hex(&a), "ext": "jpg", "bytes": 1}))
        .collect();
    problem(
        send(&app, ask(json!({ "objects": too_many }))).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    problem(
        send(
            &app,
            ask(json!({"objects": [{"sha256": "XYZ", "ext": "jpg", "bytes": 1}]})),
        )
        .await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
}

/// A small desktop library with real images: a carousel, a video post, an
/// Instagram post whose cover URL expired, an X post with only a preview, a
/// captured site, a manual AI edit, and the desktop settings in its
/// localStorage.
struct Desktop {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
}

const PK: &str = "3191575067010950169";

impl Desktop {
    fn empty() -> (tempfile::TempDir, PathBuf, PathBuf, Connection) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Shelfy");
        for sub in ["thumbnails", "images", "videos", "web", "previews"] {
            std::fs::create_dir_all(root.join("assets").join(sub)).unwrap();
        }
        let db = root.join("shelfy.sqlite");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(DESKTOP_SCHEMA_CURRENT).unwrap();
        (dir, root, db, c)
    }

    fn asset(root: &Path, relative: &str, bytes: &[u8]) -> String {
        std::fs::write(root.join("assets").join(relative), bytes).unwrap();
        format!("/Users/someone/Library/Application Support/Shelfy/assets/{relative}")
    }

    fn new() -> Desktop {
        let (dir, root, db, c) = Self::empty();
        let asset = |relative: &str, bytes: &[u8]| Self::asset(&root, relative, bytes);
        let now = now_ms() / 1000;
        let cover = asset("thumbnails/instagram-1.jpg", &jpeg(1080, 1350, 10));
        let slide0 = asset("images/instagram-1-0.jpg", &jpeg(1080, 1350, 10));
        let slide1 = asset("images/instagram-1-1.png", &png(600, 600, 20));
        c.execute(
            "INSERT INTO posts (id, platform, text, media_type, timestamp, thumbnail_path,
               image_path, imported_at, ai_tags, ai_status, ai_model)
             VALUES (?1, 'instagram', 'Lampada in vetro soffiato', 'carousel',
               '2024-01-01T10:00:00Z', ?2, ?3, ?4, '[\"Glass\"]', 'done', 'qwen2.5vl')",
            params![format!("{PK}_1"), cover, slide0, now],
        )
        .unwrap();
        for (position, local) in [(0, &slide0), (1, &slide1)] {
            c.execute(
                "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
                 VALUES (?1, ?2, 'image', 'https://scontent.cdninstagram.com/s.jpg', ?3)",
                params![format!("{PK}_1"), position, local],
            )
            .unwrap();
        }
        c.execute(
            "INSERT INTO post_tags (post_id, tag_norm, tag_form, tier) VALUES (?1, 'glass', 'Glass', 'general')",
            [format!("{PK}_1")],
        )
        .unwrap();
        let poster = asset("thumbnails/instagram-2.jpg", &jpeg(720, 1280, 30));
        let video = asset(
            "videos/instagram-2.mp4",
            b"\0\0\0\x18ftypisom\0\0\0\0isom-video",
        );
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_path, video_path, imported_at,
               ai_description, ai_status, ai_model)
             VALUES ('2_1', 'instagram', 'video', ?1, ?2, ?3, 'Written by hand', 'done',
               'manuale')",
            params![poster, video, now],
        )
        .unwrap();
        c.execute(
            "INSERT INTO post_media (post_id, position, media_type, source_url)
             VALUES ('2_1', 0, 'video', 'https://scontent.cdninstagram.com/v.jpg')",
            [],
        )
        .unwrap();
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_url, imported_at)
             VALUES ('3_1', 'instagram', 'image',
               'https://scontent.cdninstagram.com/c.jpg?oe=5F5E1000', ?1)",
            [now],
        )
        .unwrap();
        let preview = asset("previews/twitter-4.jpg", &jpeg(640, 480, 40));
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_url, preview_path, imported_at)
             VALUES ('1800000000000000004', 'twitter', 'image', 'https://pbs.twimg.com/a.jpg', ?1, ?2)",
            params![preview, now],
        )
        .unwrap();
        let hero = asset("web/1-hero.png", &png(1440, 900, 50));
        let favicon = asset("web/fav.png", &png(32, 32, 60));
        let url = "https://studio.example.test/";
        let pages = json!([{"url": url, "title": "Home", "screenshotPath": hero,
                            "hero": {"path": hero}, "contentText": "Selected work"}])
        .to_string();
        let meta = json!({"title": "Studio", "favicon": favicon}).to_string();
        c.execute(
            "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url,
               thumbnail_path, web_pages_json, web_meta_json, web_captured_at, imported_at)
             VALUES (?1, 'web', 'website', ?2, ?2, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                shelfy_core::ids::web::legacy_post_id(url),
                url,
                hero,
                pages,
                meta,
                now
            ],
        )
        .unwrap();
        c.execute_batch(&format!(
            "INSERT INTO collections (id, name, color) VALUES (1, 'Lamps', '#123456');
             INSERT INTO post_collections (post_id, collection_id) VALUES ('{PK}_1', 1);"
        ))
        .unwrap();
        drop(c);
        local_storage::write(
            &root,
            &[
                (
                    local_storage::key("file://", "app:language"),
                    local_storage::latin1("it"),
                ),
                (
                    local_storage::key("file://", "download:assetTypes"),
                    local_storage::latin1(r#"{"video":false}"#),
                ),
            ],
        );
        Desktop {
            _dir: dir,
            root,
            db,
        }
    }

    /// A second desktop library that shares posts with [`Desktop::new`]: the
    /// carousel with a note and a new folder, the X post now with a cover,
    /// slides and an analysis, the site with a newer version; and a new X
    /// post with a big slide.
    fn second() -> Desktop {
        let (dir, root, db, c) = Self::empty();
        let asset = |relative: &str, bytes: &[u8]| Self::asset(&root, relative, bytes);
        let now = now_ms() / 1000;
        c.execute(
            "INSERT INTO posts (id, platform, media_type, imported_at, user_note, user_tags)
             VALUES (?1, 'instagram', 'carousel', ?2, 'seen again', '[\"lighting\"]')",
            params![format!("{PK}_1"), now],
        )
        .unwrap();
        let x_cover = asset("thumbnails/twitter-4.jpg", &jpeg(1200, 800, 70));
        let x_slide = asset("images/twitter-4-0.jpg", &jpeg(1200, 800, 71));
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_url, thumbnail_path,
               image_path, imported_at, ai_description, ai_status, ai_model)
             VALUES ('1800000000000000004', 'twitter', 'image', 'https://pbs.twimg.com/a.jpg',
               ?1, ?2, ?3, 'A desk lamp', 'done', 'qwen2.5vl')",
            params![x_cover, x_slide, now],
        )
        .unwrap();
        c.execute(
            "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
             VALUES ('1800000000000000004', 0, 'image', 'https://pbs.twimg.com/a.jpg', ?1)",
            [&x_slide],
        )
        .unwrap();
        let big = asset("images/twitter-9-5.jpg", &big_jpeg_like(256 * 1024));
        let x9 = asset("thumbnails/twitter-9.jpg", &jpeg(800, 600, 80));
        c.execute(
            "INSERT INTO posts (id, platform, media_type, thumbnail_path, imported_at)
             VALUES ('1800000000000000009', 'twitter', 'images', ?1, ?2)",
            params![x9, now],
        )
        .unwrap();
        for position in 0..6 {
            let local = (position == 5).then(|| big.clone());
            c.execute(
                "INSERT INTO post_media (post_id, position, media_type, source_url, local_path)
                 VALUES ('1800000000000000009', ?1, 'image', 'https://pbs.twimg.com/b.jpg', ?2)",
                params![position, local],
            )
            .unwrap();
        }
        let url = "https://studio.example.test/";
        let hero = asset("web/2-hero.png", &png(1440, 900, 90));
        let pages = json!([{"url": url, "title": "Home, redesigned", "screenshotPath": hero,
                            "hero": {"path": hero}}])
        .to_string();
        c.execute(
            "INSERT INTO posts (id, platform, media_type, web_url, web_final_url, post_url,
               thumbnail_path, web_pages_json, web_captured_at, imported_at)
             VALUES (?1, 'web', 'website', ?2, ?2, ?2, ?3, ?4, ?5, ?5)",
            params![
                shelfy_core::ids::web::legacy_post_id(url),
                url,
                hero,
                pages,
                now + 3600
            ],
        )
        .unwrap();
        c.execute_batch(&format!(
            "INSERT INTO collections (id, name, color) VALUES (1, 'lamps', '#654321');
             INSERT INTO collections (id, name, color) VALUES (2, 'Chairs', '#111111');
             INSERT INTO post_collections (post_id, collection_id) VALUES ('{PK}_1', 1);
             INSERT INTO post_collections (post_id, collection_id) VALUES ('{PK}_1', 2);
             INSERT INTO post_collections (post_id, collection_id)
               VALUES ('1800000000000000009', 2);"
        ))
        .unwrap();
        drop(c);
        Desktop {
            _dir: dir,
            root,
            db,
        }
    }

    fn options(&self, server: &str, token: &str, work: &Path) -> RunOptions {
        let mut options = RunOptions::new(self.db.clone(), server, token, work.to_path_buf());
        options.media_root = Some(self.root.clone());
        options.poll_interval = Duration::from_millis(50);
        options
    }
}

/// `len` bytes that start like a JPEG: stored as an image object, never
/// decoded (a slide past the grid's first four).
fn big_jpeg_like(len: usize) -> Vec<u8> {
    let mut bytes = b"\xFF\xD8\xFF\xE0\0\x10JFIF\0".to_vec();
    let mut x: u32 = 0x1234_5678;
    while bytes.len() < len {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        bytes.push((x & 0xff) as u8);
    }
    bytes
}

/// Serves `app` on a local port; returns its origin.
async fn serve_app(app: Router) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    format!("http://{addr}")
}

/// Serves the application and runs its job scheduler; returns its origin.
async fn serve(t: &TestState) -> String {
    start_jobs(t);
    serve_app(t.app()).await
}

fn start_jobs(t: &TestState) {
    let scheduler = t
        .state
        .jobs()
        .start(t.state.clone(), CancellationToken::new());
    // The scheduler runs as long as the test.
    std::mem::forget(scheduler);
}

async fn run(options: RunOptions) -> anyhow::Result<RunOutcome> {
    tokio::task::spawn_blocking(move || {
        let mut log = Vec::new();
        shelfy_migrate::run::run(&options, &mut log)
    })
    .await
    .unwrap()
}

/// The `job.updated` events of `user`'s `migrate` jobs, until one ends.
async fn migrate_events(t: &TestState, user: &str) -> tokio::task::JoinHandle<Vec<Value>> {
    let mut events = t.state.events().subscribe(user, None);
    tokio::spawn(async move {
        let mut seen = Vec::new();
        loop {
            let next = tokio::time::timeout(Duration::from_secs(60), events.next()).await;
            let Ok(Delivery::Event(event)) = next else {
                break;
            };
            if event.topic != EventTopic::JobUpdated {
                continue;
            }
            let data: Value = serde_json::from_str(&event.data).unwrap();
            if data["kind"] != MIGRATE {
                continue;
            }
            let state = data["state"].as_str().unwrap_or_default().to_owned();
            seen.push(data);
            if matches!(state.as_str(), "succeeded" | "failed" | "cancelled") {
                break;
            }
        }
        seen
    })
}

fn migrate_jobs(t: &TestState) -> Vec<(i64, String, Option<String>, u32)> {
    control(t)
        .prepare(
            "SELECT id, state, error_code, attempts FROM jobs WHERE kind = 'migrate' ORDER BY id",
        )
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_library_is_installed_end_to_end() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
    let owner_id = owner(&t);
    let desktop = Desktop::new();
    let work = t.dir.path().join("work").join("migrate-cli");

    // An interrupted upload from an earlier run: the run continues it.
    let slide1 = desktop.root.join("assets/images/instagram-1-1.png");
    let (slide_sha, slide_len) = sha256_file(&slide1).unwrap();
    let bytes = std::fs::read(&slide1).unwrap();
    let url = {
        let (origin, token) = (origin.clone(), token.clone());
        tokio::task::spawn_blocking(move || {
            let client = Client::new(&origin, &token).unwrap();
            let meta = [
                ("purpose", "migration-object"),
                ("sha256", slide_sha.as_str()),
                ("ext", "png"),
            ];
            let url = client.create_upload(slide_len, &meta).unwrap();
            assert_eq!(client.append(&url, 0, &bytes[..100]).unwrap(), 100);
            (url, slide_sha)
        })
        .await
        .unwrap()
    };
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(
        work.join(STATE_FILE),
        json!({"uploads": {url.1.clone(): url.0}}).to_string(),
    )
    .unwrap();

    let events = migrate_events(&t, &owner_id).await;
    let outcome = run(desktop.options(&origin, &token, &work)).await.unwrap();
    assert!(outcome.matches, "{:#?}", outcome.reconciliation);
    assert_eq!(outcome.upload.resumed, 1);
    assert_eq!(outcome.upload.missing, outcome.upload.objects);
    let report = &outcome.report;
    assert_eq!(report.mode, "replace");
    let posts: u64 = report.installed.posts.values().sum();
    assert_eq!(posts, 5);
    assert_eq!(report.installed.posts["instagram"], 3);
    assert_eq!(report.installed.memberships, 1);
    assert_eq!(report.installed.post_tags, 1);
    assert_eq!(report.installed.web_captures, 1);
    // Covers (the carousel's is also its slide 0), slide 1 and the site hero.
    assert_eq!(report.renditions.rendered, 5);
    assert_eq!(report.renditions.failed, 0);
    assert_eq!(report.renditions.thumbhashes, 4);
    let covers = report.renditions.cover_bytes.unwrap();
    assert_eq!(covers.count, 4);
    assert!(covers.p50 > 0 && covers.p50 <= covers.p95 && covers.p95 <= covers.max);
    assert_eq!(report.objects.from_uploads, report.objects.total);
    assert_eq!(report.archive.ig_cover_expired, 1);
    assert_eq!(
        report.archive.by_state,
        [("client".to_owned(), 1), ("done".to_owned(), 4)].into()
    );
    assert_eq!(report.settings, ["archiveAssetTypes", "language"]);
    let previous = report.previous.clone().unwrap();
    assert!(previous.starts_with("library.prev-"), "{previous}");
    assert!(!work.exists(), "the work files are removed");

    // The job: queued, then its stages on job.updated, then succeeded.
    let jobs = migrate_jobs(&t);
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].1, "succeeded");
    assert_eq!(outcome.migration_id, jobs[0].0.to_string());
    // Events are held to one per 250 ms per job, so a quick install shows
    // some of its stages, in order, with growing progress.
    let seen = events.await.unwrap();
    let order = ["validating", "objects", "index", "report", "installing"];
    let stages: Vec<usize> = seen
        .iter()
        .filter(|e| e["state"] == "running")
        .filter_map(|e| e["stage"].as_str())
        .map(|stage| {
            order
                .iter()
                .position(|s| *s == stage)
                .expect("a known stage")
        })
        .collect();
    assert!(!stages.is_empty(), "{seen:?}");
    assert!(stages.windows(2).all(|w| w[0] <= w[1]), "{seen:?}");
    let progress: Vec<f64> = seen.iter().filter_map(|e| e["progress"].as_f64()).collect();
    assert!(progress.windows(2).all(|w| w[0] <= w[1]), "{progress:?}");
    assert_eq!(seen.last().unwrap()["state"], "succeeded");

    // The installed library through the API, signed in.
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let stats = json(send(&app, with_session(get("/api/v1/stats"), &cookie)).await).await;
    assert_eq!(stats["total"], 5);
    assert_eq!(stats["byPlatform"]["twitter"], 1);
    let page = json(send(&app, with_session(get("/api/v1/posts?limit=10"), &cookie)).await).await;
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let carousel = items
        .iter()
        .find(|p| p["key"] == format!("ig_{PK}"))
        .unwrap();
    assert!(carousel["thumbhash"].is_string());
    assert_eq!(carousel["archiveState"], "done");
    let g480 = carousel["cover"]["g480Url"].as_str().unwrap().to_owned();
    let response = send(&app, with_session(get(&g480), &cookie)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_TYPE], "image/webp");
    assert!(!body(response).await.is_empty());
    assert_eq!(carousel["cover"]["width"], 1080);
    let found =
        json(send(&app, with_session(get("/api/v1/search?q=lampada"), &cookie)).await).await;
    assert_eq!(found["items"][0]["key"], format!("ig_{PK}"));
    // The manual AI edit is the web's manual edit.
    let library = t.state.user_db(&owner_id).await.unwrap();
    let (model, provider, schema): (String, Option<String>, Option<i64>) = library
        .read(|c| {
            c.query_row(
                "SELECT ai_model, ai_provider, ai_schema_version FROM posts WHERE key = 'ig_2'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(shelfy_core::db::DbError::from)
        })
        .unwrap();
    assert_eq!((model.as_str(), provider, schema), ("manual", None, None));
    // The desktop settings, as `GET /me/settings` reads them.
    let settings = json(send(&app, with_session(get("/api/v1/me/settings"), &cookie)).await).await;
    assert_eq!(settings["language"], "it", "{settings}");
    assert_eq!(settings["archiveAssetTypes"]["video"], false, "{settings}");
    // The reconciliation is in the activity, once.
    let notes = json(send(&app, with_session(get("/api/v1/notifications"), &cookie)).await).await;
    let installed: Vec<&Value> = notes["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n["code"] == "migration.installed")
        .collect();
    assert_eq!(installed.len(), 1, "{notes}");
    let params = &installed[0]["params"];
    assert_eq!(params["mode"], "replace");
    assert_eq!(params["desktopPosts"], 5);
    assert_eq!(params["installedPosts"], 5);
    assert_eq!(params["matches"], true);
    assert_eq!(params["jobId"], jobs[0].0);
    // The status route has the report; a usage count was queued.
    let status = json(
        send(
            &app,
            bearer_get(&token, &format!("/api/v1/migrations/{}", jobs[0].0)),
        )
        .await,
    )
    .await;
    assert_eq!(status["state"], "succeeded");
    assert_eq!(status["report"]["installed"]["posts"]["web"], 1);
    let counted: i64 = control(&t)
        .query_row(
            "SELECT count(*) FROM jobs WHERE kind = 'usage.recompute'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(counted >= 1);

    // Uploads are consumed; the previous library is kept next to the new one.
    let uploads: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uploads, 0);
    let user_dir = t.data_dir().users_dir().join(&owner_id);
    assert!(user_dir.join(&previous).is_file());
    assert!(
        std::fs::read_dir(t.data_dir().migrations_dir())
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );

    // A second run without --merge stops before it uploads anything.
    let again = run(desktop.options(&origin, &token, &work))
        .await
        .unwrap_err();
    assert!(format!("{again:#}").contains("--merge"), "{again:#}");
    let uploads: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uploads, 0, "nothing was uploaded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_second_library_merges_into_one_that_is_not_empty() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
    let owner_id = owner(&t);
    let work = t.dir.path().join("work").join("migrate-cli");
    let first = run(Desktop::new().options(&origin, &token, &work))
        .await
        .unwrap();
    assert_eq!(first.report.mode, "replace");

    let second = Desktop::second();
    let mut options = second.options(&origin, &token, &work);
    options.merge = true;
    let merged = run(options.clone()).await.unwrap();
    assert!(merged.matches, "{:#?}", merged.reconciliation);
    let report = &merged.report;
    assert_eq!(report.mode, "merge");
    let m = report.merge.as_ref().unwrap();
    assert_eq!(m.posts["instagram"].merged, 1);
    assert_eq!(m.posts["twitter"].merged, 1);
    assert_eq!(m.posts["twitter"].inserted, 1);
    assert_eq!(m.posts["web"].merged, 1);
    // The X post's desktop row has a cover, a slide and an analysis: it wins.
    assert_eq!(m.replaced, 1);
    assert_eq!(m.notes_joined, 1);
    assert_eq!(m.tags_added, 1);
    // "lamps" is the library's "Lamps" folder; "Chairs" is new.
    assert_eq!((m.collections_matched, m.collections_inserted), (1, 1));
    assert_eq!((m.memberships_present, m.memberships_added), (1, 2));
    assert_eq!((m.captures_added, m.captures_present), (1, 0));
    assert_eq!(report.installed.posts.values().sum::<u64>(), 6);
    assert_eq!(report.installed.collections, 2);
    assert_eq!(report.installed.web_captures, 2);
    assert!(
        report
            .previous
            .as_deref()
            .unwrap()
            .starts_with("library.prev-")
    );

    let library = t.state.user_db(&owner_id).await.unwrap();
    let read = |sql: &'static str| -> Vec<String> {
        library
            .read(|c| {
                c.prepare(sql)
                    .unwrap()
                    .query_map([], |r| r.get(0))
                    .unwrap()
                    .collect::<rusqlite::Result<Vec<String>>>()
                    .map_err(shelfy_core::db::DbError::from)
            })
            .unwrap()
    };
    assert_eq!(
        read("SELECT user_note FROM posts WHERE user_note IS NOT NULL"),
        ["seen again"]
    );
    assert_eq!(
        read(
            "SELECT p.key || ':' || coalesce(p.ai_description, '') || ':' || p.archive_state
             FROM posts p WHERE p.platform = 'twitter' ORDER BY p.key"
        ),
        [
            "x_1800000000000000004:A desk lamp:done",
            "x_1800000000000000009::partial"
        ]
    );
    // The site keeps its current version (its row had files too) and gains
    // the newer one.
    let titles =
        read("SELECT c.title FROM web_captures c JOIN posts p ON p.current_capture_id = c.id");
    assert_eq!(titles.len(), 1);
    let thumbhashes = read(
        "SELECT key FROM posts WHERE cover_object IS NOT NULL AND thumbhash IS NULL ORDER BY key",
    );
    assert!(thumbhashes.is_empty(), "{thumbhashes:?}");
    let unreferenced =
        read("SELECT CAST(id AS TEXT) FROM media_objects WHERE unreferenced_since IS NOT NULL");
    assert_eq!(
        unreferenced.len(),
        1,
        "the X post's old preview lost its last reference"
    );

    // Merging the same library again changes nothing.
    let again = run(options).await.unwrap();
    assert!(again.matches, "{:#?}", again.reconciliation);
    let m = again.report.merge.as_ref().unwrap();
    assert_eq!(m.posts.values().map(|p| p.inserted).sum::<u64>(), 0);
    assert_eq!(m.unchanged, 4, "{m:?}");
    assert_eq!((m.collections_inserted, m.memberships_added), (0, 0));
    assert_eq!((m.captures_added, m.captures_present), (0, 1));
    assert_eq!(again.report.installed.posts.values().sum::<u64>(), 6);
    assert_eq!(again.upload.missing, 0, "every object is stored already");
    let notes: i64 = library
        .read(|c| {
            c.query_row(
                "SELECT count(*) FROM notifications WHERE code = 'migration.installed'",
                [],
                |r| r.get(0),
            )
            .map_err(shelfy_core::db::DbError::from)
        })
        .unwrap();
    assert_eq!(notes, 3, "one per install");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_tampered_bundle_is_refused_and_the_library_stays_as_it_was() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
    let desktop = Desktop::new();
    let work = t.dir.path().join("work").join("migrate-cli");
    let failed = {
        let (origin, token) = (origin.clone(), token.clone());
        let options = desktop.options(&origin, &token, &work);
        tokio::task::spawn_blocking(move || {
            // Build the bundle as `run` does, then add a trigger to its database.
            let legacy = shelfy_core::legacy::LegacyDb::open(&options.db).unwrap();
            let (_, mapping) = shelfy_migrate::plan::plan_with_mapping(
                &legacy,
                &shelfy_migrate::plan::PlanOptions {
                    media_root: options.media_root.clone(),
                    redact: true,
                    now_ms: now_ms(),
                },
            )
            .unwrap();
            std::fs::create_dir_all(&options.work_dir).unwrap();
            let bundle = shelfy_migrate::bundle::build(
                &legacy,
                &mapping,
                &options.work_dir,
                &shelfy_migrate::bundle::BundleOptions {
                    now_ms: now_ms(),
                    ..shelfy_migrate::bundle::BundleOptions::default()
                },
            )
            .unwrap();
            Connection::open(&bundle.db_path)
                .unwrap()
                .execute_batch("CREATE TRIGGER t AFTER INSERT ON posts BEGIN SELECT 1; END;")
                .unwrap();
            let client = Client::new(&origin, &token).unwrap();
            for object in &bundle.objects {
                let meta = [
                    ("purpose", "migration-object"),
                    ("sha256", object.sha256.as_str()),
                    ("ext", object.ext),
                ];
                let url = client.create_upload(object.bytes, &meta).unwrap();
                client
                    .append(&url, 0, &std::fs::read(&object.path).unwrap())
                    .unwrap();
            }
            let (sha, length) = sha256_file(&bundle.db_path).unwrap();
            let meta = [("purpose", "migration-db"), ("sha256", sha.as_str())];
            let url = client.create_upload(length, &meta).unwrap();
            client
                .append(&url, 0, &std::fs::read(&bundle.db_path).unwrap())
                .unwrap();
            let id = url.rsplit('/').next().unwrap();
            let mut status = client.start_migration(id, false, "tampered").unwrap();
            // The same key answers the same install.
            let replayed = client.start_migration(id, false, "tampered").unwrap();
            assert_eq!(replayed.id, status.id);
            while status.state == "running" {
                std::thread::sleep(Duration::from_millis(50));
                status = client.migration(&status.id).unwrap();
            }
            status
        })
        .await
        .unwrap()
    };
    assert_eq!(failed.state, "failed");
    let error = failed.error.unwrap();
    assert_eq!(error.code, "validation_failed");
    assert!(error.detail.unwrap().contains("triggers"));
    assert_eq!(
        (failed.attempts, failed.max_attempts),
        (1, 2),
        "a refused bundle is not tried again"
    );

    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let stats = json(send(&app, with_session(get("/api/v1/stats"), &cookie)).await).await;
    assert_eq!(stats["total"], 0, "the live library is untouched");
    let kept: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert!(kept > 0, "the uploads stay for a retry");
}

#[tokio::test]
async fn an_install_needs_a_complete_database_upload_and_an_empty_library_or_merge() {
    let t = TestState::new();
    let app = t.app();
    let token = migrate_token(&t);
    let start = |id: &str, merge: bool, key: Option<&str>| {
        let mut request = Request::post("/api/v1/migrations")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(key) = key {
            request = request.header("idempotency-key", key);
        }
        request
            .body(Body::from(
                json!({ "dbUploadId": id, "merge": merge }).to_string(),
            ))
            .unwrap()
    };
    let refused = problem(
        send(&app, start("01ARZ3NDEKTSV4RRFFQ69G5FAV", false, None)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.errors[0].field, "dbUploadId");

    // An unfinished database upload is not installable either.
    let meta = metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(b"db"))]);
    let response = send(&app, create(&token, 2, &meta)).await;
    let id = json(response).await["id"].as_str().unwrap().to_owned();
    problem(
        send(&app, start(&id, false, None)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // A complete one, into a library that already has posts: 409 without
    // merge.
    let db = b"SQLite format 3\0 is not enough, but the route checks the library first";
    let meta = metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(db))]);
    let response = send(&app, create(&token, db.len(), &meta)).await;
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        send(&app, patch(&token, &location, 0, db)).await.status(),
        StatusCode::NO_CONTENT
    );
    let owner_id = owner(&t);
    let library = t.state.user_db(&owner_id).await.unwrap();
    library
        .write(|tx| {
            let post = shelfy_core::repo::posts::NewPost::new(
                "x_1",
                shelfy_core::repo::Platform::Twitter,
                "1",
                "text",
                1,
            );
            shelfy_core::repo::posts::insert(tx, &post, 1)
        })
        .unwrap();
    let upload = location.rsplit('/').next().unwrap();
    let conflict = problem(
        send(&app, start(upload, false, None)).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(conflict.detail.unwrap().contains("--merge"));
    let preflight =
        json(send(&app, bearer_get(&token, "/api/v1/migrations/preflight")).await).await;
    assert_eq!(preflight["libraryEmpty"], false);
    assert_eq!(preflight["posts"], 1);
    assert_eq!(preflight["quotaBytes"], 0);

    // With merge, the job is queued (the scheduler is not running here); a
    // repeat with the same key gets the same answer back.
    let response = send(&app, start(upload, true, Some("k1"))).await;
    assert_eq!(response.status(), StatusCode::ACCEPTED);
    let location = response.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let queued = json(response).await;
    assert_eq!(queued["state"], "running");
    assert_eq!(queued["stage"], "queued");
    assert_eq!(queued["merge"], true);
    assert_eq!(queued["maxAttempts"], 2);
    assert_eq!(
        location,
        format!("/api/v1/migrations/{}", queued["id"].as_str().unwrap())
    );
    let replay = send(&app, start(upload, true, Some("k1"))).await;
    assert_eq!(replay.status(), StatusCode::ACCEPTED);
    assert_eq!(replay.headers()["idempotent-replayed"], "true");
    assert_eq!(json(replay).await["jobId"], queued["jobId"]);
    // Without the key, the same request finds the queued install.
    let same = json(send(&app, start(upload, true, None)).await).await;
    assert_eq!(same["jobId"], queued["jobId"]);
    // Another request while it is queued: 409.
    problem(
        send(&app, start(upload, false, Some("k2"))).await,
        StatusCode::CONFLICT,
    )
    .await;
    let preflight =
        json(send(&app, bearer_get(&token, "/api/v1/migrations/preflight")).await).await;
    assert_eq!(preflight["activeJobId"], queued["jobId"]);

    let unknown = problem(
        send(
            &app,
            bearer_get(&token, "/api/v1/migrations/01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::NotFound);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_install_waits_while_an_operator_holds_the_library_locked() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
    let owner_id = owner(&t);
    let desktop = Desktop::new();
    let work = t.dir.path().join("work").join("migrate-cli");
    // The library exists, then an operator locks it for a restore.
    drop(t.state.user_db(&owner_id).await.unwrap());
    shelfy_core::db::lock_library(&t.data_dir().users_dir(), &owner_id, "restore").unwrap();
    let options = desktop.options(&origin, &token, &work);
    // The preflight reads the library: the CLI hears it is locked at once.
    let refused = run(options).await.unwrap_err();
    assert!(
        format!("{refused:#}").contains("user_locked"),
        "{refused:#}"
    );

    // An install queued for an uploaded bundle finds the lock: its try ends
    // `user_locked`, a transient error, and the job goes back to the queue
    // without using a try, held until the operator is likely done (F4).
    shelfy_core::db::unlock_library(&t.data_dir().users_dir(), &owner_id).unwrap();
    let uploaded = upload_bundle(&origin, &token, &desktop).await;
    shelfy_core::db::lock_library(&t.data_dir().users_dir(), &owner_id, "restore").unwrap();
    let job = shelfy_server::jobs::migrate::enqueue(
        t.state.jobs(),
        &owner_id,
        &shelfy_server::jobs::migrate::Payload {
            db_upload_id: uploaded,
            merge: false,
        },
    )
    .await
    .unwrap()
    .job;
    // The try ends at once and puts the job back for later.
    let mut tried: Option<JobRow> = None;
    for _ in 0..600 {
        let row = t
            .state
            .jobs()
            .get(&owner_id, job.id)
            .await
            .unwrap()
            .unwrap();
        assert!(!row.state.is_final(), "the locked install ended: {row:?}");
        if row.state == JobState::Queued && row.run_at > job.run_at + 30_000 {
            tried = Some(row);
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let job = tried.expect("the install was tried and put back");
    assert_eq!(job.attempts, 0, "no try used: {job:?}");
    let posts: i64 = Connection::open(t.data_dir().library_db(&owner_id))
        .unwrap()
        .query_row("SELECT count(*) FROM posts", [], |r| r.get(0))
        .unwrap();
    assert_eq!(posts, 0, "the locked library is untouched");
}

/// Uploads `desktop`'s bundle as `run` does, without installing it; returns
/// the database upload's id.
async fn upload_bundle(origin: &str, token: &str, desktop: &Desktop) -> String {
    let (origin, token) = (origin.to_owned(), token.to_owned());
    let (db, root) = (desktop.db.clone(), desktop.root.clone());
    tokio::task::spawn_blocking(move || {
        let legacy = shelfy_core::legacy::LegacyDb::open(&db).unwrap();
        let (_, mapping) = shelfy_migrate::plan::plan_with_mapping(
            &legacy,
            &shelfy_migrate::plan::PlanOptions {
                media_root: Some(root),
                redact: true,
                now_ms: now_ms(),
            },
        )
        .unwrap();
        let out = tempfile::tempdir().unwrap();
        let bundle = shelfy_migrate::bundle::build(
            &legacy,
            &mapping,
            out.path(),
            &shelfy_migrate::bundle::BundleOptions {
                now_ms: now_ms(),
                ..shelfy_migrate::bundle::BundleOptions::default()
            },
        )
        .unwrap();
        let client = Client::new(&origin, &token).unwrap();
        for object in &bundle.objects {
            let meta = [
                ("purpose", "migration-object"),
                ("sha256", object.sha256.as_str()),
                ("ext", object.ext),
            ];
            let url = client.create_upload(object.bytes, &meta).unwrap();
            client
                .append(&url, 0, &std::fs::read(&object.path).unwrap())
                .unwrap();
        }
        let (sha, length) = sha256_file(&bundle.db_path).unwrap();
        let meta = [("purpose", "migration-db"), ("sha256", sha.as_str())];
        let url = client.create_upload(length, &meta).unwrap();
        client
            .append(&url, 0, &std::fs::read(&bundle.db_path).unwrap())
            .unwrap();
        url.rsplit('/').next().unwrap().to_owned()
    })
    .await
    .unwrap()
}

/// Holds the first `PATCH` at or past `offset` until the client is gone,
/// and tells the test it arrived.
#[derive(Default)]
struct Gate {
    armed: AtomicBool,
    offset: std::sync::atomic::AtomicU64,
    hit: Notify,
}

async fn gate(State(gate): State<Arc<Gate>>, request: AxumRequest, next: Next) -> Response {
    let offset: u64 = request
        .headers()
        .get("upload-offset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    if request.method() == Method::PATCH
        && gate.armed.load(Ordering::Acquire)
        && offset >= gate.offset.load(Ordering::Acquire)
    {
        gate.hit.notify_one();
        std::future::pending::<()>().await;
    }
    next.run(request).await
}

/// The child process of [`an_upload_killed_mid_way_continues_on_the_next_run`]:
/// runs the migration the CLI's way until the parent kills it.
#[test]
fn child_run_for_parent() {
    let Ok(options) = std::env::var("SHELFY_TEST_CHILD_RUN") else {
        return;
    };
    let options: Value = serde_json::from_str(&options).unwrap();
    let text = |name: &str| options[name].as_str().unwrap().to_owned();
    let mut run = RunOptions::new(
        PathBuf::from(text("db")),
        &text("server"),
        &text("token"),
        PathBuf::from(text("work")),
    );
    run.media_root = Some(PathBuf::from(text("root")));
    run.merge = true;
    run.chunk_bytes = usize::try_from(options["chunk"].as_u64().unwrap()).unwrap();
    let _ = shelfy_migrate::run::run(&run, &mut std::io::sink());
    panic!("the parent kills this process before the upload ends");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_upload_killed_mid_way_continues_on_the_next_run() {
    let t = TestState::new();
    start_jobs(&t);
    let gate_state = Arc::new(Gate::default());
    gate_state.offset.store(128 * 1024, Ordering::Release);
    gate_state.armed.store(true, Ordering::Release);
    let app = t.app().layer(axum::middleware::from_fn_with_state(
        Arc::clone(&gate_state),
        gate,
    ));
    let origin = serve_app(app).await;
    let token = migrate_token(&t);
    let desktop = Desktop::second();
    let work = t.dir.path().join("work").join("migrate-cli");
    let chunk = 64 * 1024;

    // The CLI, in a process of its own, uploads 64 KiB at a time; the big
    // slide (256 KiB) is held at 128 KiB, and the process is killed there.
    let options = json!({
        "db": desktop.db, "root": desktop.root, "server": origin, "token": token,
        "work": work, "chunk": chunk,
    });
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "child_run_for_parent",
            "--nocapture",
            "--test-threads=1",
        ])
        .env("SHELFY_TEST_CHILD_RUN", options.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(60), gate_state.hit.notified())
        .await
        .expect("the upload reached the gate");
    child.kill().unwrap();
    child.wait().unwrap();
    gate_state.armed.store(false, Ordering::Release);

    // What the server has: the big upload, unfinished at 128 KiB.
    let (offset, length): (i64, i64) = control(&t)
        .query_row(
            "SELECT upload_offset, length FROM uploads WHERE completed_at IS NULL",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((offset, length), (128 * 1024, 256 * 1024));
    assert!(work.join(STATE_FILE).is_file(), "the resume state survives");

    // The next run continues it, and the install completes.
    let mut options = desktop.options(&origin, &token, &work);
    options.merge = true;
    options.chunk_bytes = chunk;
    let outcome = run(options).await.unwrap();
    assert!(outcome.matches, "{:#?}", outcome.reconciliation);
    assert_eq!(outcome.upload.resumed, 1);
    assert_eq!(outcome.upload.missing, 1, "only the big slide was left");
    let big = sha256_file(&desktop.root.join("assets/images/twitter-9-5.jpg")).unwrap();
    let owner_id = owner(&t);
    let stored = t
        .data_dir()
        .users_dir()
        .join(&owner_id)
        .join("media")
        .join(&big.0[..2])
        .join(format!("{}.jpg", big.0));
    assert_eq!(
        sha256_file(&stored).unwrap(),
        big,
        "the bytes arrived whole"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn login_signs_the_cli_in_with_the_device_flow() {
    let t = TestState::with_config(|config| {
        config.auth.device_poll_interval = Duration::from_secs(1);
    });
    let origin = serve(&t).await;
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let token_file = t.dir.path().join("home/.config/shelfy-migrate/token");
    let (code_tx, mut code_rx) = tokio::sync::mpsc::unbounded_channel();
    let options = LoginOptions {
        server: origin.clone(),
        headers: vec![shelfy_migrate::client::Header::parse("X-Probe: 1").unwrap()],
        token_file: token_file.clone(),
        min_interval: Duration::from_millis(100),
        max_restarts: 0,
    };
    let login = tokio::task::spawn_blocking(move || {
        login::login(&options, &mut |event| {
            if let LoginEvent::Code(code) = event {
                code_tx.send(code.user_code.clone()).unwrap();
            }
        })
    });
    let user_code = code_rx.recv().await.unwrap();
    // The owner approves it on the `/device` page, signed in a moment ago.
    let request = Request::post("/api/v1/auth/device/approve")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(json!({ "userCode": user_code }).to_string()))
        .unwrap();
    let response = send(&app, spa(&t, request, &cookie)).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    let saved = login.await.unwrap().unwrap();
    assert_eq!(saved.path, token_file);
    assert!(saved.expires_at > now_ms());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&token_file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    let token = login::read_token(&token_file).unwrap();
    assert!(token.starts_with("shx_"));
    // The token works for the migration routes.
    let preflight = tokio::task::spawn_blocking(move || {
        Client::new(&origin, &token).unwrap().preflight().unwrap()
    })
    .await
    .unwrap();
    assert!(preflight.library_empty);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_housekeeping_removes_what_installs_left_behind() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
    let owner_id = owner(&t);
    let work = t.dir.path().join("work").join("migrate-cli");
    let outcome = run(Desktop::new().options(&origin, &token, &work))
        .await
        .unwrap();
    let previous = t
        .data_dir()
        .users_dir()
        .join(&owner_id)
        .join(outcome.report.previous.unwrap());
    assert!(previous.is_file());

    // Another user's unfinished upload, and a complete one nobody installed.
    let member = add_member(&t, "01J9Z3B8K4QW6TFX0V7G2N5RCZ");
    let member_token = member_token(&t, &member);
    let app = t.app();
    let bytes = jpeg(16, 16, 3);
    let meta = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha256_hex(&bytes)),
        ("ext", "jpg"),
    ]);
    for complete in [false, true] {
        let response = send(&app, create(&member_token, bytes.len(), &meta)).await;
        let location = response.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .to_owned();
        let end = if complete { bytes.len() } else { 10 };
        send(&app, patch(&member_token, &location, 0, &bytes[..end])).await;
    }
    // A work directory a crashed install left.
    let stale = t.data_dir().migrations_dir().join("424242");
    std::fs::create_dir_all(&stale).unwrap();

    // Now: only the stale work directory goes.
    let now = now_ms();
    let swept = housekeeping::sweep(&t.state, now).await;
    assert_eq!(
        (swept.previous, swept.uploads, swept.work_dirs),
        (0, 0, 1),
        "{swept:?}"
    );
    assert!(previous.is_file());

    // Eight days later the previous library and both uploads go too.
    let later = now + 8 * 86_400_000;
    let swept = housekeeping::sweep(&t.state, later).await;
    assert_eq!((swept.previous, swept.uploads), (1, 2), "{swept:?}");
    assert!(!previous.exists());
    let left: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(left, 0);
    let files = std::fs::read_dir(t.data_dir().uploads_dir())
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(files, 0);
}
