//! Durable export metadata. Files are addressed only with these server-minted ids.
use rusqlite::{Connection, OptionalExtension as _, params};
use serde::Serialize;
use shelfy_core::repo::Result;
use utoipa::ToSchema;

/// A live export and its worker, polled through the jobs API.
#[derive(Debug, Clone, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Export {
    pub id: String,
    pub job_id: i64,
    pub created_at: i64,
    pub expires_at: i64,
    /// Absent while the worker builds the bundle.
    pub bytes: Option<u64>,
}
fn row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Export> {
    Ok(Export {
        id: r.get(0)?,
        job_id: r.get(1)?,
        created_at: r.get(2)?,
        expires_at: r.get(3)?,
        bytes: r.get::<_, Option<i64>>(4)?.map(|n| n.max(0) as u64),
    })
}
/// Live exports belonging to this user only.
pub fn list(c: &Connection, user: &str, now: i64) -> Result<Vec<Export>> {
    Ok(c.prepare("SELECT id, job_id, created_at, expires_at, bytes FROM exports WHERE user_id=?1 AND deleted_at IS NULL AND expires_at>?2 ORDER BY created_at DESC")?
        .query_map(params![user, now], row)?.collect::<rusqlite::Result<_>>()?)
}
/// Missing, deleted and expired exports have the same public result.
pub fn get(c: &Connection, user: &str, id: &str, now: i64) -> Result<Option<Export>> {
    Ok(c.query_row("SELECT id, job_id, created_at, expires_at, bytes FROM exports WHERE user_id=?1 AND id=?2 AND deleted_at IS NULL AND expires_at>?3", params![user,id,now], row).optional()?)
}
/// Tombstone before cancelling: a stopping worker cannot publish afterwards.
pub fn mark_deleted(c: &Connection, user: &str, id: &str, now: i64) -> Result<bool> {
    Ok(c.execute(
        "UPDATE exports SET deleted_at=?3 WHERE user_id=?1 AND id=?2 AND deleted_at IS NULL",
        params![user, id, now],
    )? == 1)
}
