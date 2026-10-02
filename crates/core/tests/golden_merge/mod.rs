//! The merge golden sets, `shared/golden/merge/*.jsonl`: the desktop's
//! `bulkUpsert` run by `scripts/golden/merge.ts`, against
//! `shelfy_core::ingest::merge`.
//!
//! Each file is a scenario and each case a step of it, run in order on one
//! library. A step's posts are the desktop's write-path shape; [`incoming`]
//! maps them the way an import would: canonical keys from the desktop ids,
//! ISO dates to unix ms, analysis times from seconds to ms, and every local
//! path to a media object (the post's own path to its cover object, a slide's
//! to the slide's object). After the step, [`view`] reads the library back in
//! the generator's view, which must equal the recorded output byte for byte.

use std::collections::HashMap;
use std::path::Path;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use serde_json::{Map, Value};
use sha1::{Digest, Sha1};
use shelfy_core::ids::{ig, pinterest, x};
use shelfy_core::ingest::merge::{AiFields, IncomingPost, UpsertOptions, upsert_batch};
use shelfy_core::legacy::convert::parse_iso8601_ms;
use shelfy_core::repo::Platform;
use shelfy_core::repo::media::{NewMediaObject, upsert_object};
use shelfy_core::repo::posts::NewMedia;
use shelfy_core::schema::{self, Kind};

#[derive(Deserialize)]
struct Header {
    golden: String,
    source: String,
    format: u32,
}

#[derive(Deserialize)]
struct Case<'a> {
    id: String,
    args: (Step,),
    #[serde(borrow)]
    output: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Step {
    now: i64,
    overwrite_ai: bool,
    #[serde(default)]
    aliases: Vec<Alias>,
    posts: Vec<Map<String, Value>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Alias {
    alias_norm: String,
    canonical_norm: String,
    canonical_form: String,
    status: String,
}

/// Runs every scenario of `dir` and returns one message per differing step.
pub fn check_dir(dir: &Path) -> (usize, Vec<String>) {
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "{}: no merge golden files",
        dir.display()
    );
    let mut steps = 0;
    let mut failures = Vec::new();
    for file in &files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        let text = std::fs::read_to_string(file).unwrap();
        steps += check_scenario(&name, &text, &mut failures);
    }
    (steps, failures)
}

fn check_scenario(name: &str, text: &str, failures: &mut Vec<String>) -> usize {
    let mut lines = text.lines();
    let header: Header = serde_json::from_str(lines.next().expect("header line")).unwrap();
    assert_eq!(header.golden, format!("merge/{name}"));
    assert_eq!(header.source, "electron/db.ts#bulkUpsert");
    assert_eq!(header.format, 1, "merge/{name}: unsupported format");

    let mut conn = Connection::open_in_memory().unwrap();
    conn.pragma_update(None, "foreign_keys", "ON").unwrap();
    schema::migrate(&mut conn, Kind::Library).unwrap();
    let mut objects = Objects::default();
    let mut steps = 0;
    for line in lines {
        let case: Case<'_> =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("merge/{name}: {e}: {line}"));
        let step = case.args.0;
        let tx = conn.transaction().unwrap();
        for alias in &step.aliases {
            tx.execute(
                "INSERT INTO tag_alias (alias_norm, canonical_norm, canonical_form, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    alias.alias_norm,
                    alias.canonical_norm,
                    alias.canonical_form,
                    alias.status,
                    step.now
                ],
            )
            .unwrap();
        }
        let batch: Vec<IncomingPost> = step
            .posts
            .iter()
            .map(|post| incoming(&tx, &mut objects, post, step.now))
            .collect();
        let options = UpsertOptions {
            overwrite_ai: step.overwrite_ai,
        };
        let summary = upsert_batch(&tx, &batch, options, step.now)
            .unwrap_or_else(|e| panic!("merge/{name}/{}: {e}", case.id));
        tx.commit().unwrap();
        let actual = serde_json::to_string(&StepView {
            inserted: summary.inserted,
            skipped: summary.merged,
            ai_updated: summary.ai_updated,
            posts: view(&conn, &objects),
        })
        .unwrap();
        if actual != case.output.get() {
            failures.push(describe(name, &case.id, case.output.get(), &actual));
        }
        steps += 1;
    }
    steps
}

