//! Deterministic taxonomy maintenance. No provider calls; callers run graph
//! computation on a blocking thread and persist proposals in a library write.
pub mod aliases;
pub mod clusters;
pub mod embeddings;
pub mod graph;

use crate::repo::Result;
use crate::search::terms::js_trim;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Proposed,
    Accepted,
}
impl Status {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Accepted => "accepted",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VocabTag {
    pub norm: String,
    pub form: String,
    pub count: u64,
}
pub(crate) fn norm(s: &str) -> String {
    js_trim(s).to_lowercase()
}
pub(crate) fn js_cmp(a: &str, b: &str) -> Ordering {
    a.encode_utf16().cmp(b.encode_utf16())
}
/// Counts distinct posts: a tag written in both layers is still one occurrence.
pub(crate) fn vocabulary(conn: &Connection) -> Result<Vec<VocabTag>> {
    let mut stmt = conn.prepare_cached(
        "SELECT t.tag_norm, COUNT(DISTINCT t.post_id),
        (SELECT f.tag_form FROM post_tags f JOIN posts fp ON fp.id=f.post_id
         WHERE f.tag_norm=t.tag_norm AND fp.deleted_at IS NULL
         GROUP BY f.tag_form ORDER BY COUNT(DISTINCT f.post_id) DESC, f.tag_form LIMIT 1)
        FROM post_tags t JOIN posts p ON p.id=t.post_id WHERE p.deleted_at IS NULL
        GROUP BY t.tag_norm ORDER BY COUNT(DISTINCT t.post_id) DESC, t.tag_norm",
    )?;
    Ok(stmt
        .query_map([], |r| {
            Ok(VocabTag {
                norm: r.get(0)?,
                count: r.get::<_, i64>(1)? as u64,
                form: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}
