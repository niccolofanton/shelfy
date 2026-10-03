//! The search-eval gate covers the HTTP routes (P1-05, plan §2.14): on the
//! gate's synthetic library, `GET /api/v1/search` and `GET /api/v1/posts?q=`
//! return, for every eval case, exactly the ranking of the core builder the
//! gate measures (`posts::list` with relevance order), page after page, with
//! the core's total. The hybrid probe of the gate (two gold tags with the
//! text) goes through `GET /search` the same way.
//!
//! The library, the cases and the oracle are the gate's own
//! (`crates/core/tests/search_eval/`). All data is synthetic.

mod support;

#[allow(dead_code)]
#[path = "../../core/tests/search_eval/cases.rs"]
mod cases;
#[allow(dead_code)]
#[path = "../../core/tests/search_eval/corpus.rs"]
mod corpus;
#[allow(dead_code)]
#[path = "../../core/tests/search_eval/oracle.rs"]
mod oracle;
#[allow(dead_code)]
#[path = "../../core/tests/search_eval/synthetic.rs"]
mod synthetic;

use std::sync::Arc;

use axum::Router;
use axum::http::StatusCode;
use serde_json::Value;
use shelfy_core::repo::RepoError;
use shelfy_core::repo::posts::{self, Mode, PageRequest, PostFilter, Sort, SourceBucket};
use support::library::ALICE;
use support::{TestState, get, json, send};

/// The gate's first page (`RESULT_LIMIT` of the desktop harness).
const PAGE: usize = 60;

async fn get_ok(app: &Router, uri: &str) -> Value {
    let response = send(app, get(uri)).await;
    assert_eq!(response.status(), StatusCode::OK, "GET {uri}");
    json(response).await
}

fn keys(page: &Value) -> Vec<String> {
    page["items"]
        .as_array()
        .expect("items")
        .iter()
        .map(|p| p["key"].as_str().expect("key").to_owned())
        .collect()
}

