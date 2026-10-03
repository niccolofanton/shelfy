//! Per-library embedding cache. The model key includes the resolved provider,
//! so routing between providers with the same model name never reuses vectors.
//! The schema's blobs use little-endian float32, matching the desktop cache.
use std::collections::HashMap;

use rusqlite::{Connection, Transaction, params};

use super::{VocabTag, graph::Vectors};
use crate::repo::{RepoError, Result};

/// Bound corrupt cache entries and provider output before allocating vectors.
pub const MAX_DIM: usize = 4096;

/// Distinct tags on live posts, in the vocabulary's stable frequency order.
pub fn vocabulary(conn: &Connection) -> Result<Vec<VocabTag>> {
    super::vocabulary(conn)
}

/// Cached vectors for these tags and this resolved model; corrupt rows are
/// cache misses. Stale tags outside the requested vocabulary are not loaded.
pub fn cached(conn: &Connection, model: &str, tags: &[String]) -> Result<Vectors> {
    let mut statement = conn.prepare_cached(
        "SELECT tag_norm, dim, vec FROM tag_embeddings
        WHERE model=?1 AND tag_norm IN (SELECT value FROM json_each(?2))",
    )?;
    let rows = statement.query_map(
        params![
            model,
            serde_json::to_string(tags).expect("string list JSON")
        ],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Vec<u8>>(2)?,
            ))
        },
    )?;
    let mut vectors = HashMap::new();
    for row in rows {
        let (tag, dim, bytes) = row?;
        let Ok(dim) = usize::try_from(dim) else {
            continue;
        };
        if dim == 0 || dim > MAX_DIM || bytes.len() != dim * 4 {
            continue;
        }
        let vector: Vec<f32> = bytes
            .as_chunks::<4>()
            .0
            .iter()
            .map(|bytes| f32::from_le_bytes(*bytes))
            .collect();
        if let Some(vector) = normalize(&vector) {
            vectors.insert(tag, vector);
        }
    }
    Ok(vectors)
}

/// Validates a whole batch before writing any vector. Every embedding for a
/// model must have the same dimension, and vectors are normalized for cosine.
pub fn save(
    tx: &Transaction<'_>,
    model: &str,
    tags: &[String],
    vectors: &[Vec<f32>],
) -> Result<usize> {
    let invalid = || RepoError::Invalid {
        field: "embeddings",
        reason: "invalid vectors or dimensions",
    };
    if tags.len() != vectors.len() {
        return Err(invalid());
    }
    if tags.is_empty() {
        return Ok(0);
    }
    let dim = vectors[0].len();
    if dim == 0 || dim > MAX_DIM {
        return Err(invalid());
    }
    let existing: Option<i64> = tx
        .query_row(
            "SELECT dim FROM tag_embeddings WHERE model=?1 AND dim BETWEEN 1 AND ?2 AND length(vec)=dim*4 LIMIT 1",
            params![model,MAX_DIM as i64],
            |row| row.get(0),
        )
        .optional()?;
    if existing.is_some_and(|old| old != dim as i64) {
        return Err(invalid());
    }
    let prepared: Vec<Vec<f64>> = vectors
        .iter()
        .map(|vector| {
            if vector.len() != dim {
                return Err(invalid());
            }
            normalize(vector).ok_or_else(invalid)
        })
        .collect::<Result<_>>()?;
    let mut statement = tx.prepare_cached(
        "INSERT INTO tag_embeddings (tag_norm,model,dim,vec) VALUES (?1,?2,?3,?4)
        ON CONFLICT(tag_norm,model) DO UPDATE SET dim=excluded.dim,vec=excluded.vec",
    )?;
    for (tag, vector) in tags.iter().zip(prepared) {
        let bytes: Vec<u8> = vector
            .into_iter()
            .flat_map(|v| (v as f32).to_le_bytes())
            .collect();
        statement.execute(params![tag, model, dim as i64, bytes])?;
    }
    Ok(tags.len())
}

fn normalize(vector: &[f32]) -> Option<Vec<f64>> {
    if vector.iter().any(|v| !v.is_finite()) {
        return None;
    }
    let length = vector
        .iter()
        .map(|v| f64::from(*v).powi(2))
        .sum::<f64>()
        .sqrt();
    if length == 0.0 || !length.is_finite() {
        return None;
    }
    Some(vector.iter().map(|v| f64::from(*v) / length).collect())
}

use rusqlite::OptionalExtension as _;
