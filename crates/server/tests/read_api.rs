//! The read API (T11) through the real middleware stack: `GET /api/v1/posts`,
//! `/posts/{key}`, `/search`, `/stats` and `/collections`.
//!
//! Requests carry a user through the test-only stand-in for authentication
//! (`TestState::app_as`); `TestState::app` has none. Responses are checked
//! against the OpenAPI document, so the generated TypeScript types describe
//! what the server really sends.

mod support;

use std::collections::HashSet;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Value, json};
use shelfy_core::db::{DbError, UserDbConfig};
use shelfy_core::repo::posts::{self, NewPost, PostFilter};
use shelfy_core::repo::{Platform, RepoError};
use shelfy_server::conditional::CACHE_CONTROL;
use shelfy_server::error::ErrorCode;
use shelfy_server::routes;
use support::library::{
    ALICE, BOB, FIXTURE_NEWEST, FIXTURE_TRASHED, Fixture, NOW, THUMBHASH, bob_library, fixture,
    object_sha, synthetic_library,
};
use support::{TestState, body, get, json, problem, send};

/// Alice's fixture library and Bob's library on one server.
async fn two_libraries() -> (TestState, Fixture) {
    let t = TestState::new();
    let ids = t.write(ALICE, |tx| fixture(tx)).await;
    t.write(BOB, |tx| bob_library(tx)).await;
    (t, ids)
}

/// `GET uri` expecting 200 and a JSON body.
async fn get_ok(app: &Router, uri: &str) -> Value {
    let response = send(app, get(uri)).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    json(response).await
}

/// `GET uri` with `If-None-Match`.
fn get_if_none_match(uri: &str, etag: &str) -> Request<Body> {
    Request::get(uri)
        .header(header::IF_NONE_MATCH, etag)
        .body(Body::empty())
        .unwrap()
}

/// The keys of a page, in order.
fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["key"].as_str().expect("key").to_owned())
        .collect()
}

/// Checks `value` against the schema `name` of the OpenAPI document.
fn assert_schema(value: &Value, name: &str) {
    let doc = serde_json::to_value(routes::openapi()).unwrap();
    let schema = json!({
        "$ref": format!("#/components/schemas/{name}"),
        "components": doc["components"],
    });
    let validator = jsonschema::validator_for(&schema).expect("the schema compiles");
    let errors: Vec<String> = validator
        .iter_errors(value)
        .map(|e| format!("{e} at {}", e.instance_path()))
        .collect();
    assert!(errors.is_empty(), "{name}: {errors:#?}");
}

/// Every page of `uri` (which must not have a cursor yet), following
/// `nextCursor`; returns the keys in order.
async fn walk(app: &Router, uri: &str) -> Vec<String> {
    let separator = if uri.contains('?') { '&' } else { '?' };
    let mut all = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let page_uri = match &next {
            Some(cursor) => format!("{uri}{separator}cursor={cursor}"),
            None => uri.to_owned(),
        };
        let page = get_ok(app, &page_uri).await;
        all.extend(keys(&page));
        match page["nextCursor"].as_str() {
            Some(cursor) => next = Some(cursor.to_owned()),
            None => return all,
        }
    }
}

#[tokio::test]
async fn every_read_route_needs_a_user() {
    let (t, _) = two_libraries().await;
    // A fresh state: the libraries above must not be opened by a 401.
    let fresh = TestState::new();
    for app in [t.app(), fresh.app()] {
        for uri in [
            "/api/v1/posts",
            "/api/v1/posts?platform=tiktok",
            "/api/v1/posts/ig_1001",
            "/api/v1/search?q=lampada",
            "/api/v1/stats",
            "/api/v1/collections",
            "/api/v1/events",
        ] {
            let problem = problem(send(&app, get(uri)).await, StatusCode::UNAUTHORIZED).await;
            assert_eq!(problem.code, ErrorCode::Unauthorized, "{uri}");
        }
    }
    let users = fresh.data_dir().users_dir();
    let opened: Vec<_> = std::fs::read_dir(&users).unwrap().collect();
    assert!(opened.is_empty(), "a refused request opened a library");
}

