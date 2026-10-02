//! Notifications (plan §2.7 `notifications`, §2.10 `notification`): the
//! activity history that the desktop kept in memory, persisted per library.
//!
//! The server writes one when something needs the user's attention (a job
//! failed for good, a migration finished, a quota ran out). A notification
//! carries no prose, only codes the client maps to its own strings:
//!
//! - `kind`: the area it belongs to (`job`, `migration`, `quota`, …);
//! - `code`: what happened (`job.failed`, …), stable like an API error code;
//! - `params`: the values its message needs, as a JSON object;
//! - `target`: where it leads, a post key or an app route, if anywhere.
//!
//! Ids grow with time, so the newest first order is `id DESC` and a page
//! cursor is the last id seen. A library keeps its newest [`KEEP`]
//! notifications; [`create`] drops older ones.

use rusqlite::{Connection, Row, params};
use serde::Serialize;
use serde_json::{Map, Value};

use super::{RepoError, Result};

/// Notifications kept per library; [`create`] deletes the oldest beyond it.
pub const KEEP: u32 = 1_000;
/// Longest `kind` or `code`, in characters.
pub const MAX_CODE_CHARS: usize = 100;
/// Longest `target`, in characters.
pub const MAX_TARGET_CHARS: usize = 500;
/// Largest `params`, serialized as JSON, in bytes.
pub const MAX_PARAMS_BYTES: usize = 4_096;
/// Largest page of [`list`].
pub const MAX_PAGE_SIZE: u32 = 200;

/// A notification to create.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewNotification {
    /// Area (`job`, `migration`, …): 1–100 characters of `a-z`, `0-9`, `.`,
    /// `_` and `-`.
    pub kind: String,
    /// What happened (`job.failed`, …), with the charset of `kind`.
    pub code: String,
    /// Values for the client's message; at most [`MAX_PARAMS_BYTES`] as JSON.
    pub params: Map<String, Value>,
    /// A post key or an app route; at most [`MAX_TARGET_CHARS`].
    pub target: Option<String>,
}

/// A stored notification.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    /// Id; newer notifications have larger ids.
    pub id: i64,
    /// Area.
    pub kind: String,
    /// What happened.
    pub code: String,
    /// Values for the message; empty when there are none.
    pub params: Map<String, Value>,
    /// Where it leads.
    pub target: Option<String>,
    /// When it was created.
    pub created_at: i64,
    /// When it was marked read; `None` while unread.
    pub read_at: Option<i64>,
}

/// One page of [`list`], newest first.
#[derive(Clone, Debug, PartialEq)]
pub struct NotificationPage {
    /// The notifications.
    pub items: Vec<Notification>,
    /// The id to pass as `before` for the next page; `None` on the last page.
    pub next_before: Option<i64>,
}

/// Which notifications [`mark_read`] marks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReadSelector {
    /// These ids; unknown ids are skipped.
    Ids(Vec<i64>),
    /// Every notification up to this id, included: "mark all read" without
    /// touching one that arrived after the list was shown.
    UpTo(i64),
}

