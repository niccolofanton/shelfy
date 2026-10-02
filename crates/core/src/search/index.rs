//! Maintenance of the `posts_fts` index (plan §2.7, §2.14).
//!
//! The index is contentless (`content=''`, `contentless_delete=1`): it keeps
//! only the inverted index, keyed by `posts.id`. It is maintained explicitly, in
//! the same transaction as every write that changes searchable text, by deleting
//! a post's row and inserting it again ([`reindex_post`]); there are no triggers,
//! so the core decides exactly what is indexed. A post's document is rebuilt
//! from the database each time ([`document`]), so callers never pass text.
//!
//! Only live posts are indexed: moving a post to the trash removes its row and a
//! restore indexes it again, so bm25 statistics ignore the trash. A purge must
//! remove the row too: `posts.id` is a plain rowid that SQLite may reuse for
//! the next insert.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;

/// Maximum characters of a document's `web_text` column (plan §2.7).
pub const WEB_TEXT_MAX_CHARS: usize = 8000;

/// The text of one post, column by column, as stored in `posts_fts`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    /// AI and manual tags: canonical forms from `post_tags` plus the raw forms.
    pub tags: String,
    /// AI keywords.
    pub keywords: String,
    /// AI entities.
    pub entities: String,
    /// AI description.
    pub description: String,
    /// The user's note.
    pub note: String,
    /// Caption (for websites: title, description and hero text).
    pub caption: String,
    /// Author username and display name.
    pub author: String,
    /// Website title, meta description and page digests of the current capture.
    pub web_text: String,
}

impl Document {
    fn is_empty(&self) -> bool {
        self.columns().iter().all(|c| c.is_empty())
    }

    fn columns(&self) -> [&str; 8] {
        [
            &self.tags,
            &self.keywords,
            &self.entities,
            &self.description,
            &self.note,
            &self.caption,
            &self.author,
            &self.web_text,
        ]
    }
}

/// Builds the document of `post_id` from the current database state, or `None`
/// when the post does not exist.
///
/// # Errors
///
/// Fails when a query fails.
pub fn document(conn: &Connection, post_id: i64) -> rusqlite::Result<Option<Document>> {
    let row = conn
        .prepare_cached(
            "SELECT p.ai_tags_json, p.user_tags_json, p.ai_keywords_json, p.ai_entities_json,
                    p.ai_description, p.user_note, p.caption, p.author_username, p.author_name,
                    wc.title, wc.meta_json, wc.pages_json
             FROM posts p LEFT JOIN web_captures wc ON wc.id = p.current_capture_id
             WHERE p.id = ?1",
        )?
        .query_row([post_id], |r| {
            Ok(PostText {
                ai_tags: r.get(0)?,
                user_tags: r.get(1)?,
                keywords: r.get(2)?,
                entities: r.get(3)?,
                description: r.get(4)?,
                note: r.get(5)?,
                caption: r.get(6)?,
                author_username: r.get(7)?,
                author_name: r.get(8)?,
                web_title: r.get(9)?,
                web_meta: r.get(10)?,
                web_pages: r.get(11)?,
            })
        })
        .optional()?;
    let Some(text) = row else {
        return Ok(None);
    };

    let mut tags = Joiner::default();
    {
        let mut stmt = conn.prepare_cached(
            "SELECT tag_form FROM post_tags WHERE post_id = ?1 ORDER BY tag_norm, source",
        )?;
        let forms = stmt.query_map([post_id], |r| r.get::<_, String>(0))?;
        for form in forms {
            tags.push(&form?);
        }
    }
    for form in string_items(text.ai_tags.as_deref())
        .into_iter()
        .chain(string_items(text.user_tags.as_deref()))
    {
        tags.push(&form);
    }

    let mut entities = Joiner::default();
    for form in string_items(text.entities.as_deref()) {
        entities.push(&form);
    }
    {
        let mut stmt = conn.prepare_cached(
            "SELECT ent_form FROM post_entities WHERE post_id = ?1 ORDER BY ent_norm",
        )?;
        let forms = stmt.query_map([post_id], |r| r.get::<_, String>(0))?;
        for form in forms {
            entities.push(&form?);
        }
    }

    let mut keywords = Joiner::default();
    for kw in string_items(text.keywords.as_deref()) {
        keywords.push(&kw);
    }

    let mut author = Joiner::default();
    for part in [text.author_username, text.author_name]
        .into_iter()
        .flatten()
    {
        author.push(&part);
    }

    Ok(Some(Document {
        tags: tags.finish(),
        keywords: keywords.finish(),
        entities: entities.finish(),
        description: text.description.unwrap_or_default(),
        note: text.note.unwrap_or_default(),
        caption: text.caption.unwrap_or_default(),
        author: author.finish(),
        web_text: web_text(
            text.web_title.as_deref(),
            text.web_meta.as_deref(),
            text.web_pages.as_deref(),
        ),
    }))
}