#[tokio::test]
async fn posts_list_the_library_newest_first() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);

    let page = get_ok(&app, "/api/v1/posts").await;
    assert_eq!(keys(&page), FIXTURE_NEWEST);
    assert_eq!(page["nextCursor"], Value::Null);
    assert!(page.get("total").is_none(), "total only when asked");
    assert_schema(&page, "PostPage");

    let page = get_ok(&app, "/api/v1/posts?sort=oldest&includeTotal=true").await;
    let mut oldest = FIXTURE_NEWEST.to_vec();
    oldest.reverse();
    assert_eq!(keys(&page), oldest);
    assert_eq!(page["total"], 7);
    assert_schema(&page, "PostPage");

    // Small pages: the same posts, in the same order.
    assert_eq!(walk(&app, "/api/v1/posts?limit=2").await, FIXTURE_NEWEST);
    assert_eq!(
        walk(&app, "/api/v1/posts?limit=3&sort=oldest").await,
        oldest
    );
    // `limit` is clamped, never refused.
    let page = get_ok(&app, "/api/v1/posts?limit=0").await;
    assert_eq!(keys(&page), FIXTURE_NEWEST[..1]);
    let page = get_ok(&app, "/api/v1/posts?limit=100000").await;
    assert_eq!(keys(&page).len(), 7);
}

#[tokio::test]
async fn every_filter_selects_its_posts() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let [web, pin, manual, tweet, carousel, video, images] = FIXTURE_NEWEST;
    let lighting = format!("collection={}", ids.lighting);
    let inspiration = format!("collection={}", ids.inspiration);
    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("platform=instagram", vec![carousel, video]),
        ("platform=manual", vec![manual]),
        ("source=web", vec![web]),
        (
            "source=social",
            vec![pin, manual, tweet, carousel, video, images],
        ),
        (&lighting, vec![carousel]),
        (&inspiration, vec![pin]),
        ("collection=999", vec![]),
        ("mediaType=video&mediaType=text", vec![tweet, video]),
        ("mediaType=website", vec![web]),
        ("stored=yes", vec![carousel]),
        ("stored=no", vec![web, pin, manual, tweet, video, images]),
        ("aiTagged=yes", vec![carousel, images]),
        ("aiTagged=no", vec![web, pin, manual, tweet, video]),
        ("aiStatus=done", vec![carousel, images]),
        ("tag=lighting", vec![carousel]),
        ("tag=Glass", vec![carousel]),
        ("tags=glass&tags=kitchen", vec![carousel, images]),
        ("tags=glass&tags=kitchen&tagMode=and", vec![]),
        ("tags=glass&tags=lamp&tagMode=and", vec![carousel]),
        ("entity=Murano", vec![carousel]),
        ("category=interior", vec![carousel]),
        ("contentType=product", vec![carousel]),
        ("q=vetro", vec![carousel]),
        ("q=lamp&sort=newest", vec![carousel]),
        ("q=design&sort=newest", vec![web, pin, tweet]),
        ("q=pasta&concept=marble&sort=newest", vec![video, images]),
        ("q=pasta&concept=marble&conceptMode=and", vec![]),
        ("q=%21%21%21", vec![]),
        ("platform=instagram&stored=no", vec![video]),
        ("q=design&source=social&sort=newest", vec![pin, tweet]),
    ];
    for (query, expected) in cases {
        let page = get_ok(&app, &format!("/api/v1/posts?{query}&includeTotal=true")).await;
        assert_eq!(keys(&page), expected, "{query}");
        assert_eq!(page["total"], expected.len(), "{query}");
    }
    // The trash, and only the trash.
    for flag in ["1", "true"] {
        let page = get_ok(&app, &format!("/api/v1/posts?trash={flag}")).await;
        assert_eq!(keys(&page), [FIXTURE_TRASHED], "trash={flag}");
        assert!(page["items"][0]["deletedAt"].is_i64());
    }
    let page = get_ok(&app, "/api/v1/posts?trash=0").await;
    assert_eq!(keys(&page), FIXTURE_NEWEST);
}

#[tokio::test]
async fn search_text_is_ranked_by_relevance_by_default() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let ranked = get_ok(&app, "/api/v1/posts?q=design").await;
    let mut found = keys(&ranked);
    // The post tagged "design" outranks the captions that mention it.
    assert_eq!(found[0], "x_2001");
    found.sort();
    assert_eq!(
        found,
        ["pin_3001", "web_00a1b2c3d4e5f6a7b8c9", "x_2001"],
        "the same posts as newest order"
    );
    // `sort=relevance` without text falls back to newest.
    let page = get_ok(&app, "/api/v1/posts?sort=relevance").await;
    assert_eq!(keys(&page), FIXTURE_NEWEST);
}

