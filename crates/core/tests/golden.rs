//! Golden fixtures (plan §6.1): the desktop's TypeScript functions run on fixed
//! inputs by `scripts/golden/`, their outputs stored in `shared/golden/*.jsonl`,
//! and the Rust ports must produce the same JSON, byte for byte.
//!
//! Every fixture file needs a check here; `every_golden_file_has_a_check`
//! fails for a file without one. How to regenerate the files and add a
//! function: `scripts/golden/README.md`.

mod golden_merge;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;
use shelfy_core::repo::Platform;
use shelfy_core::repo::posts::{self, AiPatch, NewPost, UserContentPatch};
use shelfy_core::schema::{self, Kind};
use shelfy_core::search::terms::{SHORT_CONTENT_TERMS, STOPWORDS, extract_content_terms};

/// Golden sets with a check in this file; `<dir>/` stands for every file in
/// that directory.
const CHECKED: &[&str] = &["edits", "extract-content-terms", "merge/"];

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
