//! The extension's tasks and uploads (P2-14; plan §2.13, §2.16; contracts
//! C6, C10) through the real middleware stack:
//!
//! - tasks are derived from `client` posts: `upload_media` per wanted asset
//!   with a valid URL, one `refresh_media` per Instagram post with expired
//!   URLs, `hydrate_link` for an Instagram post without media; failed,
//!   backed-off, trashed and server-side posts give none; `waiting` counts
//!   per platform;
//! - a lease is one poller's for 5 minutes; the same poller gets its tasks
//!   again;
//! - the long poll wakes up when an ingest batch hands posts to the
//!   extension, waits out its `wait` otherwise, and ends at shutdown;
//! - `uploaded` stores a tus `archive-object` upload into the slot (origin
//!   `extension`, the post's state derived again), once: completions are
//!   idempotent; bytes that do not match their SHA-256 never get there;
//! - `failed`, `skipped` and `refreshed` use a try with backoff, `gone`
//!   fails the items, a hydration's last try fails its post;
//! - authz: the `tasks` scope, the version gate, each upload pairing, and
//!   one user's tasks never reaching another.

mod support;

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use image::RgbImage;
use image::codecs::jpeg::JpegEncoder;
use rusqlite::{Connection, OptionalExtension as _, params};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};
use shelfy_core::ingest::archive::{ArchiveMode, FETCH_TRIES};
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, NewMedia, NewPost};
use shelfy_core::repo::settings::{self, ArchiveAssetTypes, SettingsChange};
use shelfy_server::control::uploads as upload_rows;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::EventTopic;
use shelfy_server::extension::VERSION_HEADER;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{sign_in, spa};
use support::library::{ALICE, BOB};
use support::{TestState, get, json, post_json, problem, send};

type LeaseCache = HashMap<(String, String), String>;
static LEASES: OnceLock<Mutex<LeaseCache>> = OnceLock::new();
fn leases() -> &'static Mutex<LeaseCache> {
    LEASES.get_or_init(Mutex::default)
}

const VERSION: &str = "0.2.0";
const X_CDN: &str = "https://pbs.twimg.com/media";
const IG_CDN: &str = "https://scontent-mxp1-1.cdninstagram.com/v";

/// A state where Instagram and X media are the extension's (mode
/// `client`), with Alice and Bob.
fn bench() -> TestState {
    let t = TestState::with_config(|config| {
        config.archive.modes.instagram = ArchiveMode::Client;
        config.archive.modes.twitter = ArchiveMode::Client;
    });
    t.add_user(ALICE);
    t.add_user(BOB);
    t
}