#[tokio::test]
async fn posts_have_a_stable_json_shape() {
    let (t, ids) = two_libraries().await;
    let app = t.app_as(ALICE);
    let page = get_ok(&app, "/api/v1/posts?platform=instagram").await;
    let carousel = &page["items"][0];
    assert_eq!(carousel["key"], "ig_1001");
    assert!(carousel.get("id").is_none(), "the row id stays internal");

    // camelCase, every field present, null when empty.
    let fields: HashSet<&str> = carousel
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for field in [
        "key",
        "platform",
        "shortcode",
        "postUrl",
        "profileUrl",
        "authorUsername",
        "authorName",
        "caption",
        "mediaType",
        "mediaCount",
        "postedAt",
        "importedAt",
        "sortTs",
        "cover",
        "coverUrl",
        "thumbhash",
        "archiveState",
        "aiStatus",
        "aiTags",
        "userNote",
        "userTags",
        "updatedAt",
        "deletedAt",
        "media",
        "collectionIds",
        "webCapture",
    ] {
        assert!(fields.contains(field), "missing {field}");
    }
    assert!(fields.iter().all(|f| !f.contains('_')), "{fields:?}");
    assert_eq!(carousel["platform"], "instagram");
    assert_eq!(carousel["mediaType"], "carousel");
    assert_eq!(carousel["archiveState"], "done");
    assert_eq!(carousel["profileUrl"], Value::Null);
    assert_eq!(carousel["deletedAt"], Value::Null);
    assert_eq!(carousel["postedAt"], NOW - 10 * 86_400_000);
    assert_eq!(carousel["collectionIds"], json!([ids.lighting]));
    assert_eq!(carousel["userTags"], json!(["Lighting"]));
    assert_eq!(carousel["aiTags"], json!(["glass", "lamp"]));

    // ThumbHash as standard base64.
    let thumbhash = carousel["thumbhash"].as_str().unwrap();
    assert_eq!(thumbhash, "HQgKA4I=");
    assert_eq!(STANDARD.decode(thumbhash).unwrap(), THUMBHASH);

    // Stored objects come with their URLs; the rendition only when it exists.
    let cover = &carousel["cover"];
    assert_eq!(cover["url"], format!("/media/{}.jpg", object_sha(1)));
    assert_eq!(
        cover["g480Url"],
        format!("/media/{}.g480.webp", object_sha(1))
    );
    assert_eq!(cover["sha256"], object_sha(1));
    assert_eq!(cover["mime"], "image/jpeg");
    let slides = carousel["media"].as_array().unwrap();
    assert_eq!(slides.len(), 2);
    assert_eq!(slides[0]["kind"], "image");
    assert_eq!(slides[0]["object"]["g480Url"], Value::Null);
    assert_eq!(slides[0]["videoObject"], Value::Null);
    assert_eq!(slides[1]["kind"], "video");
    assert_eq!(
        slides[1]["object"]["url"],
        format!("/media/{}.webp", object_sha(3))
    );
    assert_eq!(slides[1]["durationMs"], 12_000);

    // A post without media or stored objects.
    let tweet = &get_ok(&app, "/api/v1/posts?mediaType=text").await["items"][0];
    assert_eq!(tweet["key"], "x_2001");
    assert_eq!(tweet["cover"], Value::Null);
    assert_eq!(tweet["thumbhash"], Value::Null);
    assert_eq!(tweet["shortcode"], Value::Null);
    assert_eq!(tweet["media"], json!([]));
    assert_eq!(tweet["archiveState"], "pending");
    assert_schema(&page, "PostPage");
}

#[tokio::test]
async fn a_post_comes_with_its_tags_entities_and_ai_fields() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);
    let detail = get_ok(&app, "/api/v1/posts/ig_1001").await;
    assert_schema(&detail, "PostDetail");
    assert_eq!(detail["key"], "ig_1001");
    assert_eq!(detail["nativeId"], "1001");
    assert_eq!(detail["aiModel"], "model-a");
    assert_eq!(detail["aiEntities"], json!(["Murano"]));
    assert_eq!(detail["aiKeywords"], json!(["blown glass"]));
    assert_eq!(detail["aiAttempts"], 0);
    assert_eq!(detail["thumbhash"], "HQgKA4I=");
    let tags: Vec<(String, String)> = detail["tags"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["norm"].as_str().unwrap().to_owned(),
                t["source"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        tags,
        [
            ("glass".to_owned(), "ai".to_owned()),
            ("lamp".to_owned(), "ai".to_owned()),
            ("lighting".to_owned(), "manual".to_owned()),
        ]
    );
    assert_eq!(
        detail["entities"],
        json!([{ "entity": "Murano", "norm": "murano" }])
    );

    // The trash is readable too.
    let trashed = get_ok(&app, &format!("/api/v1/posts/{FIXTURE_TRASHED}")).await;
    assert!(trashed["deletedAt"].is_i64());
    assert_schema(&trashed, "PostDetail");
}