/// Every page of `uri`, following `nextCursor`.
async fn walk(app: &Router, uri: &str) -> Vec<String> {
    let mut all = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let page_uri = match &next {
            Some(cursor) => format!("{uri}&cursor={cursor}"),
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

fn encode(value: &str) -> String {
    url::form_urlencoded::byte_serialize(value.as_bytes()).collect()
}

/// The core's answer for `filter`: the first page of the gate, the whole
/// ranking (at most 1,000 posts) and the total, as keys.
fn core_answer(
    db: &shelfy_core::db::UserDb,
    filter: &PostFilter,
) -> (Vec<String>, Vec<String>, u64) {
    db.read(|conn| {
        let first = PageRequest {
            sort: Sort::Relevance,
            limit: PAGE as u32,
            cursor: None,
        };
        let page: Vec<String> = posts::list(conn, filter, &first)?
            .items
            .into_iter()
            .map(|p| p.key)
            .collect();
        let ranked = posts::keys_of(conn, &posts::rank(conn, filter)?)?;
        let total = posts::count(conn, filter)?;
        Ok::<_, RepoError>((page, ranked, total))
    })
    .expect("core search")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_search_routes_rank_every_eval_case_like_the_gate() {
    let t = TestState::new();
    let dir = tempfile::tempdir().expect("temp dir");
    let legacy = dir.path().join("shelfy.sqlite");
    synthetic::write(&legacy);
    let db = t.state.user_db(ALICE).await.expect("open the library");
    let filled = {
        let (db, legacy) = (Arc::clone(&db), legacy.clone());
        tokio::task::spawn_blocking(move || corpus::fill(&legacy, &db))
    };
    let (_, stats) = filled.await.expect("fill the library");
    assert_eq!(stats.posts, synthetic::POSTS);
    let oracle = oracle::Oracle::open(&legacy).expect("open the oracle");
    let app = t.app_as(ALICE);

    for case in cases::CASES {
        let q = encode(case.query);
        let text = PostFilter {
            q: Some(case.query.to_owned()),
            ..PostFilter::default()
        };
        let (first, ranked, total) = core_answer(&db, &text);
        assert!(ranked.len() > 10, "{}: {} results", case.id, ranked.len());
        assert_eq!(first, ranked[..PAGE.min(ranked.len())], "{}", case.id);

        for route in ["search", "posts"] {
            let page = get_ok(
                &app,
                &format!("/api/v1/{route}?q={q}&limit={PAGE}&includeTotal=true"),
            )
            .await;
            assert_eq!(keys(&page), first, "{}: GET /{route}, first page", case.id);
            assert_eq!(page["total"], total, "{}: GET /{route}, total", case.id);
        }
        assert_eq!(
            walk(&app, &format!("/api/v1/search?q={q}&limit=37")).await,
            ranked,
            "{}: GET /search, every page",
            case.id
        );
        assert_eq!(
            walk(&app, &format!("/api/v1/posts?q={q}&limit=200")).await,
            ranked,
            "{}: GET /posts?q=, every page",
            case.id
        );

        // The gate's hybrid probe: the two strongest gold tags with the text.
        let gold = oracle.gold_posts(case.gold_terms).expect("gold posts");
        let probe: Vec<String> = oracle
            .gold_tags(&gold, 12)
            .expect("gold tags")
            .into_iter()
            .take(2)
            .collect();
        if probe.is_empty() {
            continue;
        }
        let hybrid = PostFilter {
            tags: probe.clone(),
            tag_mode: Mode::Or,
            ..text.clone()
        };
        let (first, _, total) = core_answer(&db, &hybrid);
        let tags: String = probe
            .iter()
            .map(|t| format!("&tags={}", encode(t)))
            .collect();
        let page = get_ok(&app, &format!("/api/v1/search?q={q}{tags}&limit={PAGE}")).await;
        assert_eq!(keys(&page), first, "{}: hybrid probe", case.id);
        assert_eq!(page["total"], total, "{}: hybrid probe, total", case.id);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tag_only_routes_share_ranked_pages_aliases_scopes_and_totals() {
    let t = TestState::new();
    let dir = tempfile::tempdir().unwrap();
    let legacy = dir.path().join("shelfy.sqlite");
    synthetic::write(&legacy);
    let db = t.state.user_db(ALICE).await.unwrap();
    let (copied, path) = (Arc::clone(&db), legacy.clone());
    tokio::task::spawn_blocking(move || corpus::fill(&path, &copied))
        .await
        .unwrap();
    let oracle = oracle::Oracle::open(&legacy).unwrap();
    let app = t.app_as(ALICE);
    for case in cases::CASES {
        let gold = oracle.gold_posts(case.gold_terms).unwrap();
        let probe: Vec<String> = case.tag_probe_override.map_or_else(
            || {
                oracle
                    .gold_tags(&gold, 12)
                    .unwrap()
                    .into_iter()
                    .take(5)
                    .collect()
            },
            |tags| tags.iter().map(|t| (*t).into()).collect(),
        );
        if probe.is_empty() {
            continue;
        }
        db.write(|tx| {
            tx.execute("INSERT OR REPLACE INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at) VALUES ('probe alias', ?1, ?1, 'accepted', 0)", [&probe[0]])?;
            Ok::<_, RepoError>(())
        }).unwrap();
        let mut queried = probe.clone();
        queried.extend([" PROBE ALIAS ".into(), probe[0].clone()]);
        let tags: String = queried
            .iter()
            .map(|tag| format!("&tags={}", encode(tag)))
            .collect();
        for mode in [Mode::Or, Mode::And] {
            let mode_name = if mode == Mode::And { "and" } else { "or" };
            for (scope, source) in [
                ("all", None),
                ("sites", Some(SourceBucket::Web)),
                ("social", Some(SourceBucket::Social)),
            ] {
                let filter = PostFilter {
                    tags: queried.clone(),
                    tag_mode: mode,
                    source,
                    ..PostFilter::default()
                };
                let (_, ranked, total) = core_answer(&db, &filter);
                let search_uri =
                    format!("/api/v1/search?scope={scope}&tagMode={mode_name}{tags}&limit=37");
                let page = get_ok(&app, &search_uri).await;
                assert_eq!(page["total"], total, "{} {scope} {mode_name}", case.id);
                assert_eq!(
                    walk(&app, &search_uri).await,
                    ranked,
                    "{}: search {scope} {mode_name}",
                    case.id
                );
                let source = match source {
                    Some(SourceBucket::Web) => "&source=web",
                    Some(SourceBucket::Social) => "&source=social",
                    None => "",
                };
                let posts_uri = format!(
                    "/api/v1/posts?includeTotal=true{source}&tagMode={mode_name}{tags}&limit=113"
                );
                let page = get_ok(&app, &posts_uri).await;
                assert_eq!(page["total"], total);
                assert_eq!(
                    walk(&app, &posts_uri).await,
                    ranked,
                    "{}: posts {scope} {mode_name}",
                    case.id
                );
            }
        }
    }
}
