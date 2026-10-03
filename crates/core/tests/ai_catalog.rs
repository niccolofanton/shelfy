//! The catalog pipeline of the core, offline, on the synthetic fixtures of
//! `shared/ai/fixtures/` (P3-03): every fixture post gets its request, every
//! fixture answer becomes the post's AI layer, and an answer off its schema is
//! a typed error that writes nothing.

use std::path::Path;

use rusqlite::Connection;
use serde::Deserialize;
use serde_json::{Map, Value};
use shelfy_core::ai::catalog::{self, CatalogKind};
use shelfy_core::ai::normalize::{self, OutputError};
use shelfy_core::ai::prompts::SCHEMA_VERSION;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, NewPost, PostDetail, TagSource};
use shelfy_core::schema::{self, Kind};

const NOW_MS: i64 = 1_790_899_200_000;

#[derive(Deserialize)]
struct Fixtures {
    vocabulary: Vec<String>,
    posts: Vec<FixturePost>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixturePost {
    id: String,
    platform: Platform,
    media_type: String,
    caption: String,
    #[serde(default)]
    tech: Vec<String>,
    images: usize,
}

#[derive(Deserialize)]
struct InvalidAnswer {
    why: String,
    kind: String,
    answer: Value,
}

fn fixture<T: for<'de> Deserialize<'de>>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../shared/ai/fixtures")
        .join(name);
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn kind_of(post: &FixturePost) -> CatalogKind {
    CatalogKind::of(post.platform.as_str(), &post.media_type)
}

/// A library holding the fixture posts, keyed by their fixture id.
fn library(fixtures: &Fixtures) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    for post in &fixtures.posts {
        let mut new = NewPost::new(&post.id, post.platform, &post.id, &post.media_type, NOW_MS);
        new.caption = Some(post.caption.clone());
        posts::insert(&conn, &new, NOW_MS).unwrap();
    }
    conn
}

fn detail(conn: &Connection, key: &str) -> PostDetail {
    posts::get(conn, key).unwrap().unwrap()
}

#[test]
fn every_fixture_post_gets_its_request() {
    let fixtures: Fixtures = fixture("posts.json");
    for post in &fixtures.posts {
        let kind = kind_of(post);
        let hints = match kind {
            CatalogKind::Social => &fixtures.vocabulary,
            CatalogKind::Web => &post.tech,
        };
        let request = catalog::request(kind, Some(&post.caption), hints, post.images > 0).unwrap();
        let (schema, system) = match kind {
            CatalogKind::Social => ("video_catalog", "You are an assistant that catalogs images"),
            CatalogKind::Web => ("web_catalog", "You are an assistant that catalogs WEBSITES"),
        };
        assert_eq!(request.schema.name, schema, "{}", post.id);
        assert!(request.system.starts_with(system), "{}", post.id);
        // The caption sits once between the markers, its own markers neutralized.
        assert_eq!(
            request.user.matches("<<<CAPTION>>>").count(),
            1,
            "{}",
            post.id
        );
        assert_eq!(
            request.user.matches("<<<END CAPTION>>>").count(),
            1,
            "{}",
            post.id
        );
        let images_intro = match kind {
            CatalogKind::Social => "These media belong to one saved post",
            CatalogKind::Web => "These are screenshots",
        };
        assert_eq!(
            request.user.starts_with(images_intro),
            post.images > 0,
            "{}",
            post.id
        );
    }
    // The website prompt carries its tech stack, the social one the vocabulary.
    let site = fixtures
        .posts
        .iter()
        .find(|p| p.id == "studio-site")
        .unwrap();
    let request =
        catalog::request(CatalogKind::Web, Some(&site.caption), &site.tech, true).unwrap();
    assert!(
        request
            .user
            .contains("(NOT inferred): Next.js, GSAP, Vercel.")
    );
    let lamp = fixtures.posts.iter().find(|p| p.id == "lamp").unwrap();
    let request = catalog::request(
        CatalogKind::Social,
        Some(&lamp.caption),
        &fixtures.vocabulary,
        true,
    )
    .unwrap();
    assert!(
        request
            .user
            .contains(": design, architecture, typography, lighting,")
    );
}