#[tokio::test]
async fn another_users_posts_are_not_found() {
    let (t, _) = two_libraries().await;
    let alice = t.app_as(ALICE);
    let bob = t.app_as(BOB);

    for uri in [
        "/api/v1/posts/ig_9001",
        "/api/v1/posts/x_9002",
        "/api/v1/posts/ig_unknown",
    ] {
        let problem = problem(send(&alice, get(uri)).await, StatusCode::NOT_FOUND).await;
        assert_eq!(problem.code, ErrorCode::NotFound, "{uri}");
    }
    let long = format!("/api/v1/posts/ig_{}", "1".repeat(300));
    problem(send(&alice, get(&long)).await, StatusCode::NOT_FOUND).await;
    assert_eq!(
        get_ok(&bob, "/api/v1/posts/ig_9001").await["key"],
        "ig_9001"
    );
    let problem = problem(
        send(&bob, get("/api/v1/posts/ig_1001")).await,
        StatusCode::NOT_FOUND,
    )
    .await;
    assert_eq!(problem.code, ErrorCode::NotFound);

    // Lists, search, stats and collections only ever show the caller's data.
    assert_eq!(walk(&alice, "/api/v1/posts").await, FIXTURE_NEWEST);
    assert_eq!(walk(&bob, "/api/v1/posts").await, ["ig_9001", "x_9002"]);
    let found = get_ok(&alice, "/api/v1/search?q=vetro").await;
    assert_eq!(keys(&found), ["ig_1001"]);
    let found = get_ok(&bob, "/api/v1/search?q=vetro").await;
    assert_eq!(keys(&found), ["ig_9001"]);
    assert_eq!(get_ok(&bob, "/api/v1/stats").await["total"], 2);
    let folders = get_ok(&bob, "/api/v1/collections").await;
    assert_eq!(folders["items"].as_array().unwrap().len(), 1);
    assert_eq!(folders["items"][0]["name"], "Bob's folder");
}

#[tokio::test]
async fn pages_never_repeat_or_skip_a_post() {
    let t = TestState::new();
    let all = t.write(ALICE, |tx| synthetic_library(tx, 300, 11)).await;
    let app = t.app_as(ALICE);

    // The expected orders, straight from the core.
    let db = t.state.user_db(ALICE).await.unwrap();
    let expected = |filter: PostFilter| -> Vec<String> {
        db.read(|conn| -> Result<Vec<String>, RepoError> {
            let mut keys = Vec::new();
            for id in posts::list_ids(conn, &filter)? {
                keys.push(
                    conn.query_row("SELECT key FROM posts WHERE id = ?1", [id], |r| r.get(0))?,
                );
            }
            Ok(keys)
        })
        .unwrap()
    };
    let newest = expected(PostFilter::default());
    assert_eq!(newest.len(), all.len());
    let unique: HashSet<&String> = newest.iter().collect();
    assert_eq!(unique.len(), all.len());

    assert_eq!(walk(&app, "/api/v1/posts?limit=7").await, newest);
    let mut oldest = newest.clone();
    oldest.reverse();
    assert_eq!(
        walk(&app, "/api/v1/posts?limit=13&sort=oldest").await,
        oldest
    );
    let instagram = expected(PostFilter {
        platform: Some(Platform::Instagram),
        ..PostFilter::default()
    });
    assert_eq!(
        walk(&app, "/api/v1/posts?limit=9&platform=instagram").await,
        instagram
    );

    // The page size may change between pages.
    let first = get_ok(&app, "/api/v1/posts?limit=5").await;
    let cursor = first["nextCursor"].as_str().unwrap();
    let second = get_ok(&app, &format!("/api/v1/posts?limit=50&cursor={cursor}")).await;
    let mut joined = keys(&first);
    joined.extend(keys(&second));
    assert_eq!(joined, newest[..55]);

    // Relevance: small pages give the one ranking of a single large page.
    for uri in [
        "/api/v1/posts?q=lampada",
        "/api/v1/search?q=typography",
        "/api/v1/search?q=kitchen&tags=marble",
    ] {
        let whole = get_ok(&t.app_as(ALICE), &format!("{uri}&limit=200")).await;
        assert_eq!(
            whole["nextCursor"],
            Value::Null,
            "{uri}: one page holds all"
        );
        let ranked = keys(&whole);
        assert!(ranked.len() > 20, "{uri}: {} results", ranked.len());
        let paged = walk(&app, &format!("{uri}&limit=8")).await;
        assert_eq!(paged, ranked, "{uri}");
        let distinct: HashSet<&String> = paged.iter().collect();
        assert_eq!(distinct.len(), paged.len(), "{uri}: duplicates");
    }
}

