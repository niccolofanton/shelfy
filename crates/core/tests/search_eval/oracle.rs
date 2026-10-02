//! The ground truth, computed like the desktop harness does (`makeOracle` in
//! `scripts/search-eval/run.ts`): raw SQL on its own read-only connection to the
//! desktop library, never through the code under test.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags};

/// Read-only access to the desktop library for the gold sets.
pub struct Oracle {
    conn: Connection,
    /// `tag_norm` → posts carrying it, over the whole library.
    global_tag_count: HashMap<String, i64>,
}

impl Oracle {
    /// Opens `path` read-only, never creating or writing it. As `core::legacy`
    /// does, a library with no `-wal` or `-journal` file is opened immutable:
    /// a plain read-only open of a closed WAL library leaves new `-wal` and
    /// `-shm` files next to it.
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
        let sidecar = |suffix: &str| {
            let mut name = path.as_os_str().to_owned();
            name.push(suffix);
            PathBuf::from(name).exists()
        };
        let conn = match immutable_uri(path) {
            Some(uri) if !sidecar("-wal") && !sidecar("-journal") => {
                Connection::open_with_flags(uri, flags | OpenFlags::SQLITE_OPEN_URI)?
            }
            _ => Connection::open_with_flags(path, flags)?,
        };
        conn.execute_batch("PRAGMA query_only = ON;")?;
        let global_tag_count = conn
            .prepare("SELECT tag_norm AS t, COUNT(*) c FROM post_tags GROUP BY tag_norm")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(Self {
            conn,
            global_tag_count,
        })
    }

    /// Legacy ids of the posts whose caption, AI description, AI keywords or AI
    /// tags contain any of `terms` (`goldPosts`: SQLite `LIKE '%term%'`, so ASCII
    /// case-insensitive).
    pub fn gold_posts(&self, terms: &[&str]) -> rusqlite::Result<HashSet<String>> {
        let cols = ["text", "ai_description", "ai_keywords", "ai_tags"];
        let clause = |_: &&str| {
            let any: Vec<String> = cols.iter().map(|c| format!("{c} LIKE ?")).collect();
            format!("({})", any.join(" OR "))
        };
        let clauses: Vec<String> = terms.iter().map(clause).collect();
        let sql = format!("SELECT id FROM posts WHERE {}", clauses.join(" OR "));
        let params: Vec<String> = terms
            .iter()
            .flat_map(|t| cols.iter().map(move |_| format!("%{t}%")))
            .collect();
        self.conn
            .prepare(&sql)?
            .query_map(rusqlite::params_from_iter(params.iter()), |r| r.get(0))?
            .collect()
    }

    /// The tags a query ideally maps to (`goldTags`): tags carried by the gold
    /// posts, ranked by prevalence × distinctiveness (`in_set × lift`). Tags
    /// with real mass (`in_set ≥ 2`, `lift ≥ 0.03`) first; only when none
    /// qualifies, distinctive one-offs (`lift ≥ 0.25`). At most `top`.
    pub fn gold_tags(&self, gold: &HashSet<String>, top: usize) -> rusqlite::Result<Vec<String>> {
        if gold.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(&gold.iter().collect::<Vec<_>>()).expect("ids serialize");
        // GROUP BY returns the groups in tag order; the stable sort below keeps
        // that order among equal scores, as the desktop's stable sort does.
        let counts: Vec<(String, i64)> = self
            .conn
            .prepare(
                "SELECT tag_norm AS t, COUNT(*) c FROM post_tags
                 WHERE post_id IN (SELECT value FROM json_each(?1)) GROUP BY tag_norm",
            )?
            .query_map([ids], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?;
        let rows: Vec<(String, i64, f64)> = counts
            .into_iter()
            .map(|(tag, in_set)| {
                let global = self
                    .global_tag_count
                    .get(&tag)
                    .copied()
                    .filter(|&g| g != 0)
                    .unwrap_or(in_set);
                let lift = in_set as f64 / global as f64;
                (tag, in_set, lift)
            })
            .collect();
        let main: Vec<_> = rows
            .iter()
            .filter(|(_, in_set, lift)| *in_set >= 2 && *lift >= 0.03)
            .cloned()
            .collect();
        let mut pick = if main.is_empty() {
            rows.into_iter()
                .filter(|(_, _, lift)| *lift >= 0.25)
                .collect()
        } else {
            main
        };
        let score = |r: &(String, i64, f64)| r.1 as f64 * r.2;
        pick.sort_by(|a, b| score(b).total_cmp(&score(a)));
        Ok(pick.into_iter().take(top).map(|r| r.0).collect())
    }
}

/// `file://<absolute path>?immutable=1`, percent-encoded; `None` for a path
/// that is not valid UTF-8.
fn immutable_uri(path: &Path) -> Option<String> {
    let absolute = std::path::absolute(path).ok()?;
    let mut text = absolute.to_str()?.replace('\\', "/");
    if !text.starts_with('/') {
        // A Windows drive path: file:///C:/…
        text.insert(0, '/');
    }
    let mut uri = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'/' | b'-' | b'.' | b'_' | b'~' | b':') {
            uri.push(char::from(byte));
        } else {
            uri.push_str(&format!("%{byte:02X}"));
        }
    }
    uri.push_str("?immutable=1");
    Some(uri)
}
