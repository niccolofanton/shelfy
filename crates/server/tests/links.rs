//! `POST /links` and `link.hydrate` (P2-11) through the real middleware
//! stack and the job scheduler, against the fixture CDN standing in for the
//! platforms' public endpoints (synthetic answers shaped like SPIKE-9's):
//!
//! - the route: the JSON the `/share` page sends, a known key (tags
//!   united, note joined), web links (`link_only`, tracking dropped),
//!   hostile URLs, `pin.it` short links (resolved within the allowlist, a
//!   redirect off it refused before anything is sent there);
//! - the authz rules every new route ships: 401 without credentials, the
//!   CSRF guard on sessions, a `links:create` token (the iOS Shortcut)
//!   accepted without it, a token without the scope refused, the Shortcut
//!   token refused on cookie-only routes, and one user's links never
//!   reaching another's library;
//! - the hydration: X (`tweet-result`, a deleted tweet, the oEmbed
//!   fallback), Pinterest (`PinResource`, the pidgets fallback) and
//!   Instagram (the post page, a gated post handed to the extension, a 429
//!   that trips the breaker and hands the post over).

mod support;

use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Method, Request, StatusCode, header};
use rusqlite::params;
use serde_json::{Value, json};
use shelfy_server::control::jobs::JobRow;
use shelfy_server::error::ErrorCode;
use shelfy_server::events::Delivery;
use shelfy_server::events::model::{EventTopic, JobState};
use shelfy_server::extension::VERSION_HEADER;
use shelfy_server::ids::{new_ulid, now_ms};
use shelfy_server::jobs::Scheduler;
use shelfy_server::jobs::hydrate::{pinterest, x};
use shelfy_server::outbound::{BreakerState, HostGroup};
use shelfy_server::tokens::{SecretToken, hash_token};
use support::auth::{owner, sign_in, spa, with_session};
use support::cdn::{Answer, FixtureCdn};
use support::library::{ALICE, BOB};
use support::{TestState, get, json, problem, send};
use tokio_util::sync::CancellationToken;

const LINKS: &str = "/api/v1/links";
/// `CuZLd-iMknW` is the code of this pk.
const IG_KEY: &str = "ig_3141592653589793238";
const IG_CODE: &str = "CuZLd-iMknW";
const TWEET: &str = "1700000000000000001";
const PIN: &str = "987654321012345678";

/// The platform hosts the fixture answers for.
const HOSTS: &[&str] = &[
    "www.instagram.com",
    "cdn.syndication.twimg.com",
    "publish.x.com",
    "www.pinterest.com",
    "widgets.pinterest.com",
    "pin.it",
    "api.pinterest.com",
];

struct Bench {
    t: TestState,
    cdn: FixtureCdn,
    _scheduler: Option<Scheduler>,
}

/// A state whose outbound client reaches the fixture for [`HOSTS`] only,
/// with fast pacing; ALICE and BOB exist; the scheduler runs when `jobs`.
async fn bench(jobs: bool) -> Bench {
    let cdn = FixtureCdn::start().await;
    let mut outbound = cdn.config(HOSTS, &[], &[]);
    for group in HostGroup::ALL {
        let limits = outbound.limits.get_mut(group);
        limits.rate = 100.0;
        limits.jitter = None;
        limits.spread = Duration::ZERO;
    }
    let t = TestState::with_config(|config| config.outbound = outbound);
    t.add_user(ALICE);
    t.add_user(BOB);
    let scheduler = jobs.then(|| {
        t.state
            .jobs()
            .start(t.state.clone(), CancellationToken::new())
    });
    Bench {
        t,
        cdn,
        _scheduler: scheduler,
    }
}

fn post_link(body: &Value) -> Request<Body> {
    support::post_json(LINKS, body.to_string())
}

/// `POST /links` as ALICE, expecting `status`.
async fn save(app: &Router, body: Value, status: StatusCode) -> Value {
    let response = send(app, post_link(&body)).await;
    assert_eq!(response.status(), status, "{body}");
    json(response).await
}