#[tokio::test]
async fn search_ranks_with_a_total() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);

    let found = get_ok(&app, "/api/v1/search?q=design").await;
    assert_schema(&found, "SearchPage");
    assert_eq!(found["total"], 3);
    assert_eq!(keys(&found)[0], "x_2001");
    let page = get_ok(&app, "/api/v1/posts?q=design").await;
    assert_eq!(keys(&found), keys(&page), "one engine");

    let cases: Vec<(&str, Vec<&str>)> = vec![
        ("q=design&scope=sites", vec!["web_00a1b2c3d4e5f6a7b8c9"]),
        ("q=design&scope=social", vec!["x_2001", "pin_3001"]),
        ("tags=glass", vec!["ig_1001"]),
        ("tags=glass&tags=kitchen", vec!["ig_1001", "x_2002"]),
        ("tags=glass&tags=kitchen&tagMode=and", vec![]),
        ("q=pasta&tags=kitchen", vec!["ig_1002", "x_2002"]),
        ("q=pasta&tags=kitchen&tagMode=and", vec![]),
        ("q=kitchen&tags=kitchen&tagMode=and", vec!["x_2002"]),
        ("q=pasta&concept=marble", vec!["ig_1002", "x_2002"]),
        ("concept=marble", vec!["x_2002"]),
    ];
    for (query, expected) in cases {
        let found = get_ok(&app, &format!("/api/v1/search?{query}")).await;
        let mut got = keys(&found);
        let mut want: Vec<String> = expected.iter().map(|k| (*k).to_owned()).collect();
        assert_eq!(found["total"], want.len(), "{query}");
        if query.starts_with("q=design") {
            assert_eq!(got, want, "{query}: ranked");
        } else {
            got.sort();
            want.sort();
            assert_eq!(got, want, "{query}");
        }
    }

    // Nothing to search for: no results, not the whole library.
    for query in ["", "q=", "q=%20%20", "tags=%20", "scope=sites"] {
        let found = get_ok(&app, &format!("/api/v1/search?{query}")).await;
        assert_eq!(found["items"], json!([]), "{query}");
        assert_eq!(found["total"], 0, "{query}");
        assert_eq!(found["nextCursor"], Value::Null, "{query}");
    }
}

#[tokio::test]
async fn stats_count_the_callers_library() {
    let (t, _) = two_libraries().await;
    let stats = get_ok(&t.app_as(ALICE), "/api/v1/stats").await;
    assert_schema(&stats, "Stats");
    assert_eq!(
        stats,
        json!({
            "total": 7,
            "byPlatform": { "instagram": 2, "twitter": 2, "pinterest": 1, "web": 1, "manual": 1 },
            "byMediaType": {
                "carousel": 1, "file": 1, "image": 1, "images": 1, "text": 1, "video": 1, "website": 1
            },
            "stored": 1,
            "storedByKind": { "covers": 1, "images": 1, "videos": 0 },
            "trashed": 1,
        })
    );
    // A user without a library yet gets zeros.
    let empty = get_ok(&t.app_as("01J9Z3B8K4QW6TFX0V7G2N5RCC"), "/api/v1/stats").await;
    assert_eq!(empty["total"], 0);
    assert_eq!(empty["byPlatform"]["manual"], 0);
}