/// The first differences between the desktop's output and Rust's.
fn describe(scenario: &str, step: &str, desktop: &str, rust: &str) -> String {
    let parse = |text: &str| serde_json::from_str::<Value>(text).unwrap();
    let (d, r) = (parse(desktop), parse(rust));
    let mut lines = vec![format!("  merge/{scenario}/{step}")];
    for field in ["inserted", "skipped", "aiUpdated"] {
        if d[field] != r[field] {
            lines.push(format!(
                "    {field}: desktop {} rust {}",
                d[field], r[field]
            ));
        }
    }
    let empty = Vec::new();
    let posts = |v: &Value| -> HashMap<String, Value> {
        v["posts"]
            .as_array()
            .unwrap_or(&empty)
            .iter()
            .map(|p| (p["id"].as_str().unwrap_or_default().to_owned(), p.clone()))
            .collect()
    };
    let (dp, rp) = (posts(&d), posts(&r));
    let mut ids: Vec<&String> = dp.keys().chain(rp.keys()).collect();
    ids.sort();
    ids.dedup();
    for id in ids {
        if dp.get(id) != rp.get(id) {
            let show = |p: Option<&Value>| p.map_or("(missing)".to_owned(), Value::to_string);
            lines.push(format!(
                "    post {id}\n      desktop: {}",
                show(dp.get(id))
            ));
            lines.push(format!("      rust:    {}", show(rp.get(id))));
        }
    }
    if lines.len() == 1 {
        lines.push(format!("    desktop: {desktop}\n    rust:    {rust}"));
    }
    lines.join("\n")
}

// ── From the desktop's shape ─────────────────────────────────────────────────

/// Local paths, as media objects of the library.
#[derive(Default)]
struct Objects {
    by_path: HashMap<String, i64>,
    paths: HashMap<i64, String>,
}

impl Objects {
    fn id(&mut self, conn: &Connection, path: &str, now: i64) -> i64 {
        if let Some(&id) = self.by_path.get(path) {
            return id;
        }
        let mut sha256 = [0u8; 32];
        sha256[..20].copy_from_slice(&Sha1::digest(path.as_bytes()));
        let object = NewMediaObject {
            sha256,
            ext: "bin".to_owned(),
            mime: "application/octet-stream".to_owned(),
            bytes: i64::try_from(path.len()).unwrap(),
            width: None,
            height: None,
            duration_ms: None,
            role: "image".to_owned(),
            variants: 0,
            origin: "migration".to_owned(),
        };
        let id = upsert_object(conn, &object, now).unwrap();
        self.by_path.insert(path.to_owned(), id);
        self.paths.insert(id, path.to_owned());
        id
    }

    fn path(&self, id: Option<i64>) -> Option<String> {
        id.map(|id| self.paths[&id].clone())
    }
}