/// Replaces the index row of `post_id` with its current document, or removes it
/// when the post no longer exists, is in the trash, or has no text.
///
/// # Errors
///
/// Fails when a query fails.
pub fn reindex_post(conn: &Connection, post_id: i64) -> rusqlite::Result<()> {
    remove_post(conn, post_id)?;
    let live: bool = conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM posts WHERE id = ?1 AND deleted_at IS NULL)")?
        .query_row([post_id], |r| r.get(0))?;
    if !live {
        return Ok(());
    }
    if let Some(doc) = document(conn, post_id)?
        && !doc.is_empty()
    {
        let [
            tags,
            keywords,
            entities,
            description,
            note,
            caption,
            author,
            web_text,
        ] = doc.columns();
        conn.prepare_cached(
            "INSERT INTO posts_fts (rowid, tags, keywords, entities, description, note, caption,
                                    author, web_text)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )?
        .execute(params![
            post_id,
            tags,
            keywords,
            entities,
            description,
            note,
            caption,
            author,
            web_text
        ])?;
    }
    Ok(())
}

/// Removes the index row of `post_id` (a no-op when there is none).
///
/// # Errors
///
/// Fails when the delete fails.
pub fn remove_post(conn: &Connection, post_id: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("DELETE FROM posts_fts WHERE rowid = ?1")?
        .execute([post_id])?;
    Ok(())
}

/// Rebuilds the whole index from the live posts. Used after a bulk install
/// (migration) or a tokenizer change. Returns the posts visited.
///
/// # Errors
///
/// Fails when a query fails; run it inside a transaction to keep the old index
/// on failure.
pub fn rebuild(conn: &Connection) -> rusqlite::Result<usize> {
    conn.execute(
        "INSERT INTO posts_fts (posts_fts) VALUES ('delete-all')",
        [],
    )?;
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM posts WHERE deleted_at IS NULL ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for &id in &ids {
        reindex_post(conn, id)?;
    }
    Ok(ids.len())
}

/// Names of the temporary tables of [`verify`].
const EXPECTED: &str = "shelfy_verify_expected";
const EXPECTED_VOCAB: &str = "shelfy_verify_expected_vocab";
const LIVE_VOCAB: &str = "shelfy_verify_live_vocab";

/// Checks the index against the database: returns the rowids whose index
/// entries differ from their post's current [`document`] (a post missing
/// from the index or indexed with stale text, or a row left behind by a
/// trashed, purged or textless post), sorted. Empty when the index is
/// consistent.
///
/// It indexes every live post's document again in a temporary FTS5 table
/// with the live table's columns and tokenizer, and compares the two indexes
/// token by token (term, column and offset) through `fts5vocab`. For tests
/// and operator checks: it reads the whole library, and needs a connection
/// that may create temporary tables.
///
/// # Errors
///
/// Fails when a query fails, or when the live table's definition cannot be
/// read.
pub fn verify(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let definition: String = conn.query_row(
        "SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'posts_fts'",
        [],
        |r| r.get(0),
    )?;
    let arguments = expected_arguments(&definition).ok_or_else(|| {
        rusqlite::Error::SqliteFailure(
            rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_ERROR),
            Some("unexpected posts_fts definition".to_owned()),
        )
    })?;
    drop_verify_tables(conn)?;
    conn.execute_batch(&format!(
        "CREATE VIRTUAL TABLE temp.{EXPECTED} USING fts5({arguments});
         CREATE VIRTUAL TABLE temp.{EXPECTED_VOCAB} USING fts5vocab(temp, {EXPECTED}, instance);
         CREATE VIRTUAL TABLE temp.{LIVE_VOCAB} USING fts5vocab(main, posts_fts, instance);"
    ))?;
    let outcome = compare(conn);
    drop_verify_tables(conn)?;
    outcome
}

fn compare(conn: &Connection) -> rusqlite::Result<Vec<i64>> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM posts WHERE deleted_at IS NULL ORDER BY id")?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut insert = conn.prepare(&format!(
        "INSERT INTO temp.{EXPECTED} (rowid, tags, keywords, entities, description, note,
                                      caption, author, web_text)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
    ))?;
    for id in ids {
        let Some(doc) = document(conn, id)? else {
            continue;
        };
        if doc.is_empty() {
            continue;
        }
        let [c1, c2, c3, c4, c5, c6, c7, c8] = doc.columns();
        insert.execute(params![id, c1, c2, c3, c4, c5, c6, c7, c8])?;
    }
    let columns = "term, doc, col, offset";
    let sql = format!(
        "SELECT doc FROM (SELECT {columns} FROM temp.{LIVE_VOCAB}
                          EXCEPT SELECT {columns} FROM temp.{EXPECTED_VOCAB})
         UNION
         SELECT doc FROM (SELECT {columns} FROM temp.{EXPECTED_VOCAB}
                          EXCEPT SELECT {columns} FROM temp.{LIVE_VOCAB})
         ORDER BY doc"
    );
    conn.prepare(&sql)?.query_map([], |r| r.get(0))?.collect()
}

fn drop_verify_tables(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(&format!(
        "DROP TABLE IF EXISTS temp.{LIVE_VOCAB};
         DROP TABLE IF EXISTS temp.{EXPECTED_VOCAB};
         DROP TABLE IF EXISTS temp.{EXPECTED};"
    ))
}