#[test]
fn every_fixture_answer_becomes_the_posts_ai_layer() {
    let fixtures: Fixtures = fixture("posts.json");
    let answers: Map<String, Value> = fixture("answers.json");
    let conn = library(&fixtures);
    for post in &fixtures.posts {
        let answer = &answers[&post.id];
        let kind = kind_of(post);
        let patch = normalize::catalog(kind, answer)
            .unwrap_or_else(|e| panic!("{}: {e}", post.id))
            .into_patch("stub", "stub-vision");
        let id = posts::id_for_key(&conn, &post.id).unwrap().unwrap();
        assert!(posts::update_ai(&conn, id, &patch, NOW_MS).unwrap());
        let detail = detail(&conn, &post.id);
        assert_eq!(
            detail.summary.ai_status.as_deref(),
            Some("done"),
            "{}",
            post.id
        );
        assert_eq!(detail.ai_provider.as_deref(), Some("stub"));
        assert_eq!(detail.ai_model.as_deref(), Some("stub-vision"));
        assert_eq!(detail.ai_schema_version, Some(SCHEMA_VERSION));
        assert_eq!(detail.ai_error, None);
        let tiers: Vec<(&str, Option<&str>)> = detail
            .tags
            .iter()
            .filter(|t| t.source == TagSource::Ai)
            .map(|t| (t.norm.as_str(), t.tier.as_deref()))
            .collect();
        assert!(!tiers.is_empty(), "{}", post.id);
        assert!(
            tiers.iter().all(|(_, tier)| tier.is_some()),
            "{}: {tiers:?}",
            post.id
        );
        match kind {
            CatalogKind::Web => {
                assert_eq!(detail.summary.ai_content_type.as_deref(), Some("portfolio"));
                assert_eq!(detail.summary.ai_category.as_deref(), Some("architecture"));
            }
            CatalogKind::Social => {
                assert_eq!(detail.summary.ai_content_type, None, "{}", post.id);
                assert_eq!(detail.summary.ai_category, None, "{}", post.id);
            }
        }
    }
    // The caps: the stairs answer has 4 general and 8 specific tags.
    let stairs = detail(&conn, "stairs");
    let tier = |name: &str| {
        stairs
            .tags
            .iter()
            .filter(|t| t.source == TagSource::Ai && t.tier.as_deref() == Some(name))
            .count()
    };
    assert_eq!(tier("general"), 3);
    assert_eq!(tier("specific"), 7);
    assert_eq!(stairs.ai_entities, ["Barbican Estate"]);
}

#[test]
fn an_answer_off_its_schema_is_a_typed_error_and_writes_nothing() {
    let fixtures: Fixtures = fixture("posts.json");
    let answers: Map<String, Value> = fixture("answers.json");
    let invalid: Vec<InvalidAnswer> = fixture("invalid-answers.json");
    let conn = library(&fixtures);
    // The lamp already holds an analysis; the website does not.
    let lamp = posts::id_for_key(&conn, "lamp").unwrap().unwrap();
    let patch = normalize::catalog(CatalogKind::Social, &answers["lamp"])
        .unwrap()
        .into_patch("stub", "first");
    posts::update_ai(&conn, lamp, &patch, NOW_MS).unwrap();
    let before = [detail(&conn, "lamp"), detail(&conn, "studio-site")];

    assert!(invalid.len() >= 5);
    for case in &invalid {
        let kind = match case.kind.as_str() {
            "web" => CatalogKind::Web,
            _ => CatalogKind::Social,
        };
        // The drain's step: only a normalized catalog becomes a patch.
        let outcome = normalize::catalog(kind, &case.answer);
        let Err(error) = outcome else {
            panic!("{}: accepted", case.why);
        };
        assert!(
            matches!(error, OutputError::SchemaInvalid { .. }),
            "{}",
            case.why
        );
        assert_eq!(error.code(), "schema_invalid");
    }
    assert_eq!(
        normalize::parse_catalog(CatalogKind::Social, "{\"description\": \"cut"),
        Err(OutputError::NotJson)
    );
    let after = [detail(&conn, "lamp"), detail(&conn, "studio-site")];
    assert_eq!(before, after);
}