/// A desktop write-path post as an [`IncomingPost`].
fn incoming(
    conn: &Connection,
    objects: &mut Objects,
    post: &Map<String, Value>,
    now: i64,
) -> IncomingPost {
    let text = |key: &str| -> Option<String> {
        match post.get(key) {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) => Some(s.clone()),
            Some(other) => panic!("{key}: not a string: {other}"),
        }
    };
    let id = text("id").expect("id");
    let media_type = text("mediaType").expect("mediaType");
    let canonical = match text("platform").as_deref() {
        Some("instagram") => ig::parse_legacy_id(&id, text("shortcode").as_deref())
            .unwrap()
            .pk
            .canonical(),
        Some("twitter") => x::from_legacy(&id, text("postUrl").as_deref()).unwrap(),
        Some("pinterest") => pinterest::from_legacy(&id, text("postUrl").as_deref()).unwrap(),
        other => panic!("{id}: platform {other:?}"),
    };
    assert_eq!(canonical.native_id(), id, "fixture ids are canonical");
    let platform = canonical.platform().as_str().parse::<Platform>().unwrap();
    let mut p = IncomingPost::new(canonical.key(), platform, id.clone(), media_type);
    p.shortcode = text("shortcode");
    p.post_url = text("postUrl");
    p.profile_url = text("profileUrl");
    p.author_username = text("authorUsername");
    p.author_name = text("authorName");
    p.caption = text("text");
    p.cover_url = text("thumbnailUrl");
    p.web_url = text("webUrl");
    p.web_domain = text("webDomain");
    p.web_final_url = text("webFinalUrl");
    p.posted_at = match text("timestamp").as_deref() {
        None | Some("") => None,
        Some(date) => Some(parse_iso8601_ms(date).unwrap_or_else(|| panic!("{id}: {date}"))),
    };
    p.cover_object = ["thumbnailPath", "imagePath", "videoPath"]
        .into_iter()
        .find_map(text)
        .map(|path| objects.id(conn, &path, now));
    for m in post
        .get("media")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let kind = if m["type"] == "video" {
            "video"
        } else {
            "image"
        };
        p.media.push(NewMedia {
            kind: kind.to_owned(),
            source_url: m.get("url").and_then(Value::as_str).map(str::to_owned),
            object_id: m
                .get("localPath")
                .and_then(Value::as_str)
                .map(|path| objects.id(conn, path, now)),
            ..NewMedia::default()
        });
    }
    p.ai = ai_fields(post);
    p
}

fn ai_fields(post: &Map<String, Value>) -> AiFields {
    let text = |key: &str| -> Option<Option<String>> {
        post.get(key).map(|v| match v {
            Value::Null => None,
            Value::String(s) => Some(s.clone()),
            other => panic!("{key}: not a string: {other}"),
        })
    };
    let list = |key: &str| -> Option<Option<Vec<String>>> {
        post.get(key).map(|v| match v {
            Value::Null => None,
            Value::Array(items) => Some(
                items
                    .iter()
                    .map(|t| t.as_str().expect("string tags").to_owned())
                    .collect(),
            ),
            other => panic!("{key}: not a list: {other}"),
        })
    };
    AiFields {
        status: text("aiStatus"),
        model: text("aiModel"),
        description: text("aiDescription"),
        save_reason: text("aiSaveReason"),
        language: text("aiLanguage"),
        category: text("aiCategory"),
        content_type: text("aiContentType"),
        tags: list("aiTags"),
        general_tags: list("aiGeneralTags"),
        specific_tags: list("aiSpecificTags"),
        entities: list("aiEntities"),
        keywords: list("aiKeywords"),
        // The desktop counts seconds.
        analyzed_at: post
            .get("aiAnalyzedAt")
            .map(|v| v.as_i64().map(|seconds| seconds * 1_000)),
        provider: None,
        schema_version: None,
    }
}