/// Creates a notification, unread, and deletes the oldest beyond [`KEEP`].
///
/// # Errors
///
/// [`RepoError::Invalid`] for a bad `kind`, `code`, `target` or `params`;
/// database errors otherwise.
pub fn create(conn: &Connection, new: &NewNotification, now: i64) -> Result<Notification> {
    check_code("kind", &new.kind)?;
    check_code("code", &new.code)?;
    if let Some(target) = &new.target
        && (target.is_empty() || target.chars().count() > MAX_TARGET_CHARS)
    {
        return Err(RepoError::Invalid {
            field: "target",
            reason: "must be 1-500 characters",
        });
    }
    let params_json = if new.params.is_empty() {
        None
    } else {
        let json = serde_json::to_string(&new.params).expect("a JSON map serializes");
        if json.len() > MAX_PARAMS_BYTES {
            return Err(RepoError::Invalid {
                field: "params",
                reason: "larger than 4096 bytes",
            });
        }
        Some(json)
    };
    conn.prepare_cached(
        "INSERT INTO notifications (kind, code, params_json, target, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5)",
    )?
    .execute(params![new.kind, new.code, params_json, new.target, now])?;
    let id = conn.last_insert_rowid();
    // Only the oldest rows go, so ids keep growing and cursors stay valid.
    conn.prepare_cached(
        "DELETE FROM notifications WHERE id < (SELECT id FROM notifications
           ORDER BY id DESC LIMIT 1 OFFSET ?1)",
    )?
    .execute([KEEP - 1])?;
    Ok(Notification {
        id,
        kind: new.kind.clone(),
        code: new.code.clone(),
        params: new.params.clone(),
        target: new.target.clone(),
        created_at: now,
        read_at: None,
    })
}

/// A page of notifications, newest first: those with an id below `before`
/// when given. `limit` is clamped to 1–[`MAX_PAGE_SIZE`].
///
/// # Errors
///
/// Database errors.
pub fn list(conn: &Connection, before: Option<i64>, limit: u32) -> Result<NotificationPage> {
    let limit = limit.clamp(1, MAX_PAGE_SIZE);
    let mut items = conn
        .prepare_cached(
            "SELECT id, kind, code, params_json, target, created_at, read_at FROM notifications
             WHERE ?1 IS NULL OR id < ?1 ORDER BY id DESC LIMIT ?2",
        )?
        .query_map(params![before, limit + 1], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = items.len() > limit as usize;
    items.truncate(limit as usize);
    let next_before = if more {
        items.last().map(|n| n.id)
    } else {
        None
    };
    Ok(NotificationPage { items, next_before })
}

/// Number of unread notifications.
///
/// # Errors
///
/// Database errors.
pub fn unread_count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn
        .prepare_cached("SELECT count(*) FROM notifications WHERE read_at IS NULL")?
        .query_row([], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Marks notifications read at `now`; already read ones keep their time.
/// Returns how many changed.
///
/// # Errors
///
/// Database errors.
pub fn mark_read(conn: &Connection, selector: &ReadSelector, now: i64) -> Result<u64> {
    let changed = match selector {
        ReadSelector::Ids(ids) => conn
            .prepare_cached(
                "UPDATE notifications SET read_at = ?2
                 WHERE read_at IS NULL AND id IN (SELECT value FROM json_each(?1))",
            )?
            .execute(params![super::id_list(ids), now])?,
        ReadSelector::UpTo(up_to) => conn
            .prepare_cached(
                "UPDATE notifications SET read_at = ?2 WHERE read_at IS NULL AND id <= ?1",
            )?
            .execute(params![up_to, now])?,
    };
    Ok(changed as u64)
}

fn from_row(r: &Row<'_>) -> rusqlite::Result<Notification> {
    let params = match r
        .get::<_, Option<String>>(3)?
        .map(|json| serde_json::from_str::<Value>(&json))
    {
        Some(Ok(Value::Object(map))) => map,
        // Defensive, like the JSON list columns: anything else reads as empty.
        _ => Map::new(),
    };
    Ok(Notification {
        id: r.get(0)?,
        kind: r.get(1)?,
        code: r.get(2)?,
        params,
        target: r.get(4)?,
        created_at: r.get(5)?,
        read_at: r.get(6)?,
    })
}

/// A `kind` or `code`: 1–[`MAX_CODE_CHARS`] of `a-z`, `0-9`, `.`, `_`, `-`.
fn check_code(field: &'static str, value: &str) -> Result<()> {
    let valid = !value.is_empty()
        && value.len() <= MAX_CODE_CHARS
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b));
    if valid {
        Ok(())
    } else {
        Err(RepoError::Invalid {
            field,
            reason: "must be 1-100 characters of a-z, 0-9, '.', '_' and '-'",
        })
    }
}
