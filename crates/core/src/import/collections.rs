//! Desktop collection identity, preserving existing names and colors.
use crate::repo::Platform;
use crate::repo::{
    RepoError, Result,
    collections::{self, NewCollection},
};
use rusqlite::{Connection, OptionalExtension};
use serde_json::Value;
/// Accepted imported collection definition.
#[derive(Clone, Debug)]
pub struct Definition {
    /// Export key.
    pub key: String,
    /// Repository input.
    pub new: NewCollection,
}
/// Validates one bounded definition. External IDs with no platform are IG
/// folder IDs, as in the legacy extension exports.
pub fn definition(v: &Value) -> Result<Definition> {
    let ext = v
        .get("externalId")
        .or_else(|| v.get("external_id"))
        .and_then(|v| {
            v.as_str()
                .map(str::to_owned)
                .or_else(|| v.as_i64().map(|i| i.to_string()))
        })
        .filter(|s| !s.is_empty());
    let name = v.get("name").and_then(Value::as_str).unwrap_or("").trim();
    let name = if name.is_empty() {
        ext.as_deref().unwrap_or("")
    } else {
        name
    };
    if name.is_empty() || name.chars().count() > 200 || ext.as_ref().is_some_and(|s| s.len() > 256)
    {
        return Err(RepoError::Invalid {
            field: "collection",
            reason: "invalid name or external id",
        });
    }
    let platform = if ext.is_some() {
        Some(match v.get("platform").and_then(Value::as_str) {
            None | Some("instagram") => Platform::Instagram,
            Some("pinterest") => Platform::Pinterest,
            _ => {
                return Err(RepoError::Invalid {
                    field: "platform",
                    reason: "invalid linked collection platform",
                });
            }
        })
    } else {
        None
    };
    Ok(Definition {
        key: ext
            .as_ref()
            .map_or_else(|| format!("n:{name}"), |e| format!("x:{e}")),
        new: NewCollection {
            name: name.into(),
            color: v.get("color").and_then(Value::as_str).map(str::to_owned),
            platform,
            external_id: ext,
            source_name: v
                .get("sourceName")
                .and_then(Value::as_str)
                .map(str::to_owned),
        },
    })
}
/// Matches by external identity, then name among manual collections; creates
/// only when neither matches. No imported definition renames user data.
pub fn ensure(conn: &Connection, def: &Definition, now: i64) -> Result<(i64, bool)> {
    if let (Some(platform), Some(ext)) = (def.new.platform, def.new.external_id.as_deref())
        && let Some(c) = collections::find_linked(conn, platform, ext)?
    {
        return Ok((c.id, false));
    }
    let id: Option<i64> = conn
        .query_row(
            "SELECT id FROM collections WHERE name=?1 AND external_id IS NULL ORDER BY id LIMIT 1",
            [&def.new.name],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(id) = id {
        return Ok((id, false));
    }
    Ok((collections::create(conn, &def.new, now)?.id, true))
}
/// Resolves a membership that has no top-level definition.
pub fn from_key(key: &str) -> Option<Definition> {
    let (prefix, value) = key.split_once(':')?;
    if value.is_empty() {
        return None;
    }
    let v = match prefix {
        "x" => serde_json::json!({"name":value,"externalId":value}),
        "n" => serde_json::json!({"name":value}),
        _ => return None,
    };
    definition(&v).ok()
}
