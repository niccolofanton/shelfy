//! Migration v0 (T9): `admin migrate-token`, the tus upload routes, the
//! missing-objects check and the install, end to end with `shelfy-migrate
//! run` against a server on a local port. Every library here is synthetic.

mod support;

use std::io::Cursor;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
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
use shelfy_migrate::run::{RunOptions, STATE_FILE};
use shelfy_server::admin::migrate_token::{MIGRATE_TOKEN_TTL, create_migrate_token};
use shelfy_server::error::ErrorCode;
use shelfy_server::ids::now_ms;
use shelfy_server::tokens::hash_token;
use support::auth::{OWNER_EMAIL, owner, sign_in, with_session};
use support::{TestState, body, get, json, problem, send};

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
    Connection::open(t.data_dir().control_db()).unwrap()
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

    // No token: 401 with the Bearer challenge (sent as the web app would, so
    // the CSRF guard lets it reach the access gate).
    let mut anonymous = create(&token, 1, &meta);
    anonymous.headers_mut().remove(header::AUTHORIZATION);
    let response = send(&app, support::from_app(anonymous)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;

    // A signed-in session is not enough: the routes are token-only.
    let cookie = sign_in(&app, &t).await;
    for (method, uri) in [
        (Method::GET, "/api/v1/migrations/01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        (Method::POST, "/api/v1/migrations/missing-objects"),
    ] {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
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
    let refused = problem(
        send(&app, create(&other, 1, &meta)).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::Forbidden);

    // An expired token: 401.
    control(&t)
        .execute(
            "UPDATE api_tokens SET expires_at = ?1 WHERE kind = 'migrate'",
            [now_ms() - 1],
        )
        .unwrap();
    let response = send(&app, create(&token, 1, &meta)).await;
    assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    problem(response, StatusCode::UNAUTHORIZED).await;
}

#[tokio::test]
async fn uploads_follow_tus_and_resume_where_they_stopped() {
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

    // Another user's upload does not exist for this one.
    let member = "01J9Z3B8K4QW6TFX0V7G2N5RCZ";
    let member_token = format!(
        "shx_{}",
        shelfy_server::tokens::SecretToken::generate().expose()
    );
    let c = control(&t);
    c.execute(
        "INSERT INTO users (id, email, role, quota_bytes, created_at)
         VALUES (?1, 'member@example.test', 'member', 0, 1)",
        [member],
    )
    .unwrap();
    c.execute(
        "INSERT INTO api_tokens (id, user_id, kind, token_hash, scopes, created_at)
         VALUES ('T9', ?1, 'migrate', ?2, 'migrate', 1)",
        params![member, hash_token(&member_token).as_slice()],
    )
    .unwrap();
    let response = send(&app, head(&member_token, &location)).await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    problem(
        send(&app, patch(&member_token, &location, 0, &bytes)).await,
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
/// Instagram post whose cover URL expired, an X post with only a preview,
/// and a captured site.
struct Desktop {
    _dir: tempfile::TempDir,
    root: PathBuf,
    db: PathBuf,
}

const PK: &str = "3191575067010950169";

impl Desktop {
    fn new() -> Desktop {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("Shelfy");
        for sub in ["thumbnails", "images", "videos", "web", "previews"] {
            std::fs::create_dir_all(root.join("assets").join(sub)).unwrap();
        }
        let db = root.join("shelfy.sqlite");
        let c = Connection::open(&db).unwrap();
        c.execute_batch(DESKTOP_SCHEMA_CURRENT).unwrap();
        let asset = |relative: &str, bytes: &[u8]| {
            std::fs::write(root.join("assets").join(relative), bytes).unwrap();
            format!("/Users/someone/Library/Application Support/Shelfy/assets/{relative}")
        };
        let now = now_ms() / 1000;
        let cover = asset("thumbnails/instagram-1.jpg", &jpeg(1080, 1350, 10));
        let slide0 = asset("images/instagram-1-0.jpg", &jpeg(1080, 1350, 10));
        let slide1 = asset("images/instagram-1-1.png", &png(600, 600, 20));
        c.execute(
            "INSERT INTO posts (id, platform, text, media_type, timestamp, thumbnail_path,
               image_path, imported_at, ai_tags, ai_status)
             VALUES (?1, 'instagram', 'Lampada in vetro soffiato', 'carousel',
               '2024-01-01T10:00:00Z', ?2, ?3, ?4, '[\"Glass\"]', 'done')",
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
            "INSERT INTO posts (id, platform, media_type, thumbnail_path, video_path, imported_at)
             VALUES ('2_1', 'instagram', 'video', ?1, ?2, ?3)",
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
        Desktop {
            _dir: dir,
            root,
            db,
        }
    }

    fn options(&self, server: &str, token: &str, work: &Path) -> RunOptions {
        RunOptions {
            db: self.db.clone(),
            media_root: Some(self.root.clone()),
            server: server.to_owned(),
            token: token.to_owned(),
            work_dir: work.to_path_buf(),
            with_videos: false,
            merge: false,
            keep_work: false,
            list_orphans: false,
            poll_interval: Duration::from_millis(50),
        }
    }
}

/// Serves the application on a local port; returns its origin.
async fn serve(t: &TestState) -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = t.app();
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

async fn run(options: RunOptions) -> anyhow::Result<shelfy_migrate::run::RunOutcome> {
    tokio::task::spawn_blocking(move || {
        let mut log = Vec::new();
        shelfy_migrate::run::run(&options, &mut log)
    })
    .await
    .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_desktop_library_is_installed_end_to_end() {
    let t = TestState::new();
    let origin = serve(&t).await;
    let token = migrate_token(&t);
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

    let outcome = run(desktop.options(&origin, &token, &work)).await.unwrap();
    assert!(outcome.matches, "{:#?}", outcome.reconciliation);
    assert_eq!(outcome.upload.resumed, 1);
    assert_eq!(outcome.upload.missing, outcome.upload.objects);
    let report = &outcome.report;
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
    assert_eq!(report.objects.from_uploads, report.objects.total);
    assert_eq!(report.archive.ig_cover_expired, 1);
    assert_eq!(
        report.archive.by_state,
        [("client".to_owned(), 1), ("done".to_owned(), 4)].into()
    );
    assert!(!work.exists(), "the work files are removed");

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
    let notes = json(send(&app, with_session(get("/api/v1/notifications"), &cookie)).await).await;
    assert!(
        notes["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["code"] == "migration.installed"),
        "{notes}"
    );

    // Uploads are consumed; the previous library is kept next to the new one.
    let uploads: i64 = control(&t)
        .query_row("SELECT count(*) FROM uploads", [], |r| r.get(0))
        .unwrap();
    assert_eq!(uploads, 0);
    let owner_id = owner(&t);
    let user_dir = t.data_dir().users_dir().join(&owner_id);
    let previous: Vec<String> = std::fs::read_dir(&user_dir)
        .unwrap()
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|name| name.starts_with("library.prev-"))
        .collect();
    assert_eq!(previous.len(), 1, "{previous:?}");
    assert!(
        std::fs::read_dir(t.data_dir().migrations_dir())
            .map(|d| d.count())
            .unwrap_or(0)
            == 0
    );

    // A second run finds every object stored and the library no longer empty.
    let again = run(desktop.options(&origin, &token, &work))
        .await
        .unwrap_err();
    assert!(format!("{again:#}").contains("conflict"), "{again:#}");
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
                    with_videos: false,
                    snapshot: false,
                    now_ms: now_ms(),
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
            let mut status = client
                .start_migration(url.rsplit('/').next().unwrap(), false)
                .unwrap();
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
async fn an_install_needs_a_complete_database_upload_and_an_empty_library() {
    let t = TestState::new();
    let app = t.app();
    let token = migrate_token(&t);
    let start = |id: &str| {
        Request::post("/api/v1/migrations")
            .header(header::AUTHORIZATION, format!("Bearer {token}"))
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(json!({ "dbUploadId": id }).to_string()))
            .unwrap()
    };
    let refused = problem(
        send(&app, start("01ARZ3NDEKTSV4RRFFQ69G5FAV")).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.errors[0].field, "dbUploadId");

    // An unfinished database upload is not installable either.
    let meta = metadata(&[("purpose", "migration-db"), ("sha256", &sha256_hex(b"db"))]);
    let response = send(&app, create(&token, 2, &meta)).await;
    let id = json(response).await["id"].as_str().unwrap().to_owned();
    problem(
        send(&app, start(&id)).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;

    // A complete one, into a library that already has posts: 409.
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
    let conflict = problem(
        send(&app, start(location.rsplit('/').next().unwrap())).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert!(conflict.detail.unwrap().contains("not empty"));
    let unknown = problem(
        send(
            &app,
            Request::get("/api/v1/migrations/01ARZ3NDEKTSV4RRFFQ69G5FAV")
                .header(header::AUTHORIZATION, format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(unknown.code, ErrorCode::NotFound);
}