/// The `fts5(…)` arguments of the live table without its content options:
/// the same columns, tokenizer and prefixes, for a table with content.
fn expected_arguments(definition: &str) -> Option<String> {
    let start = definition.find("fts5(")? + "fts5(".len();
    let end = definition.rfind(')')?;
    let arguments: Vec<&str> = definition
        .get(start..end)?
        .split(',')
        .map(str::trim)
        .filter(|a| !a.starts_with("content") && !a.starts_with("contentless_delete"))
        .collect();
    Some(arguments.join(", "))
}

struct PostText {
    ai_tags: Option<String>,
    user_tags: Option<String>,
    keywords: Option<String>,
    entities: Option<String>,
    description: Option<String>,
    note: Option<String>,
    caption: Option<String>,
    author_username: Option<String>,
    author_name: Option<String>,
    web_title: Option<String>,
    web_meta: Option<String>,
    web_pages: Option<String>,
}

/// Newline-joined distinct values (case-insensitive), in insertion order.
#[derive(Default)]
struct Joiner {
    seen: HashSet<String>,
    out: String,
}

impl Joiner {
    fn push(&mut self, value: &str) {
        let value = value.trim();
        if value.is_empty() || !self.seen.insert(value.to_lowercase()) {
            return;
        }
        if !self.out.is_empty() {
            self.out.push('\n');
        }
        self.out.push_str(value);
    }

    fn finish(self) -> String {
        self.out
    }
}

/// The string items of a JSON array column; anything else counts as empty, as
/// the desktop's defensive `parseTags` does.
fn string_items(json: Option<&str>) -> Vec<String> {
    match json.map(serde_json::from_str::<Value>) {
        Some(Ok(Value::Array(items))) => items
            .into_iter()
            .filter_map(|v| match v {
                Value::String(s) => Some(s),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Title + meta description + per-page digests, capped at
/// [`WEB_TEXT_MAX_CHARS`]. The capture model lands with the capture service
/// (P4); until then pages are read defensively: each page object contributes
/// its `title`, `description` (or `meta.description`) and `digest` (or
/// `contentText`/`text`).
fn web_text(title: Option<&str>, meta_json: Option<&str>, pages_json: Option<&str>) -> String {
    let mut parts: Vec<String> = Vec::new();
    if let Some(title) = title {
        parts.push(title.to_owned());
    }
    if let Some(Ok(meta)) = meta_json.map(serde_json::from_str::<Value>) {
        parts.extend(first_str(&meta, &["description", "ogDescription"]));
    }
    if let Some(Ok(Value::Array(pages))) = pages_json.map(serde_json::from_str::<Value>) {
        for page in &pages {
            parts.extend(first_str(page, &["title"]));
            let meta_description = page
                .get("meta")
                .and_then(|m| first_str(m, &["description"]));
            parts.extend(first_str(page, &["description"]).or(meta_description));
            parts.extend(first_str(page, &["digest", "contentText", "text"]));
        }
    }
    let mut out = String::new();
    let mut chars = 0;
    for part in parts {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let sep = usize::from(!out.is_empty());
        let room = WEB_TEXT_MAX_CHARS.saturating_sub(chars + sep);
        if room == 0 {
            break;
        }
        if sep == 1 {
            out.push('\n');
        }
        let taken: String = part.chars().take(room).collect();
        chars += sep + taken.chars().count();
        out.push_str(&taken);
    }
    out
}

fn first_str(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| value.get(k).and_then(Value::as_str))
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn web_text_is_capped_in_characters() {
        let long = "è".repeat(WEB_TEXT_MAX_CHARS);
        let pages = serde_json::json!([{ "title": "Home", "contentText": long }]).to_string();
        let text = web_text(Some("Site"), None, Some(&pages));
        assert_eq!(text.chars().count(), WEB_TEXT_MAX_CHARS);
        assert!(text.starts_with("Site\nHome\nèè"));
    }

    #[test]
    fn web_text_reads_meta_descriptions() {
        let meta = r#"{"description":"Studio site"}"#;
        let pages =
            r#"[{"title":"Work","meta":{"description":"Our projects"},"digest":"Case studies"}]"#;
        assert_eq!(
            web_text(None, Some(meta), Some(pages)),
            "Studio site\nWork\nOur projects\nCase studies"
        );
    }

    #[test]
    fn the_check_table_keeps_columns_and_tokenizer_but_not_contentless() {
        let definition = "CREATE VIRTUAL TABLE posts_fts USING fts5(
  tags, keywords, entities, description, note, caption, author, web_text,
  content='', contentless_delete=1,
  tokenize=\"unicode61 remove_diacritics 2\", prefix='2 3')";
        assert_eq!(
            expected_arguments(definition).unwrap(),
            "tags, keywords, entities, description, note, caption, author, web_text, \
             tokenize=\"unicode61 remove_diacritics 2\", prefix='2 3'"
        );
    }

    #[test]
    fn string_items_ignore_bad_json() {
        assert_eq!(string_items(Some(r#"["a", 1, "b"]"#)), ["a", "b"]);
        assert!(string_items(Some("not json")).is_empty());
        assert!(string_items(None).is_empty());
    }
}
