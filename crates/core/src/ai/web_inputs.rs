//! Website inputs selected from the current v1 capture. Stored object references
//! only: no remote URL is fetched when a site is catalogued.
use super::{inputs::FrameObject, sanitize};
use crate::repo::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::collections::HashSet;

pub const MAX_FRAMES: usize = 4;
pub const DIGEST_MAX: usize = 8000;
#[derive(Clone, Debug)]
pub struct WebInputs {
    pub key: String,
    pub capture_id: i64,
    pub digest: String,
    pub tech: Vec<String>,
    pub frames: Vec<FrameObject>,
    /// The measured desktop web fields, including P4 metadata wrapper.
    pub post: Value,
}

pub fn select(conn: &Connection, post_id: i64) -> Result<Option<WebInputs>> {
    let row=conn.query_row("SELECT p.key,c.id,c.title,c.meta_json,c.pages_json,c.tech_json,c.hero_object
        FROM posts p JOIN web_captures c ON c.id=p.current_capture_id WHERE p.id=?1 AND c.post_id=p.id AND p.deleted_at IS NULL",[post_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,Option<i64>>(6)?))).optional()?;
    let Some((key, capture_id, title, meta, pages, tech, hero)) = row else {
        return Ok(None);
    };
    let json = |s: Option<String>| {
        s.and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .unwrap_or(Value::Null)
    };
    let meta = json(meta);
    let pages = json(pages);
    let tech = json(tech);
    let (palette, fonts, awards, traits, domain, author)=conn.query_row("SELECT c.palette_json,c.fonts_json,c.awards_json,c.traits_json,p.web_domain,p.author_name FROM web_captures c JOIN posts p ON p.id=c.post_id WHERE c.id=?1",[capture_id],|r|Ok((r.get::<_,Option<String>>(0)?,r.get::<_,Option<String>>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?)))?;
    let mut measured = meta
        .get("metadata")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| meta.clone());
    if !measured.is_object() {
        measured = serde_json::json!({});
    }
    if tech.is_array() {
        measured["tech"] = tech.clone();
    }
    let traits = json(traits);
    if traits.is_object() {
        measured["traits"] = traits;
    }
    let post = serde_json::json!({"webMeta":measured,"webPalette":json(palette),"webFonts":json(fonts),"webAwards":json(awards),"webDomain":domain,"authorName":author});
    let mut parts = title.into_iter().collect::<Vec<_>>();
    if let Some(description) = meta.get("description").and_then(Value::as_str) {
        parts.push(description.into());
    }
    for page in pages.as_array().into_iter().flatten().take(8) {
        for name in [
            "title",
            "metaDescription",
            "description",
            "contentText",
            "digest",
            "text",
        ] {
            match page.get(name) {
                Some(Value::String(s)) => parts.push(s.clone()),
                Some(Value::Object(v)) if name == "digest" => {
                    for name in ["contentText", "text", "title", "description", "h1"] {
                        if let Some(Value::String(s)) = v.get(name) {
                            parts.push(s.clone())
                        }
                    }
                    for name in ["headings", "ctas"] {
                        for item in v
                            .get(name)
                            .and_then(Value::as_array)
                            .into_iter()
                            .flatten()
                            .take(8)
                        {
                            if let Some(s) = item.as_str() {
                                parts.push(s.into());
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let digest = sanitize::text(&parts.join("\n"), DIGEST_MAX - 1);
    let mut hints = vec![];
    for item in tech.as_array().into_iter().flatten() {
        if let Some(name) = item
            .as_str()
            .or_else(|| item.get("name").and_then(Value::as_str))
        {
            let hint = sanitize::text(name, 120);
            if !hint.is_empty() && !hints.contains(&hint) {
                hints.push(hint);
            }
        }
        if hints.len() >= 30 {
            break;
        }
    }
    // Hero first. Legacy captures may omit it: use their first screenshot.
    let mut ids = hero.into_iter().collect::<Vec<_>>();
    if ids.is_empty() && let Some(id)=conn.query_row("SELECT object_id FROM web_capture_assets WHERE capture_id=?1 AND role IN ('hero','screenshot') ORDER BY CASE role WHEN 'hero' THEN 0 ELSE 1 END,page_index,seq LIMIT 1",[capture_id],|r|r.get::<_,i64>(0)).optional()? {ids.push(id);}

    let bands=conn.prepare_cached("SELECT object_id FROM web_capture_assets WHERE capture_id=?1 AND role IN ('band','section') ORDER BY page_index,COALESCE(css_top,seq),seq,role")?.query_map([capture_id],|r|r.get::<_,i64>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    let mut added = 0;
    for id in bands {
        if !ids.contains(&id) {
            ids.push(id);
            added += 1;
        }
        if added >= 3 {
            break;
        }
    }
    let mut seen = HashSet::new();
    let mut frames = vec![];
    for id in ids {
        let obj = conn
            .query_row(
                "SELECT sha256,ext,mime,variants FROM media_objects WHERE id=?1",
                params![id],
                |r| {
                    Ok(FrameObject {
                        sha256: r.get(0)?,
                        ext: r.get(1)?,
                        mime: r.get(2)?,
                        variants: r.get(3)?,
                    })
                },
            )
            .optional()?;
        if let Some(obj) = obj
            && seen.insert(obj.sha256.clone())
        {
            frames.push(obj);
        }
        if frames.len() >= MAX_FRAMES {
            break;
        }
    }
    Ok(Some(WebInputs {
        key,
        capture_id,
        digest,
        tech: hints,
        frames,
        post,
    }))
}

/// Websites in a frozen preview population (no widening or live selector).
pub fn count_ids(conn: &Connection, ids: &[i64]) -> Result<u64> {
    let ids = serde_json::to_string(ids).expect("integer ids serialize");
    let n=conn.query_row("SELECT count(*) FROM posts WHERE (platform='web' OR media_type='website') AND id IN (SELECT value FROM json_each(?1))",[ids],|r|r.get::<_,i64>(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}
