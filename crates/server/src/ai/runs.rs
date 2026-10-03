//! Durable taxonomy plans stay in the user's library, never in bounded control
//! job payloads. Each cursor is committed with the proposals it describes.
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Deserialize, Serialize};
use shelfy_core::repo::{RepoError, Result};
use shelfy_core::tags::{VocabTag, aliases, embeddings, graph::CandidateGroup};

use super::{Caller, Task};
use crate::error::ApiError;
use crate::jobs::{Enqueued, NewJob, ai_drain};
use crate::state::AppState;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum RunKind {
    Clusters,
    Aliases,
}
impl RunKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Clusters => "clusters",
            Self::Aliases => "aliases",
        }
    }
    pub const fn task(self) -> Task {
        match self {
            Self::Clusters => Task::Cluster,
            Self::Aliases => Task::Alias,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Plan {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incarnation: Option<String>,
    pub kind: RunKind,
    pub tags: Vec<VocabTag>,
    pub vocabulary: Vec<VocabTag>,
    /// None until embedding and graph computation complete.
    pub groups: Option<Vec<CandidateGroup>>,
    pub next: usize,
    pub proposed: usize,
    pub initialized: bool,
    pub finished: bool,
}

pub async fn enqueue(state: &AppState, user: &str, kind: RunKind) -> Result<Enqueued, ApiError> {
    let owner = ai_drain::is_owner(state, user).await?;
    state
        .ai()
        .route(state, Caller::new(user, owner), kind.task())
        .await?;
    state
        .jobs()
        .enqueue(
            NewJob::new(user, crate::jobs::ai_run::KIND)
                .dedupe(kind.name())
                .payload(serde_json::json!({"runKind":kind})),
        )
        .await
}

pub fn snapshot(conn: &Connection, kind: RunKind) -> Result<Plan> {
    let (tags, vocabulary) = match kind {
        RunKind::Clusters => (embeddings::vocabulary(conn)?, vec![]),
        RunKind::Aliases => (
            aliases::unaliased_tags(conn, 400)?,
            aliases::canonical_vocab(conn, 300)?,
        ),
    };
    Ok(Plan {
        incarnation: None,
        kind,
        tags,
        vocabulary,
        groups: None,
        next: 0,
        proposed: 0,
        initialized: false,
        finished: false,
    })
}

pub fn load(conn: &Connection, id: i64) -> Result<Option<Plan>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value_json FROM ai_cache WHERE kind='taxonomy.run' AND key_hash=?1",
            [id.to_be_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?;
    value
        .map(|value| {
            serde_json::from_str(&value).map_err(|_| RepoError::Invalid {
                field: "taxonomy.run",
                reason: "invalid stored plan",
            })
        })
        .transpose()
}

/// Legacy or reincarnated IDs must never load a finished marker or old cursor.
pub fn load_for(conn: &Connection, id: i64, incarnation: &str) -> Result<Option<Plan>> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value_json FROM ai_cache WHERE kind='taxonomy.run' AND key_hash=?1",
            [id.to_be_bytes().as_slice()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(value) =
        value.and_then(|value| serde_json::from_str::<serde_json::Value>(&value).ok())
    else {
        return Ok(None);
    };
    if value.get("incarnation").and_then(serde_json::Value::as_str) != Some(incarnation) {
        return Ok(None);
    }
    serde_json::from_value(value)
        .map(Some)
        .map_err(|_| RepoError::Invalid {
            field: "taxonomy.run",
            reason: "invalid stored plan",
        })
}

pub fn save(tx: &Transaction<'_>, id: i64, plan: &Plan, now: i64) -> Result<()> {
    let value = serde_json::to_string(plan).expect("taxonomy plan JSON");
    tx.execute(
        "INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES('taxonomy.run',?1,?2,?3)
        ON CONFLICT(kind,key_hash) DO UPDATE SET value_json=excluded.value_json",
        params![id.to_be_bytes().as_slice(), value, now],
    )?;
    Ok(())
}