#[tokio::test]
async fn collections_come_with_live_counts() {
    let (t, ids) = two_libraries().await;
    let list = get_ok(&t.app_as(ALICE), "/api/v1/collections").await;
    assert_schema(&list, "CollectionList");
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], ids.lighting);
    assert_eq!(items[0]["name"], "Lighting");
    assert_eq!(items[0]["platform"], "instagram");
    assert_eq!(items[0]["externalId"], "17900000000000001");
    assert_eq!(items[0]["color"], "#3d5afe");
    assert_eq!(items[0]["count"], 1);
    assert_eq!(items[1]["id"], ids.inspiration);
    assert_eq!(items[1]["platform"], Value::Null);
    assert_eq!(items[1]["color"], "#ffaa00");
    assert_eq!(items[1]["position"], Value::Null);
    assert!(items[1]["createdAt"].is_i64());
}

#[tokio::test]
async fn unchanged_views_answer_not_modified() {
    let (t, _) = two_libraries().await;
    let alice = t.app_as(ALICE);
    let views = [
        "/api/v1/posts?platform=instagram",
        "/api/v1/posts/ig_1001",
        "/api/v1/search?q=design",
        "/api/v1/stats",
        "/api/v1/collections",
    ];
    let mut etags = Vec::new();
    for uri in views {
        let response = send(&alice, get(uri)).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL);
        let etag = response.headers()[header::ETAG]
            .to_str()
            .unwrap()
            .to_owned();
        assert!(etag.starts_with("W/\""), "{uri}: {etag}");

        let response = send(&alice, get_if_none_match(uri, &etag)).await;
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED, "{uri}");
        assert_eq!(response.headers()[header::ETAG], etag.as_str(), "{uri}");
        assert_eq!(response.headers()[header::CACHE_CONTROL], CACHE_CONTROL);
        assert!(body(response).await.is_empty(), "{uri}: a 304 has no body");

        let strong = etag.trim_start_matches("W/");
        let listed = format!("\"other\", {etag}");
        for header_value in [strong, "*", listed.as_str()] {
            let response = send(&alice, get_if_none_match(uri, header_value)).await;
            assert_eq!(
                response.status(),
                StatusCode::NOT_MODIFIED,
                "{uri}: {header_value}"
            );
        }
        // Another user's ETag never matches, nor does another view's.
        let response = send(&t.app_as(BOB), get_if_none_match(uri, &etag)).await;
        assert_ne!(response.status(), StatusCode::NOT_MODIFIED, "{uri}");
        etags.push(etag);
    }
    let distinct: HashSet<&String> = etags.iter().collect();
    assert_eq!(distinct.len(), views.len(), "every view has its own ETag");

    // Every parameter is part of the ETag.
    for other in [
        "/api/v1/posts?platform=twitter",
        "/api/v1/posts?platform=instagram&limit=1",
        "/api/v1/posts?platform=instagram&includeTotal=true",
    ] {
        let response = send(&alice, get_if_none_match(other, &etags[0])).await;
        assert_eq!(response.status(), StatusCode::OK, "{other}");
    }

    // A write changes every view of the library.
    t.write(ALICE, |tx| {
        let mut post = NewPost::new("ig_1004", Platform::Instagram, "1004", "image", NOW);
        post.caption = Some("A new design lamp".into());
        posts::insert(tx, &post, NOW)
    })
    .await;
    for (uri, etag) in views.iter().zip(&etags) {
        let response = send(&alice, get_if_none_match(uri, etag)).await;
        assert_eq!(response.status(), StatusCode::OK, "{uri} after a write");
        assert_ne!(response.headers()[header::ETAG], etag.as_str(), "{uri}");
    }
    // Reads change nothing: the new ETags hold.
    for uri in views {
        let first = send(&alice, get(uri)).await;
        let etag = first.headers()[header::ETAG].to_str().unwrap().to_owned();
        let again = send(&alice, get_if_none_match(uri, &etag)).await;
        assert_eq!(again.status(), StatusCode::NOT_MODIFIED, "{uri}");
    }
}

