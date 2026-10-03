//! Golden fixtures (plan §6.1): the desktop's TypeScript functions run on fixed
//! inputs by `scripts/golden/`, their outputs stored in `shared/golden/*.jsonl`,
//! and the Rust ports must produce the same JSON, byte for byte.
//!
//! Every fixture file needs a check here; `every_golden_file_has_a_check`
//! fails for a file without one. How to regenerate the files and add a
//! function: `scripts/golden/README.md`.

mod golden_ai;
mod golden_chat;
mod golden_merge;
mod golden_tag_search;
mod golden_taxonomy;
mod golden_taxonomy_prompt;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::json;
use serde_json::value::RawValue;
use shelfy_core::ingest::hosts::PINTEREST_HOSTS;
use shelfy_core::ingest::sanitize::clean_item;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, AiLayer, AiPatch, NewPost, UserContentPatch};
use shelfy_core::schema::{self, Kind};
use shelfy_core::search::terms::{SHORT_CONTENT_TERMS, STOPWORDS, extract_content_terms};
use shelfy_core::web::captures::{self, CaptureStatus, NewCapture};
use shelfy_core::web::sites::{self, PageRequest, SiteQuery, SiteSort};
use shelfy_core::web::{color, similar};

/// Golden sets with a check in this file; `<dir>/` stands for every file in
/// that directory.
const CHECKED: &[&str] = &[
    "ai/catalog/",
    "ai/clusters/",
    "ai/aliases/",
    "ai/chat/",
    "ai/taxonomy-prompts/",
    "edits",
    "extract-content-terms",
    "hosts",
    "merge/",
    "sanitize",
    "tag-search",
    "web/",
];

#[derive(Deserialize)]
struct Header {
    golden: String,
    source: String,
    format: u32,
}

#[derive(Deserialize)]
struct Case<'a> {
    id: String,
    #[serde(borrow)]
    args: &'a RawValue,
    #[serde(borrow)]
    output: &'a RawValue,
}

fn golden_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../shared/golden")
}

fn read(name: &str) -> String {
    let path = golden_dir().join(format!("{name}.jsonl"));
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// Parses a golden file: the header, then one case per line.
fn parse<'a>(name: &str, text: &'a str) -> Vec<Case<'a>> {
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next().expect("header line")).unwrap();
    assert_eq!(header.golden, name);
    assert_eq!(header.format, 1, "{name}: unsupported format");
    assert!(!header.source.is_empty());
    lines
        .map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{name}: {e}: {line}")))
        .collect()
}