async fn post_of(app: &Router, key: &str) -> Value {
    let response = send(app, get(&format!("/api/v1/posts/{key}"))).await;
    assert_eq!(response.status(), StatusCode::OK, "{key}");
    json(response).await
}

/// The `link.hydrate` jobs of `user`, oldest first.
fn hydrations(t: &TestState, user: &str) -> Vec<(i64, String)> {
    let conn = t.control();
    let mut statement = conn
        .prepare(
            "SELECT id, payload_json FROM jobs WHERE user_id = ?1 AND kind = 'link.hydrate' \
             ORDER BY id",
        )
        .unwrap();
    statement
        .query_map([user], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

/// Waits for ALICE's only hydration job to end, or to be queued again.
async fn hydrated(t: &TestState, done: impl Fn(&JobRow) -> bool) -> JobRow {
    let jobs = hydrations(t, ALICE);
    assert_eq!(jobs.len(), 1, "one hydration: {jobs:?}");
    tokio::time::timeout(Duration::from_secs(30), t.wait_job(ALICE, jobs[0].0, done))
        .await
        .expect("the hydration ends")
}

fn ended(job: &JobRow) -> bool {
    matches!(job.state, JobState::Succeeded | JobState::Failed)
}

/// An API token of `kind` with `scopes` for `user`.
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

/// `request` as the iOS Shortcut sends it: a bearer token, JSON, no
/// `Origin` and no `X-Shelfy-Client`.
fn shortcut(method: Method, uri: &str, body: &Value, token: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::AUTHORIZATION, format!("Bearer {token}"))
        .body(Body::from(body.to_string()))
        .unwrap()
}

// ── The route ────────────────────────────────────────────────────────────────

/// P2-07's `/share` page sends exactly `{url, note: null, tags: null}`.
#[tokio::test]
async fn the_share_pages_json_saves_a_post_and_a_second_share_finds_it() {
    let b = bench(false).await;
    let app = b.t.app_as(ALICE);
    let mut events = b.t.state.events().subscribe(ALICE, None);
    let shared = format!("https://www.instagram.com/reel/{IG_CODE}/?igsh=MWQ1ZGUxMzBkMA==");
    let created = save(
        &app,
        json!({ "url": shared, "note": null, "tags": null }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        created,
        json!({ "key": IG_KEY, "platform": "instagram", "created": true })
    );
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(post["mediaType"], "image", "the placeholder (P2-G12)");
    assert_eq!(post["archiveState"], "pending", "waits for its hydration");
    assert_eq!(
        post["postUrl"],
        format!("https://www.instagram.com/reel/{IG_CODE}/"),
        "tracking dropped"
    );
    let changed = next_posts_changed(&mut events).await;
    assert_eq!(changed["keys"], json!([IG_KEY]));
    assert_eq!(changed["reason"], "ingest");

    let jobs = hydrations(&b.t, ALICE);
    assert_eq!(jobs.len(), 1);
    let payload: Value = serde_json::from_str(&jobs[0].1).unwrap();
    assert_eq!(payload, json!({ "postKey": IG_KEY }));

    // The same post again, from another form of its link, with a note and
    // tags: nothing new, the note and tags join, the job is not doubled.
    let again = save(
        &app,
        json!({ "url": format!("https://instagram.com/p/{IG_CODE}"), "note": "for the deck",
                "tags": ["Oak", "lighting"] }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        again,
        json!({ "key": IG_KEY, "platform": "instagram", "created": false })
    );
    let third = save(
        &app,
        json!({ "url": shared, "note": "second thought", "tags": ["oak", "Wood"] }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(third["created"], false);
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(post["userNote"], "for the deck\n\nsecond thought");
    assert_eq!(post["userTags"], json!(["Oak", "lighting", "Wood"]));
    assert_eq!(hydrations(&b.t, ALICE).len(), 1, "deduplicated");
    let changed = next_posts_changed(&mut events).await;
    assert_eq!(changed["keys"], json!([IG_KEY]));
}

/// The next `posts.changed` of a subscription, after a `stats.changed`
/// check on the way.
async fn next_posts_changed(events: &mut shelfy_server::events::Subscription) -> Value {
    loop {
        let next = tokio::time::timeout(Duration::from_secs(10), events.next())
            .await
            .expect("an event");
        if let Delivery::Event(event) = next
            && event.topic == EventTopic::PostsChanged
        {
            return serde_json::from_str(&event.data).unwrap();
        }
    }
}

#[tokio::test]
async fn web_links_stay_links_and_lose_their_tracking() {
    let b = bench(false).await;
    let app = b.t.app_as(ALICE);
    let created = save(
        &app,
        json!({ "url": "https://Example.test/guide?id=7&utm_source=x&fbclid=y#part-2",
                "tags": ["Reading"] }),
        StatusCode::CREATED,
    )
    .await;
    let key = created["key"].as_str().unwrap().to_owned();
    assert!(key.starts_with("web_"), "{key}");
    assert_eq!(created["platform"], "web");
    let post = post_of(&app, &key).await;
    assert_eq!(post["mediaType"], "website");
    assert_eq!(post["archiveState"], "link_only", "until P4 captures it");
    assert_eq!(post["postUrl"], "https://example.test/guide?id=7");
    assert_eq!(post["userTags"], json!(["Reading"]));
    assert!(hydrations(&b.t, ALICE).is_empty(), "web links are P4's");
    // http, www and the tracking collapse into the same post.
    let again = save(
        &app,
        json!({ "url": "http://www.example.test/guide/?id=7&gclid=1" }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(again["key"], key);
}

#[tokio::test]
async fn hostile_and_unusable_urls_are_refused() {
    let b = bench(false).await;
    let app = b.t.app_as(ALICE);
    let long = format!("https://example.test/{}", "a".repeat(4_100));
    for url in [
        "javascript:alert(document.cookie)",
        "data:text/html,<script>alert(1)</script>",
        "https://user:secret@example.test/",
        "http://127.0.0.1:8080/",
        "http://169.254.169.254/latest/meta-data/",
        "http://[::1]/",
        "http://localhost/admin",
        "https://example.test:8443/",
        "https://x.com/someone/status/not-a-number",
        long.as_str(),
        "",
    ] {
        let response = send(&app, post_link(&json!({ "url": url }))).await;
        let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(refused.code, ErrorCode::UnsupportedLink, "{url:?}");
    }
    let response = send(
        &app,
        post_link(&json!({ "url": "https://example.test/", "tags": ["a".repeat(201)] })),
    )
    .await;
    let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
    assert_eq!(refused.code, ErrorCode::ValidationFailed);
    let unknown = send(
        &app,
        post_link(&json!({ "url": "https://example.test/", "title": "x" })),
    )
    .await;
    assert!(unknown.status().is_client_error(), "unknown fields");
    assert!(b.cdn.hits().is_empty(), "nothing was fetched");
    let stats = json(send(&app, get("/api/v1/stats")).await).await;
    assert_eq!(stats["total"], 0, "nothing was saved: {stats}");
}

#[tokio::test]
async fn pinterest_short_links_resolve_within_the_allowlist() {
    let b = bench(false).await;
    let app = b.t.app_as(ALICE);
    b.cdn.route(
        "pin.it",
        "/1AbCdEf",
        [Answer::redirect(
            301,
            "https://api.pinterest.com/url_shortener/1AbCdEf/redirect/",
        )],
    );
    b.cdn.route(
        "api.pinterest.com",
        "/url_shortener/1AbCdEf/redirect/",
        [Answer::redirect(
            302,
            &format!("https://www.pinterest.com/pin/{PIN}/sent/?invite_code=abc&sfo=1"),
        )],
    );
    let created = save(
        &app,
        json!({ "url": "https://pin.it/1AbCdEf" }),
        StatusCode::CREATED,
    )
    .await;
    assert_eq!(
        created,
        json!({ "key": format!("pin_{PIN}"), "platform": "pinterest", "created": true })
    );
    let post = post_of(&app, &format!("pin_{PIN}")).await;
    assert_eq!(
        post["postUrl"],
        format!("https://www.pinterest.com/pin/{PIN}/")
    );
    assert!(
        b.cdn
            .hits()
            .iter()
            .all(|hit| hit.host != "www.pinterest.com"),
        "the pin page is never fetched"
    );

    // A short link that leads off Pinterest is refused before anything is
    // sent there.
    b.cdn.route(
        "pin.it",
        "/Evil",
        [Answer::redirect(302, "https://evil.example.test/pin/1/")],
    );
    let refused = problem(
        send(&app, post_link(&json!({ "url": "https://pin.it/Evil" }))).await,
        StatusCode::UNPROCESSABLE_ENTITY,
    )
    .await;
    assert_eq!(refused.code, ErrorCode::UnsupportedLink);
    assert!(
        b.cdn
            .hits()
            .iter()
            .all(|hit| hit.host != "evil.example.test"),
        "no request off the allowlist"
    );
    // One that ends anywhere but on a pin, or nowhere.
    b.cdn.route(
        "pin.it",
        "/Board",
        [Answer::redirect(
            302,
            "https://www.pinterest.com/someone/board/",
        )],
    );
    b.cdn.route(
        "www.pinterest.com",
        "/someone/board/",
        [Answer::status(200)],
    );
    for code in ["Board", "Missing"] {
        let response = send(
            &app,
            post_link(&json!({ "url": format!("https://pin.it/{code}") })),
        )
        .await;
        let refused = problem(response, StatusCode::UNPROCESSABLE_ENTITY).await;
        assert_eq!(refused.code, ErrorCode::UnsupportedLink, "{code}");
    }
    // A shortener that fails answers 503: the share page offers a retry.
    b.cdn.route("pin.it", "/Down", [Answer::status(502)]);
    let unavailable = problem(
        send(&app, post_link(&json!({ "url": "https://pin.it/Down" }))).await,
        StatusCode::SERVICE_UNAVAILABLE,
    )
    .await;
    assert_eq!(unavailable.code, ErrorCode::Unavailable);
}

// ── Authz ────────────────────────────────────────────────────────────────────

#[tokio::test]
async fn the_route_needs_a_session_through_the_csrf_guard_or_a_links_token() {
    let b = bench(false).await;
    let app = b.t.app();
    let body = json!({ "url": "https://example.test/a" });
    // No credentials.
    let response = send(&app, post_link(&body)).await;
    assert_eq!(
        response.headers()[header::WWW_AUTHENTICATE],
        "Bearer",
        "a token route says so"
    );
    let refused = problem(response, StatusCode::UNAUTHORIZED).await;
    assert_eq!(refused.code, ErrorCode::Unauthorized);

    // A session works through the CSRF guard, and is refused without it.
    let cookie = sign_in(&app, &b.t).await;
    let request = Request::post(LINKS)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = send(&app, spa(&b.t, request, &cookie)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let forged = with_session(
        Request::post(LINKS)
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(
                json!({ "url": "https://example.test/b" }).to_string(),
            ))
            .unwrap(),
        &cookie,
    );
    let refused = problem(send(&app, forged).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::CsrfFailed);
}

#[tokio::test]
async fn the_shortcut_token_saves_links_and_nothing_else() {
    let b = bench(false).await;
    let app = b.t.app();
    let owner_id = owner(&b.t);
    let shortcut_token = token(&b.t, &owner_id, "shortcut", "links:create");
    // The iOS Shortcut: a bearer token, no Origin, no X-Shelfy-Client.
    let body = json!({ "url": format!("https://x.com/studio/status/{TWEET}?s=46&t=abc") });
    let response = send(&app, shortcut(Method::POST, LINKS, &body, &shortcut_token)).await;
    assert_eq!(response.status(), StatusCode::CREATED);
    let created = json(response).await;
    assert_eq!(created["key"], format!("x_{TWEET}"));
    assert_eq!(created["platform"], "twitter");
    assert_eq!(hydrations(&b.t, &owner_id).len(), 1);

    // Cookie-only routes refuse it.
    for (method, uri, body) in [
        (Method::GET, "/api/v1/posts".to_owned(), json!(null)),
        (Method::GET, "/api/v1/stats".to_owned(), json!(null)),
        (Method::GET, format!("/api/v1/posts/x_{TWEET}"), json!(null)),
        (
            Method::PATCH,
            format!("/api/v1/posts/x_{TWEET}"),
            json!({ "userNote": "x" }),
        ),
        (
            Method::POST,
            "/api/v1/collections".to_owned(),
            json!({ "name": "Shortcut" }),
        ),
        (
            Method::POST,
            "/api/v1/posts/lookup".to_owned(),
            json!({ "platform": "twitter", "keys": [TWEET] }),
        ),
    ] {
        let response = send(&app, shortcut(method.clone(), &uri, &body, &shortcut_token)).await;
        assert!(
            matches!(
                response.status(),
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ),
            "{method} {uri}: {}",
            response.status()
        );
    }

    // A token without the scope (the extension's) is refused here.
    let extension = token(&b.t, &owner_id, "extension", "ingest tasks uploads lookup");
    let mut request = shortcut(Method::POST, LINKS, &body, &extension);
    request
        .headers_mut()
        .insert(VERSION_HEADER, "0.2.0".parse().unwrap());
    let refused = problem(send(&app, request).await, StatusCode::FORBIDDEN).await;
    assert_eq!(refused.code, ErrorCode::Forbidden);
}

#[tokio::test]
async fn one_users_links_never_reach_another_library() {
    let b = bench(false).await;
    let (alice, bob) = (b.t.app_as(ALICE), b.t.app_as(BOB));
    let url = json!({ "url": format!("https://www.pinterest.com/pin/{PIN}/"), "tags": ["mine"] });
    let created = save(&alice, url.clone(), StatusCode::CREATED).await;
    let key = created["key"].as_str().unwrap();
    let missing = send(&bob, get(&format!("/api/v1/posts/{key}"))).await;
    assert_eq!(missing.status(), StatusCode::NOT_FOUND);
    // Bob's share of the same link is a post of his own.
    let bobs = save(&bob, json!({ "url": url["url"] }), StatusCode::CREATED).await;
    assert_eq!(bobs["created"], true);
    assert_eq!(post_of(&bob, key).await["userTags"], json!([]));
    assert_eq!(post_of(&alice, key).await["userTags"], json!(["mine"]));
    assert_eq!(hydrations(&b.t, ALICE).len(), 1);
    assert_eq!(hydrations(&b.t, BOB).len(), 1);
}

// ── Hydration ────────────────────────────────────────────────────────────────

/// The path and query of an absolute URL.
fn target(url: &str) -> String {
    let url = url::Url::parse(url).unwrap();
    match url.query() {
        Some(query) => format!("{}?{query}", url.path()),
        None => url.path().to_owned(),
    }
}

fn tweet_result() -> Value {
    json!({
        "__typename": "Tweet",
        "id_str": TWEET,
        "text": "A staircase in oak https://t.co/abc",
        "created_at": "2026-09-30T08:15:00.000Z",
        "user": { "screen_name": "studio", "name": "Studio Oak" },
        "mediaDetails": [
            { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/Fa1.jpg" },
            { "type": "photo", "media_url_https": "https://pbs.twimg.com/media/Fa2.jpg" }
        ]
    })
}

#[tokio::test]
async fn an_x_link_is_hydrated_from_tweet_result() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    b.cdn.route(
        "cdn.syndication.twimg.com",
        &target(&x::syndication_url(TWEET)),
        [Answer::new(
            200,
            "application/json",
            tweet_result().to_string(),
        )],
    );
    save(
        &app,
        json!({ "url": format!("https://twitter.com/i/web/status/{TWEET}") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let post = post_of(&app, &format!("x_{TWEET}")).await;
    assert_eq!(post["caption"], "A staircase in oak https://t.co/abc");
    assert_eq!(post["authorUsername"], "studio");
    assert_eq!(post["authorName"], "Studio Oak");
    assert_eq!(post["mediaType"], "images");
    assert_eq!(post["mediaCount"], 2);
    assert_eq!(
        post["postUrl"],
        format!("https://x.com/studio/status/{TWEET}")
    );
    assert_eq!(post["postedAt"], 1_790_756_100_000_i64);
    assert_eq!(post["archiveState"], "pending", "the archive's turn now");
    let hit = &b.cdn.hits()[0];
    assert!(hit.header("user-agent").unwrap().contains("Chrome/"));
    assert_eq!(hit.header("cookie"), None);
}

#[tokio::test]
async fn a_deleted_tweet_fails_the_post() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    let link = format!("https://x.com/i/status/{TWEET}");
    b.cdn.route(
        "cdn.syndication.twimg.com",
        &target(&x::syndication_url(TWEET)),
        [Answer::new(404, "application/json", "{}")],
    );
    save(&app, json!({ "url": link }), StatusCode::CREATED).await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Failed);
    assert_eq!(job.error_code.as_deref(), Some("not_found"));
    let post = post_of(&app, &format!("x_{TWEET}")).await;
    assert_eq!(post["archiveState"], "failed");
    assert!(
        b.cdn
            .hits_of("publish.x.com", &target(&x::oembed_url(&link)))
            .is_empty(),
        "a 404 is a verdict: no fallback"
    );
}

#[tokio::test]
async fn x_falls_back_to_oembed() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    let link = format!("https://x.com/i/status/{TWEET}");
    b.cdn.route(
        "cdn.syndication.twimg.com",
        &target(&x::syndication_url(TWEET)),
        [Answer::status(503)],
    );
    let oembed = json!({
        "author_name": "Studio Oak",
        "author_url": "https://twitter.com/studio",
        "html": "<blockquote class=\"twitter-tweet\"><p lang=\"en\" dir=\"ltr\">Just words &amp; thoughts</p>&mdash; Studio Oak (@studio) <a href=\"https://twitter.com/studio/status/1\">September 30, 2026</a></blockquote>"
    });
    b.cdn.route(
        "publish.x.com",
        &target(&x::oembed_url(&link)),
        [Answer::new(200, "application/json", oembed.to_string())],
    );
    save(&app, json!({ "url": link }), StatusCode::CREATED).await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let post = post_of(&app, &format!("x_{TWEET}")).await;
    assert_eq!(post["caption"], "Just words & thoughts");
    assert_eq!(post["mediaType"], "text");
    assert_eq!(
        post["archiveState"], "done",
        "a text post has nothing to fetch"
    );
    assert_eq!(post["postedAt"], 1_790_726_400_000_i64);
}

#[tokio::test]
async fn a_pin_is_hydrated_from_pin_resource_or_pidgets() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    let pin = json!({
        "id": PIN,
        "description": "A reading nook",
        "created_at": "Wed, 30 Sep 2026 08:15:00 +0000",
        "pinner": { "username": "studio", "full_name": "Studio Oak" },
        "images": { "orig": { "url": "https://i.pinimg.com/originals/aa/bb/cc.jpg" } },
        "videos": { "video_list": {
            "V_720P": { "url": "https://v1.pinimg.com/videos/mc/720p/aa/bb/cc.mp4", "width": 720 }
        } }
    });
    b.cdn.route(
        "www.pinterest.com",
        &target(&pinterest::resource_url(PIN)),
        [Answer::new(
            200,
            "application/json",
            json!({ "resource_response": { "data": pin, "status": "success" } }).to_string(),
        )],
    );
    save(
        &app,
        json!({ "url": format!("https://it.pinterest.com/pin/nook--{PIN}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let key = format!("pin_{PIN}");
    let post = post_of(&app, &key).await;
    assert_eq!(post["caption"], "A reading nook");
    assert_eq!(post["mediaType"], "video");
    assert_eq!(post["postedAt"], 1_790_756_100_000_i64);
    let slide: (String, Option<String>) =
        b.t.write(ALICE, |tx| {
            tx.query_row(
                "SELECT source_url, video_url FROM post_media WHERE post_id = \
                 (SELECT id FROM posts WHERE key = ?1)",
                [format!("pin_{PIN}")],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .await;
    assert_eq!(slide.0, "https://i.pinimg.com/originals/aa/bb/cc.jpg");
    assert_eq!(
        slide.1.as_deref(),
        Some("https://v1.pinimg.com/videos/mc/720p/aa/bb/cc.mp4")
    );
}

#[tokio::test]
async fn pinterest_falls_back_to_the_widget_api() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    b.cdn.route(
        "www.pinterest.com",
        &target(&pinterest::resource_url(PIN)),
        [Answer::status(500)],
    );
    let pidgets = json!({ "status": "success", "data": [{
        "id": PIN,
        "description": "From the widget",
        "pinner": { "username": "studio" },
        "images": { "564x": { "url": "https://i.pinimg.com/564x/aa/bb/cc.jpg" } }
    }] });
    b.cdn.route(
        "widgets.pinterest.com",
        &target(&pinterest::pidgets_url(PIN)),
        [Answer::new(200, "application/json", pidgets.to_string())],
    );
    save(
        &app,
        json!({ "url": format!("https://www.pinterest.com/pin/{PIN}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let post = post_of(&app, &format!("pin_{PIN}")).await;
    assert_eq!(post["caption"], "From the widget");
    assert_eq!(post["mediaType"], "image");
    assert_eq!(post["archiveState"], "pending");
}

/// A post page with the logged-out media object inline, as SPIKE-9 read it.
fn post_page(media: &Value) -> String {
    let data = json!({ "require": [["ScheduledServerJS", "handle", null, [{ "__bbox": { "result": {
        "data": { "xig_polaris_media": { "if_not_gated_logged_out": media } } } } }]]] });
    format!(
        "<!DOCTYPE html><html><head><title>Instagram</title></head><body>\
         <script type=\"application/json\" data-sjs>{{\"require\":[]}}</script>\
         <script type=\"application/json\" data-content-len=\"1\" data-sjs>{data}</script>\
         </body></html>"
    )
}

#[tokio::test]
async fn an_instagram_link_is_hydrated_from_the_post_page() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    let media = json!({
        "code": IG_CODE,
        "pk": "3141592653589793238",
        "media_type": 8,
        "taken_at": 1_790_756_100,
        "caption": { "text": "Oak stairs, two angles" },
        "user": { "username": "studio", "full_name": "Studio Oak" },
        "carousel_media": [
            { "media_type": 1, "image_versions2": { "candidates": [
                { "url": "https://scontent-mxp1-1.cdninstagram.com/v/t51/a.jpg?oe=70000000" } ] } },
            { "media_type": 2, "image_versions2": { "candidates": [
                { "url": "https://scontent-mxp1-1.cdninstagram.com/v/t51/b.jpg?oe=70000000" } ] },
              "video_versions": [
                { "url": "https://scontent-mxp1-1.cdninstagram.com/o1/v/t16/b.mp4?oe=70000000" } ] }
        ]
    });
    b.cdn.route(
        "www.instagram.com",
        &format!("/p/{IG_CODE}/"),
        [Answer::new(200, "text/html", post_page(&media))],
    );
    save(
        &app,
        json!({ "url": format!("https://www.instagram.com/p/{IG_CODE}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(post["caption"], "Oak stairs, two angles");
    assert_eq!(post["mediaType"], "carousel");
    assert_eq!(post["mediaCount"], 2);
    assert_eq!(post["shortcode"], IG_CODE);
    assert_eq!(post["profileUrl"], "https://www.instagram.com/studio/");
    assert_eq!(post["postedAt"], 1_790_756_100_000_i64);
    assert_eq!(post["archiveState"], "pending");
    let hits = b.cdn.hits();
    assert_eq!(hits.len(), 1, "the post page answered: no GraphQL");
    assert_eq!(hits[0].header("sec-fetch-mode"), Some("navigate"));
}

#[tokio::test]
async fn a_gated_instagram_post_goes_to_the_extension() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    b.cdn.route(
        "www.instagram.com",
        &format!("/p/{IG_CODE}/"),
        [Answer::new(200, "text/html", post_page(&json!(null)))],
    );
    save(
        &app,
        json!({ "url": format!("https://www.instagram.com/p/{IG_CODE}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "handed over: {job:?}");
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(
        post["archiveState"], "client",
        "the extension's hydrate_link"
    );
    assert_eq!(post["mediaType"], "image");
    // Sharing it again does not send the server back: it is the extension's.
    save(
        &app,
        json!({ "url": format!("https://www.instagram.com/p/{IG_CODE}/") }),
        StatusCode::OK,
    )
    .await;
    assert_eq!(hydrations(&b.t, ALICE).len(), 1);
}

#[tokio::test]
async fn a_429_trips_the_breaker_and_hands_instagram_to_the_extension() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    b.cdn.route(
        "www.instagram.com",
        &format!("/p/{IG_CODE}/"),
        [Answer::text(
            429,
            "Please wait a few minutes before you try again.",
        )],
    );
    let before = now_ms();
    save(
        &app,
        json!({ "url": format!("https://www.instagram.com/p/{IG_CODE}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, |job| {
        job.state == JobState::Queued && job.run_at > before + 60_000
    })
    .await;
    assert_eq!(job.attempts, 0, "a block uses no try");
    assert!(
        job.run_at >= before + 29 * 60_000,
        "back when the breaker lets a probe through"
    );
    let breaker =
        b.t.state
            .outbound()
            .breakers()
            .get(HostGroup::InstagramWeb)
            .state(tokio::time::Instant::now());
    assert!(matches!(breaker, BreakerState::Open { .. }), "{breaker:?}");
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(post["archiveState"], "client", "the extension's while open");
    assert_eq!(
        b.cdn.hits().len(),
        1,
        "the first block signal stops the chain"
    );
}

#[tokio::test]
async fn instagram_falls_back_to_the_logged_out_graphql_query() {
    let b = bench(true).await;
    let app = b.t.app_as(ALICE);
    // A post page without the media object (the shape drifted).
    b.cdn.route(
        "www.instagram.com",
        &format!("/p/{IG_CODE}/"),
        [Answer::new(
            200,
            "text/html",
            "<html><body>app shell</body></html>",
        )],
    );
    b.cdn.route(
        "www.instagram.com",
        "/",
        [Answer::new(
            200,
            "text/html",
            r#"<script>["LSD",[],{"token":"AVq-test-lsd"},323]</script>"#,
        )],
    );
    let media = json!({
        "media_type": 2,
        "taken_at": 1_790_756_100,
        "caption": { "text": "A reel" },
        "user": { "username": "studio" },
        "image_versions2": { "candidates": [
            { "url": "https://scontent.cdninstagram.com/v/t51/p.jpg?oe=70000000" } ] },
        "video_versions": [ { "url": "https://scontent.cdninstagram.com/o1/v/t16/r.mp4?oe=70000000" } ]
    });
    b.cdn.route(
        "www.instagram.com",
        "/api/graphql",
        [Answer::new(
            200,
            "application/json",
            json!({ "data": { "xig_polaris_media": { "if_not_gated_logged_out": media } } })
                .to_string(),
        )],
    );
    save(
        &app,
        json!({ "url": format!("https://www.instagram.com/reel/{IG_CODE}/") }),
        StatusCode::CREATED,
    )
    .await;
    let job = hydrated(&b.t, ended).await;
    assert_eq!(job.state, JobState::Succeeded, "{job:?}");
    let post = post_of(&app, IG_KEY).await;
    assert_eq!(post["caption"], "A reel");
    assert_eq!(post["mediaType"], "video");
    let query = b.cdn.hits_of("www.instagram.com", "/api/graphql");
    assert_eq!(query.len(), 1);
    let form = String::from_utf8(query[0].body.clone()).unwrap();
    assert!(form.contains("doc_id=27130156389949648"), "{form}");
    assert!(
        form.contains("variables=%7B%22media_id%22%3A%223141592653589793238%22%7D"),
        "{form}"
    );
    assert_eq!(
        query[0].header("x-fb-friendly-name"),
        Some("PolarisLoggedOutDesktopWWWPostRootContentQuery")
    );
}