/// A token of `user` with `kind` and `scopes`.
fn token(t: &TestState, user: &str, kind: &str, scopes: &str) -> String {
    let token = format!("shx_{}", SecretToken::generate().expose());
    t.control()
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

/// An extension token with its four scopes.
fn ext_token(t: &TestState, user: &str) -> String {
    token(t, user, "extension", "ingest tasks uploads lookup")
}

/// `request` as the extension sends it.
fn ext(mut request: Request<Body>, token: &str) -> Request<Body> {
    request.headers_mut().insert(
        header::AUTHORIZATION,
        format!("Bearer {token}").parse().unwrap(),
    );
    request
        .headers_mut()
        .insert(VERSION_HEADER, VERSION.parse().unwrap());
    request
}

fn json_request(method: Method, uri: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

/// `GET /ingest/tasks` with `query`, as `token`; expects 200.
async fn poll(app: &Router, token: &str, query: &str) -> Value {
    let response = send(
        app,
        ext(get(&format!("/api/v1/ingest/tasks{query}")), token),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK, "poll {query}");
    let body = json(response).await;
    for task in body["tasks"].as_array().unwrap() {
        leases().lock().unwrap().insert(
            (token.to_owned(), task["id"].as_str().unwrap().to_owned()),
            task["leaseId"].as_str().unwrap().to_owned(),
        );
    }
    body
}

fn ids(polled: &Value) -> Vec<String> {
    polled["tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|task| task["id"].as_str().unwrap().to_owned())
        .collect()
}

/// `POST /ingest/tasks/{id}/complete` with `body`, as `token`.
async fn complete(
    app: &Router,
    token: &str,
    id: &str,
    mut body: Value,
) -> axum::response::Response<Body> {
    if body.get("leaseId").is_none() {
        let key = (token.to_owned(), id.to_owned());
        if !leases().lock().unwrap().contains_key(&key) {
            poll(app, token, "").await;
        }
        body["leaseId"] = leases()
            .lock()
            .unwrap()
            .get(&key)
            .cloned()
            .unwrap_or_default()
            .into();
    }
    let uri = format!("/api/v1/ingest/tasks/{id}/complete");
    send(app, ext(json_request(Method::POST, &uri, &body), token)).await
}

async fn complete_ok(app: &Router, token: &str, id: &str, body: Value) {
    let response = complete(app, token, id, body).await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT, "complete {id}");
}

fn post(key: &str, platform: Platform, media_type: &str, state: &str) -> NewPost {
    let native = key.split_once('_').unwrap().1;
    let mut post = NewPost::new(key, platform, native, media_type, now_ms() - 60_000);
    post.archive_state = Some(state.to_owned());
    post
}

fn slide(kind: &str, url: &str) -> NewMedia {
    NewMedia {
        kind: kind.to_owned(),
        source_url: Some(url.to_owned()),
        ..NewMedia::default()
    }
}

/// An X image carousel of three slides; the cover is slide 0's URL.
fn x_carousel(key: &str, state: &str) -> NewPost {
    let mut post = post(key, Platform::Twitter, "carousel", state);
    post.cover_url = Some(format!("{X_CDN}/{key}-0.jpg"));
    post.media = (0..3)
        .map(|n| slide("image", &format!("{X_CDN}/{key}-{n}.jpg")))
        .collect();
    post
}

/// An Instagram URL whose `oe` is `seconds` from now.
fn ig_url(name: &str, seconds: i64) -> String {
    format!(
        "{IG_CDN}/{name}.jpg?stp=dst-jpg&oe={:X}&oh=00_x",
        now_ms() / 1000 + seconds
    )
}

async fn seed(t: &TestState, user: &str, posts: Vec<NewPost>) {
    t.write(user, move |tx| {
        for post in &posts {
            posts::insert(tx, post, post.imported_at)?;
        }
        Ok(())
    })
    .await;
}

fn library(t: &TestState, user: &str) -> Connection {
    let conn = Connection::open(t.data_dir().library_db(user)).unwrap();
    conn.busy_timeout(Duration::from_secs(5)).unwrap();
    conn
}

fn state_of(t: &TestState, key: &str) -> String {
    library(t, ALICE)
        .query_row(
            "SELECT archive_state FROM posts WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .unwrap()
}

/// `(fetch_attempts, fetch_next_at IS NOT NULL, fetch_error)` of a slide.
fn tries(t: &TestState, key: &str, position: i64) -> (i64, bool, Option<String>) {
    library(t, ALICE)
        .query_row(
            "SELECT m.fetch_attempts, m.fetch_next_at IS NOT NULL, m.fetch_error
             FROM post_media m JOIN posts p ON p.id = m.post_id
             WHERE p.key = ?1 AND m.position = ?2",
            params![key, position],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
}

/// Makes every backed-off item of Alice due again.
fn make_due(t: &TestState) {
    let conn = library(t, ALICE);
    conn.execute("UPDATE post_media SET fetch_next_at = NULL", [])
        .unwrap();
    conn.execute("UPDATE posts SET cover_fetch_next_at = NULL", [])
        .unwrap();
}

/// Alice's library of every case of the derivation.
async fn seed_cases(t: &TestState) {
    let mut expired = post("ig_2", Platform::Instagram, "carousel", "client");
    expired.cover_url = Some(ig_url("c2", -3600));
    expired.media = vec![
        slide("image", &ig_url("s2a", -3600)),
        slide("image", &ig_url("s2b", -60)),
    ];
    let hydrate = post("ig_3", Platform::Instagram, "image", "client");
    let mut gone = x_carousel("x_5", "client");
    gone.media.truncate(1);
    let backed_off = {
        let mut post = x_carousel("x_6", "client");
        post.media.truncate(1);
        post
    };
    let trashed = x_carousel("x_7", "client");
    seed(
        t,
        ALICE,
        vec![
            x_carousel("x_1", "client"),
            expired,
            hydrate,
            x_carousel("x_4", "pending"),
            gone,
            backed_off,
            trashed,
        ],
    )
    .await;
    let conn = library(t, ALICE);
    conn.execute(
        "UPDATE post_media SET fetch_error = 'gone'
         WHERE post_id = (SELECT id FROM posts WHERE key = 'x_5')",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE post_media SET fetch_attempts = 1, fetch_next_at = ?1
         WHERE post_id = (SELECT id FROM posts WHERE key = 'x_6')",
        [now_ms() + 3_600_000],
    )
    .unwrap();
    conn.execute(
        "UPDATE posts SET deleted_at = ?1 WHERE key = 'x_7'",
        [now_ms()],
    )
    .unwrap();
}

#[tokio::test]
async fn tasks_are_derived_from_the_posts_handed_to_the_extension() {
    let t = bench();
    seed_cases(&t).await;
    let app = t.app();
    let token = ext_token(&t, ALICE);

    let polled = poll(&app, &token, "").await;
    assert_eq!(
        ids(&polled),
        [
            "upload_media.x_1.cover",
            "upload_media.x_1.1",
            "upload_media.x_1.2",
            "refresh_media.ig_2.post",
            "hydrate_link.ig_3.post",
        ]
    );
    assert_eq!(
        polled["waiting"],
        json!({"instagram": 2, "twitter": 3, "pinterest": 0})
    );
    let tasks = polled["tasks"].as_array().unwrap();
    let cover = &tasks[0];
    assert_eq!(cover["kind"], "upload_media");
    assert_eq!(cover["platform"], "twitter");
    assert_eq!(cover["postKey"], "x_1");
    assert_eq!(cover["nativeId"], "1");
    assert_eq!(cover["url"], format!("{X_CDN}/x_1-0.jpg"));
    assert_eq!(cover["position"], Value::Null, "the cover");
    assert_eq!(cover["postUrl"], "https://x.com/i/status/1");
    assert!(cover["leaseUntil"].as_i64().unwrap() > now_ms() + 4 * 60_000);
    assert_eq!(tasks[1]["position"], 1);
    let refresh = &tasks[3];
    assert_eq!(
        (&refresh["url"], &refresh["position"]),
        (&Value::Null, &Value::Null)
    );
    assert!(refresh["expiresAt"].as_i64().unwrap() < now_ms() - 3_000_000);
    assert_eq!(
        tasks[4]["postUrl"], "https://www.instagram.com/p/D/",
        "a link from the pk"
    );

    // The asset types narrow the uploads: without images, the cover only.
    library(&t, ALICE)
        .execute(
            "INSERT OR REPLACE INTO settings (key, value_json, updated_at)
             VALUES ('archiveAssetTypes', '{\"thumbnail\":true,\"image\":false,\"video\":false}', 1)",
            [],
        )
        .unwrap();
    let other = ext_token(&t, ALICE);
    let polled = poll(&app, &other, "").await;
    assert_eq!(
        ids(&polled),
        ["refresh_media.ig_2.post"],
        "the narrowed refresh has new work; unchanged tasks remain leased"
    );
    assert_eq!(
        polled["waiting"],
        json!({"instagram": 2, "twitter": 1, "pinterest": 0})
    );
}

#[tokio::test]
async fn a_lease_is_one_pollers_and_the_limit_holds() {
    let t = bench();
    seed(
        &t,
        ALICE,
        vec![x_carousel("x_1", "client"), x_carousel("x_2", "client")],
    )
    .await;
    let app = t.app();
    let (first, second) = (ext_token(&t, ALICE), ext_token(&t, ALICE));

    let a = poll(&app, &first, "?limit=4").await;
    assert_eq!(ids(&a).len(), 4);
    let b = poll(&app, &second, "?limit=20").await;
    assert_eq!(ids(&b), ["upload_media.x_2.1", "upload_media.x_2.2"]);
    assert!(ids(&a).iter().all(|id| !ids(&b).contains(id)), "exclusive");
    // The first poller gets its own tasks again, the second nothing new.
    assert_eq!(ids(&poll(&app, &first, "?limit=20").await), ids(&a));
    assert_eq!(ids(&poll(&app, &second, "").await), ids(&b));
    // A completed task's lease ends; the others stay.
    complete_ok(
        &app,
        &first,
        "upload_media.x_1.1",
        json!({"outcome": "skipped", "errorCode": "no_tab"}),
    )
    .await;
    make_due(&t);
    assert_eq!(ids(&poll(&app, &second, "").await).len(), 3);

    // Bad paging values.
    for query in ["?limit=0", "?limit=51", "?wait=26"] {
        let response = send(
            &app,
            ext(get(&format!("/api/v1/ingest/tasks{query}")), &first),
        )
        .await;
        let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(refused.code, ErrorCode::ValidationFailed, "{query}");
    }
}

/// An Instagram capture page whose post is handed to the extension (mode
/// `client`).
fn ig_page() -> Value {
    json!([{
        "id": "3191575067010950169_25025320",
        "shortcode": "CxKwJ0fLmQZ",
        "postUrl": "https://www.instagram.com/p/CxKwJ0fLmQZ/",
        "text": "A lamp",
        "thumbnailUrl": "https://scontent.cdninstagram.com/v/1.jpg?oe=7FFFFF00",
        "mediaType": "image",
        "media": [{"type": "image", "url": "https://scontent.cdninstagram.com/v/1.jpg?oe=7FFFFF00"}],
    }])
}

#[tokio::test]
async fn the_long_poll_wakes_up_when_ingest_hands_posts_over() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    // Nothing to do: the wait runs out.
    let started = Instant::now();
    let polled = poll(&app, &token, "?wait=1").await;
    assert!(ids(&polled).is_empty());
    assert!(started.elapsed() >= Duration::from_millis(900));

    let waiting = {
        let (app, token) = (app.clone(), token.clone());
        tokio::spawn(async move {
            let started = Instant::now();
            (poll(&app, &token, "?wait=25").await, started.elapsed())
        })
    };
    tokio::time::sleep(Duration::from_millis(300)).await;
    let run = {
        let body = json!({
            "platform": "instagram", "trigger": "manual",
            "listing": {"kind": "ig_saved", "externalId": null, "name": null},
            "collection": {"mode": "none"},
        });
        let response = send(
            &app,
            ext(
                json_request(Method::POST, "/api/v1/sync-runs", &body),
                &token,
            ),
        )
        .await;
        assert_eq!(response.status(), StatusCode::CREATED);
        json(response).await["id"].as_str().unwrap().to_owned()
    };
    let batch = json!({
        "syncRunId": run, "platform": "instagram", "source": "scroll",
        "hasNextPage": false, "items": ig_page(),
    });
    let mut request = ext(
        json_request(Method::POST, "/api/v1/ingest/batches", &batch),
        &token,
    );
    request
        .headers_mut()
        .insert("idempotency-key", new_ulid().parse().unwrap());
    assert_eq!(send(&app, request).await.status(), StatusCode::OK);

    let (polled, waited) = tokio::time::timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the poll woke up")
        .unwrap();
    assert!(waited < Duration::from_secs(10), "{waited:?}");
    assert_eq!(ids(&polled), ["upload_media.ig_3191575067010950169.cover"]);
}

#[tokio::test]
async fn the_long_poll_ends_at_shutdown() {
    let t = bench();
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let waiting = tokio::spawn(async move { poll(&app, &token, "?wait=25").await });
    tokio::time::sleep(Duration::from_millis(200)).await;
    t.state.shutdown_token().cancel();
    let polled = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the poll ended")
        .unwrap();
    assert!(ids(&polled).is_empty());
}

// ── Uploads ──────────────────────────────────────────────────────────────────

fn jpeg(seed: u8) -> Vec<u8> {
    let image = RgbImage::from_fn(64, 48, |x, y| {
        image::Rgb([seed, (x * 4 % 256) as u8, (y * 5 % 256) as u8])
    });
    let mut out = Vec::new();
    JpegEncoder::new_with_quality(&mut out, 85)
        .encode_image(&image)
        .unwrap();
    out
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

fn archive_object(sha256: &str) -> String {
    metadata(&[
        ("purpose", "archive-object"),
        ("sha256", sha256),
        ("ext", "jpg"),
    ])
}

fn tus_create(length: usize, meta: &str) -> Request<Body> {
    Request::post("/api/v1/uploads")
        .header("tus-resumable", "1.0.0")
        .header("upload-length", length.to_string())
        .header("upload-metadata", meta)
        .body(Body::empty())
        .unwrap()
}

fn tus_patch(location: &str, bytes: &[u8]) -> Request<Body> {
    Request::patch(location)
        .header("tus-resumable", "1.0.0")
        .header("upload-offset", "0")
        .header(header::CONTENT_TYPE, "application/offset+octet-stream")
        .body(Body::from(bytes.to_vec()))
        .unwrap()
}

/// Uploads `bytes` as an `archive-object` declared with `sha256`; returns
/// the upload id and the status of the last `PATCH`.
async fn upload(app: &Router, token: &str, bytes: &[u8], sha256: &str) -> (String, StatusCode) {
    let created = send(
        app,
        ext(tus_create(bytes.len(), &archive_object(sha256)), token),
    )
    .await;
    assert_eq!(created.status(), StatusCode::CREATED);
    let location = created.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    let patched = send(app, ext(tus_patch(&location, bytes), token)).await;
    (
        location.rsplit('/').next().unwrap().to_owned(),
        patched.status(),
    )
}

/// `(cover_object, slide 0's object, slide 1's object)` of `key`.
fn objects(t: &TestState, key: &str) -> (Option<i64>, Option<i64>, Option<i64>) {
    let conn = library(t, ALICE);
    let slide = |position: i64| -> Option<i64> {
        conn.query_row(
            "SELECT m.object_id FROM post_media m JOIN posts p ON p.id = m.post_id
             WHERE p.key = ?1 AND m.position = ?2",
            params![key, position],
            |r| r.get(0),
        )
        .optional()
        .unwrap()
        .flatten()
    };
    let cover = conn
        .query_row(
            "SELECT cover_object FROM posts WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .unwrap();
    (cover, slide(0), slide(1))
}

#[tokio::test]
async fn an_upload_is_stored_into_its_slot_once() {
    let t = bench();
    let mut single = x_carousel("x_1", "client");
    single.media.truncate(2);
    seed(&t, ALICE, vec![single]).await;
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let mut events = t.state.events().subscribe(ALICE, None);
    assert_eq!(
        ids(&poll(&app, &token, "").await),
        ["upload_media.x_1.cover", "upload_media.x_1.1"]
    );

    let cover = jpeg(1);
    let (cover_upload, status) = upload(&app, &token, &cover, &sha256_hex(&cover)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let done = json!({"outcome": "uploaded", "uploadId": cover_upload});
    complete_ok(&app, &token, "upload_media.x_1.cover", done.clone()).await;
    let (cover_object, slide0, slide1) = objects(&t, "x_1");
    assert!(cover_object.is_some());
    assert_eq!(slide0, cover_object, "slide 0 shares the cover's object");
    assert_eq!(slide1, None);
    let conn = library(&t, ALICE);
    let (origin, variants, thumbhash): (String, i64, bool) = conn
        .query_row(
            "SELECT o.origin, o.variants, p.thumbhash IS NOT NULL
             FROM media_objects o JOIN posts p ON p.cover_object = o.id WHERE p.key = 'x_1'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(origin, "extension");
    assert_ne!(variants, 0, "a g480");
    assert!(thumbhash);
    assert_eq!(state_of(&t, "x_1"), "client", "slide 1 is left");
    let mut announced = false;
    while let Ok(Delivery::Event(event)) =
        tokio::time::timeout(Duration::from_millis(500), events.next()).await
    {
        if event.topic == EventTopic::PostsChanged {
            let data: Value = serde_json::from_str(&event.data).unwrap();
            announced |= data["reason"] == "archive" && data["keys"] == json!(["x_1"]);
        }
    }
    assert!(announced, "posts.changed with reason archive");

    // Idempotent: the same completion again changes nothing.
    complete_ok(&app, &token, "upload_media.x_1.cover", done).await;
    assert_eq!(objects(&t, "x_1").0, cover_object);
    // Another upload for the filled slot is taken, and nothing changes.
    let again = jpeg(2);
    let (late, _) = upload(&app, &token, &again, &sha256_hex(&again)).await;
    complete_ok(
        &app,
        &token,
        "upload_media.x_1.cover",
        json!({"outcome": "uploaded", "uploadId": late}),
    )
    .await;
    assert_eq!(objects(&t, "x_1").0, cover_object);

    // The last slide: the post is done, and no task is left.
    let last = jpeg(3);
    let (slide_upload, _) = upload(&app, &token, &last, &sha256_hex(&last)).await;
    complete_ok(
        &app,
        &token,
        "upload_media.x_1.1",
        json!({"outcome": "uploaded", "uploadId": slide_upload}),
    )
    .await;
    assert!(objects(&t, "x_1").2.is_some());
    assert_eq!(state_of(&t, "x_1"), "done");
    assert!(ids(&poll(&app, &token, "").await).is_empty());
    // Two objects: the cover (shared with slide 0) and slide 1; the late
    // upload added none.
    let (count,): (i64,) = conn
        .query_row("SELECT count(*) FROM media_objects", [], |r| {
            Ok((r.get(0)?,))
        })
        .unwrap();
    assert_eq!(count, 2);
}

#[tokio::test]
async fn bad_uploads_never_reach_the_library() {
    let t = bench();
    seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let bob = ext_token(&t, BOB);
    let task = "upload_media.x_1.cover";

    // Bytes that do not match their declared SHA-256: refused at their last
    // byte, and the upload is gone.
    let bytes = jpeg(1);
    let (mismatch, status) = upload(&app, &token, &bytes, &sha256_hex(b"other")).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Bob's upload, an unfinished one, a missing one: 422 on uploadId.
    let (bobs, _) = upload(&app, &bob, &bytes, &sha256_hex(&bytes)).await;
    let unfinished = {
        let created = send(
            &app,
            ext(
                tus_create(bytes.len(), &archive_object(&sha256_hex(&bytes))),
                &token,
            ),
        )
        .await;
        created.headers()[header::LOCATION]
            .to_str()
            .unwrap()
            .rsplit('/')
            .next()
            .unwrap()
            .to_owned()
    };
    for upload_id in [
        mismatch.as_str(),
        &bobs,
        &unfinished,
        "01J00000000000000000000000",
    ] {
        let refused = problem(
            complete(
                &app,
                &token,
                task,
                json!({"outcome": "uploaded", "uploadId": upload_id}),
            )
            .await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(refused.errors[0].field, "uploadId", "{upload_id}");
    }
    // Without an upload id, or with an outcome the kind does not have.
    for body in [
        json!({"outcome": "uploaded"}),
        json!({"outcome": "refreshed"}),
    ] {
        let refused = problem(
            complete(&app, &token, task, body).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(refused.code, ErrorCode::ValidationFailed);
    }
    // Not a task id.
    for id in ["nope", "upload_media.x_1.post", "download.x_1.cover"] {
        let refused = problem(
            complete(&app, &token, id, json!({"outcome": "gone"})).await,
            StatusCode::NOT_FOUND,
        )
        .await;
        assert_eq!(refused.code, ErrorCode::NotFound, "{id}");
    }
    assert_eq!(objects(&t, "x_1").0, None);

    // An upload used for another slot, then named for this one, still
    // empty: 409 `upload_consumed`.
    let (used, _) = upload(&app, &token, &bytes, &sha256_hex(&bytes)).await;
    complete_ok(
        &app,
        &token,
        "upload_media.x_1.1",
        json!({"outcome": "uploaded", "uploadId": used}),
    )
    .await;
    let refused = problem(
        complete(
            &app,
            &token,
            "upload_media.x_1.2",
            json!({"outcome": "uploaded", "uploadId": used}),
        )
        .await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::UploadConsumed);
}

// ── Outcomes ─────────────────────────────────────────────────────────────────

#[tokio::test]
async fn failures_use_a_try_with_backoff_and_gone_fails_for_good() {
    let t = bench();
    let mut expired = post("ig_2", Platform::Instagram, "carousel", "client");
    expired.cover_url = Some(ig_url("c2", -3600));
    expired.media = vec![slide("image", &ig_url("s2a", -3600))];
    seed(
        &t,
        ALICE,
        vec![
            x_carousel("x_1", "client"),
            expired,
            post("ig_3", Platform::Instagram, "image", "client"),
            post("ig_4", Platform::Instagram, "image", "client"),
        ],
    )
    .await;
    let app = t.app();
    let token = ext_token(&t, ALICE);
    poll(&app, &token, "").await;

    // `failed`: one try, backed off, so the task leaves; a repeat is a no-op.
    let failed = json!({"outcome": "failed", "errorCode": "HTTP_403"});
    complete_ok(&app, &token, "upload_media.x_1.1", failed.clone()).await;
    assert_eq!(tries(&t, "x_1", 1), (1, true, Some("ext_http_403".into())));
    complete_ok(&app, &token, "upload_media.x_1.1", failed.clone()).await;
    assert_eq!(tries(&t, "x_1", 1).0, 1, "idempotent");
    assert!(!ids(&poll(&app, &token, "").await).contains(&"upload_media.x_1.1".to_owned()));
    // The last try fails the item; the post stays the extension's for the rest.
    for _ in 1..FETCH_TRIES {
        make_due(&t);
        poll(&app, &token, "").await;
        complete_ok(&app, &token, "upload_media.x_1.1", failed.clone()).await;
    }
    assert_eq!(
        tries(&t, "x_1", 1),
        (FETCH_TRIES, false, Some("ext_http_403".into()))
    );
    make_due(&t);
    let left = ids(&poll(&app, &token, "").await);
    assert!(!left.contains(&"upload_media.x_1.1".to_owned()));
    assert!(left.contains(&"upload_media.x_1.2".to_owned()));
    // `gone` fails the item at once.
    complete_ok(
        &app,
        &token,
        "upload_media.x_1.2",
        json!({"outcome": "gone"}),
    )
    .await;
    assert_eq!(tries(&t, "x_1", 2).2.as_deref(), Some("gone"));

    // `refreshed` while the URLs are still expired: a try.
    complete_ok(
        &app,
        &token,
        "refresh_media.ig_2.post",
        json!({"outcome": "refreshed"}),
    )
    .await;
    assert_eq!(
        tries(&t, "ig_2", 0),
        (1, true, Some("ext_refreshed".into()))
    );

    // A hydration: `gone` fails the post; the last failed try does too.
    complete_ok(
        &app,
        &token,
        "hydrate_link.ig_3.post",
        json!({"outcome": "gone"}),
    )
    .await;
    assert_eq!(state_of(&t, "ig_3"), "failed");
    for _ in 0..FETCH_TRIES {
        make_due(&t);
        poll(&app, &token, "").await;
        complete_ok(
            &app,
            &token,
            "hydrate_link.ig_4.post",
            json!({"outcome": "skipped"}),
        )
        .await;
    }
    assert_eq!(state_of(&t, "ig_4"), "failed");
    make_due(&t);
    let left = ids(&poll(&app, &token, "").await);
    assert!(
        left.iter().all(|id| !id.starts_with("hydrate_link")),
        "{left:?}"
    );
}

#[tokio::test]
async fn a_refresh_that_brought_new_data_ends_its_task() {
    let t = bench();
    let mut expired = post("ig_2", Platform::Instagram, "image", "client");
    expired.cover_url = Some(ig_url("c2", -3600));
    expired.media = vec![slide("image", &ig_url("s2a", -3600))];
    seed(
        &t,
        ALICE,
        vec![
            expired,
            post("ig_3", Platform::Instagram, "image", "client"),
        ],
    )
    .await;
    let app = t.app();
    let token = ext_token(&t, ALICE);
    assert_eq!(
        ids(&poll(&app, &token, "").await),
        ["refresh_media.ig_2.post", "hydrate_link.ig_3.post"]
    );
    complete_ok(
        &app,
        &token,
        "hydrate_link.ig_3.post",
        json!({"outcome": "failed"}),
    )
    .await;

    // The refresh batches merged new URLs (as the merge does: tries reset)
    // and media for the hydrated post.
    let conn = library(&t, ALICE);
    conn.execute(
        "UPDATE post_media SET source_url = ?1, source_url_expires_at = NULL, fetch_attempts = 0, fetch_next_at = NULL, fetch_error = NULL
         WHERE post_id = (SELECT id FROM posts WHERE key = 'ig_2')",
        [ig_url("s2new", 86_400)],
    )
    .unwrap();
    conn.execute(
        "UPDATE posts SET cover_url = ?1, cover_url_expires_at = NULL, cover_fetch_attempts = 0, cover_fetch_next_at = NULL, cover_fetch_error = NULL WHERE key IN ('ig_2', 'ig_3')",
        [ig_url("c-new", 86_400)],
    )
    .unwrap();
    for id in ["refresh_media.ig_2.post", "hydrate_link.ig_3.post"] {
        complete_ok(&app, &token, id, json!({"outcome": "refreshed"})).await;
    }
    assert_eq!(tries(&t, "ig_2", 0), (0, false, None), "no try used");
    let cover_tries: (i64, Option<String>) = conn
        .query_row(
            "SELECT cover_fetch_attempts, cover_fetch_error FROM posts WHERE key = 'ig_3'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(cover_tries, (0, None), "the hydration's tries are cleared");
    // Now the server's (mode client here: uploads).
    let left = ids(&poll(&app, &token, "").await);
    assert_eq!(left, ["upload_media.ig_2.cover", "upload_media.ig_3.cover"]);
}

// ── Authz ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_routes_need_a_tasks_token_and_the_version() {
    let t = bench();
    seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
    let app = t.app();
    let complete_uri = "/api/v1/ingest/tasks/upload_media.x_1.1/complete";
    let body = json!({"outcome": "skipped"});

    // No credentials (past the CSRF guard, as the web app sends it): 401
    // with the Bearer challenge.
    for request in [
        get("/api/v1/ingest/tasks"),
        post_json(complete_uri, body.to_string()),
    ] {
        let response = send(&app, request).await;
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        problem(response, StatusCode::UNAUTHORIZED).await;
    }
    // A token without `tasks` (ingest only; the Shortcut's): 403.
    for token in [
        token(&t, ALICE, "extension", "ingest uploads lookup"),
        token(&t, ALICE, "shortcut", "links:create"),
    ] {
        for request in [
            get("/api/v1/ingest/tasks"),
            json_request(Method::POST, complete_uri, &body),
        ] {
            let refused = problem(
                send(&app, ext(request, &token)).await,
                StatusCode::FORBIDDEN,
            )
            .await;
            assert_eq!(refused.code, ErrorCode::Forbidden);
        }
    }
    // An extension token without the version header: 426.
    let token = ext_token(&t, ALICE);
    let mut request = ext(get("/api/v1/ingest/tasks"), &token);
    request.headers_mut().remove(VERSION_HEADER);
    assert_eq!(
        problem(send(&app, request).await, StatusCode::UPGRADE_REQUIRED)
            .await
            .code,
        ErrorCode::ExtensionOutdated
    );
    // A session never reaches the tasks.
    let cookie = sign_in(&app, &t).await;
    let response = send(&app, spa(&t, get("/api/v1/ingest/tasks"), &cookie)).await;
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn archive_objects_take_an_uploads_token_only() {
    let t = bench();
    let app = t.app();
    let cookie = sign_in(&app, &t).await;
    let owner = support::auth::owner(&t);
    let bytes = jpeg(1);
    let meta = archive_object(&sha256_hex(&bytes));

    // A session and a migrate token: 403. An extension token: 201.
    let refused = problem(
        send(&app, spa(&t, tus_create(bytes.len(), &meta), &cookie)).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert!(refused.detail.unwrap().contains("uploads scope"));
    let migrate = token(&t, &owner, "migrate", "migrate");
    let refused = problem(
        send(&app, ext(tus_create(bytes.len(), &meta), &migrate)).await,
        StatusCode::FORBIDDEN,
    )
    .await;
    assert!(refused.detail.unwrap().contains("uploads scope"));
    let extension = ext_token(&t, &owner);
    let created = send(&app, ext(tus_create(bytes.len(), &meta), &extension)).await;
    assert_eq!(created.status(), StatusCode::CREATED);
    // The extension's `uploads` token cannot upload migration objects.
    let migration = metadata(&[
        ("purpose", "migration-object"),
        ("sha256", &sha256_hex(&bytes)),
        ("ext", "jpg"),
    ]);
    let response = send(&app, ext(tus_create(bytes.len(), &migration), &extension)).await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    // The rules of the purpose: images only, 15 MiB, a declared hash.
    for meta in [
        metadata(&[("purpose", "archive-object"), ("ext", "jpg")]),
        metadata(&[
            ("purpose", "archive-object"),
            ("sha256", &sha256_hex(&bytes)),
            ("ext", "mp4"),
        ]),
    ] {
        let response = send(&app, ext(tus_create(bytes.len(), &meta), &extension)).await;
        assert_eq!(
            response.status(),
            StatusCode::UNPROCESSABLE_ENTITY,
            "{meta}"
        );
    }
    let response = send(
        &app,
        ext(tus_create(15 * 1024 * 1024 + 1, &meta), &extension),
    )
    .await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    // The version gate covers the uploads too.
    let mut request = ext(tus_create(bytes.len(), &meta), &extension);
    request.headers_mut().remove(VERSION_HEADER);
    assert_eq!(
        send(&app, request).await.status(),
        StatusCode::UPGRADE_REQUIRED
    );
}

#[tokio::test]
async fn one_users_tasks_never_reach_another() {
    let t = bench();
    seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
    let app = t.app();
    let alice = ext_token(&t, ALICE);
    let bob = ext_token(&t, BOB);
    assert!(ids(&poll(&app, &bob, "").await).is_empty());
    // Bob naming Alice's task changes nothing in her library.
    problem(
        complete(&app, &bob, "upload_media.x_1.1", json!({"outcome": "gone"})).await,
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(tries(&t, "x_1", 1), (0, false, None));
    assert_eq!(ids(&poll(&app, &alice, "").await).len(), 3);
}

#[tokio::test]
async fn forged_tasks_cannot_attach_an_upload_to_manual_web_or_unleased_posts() {
    let t = bench();
    seed(
        &t,
        ALICE,
        vec![
            x_carousel("x_1", "client"),
            post("manual_1", Platform::Manual, "image", "link_only"),
            post("web_1", Platform::Web, "website", "link_only"),
            x_carousel("x_2", "pending"),
        ],
    )
    .await;
    let app = t.app();
    let token = ext_token(&t, ALICE);
    let polled = poll(&app, &token, "").await;
    let generation = polled["tasks"][0]["leaseId"].as_str().unwrap();
    let bytes = jpeg(7);
    let (upload_id, _) = upload(&app, &token, &bytes, &sha256_hex(&bytes)).await;
    for key in ["manual_1", "web_1", "x_2"] {
        let id = format!("upload_media.{key}.cover");
        problem(
            complete(
                &app,
                &token,
                &id,
                json!({"outcome":"uploaded", "uploadId": upload_id, "leaseId": generation}),
            )
            .await,
            StatusCode::CONFLICT,
        )
        .await;
        assert_eq!(objects(&t, key).0, None);
    }
    let consumed = upload_rows::get(&t.control(), ALICE, &upload_id)
        .unwrap()
        .unwrap()
        .is_consumed();
    assert!(!consumed, "invalid lease never claims the upload");
}

#[tokio::test]
async fn a_user_change_or_new_url_invalidates_an_in_flight_upload() {
    for change in ["trash", "policy", "removed", "url"] {
        let t = bench();
        seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
        let app = t.app();
        let token = ext_token(&t, ALICE);
        poll(&app, &token, "").await;
        let bytes = jpeg(8);
        let (upload_id, _) = upload(&app, &token, &bytes, &sha256_hex(&bytes)).await;
        let conn = library(&t, ALICE);
        match change {
            "trash" => {
                conn.execute(
                    "UPDATE posts SET deleted_at = ?1 WHERE key = 'x_1'",
                    [now_ms()],
                )
                .unwrap();
            }
            "policy" => {
                settings::update(
                    &conn,
                    &SettingsChange {
                        archive_asset_types: Some(ArchiveAssetTypes {
                            thumbnail: false,
                            image: false,
                            video: false,
                        }),
                        ..SettingsChange::default()
                    },
                    now_ms(),
                )
                .unwrap();
            }
            "removed" => {
                conn.execute(
                    "UPDATE posts SET archive_state = 'link_only' WHERE key = 'x_1'",
                    [],
                )
                .unwrap();
            }
            "url" => {
                conn.execute("UPDATE post_media SET source_url = 'https://pbs.twimg.com/media/new.jpg' WHERE position = 0", []).unwrap();
            }
            _ => unreachable!(),
        }
        complete_ok(
            &app,
            &token,
            "upload_media.x_1.cover",
            json!({"outcome":"uploaded","uploadId":upload_id}),
        )
        .await;
        assert_eq!(objects(&t, "x_1").0, None, "{change} wins");
        let count: i64 = conn
            .query_row("SELECT count(*) FROM media_objects", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{change} leaves no recorded object");
        let consumed = upload_rows::get(&t.control(), ALICE, &upload_id)
            .unwrap()
            .unwrap()
            .is_consumed();
        assert!(consumed, "obsolete upload discarded");
        assert!(!t.data_dir().uploads_dir().join(&upload_id).exists());
        assert_eq!(t.state.quota().reserved(ALICE), 0);
    }
}

#[tokio::test]
async fn only_the_current_holder_and_generation_can_complete_any_outcome() {
    let t = bench();
    seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
    let app = t.app();
    let first = ext_token(&t, ALICE);
    let second = ext_token(&t, ALICE);
    let old = poll(&app, &first, "").await;
    let task = "upload_media.x_1.cover";
    let old_id = old["tasks"][0]["leaseId"].as_str().unwrap();
    assert_eq!(
        old["tasks"][0]["leaseId"],
        poll(&app, &first, "").await["tasks"][0]["leaseId"],
        "renewal keeps generation"
    );
    let bytes = jpeg(9);
    let (upload_id, _) = upload(&app, &first, &bytes, &sha256_hex(&bytes)).await;
    for outcome in ["uploaded", "gone", "failed", "skipped"] {
        problem(
            complete(
                &app,
                &second,
                task,
                json!({"outcome":outcome,"leaseId":old_id,"uploadId":upload_id}),
            )
            .await,
            StatusCode::CONFLICT,
        )
        .await;
    }
    assert_eq!(tries(&t, "x_1", 0).0, 0);
    t.state.extension().tasks().sweep(now_ms() + 6 * 60_000);
    let new = poll(&app, &second, "").await;
    assert_ne!(old["tasks"][0]["leaseId"], new["tasks"][0]["leaseId"]);
    for outcome in ["uploaded", "gone", "failed", "skipped"] {
        problem(
            complete(
                &app,
                &first,
                task,
                json!({"outcome":outcome,"leaseId":old_id,"uploadId":upload_id}),
            )
            .await,
            StatusCode::CONFLICT,
        )
        .await;
    }
    assert_eq!(objects(&t, "x_1").0, None);
    complete_ok(
        &app,
        &second,
        task,
        json!({"outcome":"uploaded","uploadId":upload_id}),
    )
    .await;
    assert!(objects(&t, "x_1").0.is_some());
}

#[tokio::test]
async fn completion_reserves_the_prepared_bytes_and_discards_quota_refusals() {
    for allowed in [true, false] {
        let t = bench();
        seed(&t, ALICE, vec![x_carousel("x_1", "client")]).await;
        let app = t.app();
        let token = ext_token(&t, ALICE);
        poll(&app, &token, "").await;
        let bytes = jpeg(10);
        let (upload_id, _) = upload(&app, &token, &bytes, &sha256_hex(&bytes)).await;
        t.state.quota().record_count(ALICE, 0, 0).unwrap();
        t.control()
            .execute(
                "UPDATE users SET quota_bytes = ?1 WHERE id = ?2",
                params![if allowed { 1024 * 1024 } else { 1 }, ALICE],
            )
            .unwrap();
        let result = complete(
            &app,
            &token,
            "upload_media.x_1.cover",
            json!({"outcome":"uploaded","uploadId":upload_id}),
        )
        .await;
        if allowed {
            assert_eq!(result.status(), StatusCode::NO_CONTENT);
            assert!(objects(&t, "x_1").0.is_some());
        } else {
            assert_eq!(
                problem(result, StatusCode::FORBIDDEN).await.code,
                ErrorCode::QuotaExceeded
            );
            assert_eq!(objects(&t, "x_1").0, None);
            assert_eq!(state_of(&t, "x_1"), "link_only");
        }
        let consumed = upload_rows::get(&t.control(), ALICE, &upload_id)
            .unwrap()
            .unwrap()
            .is_consumed();
        assert!(consumed);
        assert!(!t.data_dir().uploads_dir().join(&upload_id).exists());
        assert_eq!(t.state.quota().reserved(ALICE), 0);
    }
}
