//! Metadata-only imports, report/checkpoint storage and their API model.
pub mod v1;
use crate::routes::jobs::Job;
use serde::{Deserialize, Serialize};
use shelfy_core::import::Report;
use utoipa::ToSchema;

/// Job and its durable partial or completed report.
#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct Import {
    /// Import job, cancellable through the usual jobs route.
    pub job: Job,
    /// Present as soon as the first batch commits, also after failure/cancel.
    #[schema(required=true, value_type=Option<ImportReport>)]
    pub report: Option<Report>,
}
/// Schema of the core report.
#[derive(ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ImportReport {
    /// New posts.
    pub imported: u64,
    /// Changed known posts.
    pub updated: u64,
    /// New collections.
    pub collections: u64,
    /// Added memberships.
    pub links: u64,
    /// Unchanged/folded copies.
    pub skipped: u64,
    /// First 1000 failures in input order.
    pub rejected: Vec<ImportRejected>,
    /// Total failures, including omitted details.
    pub rejected_count: u64,
}
/// A rejected source record.
#[derive(ToSchema)]
pub struct ImportRejected {
    /// Zero-based position.
    pub index: u64,
    /// Stable code.
    pub code: String,
}
/// The report and cursor share the same library transaction as their posts.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Checkpoint {
    /// The durable control job lifetime and its uniquely claimed source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upload_id: Option<String>,
    /// Next source index.
    pub next: u64,
    /// Collection-only prepass was committed.
    pub definitions_done: bool,
    /// Done notification recorded.
    pub complete: bool,
    /// Incremental results.
    pub report: Report,
}
/// Stable storage key (no control migrations).
pub fn report_key(id: i64) -> String {
    format!("import.v1.{id}")
}
/// Reads the committed checkpoint, or the empty initial one.
pub fn checkpoint(
    conn: &rusqlite::Connection,
    id: i64,
) -> Result<Checkpoint, shelfy_core::repo::RepoError> {
    use rusqlite::OptionalExtension as _;
    let text: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key=?1",
            [report_key(id)],
            |r| r.get(0),
        )
        .optional()?;
    match text {
        None => Ok(Checkpoint::default()),
        Some(s) => serde_json::from_str(&s).map_err(|_| shelfy_core::repo::RepoError::Invalid {
            field: "report",
            reason: "invalid stored import checkpoint",
        }),
    }
}
/// Reads only the cursor belonging to this job lifetime and claimed upload.
/// Newly admitted jobs discard old markers; migrated jobs with unbound cursors
/// fail before any write, so their partially imported input is not replayed.
pub fn checkpoint_for(
    conn: &rusqlite::Connection,
    id: i64,
    incarnation: &str,
    upload_id: &str,
) -> Result<Checkpoint, crate::error::ApiError> {
    let exists: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM meta WHERE key=?1)",
            [report_key(id)],
            |r| r.get(0),
        )
        .map_err(crate::error::ApiError::internal)?;
    if exists {
        let checkpoint = checkpoint(conn, id)?;
        if checkpoint.incarnation.as_deref() == Some(incarnation)
            && checkpoint.upload_id.as_deref() == Some(upload_id)
        {
            return Ok(checkpoint);
        }
        if !incarnation.starts_with("job:") {
            return Err(crate::error::ApiError::new(crate::error::ErrorCode::ImportCheckpointUnbound)
                .with_detail("Start a new import and upload the file again; the legacy partial checkpoint was left unchanged."));
        }
    }
    Ok(Checkpoint {
        incarnation: Some(incarnation.to_owned()),
        upload_id: Some(upload_id.to_owned()),
        ..Checkpoint::default()
    })
}

/// Stores a cursor after its batch in the same transaction.
pub fn save(
    conn: &rusqlite::Connection,
    id: i64,
    c: &Checkpoint,
) -> Result<(), shelfy_core::repo::RepoError> {
    let value = serde_json::to_string(c).map_err(|_| shelfy_core::repo::RepoError::Invalid {
        field: "report",
        reason: "invalid import report",
    })?;
    conn.execute(
        "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
        rusqlite::params![report_key(id), value],
    )?;
    Ok(())
}