#[tokio::test]
async fn not_modified_needs_no_database_reader() {
    let t = TestState::with_config(|config| {
        config.user_db = UserDbConfig {
            max_readers: 1,
            reader_wait_timeout: Duration::from_millis(100),
            ..UserDbConfig::default()
        };
    });
    t.write(ALICE, |tx| fixture(tx)).await;
    let app = t.app_as(ALICE);
    let uri = "/api/v1/posts?limit=3";
    let etag = send(&app, get(uri)).await.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();

    // Hold the only reader.
    let db = t.state.user_db(ALICE).await.unwrap();
    let (held_tx, held_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel::<()>();
    let holder = {
        let db = Arc::clone(&db);
        std::thread::spawn(move || {
            db.read(|_| {
                held_tx.send(()).unwrap();
                release_rx.recv().unwrap();
                Ok::<_, DbError>(())
            })
            .unwrap();
        })
    };
    held_rx.recv().unwrap();

    let response = send(&app, get_if_none_match(uri, &etag)).await;
    assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
    let busy = problem(send(&app, get(uri)).await, StatusCode::SERVICE_UNAVAILABLE).await;
    assert_eq!(busy.code, ErrorCode::Unavailable);

    release_tx.send(()).unwrap();
    holder.join().unwrap();
    assert_eq!(send(&app, get(uri)).await.status(), StatusCode::OK);
}

#[tokio::test]
async fn bad_cursors_and_filters_are_problems() {
    let (t, _) = two_libraries().await;
    let app = t.app_as(ALICE);

    let page = get_ok(&app, "/api/v1/posts?limit=2").await;
    let cursor = page["nextCursor"].as_str().unwrap().to_owned();
    let search = get_ok(&app, "/api/v1/search?q=design&limit=1").await;
    let search_cursor = search["nextCursor"].as_str().unwrap().to_owned();
    let invalid = [
        "/api/v1/posts?cursor=garbage".to_owned(),
        "/api/v1/posts?cursor=".to_owned() + &"A".repeat(400),
        format!("/api/v1/posts?cursor={cursor}&sort=oldest"),
        format!("/api/v1/posts?cursor={cursor}&platform=instagram"),
        format!("/api/v1/posts?cursor={cursor}&q=design"),
        format!("/api/v1/search?q=design&cursor={cursor}"),
        format!("/api/v1/search?q=lamp&cursor={search_cursor}"),
        format!("/api/v1/posts?q=design&cursor={search_cursor}"),
    ];
    for uri in invalid {
        let problem = problem(send(&app, get(&uri)).await, StatusCode::BAD_REQUEST).await;
        assert_eq!(problem.code, ErrorCode::InvalidCursor, "{uri}");
    }
    // The same cursor with its own filters works.
    assert_eq!(
        get_ok(&app, &format!("/api/v1/posts?cursor={cursor}&limit=2")).await["items"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    for uri in [
        "/api/v1/posts?platform=tiktok",
        "/api/v1/posts?mediaType=gif",
        "/api/v1/posts?sort=random",
        "/api/v1/posts?source=all",
        "/api/v1/posts?stored=maybe",
        "/api/v1/posts?tagMode=xor",
        "/api/v1/posts?collection=abc",
        "/api/v1/posts?limit=-1",
        "/api/v1/posts?trash=maybe",
        "/api/v1/posts?includeTotal=yes",
        "/api/v1/posts?platform=instagram&platform=twitter",
        "/api/v1/search?scope=web",
        "/api/v1/search?limit=lots",
    ] {
        let problem = problem(send(&app, get(uri)).await, StatusCode::BAD_REQUEST).await;
        assert_eq!(problem.code, ErrorCode::BadRequest, "{uri}");
        assert!(problem.detail.is_some(), "{uri}");
    }

    let long = "a".repeat(501);
    let many_tags = vec!["tags=x"; 51].join("&");
    let long_tag = format!("tag={}", "t".repeat(201));
    for (uri, field) in [
        (format!("/api/v1/posts?q={long}"), "q"),
        (format!("/api/v1/search?q={long}"), "q"),
        (format!("/api/v1/posts?{many_tags}"), "tags"),
        (format!("/api/v1/search?{many_tags}"), "tags"),
        (format!("/api/v1/posts?{long_tag}"), "tag"),
        (
            format!("/api/v1/posts?concept={}", "c".repeat(201)),
            "concept",
        ),
    ] {
        let problem = problem(
            send(&app, get(&uri)).await,
            StatusCode::UNPROCESSABLE_ENTITY,
        )
        .await;
        assert_eq!(problem.code, ErrorCode::ValidationFailed, "{field}");
        assert_eq!(problem.errors[0].field, field);
    }
    // At the limit: accepted.
    let ok = format!("/api/v1/posts?q={}", "a".repeat(500));
    assert_eq!(send(&app, get(&ok)).await.status(), StatusCode::OK);
}