// ── The view ─────────────────────────────────────────────────────────────────

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StepView {
    inserted: usize,
    skipped: usize,
    ai_updated: usize,
    posts: Vec<PostView>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PostView {
    id: String,
    platform: String,
    shortcode: Option<String>,
    post_url: Option<String>,
    profile_url: Option<String>,
    author_username: Option<String>,
    author_name: Option<String>,
    text: Option<String>,
    thumbnail_url: Option<String>,
    media_type: String,
    media_count: i64,
    web_url: Option<String>,
    web_domain: Option<String>,
    web_final_url: Option<String>,
    posted_at: Option<i64>,
    sort_ts: i64,
    cover: Option<String>,
    media: Vec<SlideView>,
    ai: AiView,
    tag_rows: Vec<TagRowView>,
    entity_rows: Vec<EntityRowView>,
}

#[derive(Serialize)]
struct SlideView {
    r#type: String,
    url: Option<String>,
    local: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AiView {
    status: Option<String>,
    model: Option<String>,
    description: Option<String>,
    category: Option<String>,
    content_type: Option<String>,
    language: Option<String>,
    save_reason: Option<String>,
    analyzed_at: Option<i64>,
    tags: Vec<String>,
    entities: Vec<String>,
    keywords: Vec<String>,
}

#[derive(Serialize)]
struct TagRowView {
    norm: String,
    form: String,
    tier: Option<String>,
}

#[derive(Serialize)]
struct EntityRowView {
    norm: String,
    form: String,
}

/// Every post of the library, by native id (the desktop id).
fn view(conn: &Connection, objects: &Objects) -> Vec<PostView> {
    let mut stmt = conn
        .prepare(
            "SELECT id, native_id, platform, shortcode, post_url, profile_url, author_username,
                    author_name, caption, cover_url, media_type, media_count, web_url, web_domain,
                    web_final_url, posted_at, sort_ts, cover_object, ai_status, ai_model,
                    ai_description, ai_category, ai_content_type, ai_language, ai_save_reason,
                    ai_analyzed_at, ai_tags_json, ai_entities_json, ai_keywords_json
             FROM posts ORDER BY native_id",
        )
        .unwrap();
    let rows: Vec<(i64, PostView)> = stmt
        .query_map([], |r| {
            Ok((
                r.get(0)?,
                PostView {
                    id: r.get(1)?,
                    platform: r.get(2)?,
                    shortcode: r.get(3)?,
                    post_url: r.get(4)?,
                    profile_url: r.get(5)?,
                    author_username: r.get(6)?,
                    author_name: r.get(7)?,
                    text: r.get(8)?,
                    thumbnail_url: r.get(9)?,
                    media_type: r.get(10)?,
                    media_count: r.get(11)?,
                    web_url: r.get(12)?,
                    web_domain: r.get(13)?,
                    web_final_url: r.get(14)?,
                    posted_at: r.get(15)?,
                    sort_ts: r.get(16)?,
                    cover: objects.path(r.get(17)?),
                    media: Vec::new(),
                    ai: AiView {
                        status: r.get(18)?,
                        model: r.get(19)?,
                        description: r.get(20)?,
                        category: r.get(21)?,
                        content_type: r.get(22)?,
                        language: r.get(23)?,
                        save_reason: r.get(24)?,
                        analyzed_at: r.get(25)?,
                        tags: strings(r.get(26)?),
                        entities: strings(r.get(27)?),
                        keywords: strings(r.get(28)?),
                    },
                    tag_rows: Vec::new(),
                    entity_rows: Vec::new(),
                },
            ))
        })
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    rows.into_iter()
        .map(|(id, mut post)| {
            post.media = conn
                .prepare_cached(
                    "SELECT kind, source_url, object_id, video_object_id FROM post_media
                     WHERE post_id = ?1 ORDER BY position",
                )
                .unwrap()
                .query_map([id], |r| {
                    let video: Option<i64> = r.get(3)?;
                    assert!(video.is_none(), "the golden never keeps a video");
                    Ok(SlideView {
                        r#type: r.get(0)?,
                        url: r.get(1)?,
                        local: objects.path(r.get(2)?),
                    })
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            post.tag_rows = conn
                .prepare_cached(
                    "SELECT tag_norm, tag_form, tier FROM post_tags WHERE post_id = ?1
                     ORDER BY tag_norm",
                )
                .unwrap()
                .query_map([id], |r| {
                    Ok(TagRowView {
                        norm: r.get(0)?,
                        form: r.get(1)?,
                        tier: r.get(2)?,
                    })
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            post.entity_rows = conn
                .prepare_cached(
                    "SELECT ent_norm, ent_form FROM post_entities WHERE post_id = ?1
                     ORDER BY ent_norm",
                )
                .unwrap()
                .query_map([id], |r| {
                    Ok(EntityRowView {
                        norm: r.get(0)?,
                        form: r.get(1)?,
                    })
                })
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            let manual: Option<i64> = conn
                .query_row(
                    "SELECT 1 FROM post_tags WHERE post_id = ?1 AND source = 'manual'",
                    [id],
                    |r| r.get(0),
                )
                .optional()
                .unwrap();
            assert!(manual.is_none(), "the merge never writes manual tags");
            post
        })
        .collect()
}

/// A JSON array column of strings; NULL reads as empty.
fn strings(raw: Option<String>) -> Vec<String> {
    raw.map(|text| serde_json::from_str(&text).expect("a JSON array of strings"))
        .unwrap_or_default()
}