/// Runs `port` on every case and compares its JSON with the recorded output.
fn check<A: DeserializeOwned, R: serde::Serialize>(name: &str, port: impl Fn(A) -> R) {
    let text = read(name);
    let cases = parse(name, &text);
    assert!(!cases.is_empty(), "{name}: no cases");
    let mut failures = Vec::new();
    for case in &cases {
        let args: A = serde_json::from_str(case.args.get())
            .unwrap_or_else(|e| panic!("{name}/{}: bad args: {e}", case.id));
        let actual = serde_json::to_string(&port(args)).unwrap();
        if actual != case.output.get() {
            failures.push(format!(
                "  {}\n    desktop: {}\n    rust:    {actual}",
                case.id,
                case.output.get()
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} of {} cases differ from the desktop:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// Like [`check`], but compares the deserialized Rust value instead of the
/// raw JSON text: for an output with floats, `serde_json` always writes a
/// decimal point (`0.0`) where `JSON.stringify` drops it for a whole number
/// (`0`) — the same IEEE754 value, different bytes. Byte comparison still
/// catches a real difference (floats from the same formula either match
/// exactly or are clearly wrong), so this only trades the stricter check for
/// one that is not fooled by that one formatting quirk.
fn check_numeric<A, R>(name: &str, port: impl Fn(A) -> R)
where
    A: DeserializeOwned,
    R: DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let text = read(name);
    let cases = parse(name, &text);
    assert!(!cases.is_empty(), "{name}: no cases");
    let mut failures = Vec::new();
    for case in &cases {
        let args: A = serde_json::from_str(case.args.get())
            .unwrap_or_else(|e| panic!("{name}/{}: bad args: {e}", case.id));
        let expected: R = serde_json::from_str(case.output.get())
            .unwrap_or_else(|e| panic!("{name}/{}: bad recorded output: {e}", case.id));
        let actual = port(args);
        if actual != expected {
            failures.push(format!(
                "  {}\n    desktop: {expected:?}\n    rust:    {actual:?}",
                case.id
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{name}: {} of {} cases differ from the desktop:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

/// The golden sets under `dir`, as `<subdir>/<name>` without `.jsonl`.
fn golden_files(dir: &Path, prefix: &str, found: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_stem().unwrap().to_string_lossy().into_owned();
        if path.is_dir() {
            golden_files(&path, &format!("{prefix}{name}/"), found);
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            found.push(format!("{prefix}{name}"));
        }
    }
}

#[test]
fn every_golden_file_has_a_check() {
    let mut found = Vec::new();
    golden_files(&golden_dir(), "", &mut found);
    found.sort();
    let covers = |check: &str, name: &str| {
        check == name || (check.ends_with('/') && name.starts_with(check))
    };
    for name in &found {
        assert!(
            CHECKED.iter().any(|check| covers(check, name)),
            "{name}.jsonl has no check in golden.rs"
        );
    }
    for check in CHECKED {
        assert!(
            found.iter().any(|name| covers(check, name)),
            "{check}: no golden file"
        );
    }
}

#[test]
fn merge_matches_the_desktop() {
    let (steps, failures) = golden_merge::check_dir(&golden_dir().join("merge"));
    assert!(
        failures.is_empty(),
        "merge: {} of {steps} steps differ from the desktop:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn sanitize_matches_the_desktop() {
    // The desktop's `sanitizeInterceptedBatch` returns the items it keeps; the
    // port's `clean_item` keeps or rejects each one (`sanitize_batch` then
    // derives keys and dates, which the desktop does not).
    check("sanitize", |(items, platform): (Vec<Value>, String)| {
        let platform: Platform = platform.parse().unwrap();
        items
            .iter()
            .filter_map(|item| clean_item(platform, item).ok())
            .collect::<Vec<_>>()
    });
}

#[test]
fn pinterest_hosts_match_the_extension() {
    check("hosts", |(platform,): (String,)| {
        assert_eq!(platform, "pinterest");
        PINTEREST_HOSTS
    });
}

#[derive(Deserialize)]
struct TermOptions {
    #[serde(rename = "minLen")]
    min_len: Option<usize>,
}

#[test]
fn extract_content_terms_matches_the_desktop() {
    check(
        "extract-content-terms",
        |(query, opts): (String, TermOptions)| {
            extract_content_terms(&query, opts.min_len.unwrap_or(3))
        },
    );
}

#[test]
fn word_lists_match_the_desktop() {
    // The golden cases list every desktop stopword and short term; the outputs
    // prove Rust drops or keeps each of them, and this proves Rust has no extra.
    let text = read("extract-content-terms");
    let words = |id: &str| -> BTreeSet<String> {
        let case = parse("extract-content-terms", &text)
            .into_iter()
            .find(|c| c.id == id)
            .unwrap_or_else(|| panic!("case {id} missing"));
        let (query, _): (String, serde_json::Value) =
            serde_json::from_str(case.args.get()).unwrap();
        query.split(' ').map(str::to_owned).collect()
    };
    let rust =
        |list: &[&str]| -> BTreeSet<String> { list.iter().map(|w| (*w).to_owned()).collect() };
    assert_eq!(rust(STOPWORDS), words("stopwords-all"));
    assert_eq!(rust(SHORT_CONTENT_TERMS), words("short-terms-all"));
}

// ── edits: `updateUserContent` and `updateAiAnalysis` ───────────────────────

/// The clock the generator froze: 2026-10-02T00:00:00Z.
const NOW_MS: i64 = 1_790_899_200_000;

/// A `tag_alias` row: alias, canonical norm, canonical form, status.
type Alias = (String, String, String, String);

/// One call of a case.
#[derive(Deserialize)]
#[serde(tag = "op", content = "fields", rename_all = "lowercase")]
enum Step {
    /// `updateUserContent`.
    User(UserFields),
    /// `updateAiAnalysis`.
    Ai(Box<AiFields>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct UserFields {
    #[serde(default, deserialize_with = "present")]
    note: Option<Option<String>>,
    manual_tags: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AiFields {
    #[serde(default, deserialize_with = "present")]
    description: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    tags: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "present")]
    status: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    model: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    category: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    content_type: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    entities: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "present")]
    keywords: Option<Option<Vec<String>>>,
    #[serde(default, deserialize_with = "present")]
    language: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    save_reason: Option<Option<String>>,
    #[serde(default, deserialize_with = "present")]
    analyzed_at: Option<Option<i64>>,
    general_tags: Option<Vec<String>>,
    specific_tags: Option<Vec<String>>,
}

/// A field that is present, `null` included: JavaScript's "not undefined".
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<Option<T>>, D::Error> {
    Option::<T>::deserialize(deserializer).map(Some)
}

/// What the generator records, field for field (`layers` in `edits.ts`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Layers {
    user_note: Option<String>,
    user_tags: Option<Value>,
    ai_status: Option<String>,
    ai_model: Option<String>,
    ai_description: Option<String>,
    ai_tags: Option<Value>,
    ai_category: Option<String>,
    ai_content_type: Option<String>,
    ai_entities: Option<Value>,
    ai_keywords: Option<Value>,
    ai_language: Option<String>,
    ai_save_reason: Option<String>,
    ai_analyzed_at: Value,
    manual_tag_rows: Vec<(String, String)>,
    ai_tag_rows: Vec<(String, String, Option<String>)>,
    entity_rows: Vec<(String, String)>,
}

fn layers(conn: &Connection, id: i64) -> Layers {
    let json = |raw: Option<String>| raw.map(|s| serde_json::from_str::<Value>(&s).unwrap());
    let rows = |sql: &str| -> Vec<(String, String, Option<String>)> {
        conn.prepare(sql)
            .unwrap()
            .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    };
    let pairs = |sql: &str| -> Vec<(String, String)> {
        rows(sql).into_iter().map(|(a, b, _)| (a, b)).collect()
    };
    let mut layers = conn
        .query_row(
            "SELECT user_note, user_tags_json, ai_status, ai_model, ai_description, ai_tags_json,
                    ai_category, ai_content_type, ai_entities_json, ai_keywords_json, ai_language,
                    ai_save_reason, ai_analyzed_at
             FROM posts WHERE id = ?1",
            [id],
            |r| {
                let at: Option<i64> = r.get(12)?;
                Ok(Layers {
                    user_note: r.get(0)?,
                    user_tags: json(r.get(1)?),
                    ai_status: r.get(2)?,
                    ai_model: r.get(3)?,
                    ai_description: r.get(4)?,
                    ai_tags: json(r.get(5)?),
                    ai_category: r.get(6)?,
                    ai_content_type: r.get(7)?,
                    ai_entities: json(r.get(8)?),
                    ai_keywords: json(r.get(9)?),
                    ai_language: r.get(10)?,
                    ai_save_reason: r.get(11)?,
                    // The desktop stamps seconds, the web milliseconds.
                    ai_analyzed_at: match at {
                        None => Value::Null,
                        Some(NOW_MS) => Value::from("now"),
                        Some(at) => Value::from(at),
                    },
                    manual_tag_rows: Vec::new(),
                    ai_tag_rows: Vec::new(),
                    entity_rows: Vec::new(),
                })
            },
        )
        .unwrap();
    layers.manual_tag_rows = pairs(
        "SELECT tag_norm, tag_form, NULL FROM post_tags
         WHERE post_id = ?1 AND source = 'manual' ORDER BY tag_norm",
    );
    layers.ai_tag_rows = rows(
        "SELECT tag_norm, tag_form, tier FROM post_tags
         WHERE post_id = ?1 AND source = 'ai' ORDER BY tag_norm",
    );
    layers.entity_rows = pairs(
        "SELECT ent_norm, ent_form, NULL FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
    );
    layers
}

#[test]
fn edits_match_the_desktop() {
    check("edits", |(aliases, steps): (Vec<Alias>, Vec<Step>)| {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        for (alias, norm, form, status) in &aliases {
            conn.execute(
                "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status,
                                        created_at)
                 VALUES (?1, ?2, ?3, ?4, 0)",
                params![alias, norm, form, status],
            )
            .unwrap();
        }
        let post = NewPost::new("ig_1", Platform::Instagram, "1", "image", NOW_MS);
        let id = posts::insert(&conn, &post, NOW_MS).unwrap();
        for step in steps {
            match step {
                Step::User(f) => {
                    let patch = UserContentPatch {
                        note: f.note,
                        tags: f.manual_tags,
                    };
                    posts::update_user_content(&conn, id, &patch, NOW_MS).unwrap();
                }
                Step::Ai(f) => {
                    let f = *f;
                    let patch = AiPatch {
                        status: f.status,
                        // Web-only columns: the desktop has none.
                        provider: None,
                        schema_version: None,
                        error: None,
                        model: f.model,
                        description: f.description,
                        tags: f.tags,
                        general_tags: f.general_tags,
                        specific_tags: f.specific_tags,
                        category: f.category,
                        content_type: f.content_type,
                        entities: f.entities,
                        keywords: f.keywords,
                        language: f.language,
                        save_reason: f.save_reason,
                        analyzed_at: f.analyzed_at,
                    };
                    posts::update_ai(&conn, id, &patch, NOW_MS).unwrap();
                }
            }
        }
        layers(&conn, id)
    });
}

// ── web/*: colour math and "similar sites" (P4-05) ──────────────────────────
//
// `q` and the domain-prefix match are not golden: the port's FTS5 search
// replaces the desktop's `LIKE` scan on purpose (see
// `shelfy_core::web::sites`'s module docs), the same way `repo::posts`'s own
// search is plain-Rust-tested, never golden, against the desktop.

use std::collections::HashMap;

fn web_library() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    conn
}

/// A web post, optionally with a current capture carrying `palette` and/or
/// an AI layer carrying `ai_web_json.facets` — mirrors
/// `scripts/golden/web-sites.ts`'s `insertSite` without going through
/// `upsertWebReference`/`applyAiAnalysis` either, for the same reason: full,
/// direct control over the fixture shape.
/// One day in milliseconds, for `web_site`'s `days_ago`.
const DAY_MS: i64 = 86_400_000;

#[allow(clippy::too_many_arguments)]
fn web_site(
    conn: &Connection,
    key: &str,
    domain: &str,
    title: &str,
    palette: Option<Value>,
    facets: Option<Value>,
    days_ago: i64,
) {
    let at = NOW_MS - days_ago * DAY_MS;
    let mut post = NewPost::new(key, Platform::Web, key, "website", at);
    post.web_domain = Some(domain.to_owned());
    post.author_name = Some(title.to_owned());
    if let Some(facets) = facets {
        post.ai = Some(AiLayer {
            status: Some("done".into()),
            web: Some(serde_json::json!({ "facets": facets })),
            ..AiLayer::default()
        });
    }
    let id = posts::insert(conn, &post, at).unwrap();
    if let Some(palette) = palette {
        let mut capture = NewCapture::new(at);
        capture.status = CaptureStatus::Done;
        capture.palette = Some(palette);
        captures::insert(conn, id, &capture, &[], at).unwrap();
    }
}

#[test]
fn hex_to_lab_matches_the_desktop() {
    // `cbrt`/`powf` are not required to be bit-identical across platforms
    // (unlike +, -, *, / under IEEE754): V8's `Math.cbrt`/`Math.pow` and
    // Rust's libm can differ by a couple of ULP on the same input. An
    // absolute-or-relative epsilon many orders above that noise floor
    // (~2.2e-16 relative) still catches any real algorithmic difference.
    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() <= 1e-9 * a.abs().max(b.abs()).max(1e-9)
    }
    let text = read("web/hex-to-lab");
    let cases = parse("web/hex-to-lab", &text);
    assert!(!cases.is_empty(), "web/hex-to-lab: no cases");
    let mut failures = Vec::new();
    for case in &cases {
        let (hex,): (String,) = serde_json::from_str(case.args.get()).unwrap();
        let expected: Option<[f64; 3]> = serde_json::from_str(case.output.get()).unwrap();
        let actual = color::hex_to_lab(&hex).map(|l| [l.l, l.a, l.b]);
        let matches = match (expected, actual) {
            (None, None) => true,
            (Some(e), Some(a)) => e.iter().zip(a.iter()).all(|(&x, &y)| close(x, y)),
            _ => false,
        };
        if !matches {
            failures.push(format!(
                "  {}\n    desktop: {expected:?}\n    rust:    {actual:?}",
                case.id
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "web/hex-to-lab: {} of {} cases differ from the desktop:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

// ── web/color-filter ─────────────────────────────────────────────────────

fn color_filter_library() -> Connection {
    let conn = web_library();
    // `days_ago` matches `scripts/golden/web-sites.ts`'s `COLOR_SITES`: a
    // distinct capture time per site, so the "no filter" case's recency
    // order is unambiguous on both sides (never a real SQL tie).
    web_site(
        &conn,
        "web_near_black",
        "near-black.test",
        "Near Black",
        Some(json!(["#010101"])),
        None,
        0,
    );
    web_site(
        &conn,
        "web_mid_grey",
        "mid-grey.test",
        "Mid Grey",
        Some(json!([{"hex": "#808080", "role": "surface"}])),
        None,
        1,
    );
    web_site(
        &conn,
        "web_white",
        "white.test",
        "White",
        Some(json!([{"hex": "#ffffff", "role": "background"}])),
        None,
        2,
    );
    web_site(
        &conn,
        "web_decoy_text",
        "decoy-text.test",
        "Decoy Text",
        Some(json!([{"hex": "#fefefe", "role": "text"}, {"hex": "#303030", "role": "surface"}])),
        None,
        3,
    );
    web_site(
        &conn,
        "web_no_palette",
        "no-palette.test",
        "No Palette",
        None,
        None,
        4,
    );
    web_site(
        &conn,
        "web_empty_palette",
        "empty-palette.test",
        "Empty Palette",
        Some(json!([])),
        None,
        5,
    );
    conn
}

#[derive(Deserialize)]
struct ColorFilterArgs {
    color: String,
    #[serde(default)]
    sort: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ColorFilterOutput {
    ids: Vec<String>,
    total: i64,
}

#[test]
fn color_filter_matches_the_desktop() {
    check("web/color-filter", |(args,): (ColorFilterArgs,)| {
        let conn = color_filter_library();
        // The desktop's `!query.sort` branch ALSO resorts by distance when a
        // `color` parses (`filterWebPosts`); the port's `sort` has no such
        // third "absent" state distinct from `recent` (its HTTP default).
        // None of this fixture's cases turn on the distinction (each color
        // matches at most one site), so mapping an absent `sort` to the
        // port's own default (`recent`) is a safe, documented simplification.
        let sort = match args.sort.as_deref() {
            Some("name") => SiteSort::Name,
            Some("color") => SiteSort::Color,
            Some("recent") | None => SiteSort::Recent,
            Some(other) => panic!("unknown sort {other}"),
        };
        let query = SiteQuery {
            color: Some(args.color),
            sort,
            ..Default::default()
        };
        let page = sites::list(
            &conn,
            &query,
            &PageRequest {
                limit: 10,
                cursor: None,
            },
        )
        .unwrap();
        ColorFilterOutput {
            ids: page.items.iter().map(|s| s.key.clone()).collect(),
            total: i64::try_from(page.items.len()).unwrap(),
        }
    });
}

// ── web/facets ────────────────────────────────────────────────────────────
//
// JSON object key order is not part of the contract here (facets are a
// dynamically-keyed map; the desktop's own key order is an incidental
// artefact of a stable sort by count, not a documented property), so this
// check does not use `check()`'s literal byte comparison: it normalizes both
// sides to a `facet -> {value -> count}` map before comparing.

fn facets_library() -> Connection {
    let conn = web_library();
    web_site(
        &conn,
        "web_a",
        "a.test",
        "A",
        None,
        Some(json!({"style": ["Minimal", "Bold"], "siteType": ["portfolio"]})),
        0,
    );
    web_site(
        &conn,
        "web_b",
        "b.test",
        "B",
        None,
        Some(json!({"style": ["minimal"]})),
        0,
    );
    web_site(
        &conn,
        "web_c",
        "c.test",
        "C",
        None,
        Some(json!({"style": ["bold"], "siteType": ["blog"]})),
        0,
    );
    web_site(&conn, "web_d", "d.test", "D", None, Some(json!({})), 0);
    web_site(&conn, "web_e", "e.test", "E", None, None, 0);
    conn
}

#[derive(Deserialize)]
struct FacetsArgs {
    facets: Option<HashMap<String, Vec<String>>>,
}

/// `facet -> (value -> count)`, dropping the array/key order that is not
/// part of the contract (see above), keeping the exact display `value`
/// casing (which is).
fn facet_counts_by_value(
    counts: &serde_json::Map<String, Value>,
) -> HashMap<String, HashMap<String, i64>> {
    counts
        .iter()
        .map(|(facet, values)| {
            let by_value = values
                .as_array()
                .unwrap()
                .iter()
                .map(|v| {
                    let v = v.as_object().unwrap();
                    (
                        v["value"].as_str().unwrap().to_owned(),
                        v["count"].as_i64().unwrap(),
                    )
                })
                .collect();
            (facet.clone(), by_value)
        })
        .collect()
}

#[test]
fn facets_match_the_desktop() {
    let text = read("web/facets");
    let cases = parse("web/facets", &text);
    assert!(!cases.is_empty(), "web/facets: no cases");
    let mut failures = Vec::new();
    for case in &cases {
        let (args,): (Option<FacetsArgs>,) = serde_json::from_str(case.args.get()).unwrap();
        let conn = facets_library();
        let facets: BTreeMap<String, Vec<String>> = args
            .and_then(|a| a.facets)
            .unwrap_or_default()
            .into_iter()
            .collect();
        let query = SiteQuery {
            facets,
            ..Default::default()
        };
        let actual = sites::facet_counts(&conn, &query).unwrap();
        let actual_json = serde_json::to_value(&actual).unwrap();
        let expected_json: Value = serde_json::from_str(case.output.get()).unwrap();
        let actual_norm = facet_counts_by_value(actual_json.as_object().unwrap());
        let expected_norm = facet_counts_by_value(expected_json.as_object().unwrap());
        if actual_norm != expected_norm {
            failures.push(format!(
                "  {}\n    desktop: {:?}\n    rust:    {:?}",
                case.id, expected_norm, actual_norm
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "web/facets: {} of {} cases differ from the desktop:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
}

// ── web/similar ───────────────────────────────────────────────────────────

fn similar_library() -> Connection {
    let conn = web_library();
    web_site(
        &conn,
        "web_target",
        "target.test",
        "Target",
        Some(json!([{"hex": "#000000", "role": "background"}])),
        Some(
            json!({"style": ["minimal", "bold"], "siteType": ["portfolio"], "colorMood": ["dark"]}),
        ),
        0,
    );
    web_site(
        &conn,
        "web_close",
        "close.test",
        "Close",
        Some(json!([{"hex": "#050505", "role": "background"}])),
        Some(json!({"style": ["minimal"]})),
        0,
    );
    web_site(
        &conn,
        "web_closer",
        "closer.test",
        "Closer",
        None,
        Some(json!({"siteType": ["Portfolio"]})),
        0,
    );
    web_site(
        &conn,
        "web_tie_near",
        "tie-near.test",
        "Tie Near",
        Some(json!([{"hex": "#000000", "role": "background"}])),
        Some(json!({"style": ["bold"]})),
        0,
    );
    web_site(
        &conn,
        "web_tie_far",
        "tie-far.test",
        "Tie Far",
        Some(json!([{"hex": "#ffffff", "role": "background"}])),
        Some(json!({"style": ["bold"]})),
        0,
    );
    web_site(
        &conn,
        "web_unrelated",
        "unrelated.test",
        "Unrelated",
        None,
        Some(json!({"tech": ["wordpress"]})),
        0,
    );
    web_site(
        &conn,
        "web_no_facets",
        "no-facets.test",
        "No Facets",
        Some(json!([{"hex": "#000000", "role": "background"}])),
        None,
        0,
    );
    conn
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SharedFacetRecord {
    facet: String,
    value: String,
}

#[derive(Deserialize, Debug, PartialEq)]
#[serde(rename_all = "camelCase")]
struct SimilarRecord {
    id: String,
    score: f64,
    shared: Vec<String>,
    shared_facets: Vec<SharedFacetRecord>,
}

#[test]
fn similar_matches_the_desktop() {
    check_numeric("web/similar", |(key, limit): (String, u32)| {
        let conn = similar_library();
        similar::for_site(&conn, &key, limit)
            .unwrap()
            .into_iter()
            .map(|s| SimilarRecord {
                id: s.key,
                score: s.score,
                shared: s.shared,
                shared_facets: s
                    .shared_facets
                    .into_iter()
                    .map(|f| SharedFacetRecord {
                        facet: f.facet,
                        value: f.value,
                    })
                    .collect(),
            })
            .collect::<Vec<_>>()
    });
}
