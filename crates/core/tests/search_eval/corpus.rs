//! The evaluation library: a desktop library read with `core::legacy` and
//! written into a fresh web library with the repositories, which index every
//! post in `posts_fts` on insert.
//!
//! Only what search reads is copied: captions, authors, the AI layer, notes
//! and manual tags (plus the dates the ranking breaks ties on). Media, files,
//! collections and website captures stay behind; the desktop search reads none
//! of them either. Every desktop row becomes one web post, so the result lists
//! of both apps rank the same set of documents.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rusqlite::params;
use shelfy_core::db::{UserDb, UserDbConfig};
use shelfy_core::ids::{self, CanonicalId, IdError};
use shelfy_core::legacy::{LegacyDb, PostRow, PostTagRow, TagAliasRow, convert};
use shelfy_core::repo::posts::{self, AiLayer, CAPTION_MAX_CHARS, NewPost};
use shelfy_core::repo::{Platform, RepoError};
use tempfile::TempDir;

/// The evaluation library and the way back to the desktop ids.
pub struct Corpus {
    pub db: UserDb,
    /// Web post key → desktop `posts.id`.
    pub legacy_id: HashMap<String, String>,
    pub stats: Stats,
    _dir: TempDir,
}

/// Aggregate counts of the build (no content).
#[derive(Debug, Default)]
pub struct Stats {
    pub posts: usize,
    pub with_ai: usize,
    pub accepted_aliases: usize,
    /// Posts whose desktop id has no canonical key, or whose key another post
    /// already took: they get a synthetic key so no row is lost.
    pub synthetic_keys: usize,
    /// Captions longer than the web limit, cut to it.
    pub truncated_captions: usize,
    pub build_seconds: f64,
    /// Bytes of the token index (`posts_fts`) and of the infix index
    /// (`posts_infix`): the sizes of their data blocks.
    pub fts_bytes: u64,
    pub infix_bytes: u64,
}

/// Builds the evaluation library from the desktop library at `legacy`.
pub fn build(legacy: &Path) -> Corpus {
    let dir = tempfile::tempdir().expect("temp dir");
    let db = UserDb::open(dir.path().join("library.sqlite"), &UserDbConfig::default())
        .expect("open the evaluation library");
    let (legacy_id, stats) = fill(legacy, &db);
    Corpus {
        db,
        legacy_id,
        stats,
        _dir: dir,
    }
}

/// Writes every row of the desktop library at `legacy` into the empty web
/// library `db`; returns the way back to the desktop ids (web key →
/// desktop `posts.id`) and the counts.
pub fn fill(legacy: &Path, db: &UserDb) -> (HashMap<String, String>, Stats) {
    let started = Instant::now();
    let source = LegacyDb::open(legacy).expect("open the desktop library read-only");
    assert!(
        source.is_read_only(),
        "the desktop library must be read-only"
    );
    let rows: Vec<PostRow> = source.read_all().expect("read posts");
    let tiers = tiers(&source);
    let aliases: Vec<TagAliasRow> = source.read_all().expect("read tag aliases");

    let mut stats = Stats::default();
    let mut legacy_id = HashMap::with_capacity(rows.len());
    db.write(|tx| {
        // No repository writes aliases yet (they arrive with `core::tags`); the
        // tag sync resolves through this table, as the desktop did at analysis.
        let mut insert_alias = tx.prepare(
            "INSERT OR IGNORE INTO tag_alias (alias_norm, canonical_norm, canonical_form, status,
                                              created_at)
             VALUES (?1, ?2, ?3, 'accepted', 0)",
        )?;
        for a in aliases.iter().filter(|a| a.status == "accepted") {
            stats.accepted_aliases +=
                insert_alias.execute(params![a.alias_norm, a.canonical_norm, a.canonical_form])?;
        }
        for (n, row) in rows.iter().enumerate() {
            let mut post = new_post(row, tiers.get(&row.id), &mut stats);
            match posts::insert(tx, &post, 0) {
                Ok(_) => {}
                // Another row already has this key or (platform, native id):
                // keep both rows, as the desktop does, under a synthetic
                // identity. The failed insert wrote nothing.
                Err(RepoError::Conflict(_)) => {
                    post.key = format!("eval_row_{n}");
                    post.native_id.clone_from(&post.key);
                    post.platform = Platform::Manual;
                    stats.synthetic_keys += 1;
                    posts::insert(tx, &post, 0)?;
                }
                Err(e) => return Err(e),
            }
            legacy_id.insert(post.key, row.id.clone());
            stats.posts += 1;
        }
        Ok::<_, RepoError>(())
    })
    .expect("write the evaluation library");
    stats.build_seconds = started.elapsed().as_secs_f64();
    (stats.fts_bytes, stats.infix_bytes) = db
        .read(|conn| {
            let bytes = |table: &str| -> rusqlite::Result<u64> {
                conn.query_row(
                    &format!("SELECT coalesce(sum(length(block)), 0) FROM {table}_data"),
                    [],
                    |r| r.get::<_, i64>(0),
                )
                .map(|n| u64::try_from(n).unwrap_or(0))
            };
            Ok::<_, RepoError>((bytes("posts_fts")?, bytes("posts_infix")?))
        })
        .expect("index sizes");
    (legacy_id, stats)
}

/// AI tag tiers per desktop post: `(general, specific)`.
type Tiers = (Vec<String>, Vec<String>);

fn tiers(source: &LegacyDb) -> HashMap<String, Tiers> {
    let mut out: HashMap<String, Tiers> = HashMap::new();
    source
        .stream(|t: PostTagRow| {
            let entry = out.entry(t.post_id).or_default();
            match t.tier.as_deref() {
                Some("general") => entry.0.push(t.tag_form),
                Some("specific") => entry.1.push(t.tag_form),
                _ => {}
            }
            Ok::<_, shelfy_core::legacy::LegacyError>(())
        })
        .expect("read post tags");
    out
}

fn new_post(row: &PostRow, tiers: Option<&Tiers>, stats: &mut Stats) -> NewPost {
    let platform = row.platform.parse().unwrap_or(Platform::Manual);
    let imported_at = convert::epoch_to_ms(row.imported_at).unwrap_or(0);
    let (key, native_id) = match canonical(row, imported_at) {
        Ok(id) => (id.key().to_owned(), id.native_id().to_owned()),
        Err(_) => {
            stats.synthetic_keys += 1;
            (format!("eval_id_{}", row.id), row.id.clone())
        }
    };
    let mut post = NewPost::new(
        key,
        platform,
        native_id,
        row.media_type.clone().unwrap_or_else(|| "image".into()),
        imported_at,
    );
    post.posted_at = match convert::classify_timestamp(row.timestamp.as_deref()) {
        convert::Timestamp::Valid(ms) => Some(ms),
        _ => None,
    };
    post.shortcode = row.shortcode.clone();
    post.post_url = row.post_url.clone();
    post.profile_url = row.profile_url.clone();
    post.author_username = row.author_username.clone();
    post.author_name = row.author_name.clone();
    post.caption = row.text.clone().map(|text| {
        if text.chars().count() > CAPTION_MAX_CHARS {
            stats.truncated_captions += 1;
            text.chars().take(CAPTION_MAX_CHARS).collect()
        } else {
            text
        }
    });
    post.user_note = row.user_note.clone();
    post.user_tags = convert::json_string_array(row.user_tags.as_deref());
    post.web_url = row.web_url.clone();
    post.web_domain = row.web_domain.clone();
    post.web_final_url = row.web_final_url.clone();
    post.ai = ai_layer(row, tiers);
    if post.ai.is_some() {
        stats.with_ai += 1;
    }
    post
}

/// The web identity of a desktop post, as `shelfy-migrate` derives it.
fn canonical(row: &PostRow, imported_at: i64) -> Result<CanonicalId, IdError> {
    let url = [&row.web_url, &row.web_final_url, &row.post_url]
        .into_iter()
        .find_map(|u| u.as_deref().filter(|u| !u.is_empty()));
    match row.platform() {
        Some(ids::Platform::Instagram) => {
            ids::ig::parse_legacy_id(&row.id, row.shortcode.as_deref()).map(|d| d.pk.canonical())
        }
        Some(ids::Platform::Twitter) => ids::x::from_legacy(&row.id, row.post_url.as_deref()),
        Some(ids::Platform::Pinterest) => {
            ids::pinterest::from_legacy(&row.id, row.post_url.as_deref())
        }
        Some(ids::Platform::Web) => url.map_or(Err(IdError::Empty), ids::web::from_url),
        Some(ids::Platform::Manual) => ids::manual::from_legacy(&row.id, imported_at),
        None => Err(IdError::Empty),
    }
}

/// The AI layer of a desktop post, or `None` when it has no AI field.
fn ai_layer(row: &PostRow, tiers: Option<&Tiers>) -> Option<AiLayer> {
    let tags = convert::json_string_array(row.ai_tags.as_deref());
    let keywords = convert::json_string_array(row.ai_keywords.as_deref());
    let entities = convert::json_string_array(row.ai_entities.as_deref());
    let has_text = [&row.ai_description, &row.ai_save_reason]
        .into_iter()
        .any(|v| v.as_deref().is_some_and(|s| !s.trim().is_empty()));
    if row.ai_status.is_none()
        && !has_text
        && tags.is_empty()
        && keywords.is_empty()
        && entities.is_empty()
    {
        return None;
    }
    Some(AiLayer {
        status: row.ai_status.clone(),
        model: row.ai_model.clone(),
        description: row.ai_description.clone(),
        save_reason: row.ai_save_reason.clone(),
        language: row.ai_language.clone(),
        category: row.ai_category.clone(),
        content_type: row.ai_content_type.clone(),
        tags,
        general_tags: tiers.map(|t| t.0.clone()),
        specific_tags: tiers.map(|t| t.1.clone()),
        entities,
        keywords,
        web: row
            .ai_web_json
            .as_deref()
            .and_then(|s| serde_json::from_str(s).ok()),
        analyzed_at: convert::epoch_to_ms(row.ai_analyzed_at),
        ..AiLayer::default()
    })
}
