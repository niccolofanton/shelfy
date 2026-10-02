//! Collections ("sources" in the UI): CRUD and membership (DATA-21 to DATA-27).
//!
//! Semantics mirror the desktop, with these changes: a platform-linked
//! collection is unique per `(platform, external_id)`; counts and new
//! memberships ignore trashed posts; deleting a collection "with its posts"
//! moves the posts to the trash instead of deleting them; removing one post
//! from a collection is exposed (DATA-27); collections have a manual order
//! ([`move_to`]); posts are added by [`Selector`] in one statement
//! ([`add_selected`]).

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};
use serde::Serialize;

use super::{Platform, RepoError, Result, conflict_on_unique, id_list, posts};
use crate::search::terms::js_trim;
use crate::selector::Selector;

/// Color of a collection created without one (desktop default).
pub const DEFAULT_COLOR: &str = "#3d5afe";
/// Longest collection name, in characters.
pub const NAME_MAX_CHARS: usize = 200;

/// A collection with its live post count.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Collection {
    /// Id.
    pub id: i64,
    /// Name.
    pub name: String,
    /// Hex color (`#rrggbb` or `#rgb`).
    pub color: String,
    /// Platform of a linked saved folder or board; `None` for a manual collection.
    pub platform: Option<Platform>,
    /// Folder or board id on that platform.
    pub external_id: Option<String>,
    /// Name of the folder or board on the platform when it was linked.
    pub source_name: Option<String>,
    /// Manual order, when set.
    pub position: Option<i64>,
    /// Creation time.
    pub created_at: i64,
    /// Posts in the collection, trash excluded.
    pub count: u64,
}

/// A collection to create.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NewCollection {
    /// Name (trimmed; required).
    pub name: String,
    /// Color; defaults to [`DEFAULT_COLOR`].
    pub color: Option<String>,
    /// Platform of a linked folder or board.
    pub platform: Option<Platform>,
    /// Folder or board id (required with `platform`).
    pub external_id: Option<String>,
    /// Folder or board name on the platform.
    pub source_name: Option<String>,
}

/// Changes to a collection; `None`, or a blank value, keeps the old one
/// (desktop `COALESCE` semantics).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CollectionPatch {
    /// New name.
    pub name: Option<String>,
    /// New color.
    pub color: Option<String>,
}

/// What happens to the posts of a deleted collection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DeleteMode {
    /// Keep them; only the memberships go.
    #[default]
    KeepPosts,
    /// Move them to the trash as well.
    TrashPosts,
}

/// Every collection with its count, in manual order, then creation order.
///
/// # Errors
///
/// Database errors.
pub fn list(conn: &Connection) -> Result<Vec<Collection>> {
    let sql = format!("{SELECT} ORDER BY {ORDER}");
    let rows = conn
        .prepare_cached(&sql)?
        .query_map([], from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// One collection.
///
/// # Errors
///
/// Database errors.
pub fn get(conn: &Connection, id: i64) -> Result<Option<Collection>> {
    let sql = format!("{SELECT} WHERE c.id = ?1");
    Ok(conn
        .prepare_cached(&sql)?
        .query_row([id], from_row)
        .optional()?)
}

/// Creates a collection.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a blank or over-long name, a bad color, or a
/// platform without an external id; [`RepoError::Conflict`] when the platform
/// folder is already linked to another collection.
pub fn create(conn: &Connection, new: &NewCollection, now: i64) -> Result<Collection> {
    let name = valid_name(&new.name)?.ok_or(RepoError::Invalid {
        field: "name",
        reason: "is required",
    })?;
    let color = match new.color.as_deref().map(js_trim).filter(|c| !c.is_empty()) {
        Some(c) => valid_color(c)?,
        None => DEFAULT_COLOR.to_owned(),
    };
    let external_id = new
        .external_id
        .as_deref()
        .map(js_trim)
        .filter(|s| !s.is_empty());
    if new.platform.is_some() && external_id.is_none() {
        return Err(RepoError::Invalid {
            field: "externalId",
            reason: "is required for a linked collection",
        });
    }
    conn.prepare_cached(
        "INSERT INTO collections (name, color, platform, external_id, source_name, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    )?
    .execute(params![
        name,
        color,
        new.platform,
        external_id,
        new.source_name,
        now
    ])
    .map_err(|e| conflict_on_unique(e, "collection"))?;
    let id = conn.last_insert_rowid();
    get(conn, id)?.ok_or(RepoError::NotFound)
}

/// Renames and/or recolors a collection; its platform link is preserved.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown id; [`RepoError::Invalid`] for an
/// over-long name or a bad color.
pub fn update(conn: &Connection, id: i64, patch: &CollectionPatch) -> Result<Collection> {
    let name = match patch.name.as_deref() {
        Some(n) => valid_name(n)?,
        None => None,
    };
    let color = match patch
        .color
        .as_deref()
        .map(js_trim)
        .filter(|c| !c.is_empty())
    {
        Some(c) => Some(valid_color(c)?),
        None => None,
    };
    let changed = conn
        .prepare_cached(
            "UPDATE collections SET name = COALESCE(?2, name), color = COALESCE(?3, color)
             WHERE id = ?1",
        )?
        .execute(params![id, name, color])?;
    if changed == 0 {
        return Err(RepoError::NotFound);
    }
    get(conn, id)?.ok_or(RepoError::NotFound)
}

/// Moves a collection to `index` in the manual order (0-based; past the end
/// means last) and renumbers every collection from 0, so positions stay
/// dense: one call per drag. Collections already in place are not written,
/// so a move to where a collection already is changes nothing.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown id; database errors otherwise.
pub fn move_to(conn: &Connection, id: i64, index: usize) -> Result<Collection> {
    let mut order: Vec<i64> = conn
        .prepare_cached(&format!("SELECT c.id FROM collections c ORDER BY {ORDER}"))?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let from = order
        .iter()
        .position(|&c| c == id)
        .ok_or(RepoError::NotFound)?;
    order.remove(from);
    order.insert(index.min(order.len()), id);
    let mut place = conn.prepare_cached(
        "UPDATE collections SET position = ?2 WHERE id = ?1 AND position IS NOT ?2",
    )?;
    for (position, collection) in (0_i64..).zip(&order) {
        place.execute(params![collection, position])?;
    }
    get(conn, id)?.ok_or(RepoError::NotFound)
}

/// Deletes a collection. Returns how many posts went to the trash with it.
///
/// # Errors
///
/// [`RepoError::NotFound`] for an unknown id; database errors otherwise.
pub fn delete(conn: &Connection, id: i64, mode: DeleteMode, now: i64) -> Result<usize> {
    let trashed = if mode == DeleteMode::TrashPosts {
        let members: Vec<i64> = conn
            .prepare_cached("SELECT post_id FROM post_collections WHERE collection_id = ?1")?
            .query_map([id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        posts::trash(conn, &members, now)?
    } else {
        0
    };
    let deleted = conn
        .prepare_cached("DELETE FROM collections WHERE id = ?1")?
        .execute([id])?;
    if deleted == 0 {
        return Err(RepoError::NotFound);
    }
    Ok(trashed)
}

/// Adds every post to every collection (`INSERT OR IGNORE`, so repeats are
/// no-ops). Unknown and trashed posts are skipped. Returns the memberships
/// actually added.
///
/// # Errors
///
/// [`RepoError::NotFound`] when a collection does not exist.
pub fn add_posts(
    conn: &Connection,
    post_ids: &[i64],
    collection_ids: &[i64],
    now: i64,
) -> Result<usize> {
    let mut exists =
        conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM collections WHERE id = ?1)")?;
    for &cid in collection_ids {
        if !exists.query_row([cid], |r| r.get::<_, bool>(0))? {
            return Err(RepoError::NotFound);
        }
    }
    let mut insert = conn.prepare_cached(
        "INSERT OR IGNORE INTO post_collections (post_id, collection_id, added_at)
         SELECT p.id, ?2, ?3 FROM posts p
         WHERE p.id IN (SELECT value FROM json_each(?1)) AND p.deleted_at IS NULL",
    )?;
    let ids = id_list(post_ids);
    let mut added = 0;
    for &cid in collection_ids {
        added += insert.execute(params![ids, cid, now])?;
    }
    Ok(added)
}

/// Adds the posts `selector` selects to collection `id` in one statement
/// (`INSERT OR IGNORE … SELECT`), whatever the selection's size. Trashed
/// posts and current members are skipped. Returns the internal ids of the
/// posts added.
///
/// # Errors
///
/// [`RepoError::NotFound`] when the collection does not exist;
/// [`RepoError::Invalid`] for a selector over its caps; database errors.
pub fn add_selected(conn: &Connection, id: i64, selector: &Selector, now: i64) -> Result<Vec<i64>> {
    if !exists(conn, id)? {
        return Err(RepoError::NotFound);
    }
    let selection = selector.sql()?;
    let sql = format!(
        "INSERT OR IGNORE INTO post_collections (post_id, collection_id, added_at)
         SELECT p.id, ?, ? FROM posts p WHERE p.deleted_at IS NULL AND ({})
         RETURNING post_id",
        selection.condition
    );
    let params = [Value::Integer(id), Value::Integer(now)]
        .into_iter()
        .chain(selection.params);
    let added = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(params), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(added)
}

/// Keys of the posts in collection `id`, trash included, at most `limit`.
///
/// # Errors
///
/// Database errors.
pub fn member_keys(conn: &Connection, id: i64, limit: usize) -> Result<Vec<String>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let keys = conn
        .prepare_cached(
            "SELECT p.key FROM post_collections pc JOIN posts p ON p.id = pc.post_id
             WHERE pc.collection_id = ?1 ORDER BY pc.post_id LIMIT ?2",
        )?
        .query_map(params![id, limit], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(keys)
}

/// Removes one post from one collection (DATA-27). Returns whether it was a member.
///
/// # Errors
///
/// Database errors.
pub fn remove_post(conn: &Connection, post_id: i64, collection_id: i64) -> Result<bool> {
    let removed = conn
        .prepare_cached("DELETE FROM post_collections WHERE post_id = ?1 AND collection_id = ?2")?
        .execute([post_id, collection_id])?;
    Ok(removed > 0)
}

/// The manual order: positioned collections first, then by creation.
const ORDER: &str = "c.position IS NULL, c.position, c.created_at, c.id";

fn exists(conn: &Connection, id: i64) -> Result<bool> {
    Ok(conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM collections WHERE id = ?1)")?
        .query_row([id], |r| r.get(0))?)
}

const SELECT: &str = "SELECT c.id, c.name, c.color, c.platform, c.external_id, c.source_name,
        c.position, c.created_at,
        (SELECT count(*) FROM post_collections pc JOIN posts p ON p.id = pc.post_id
          WHERE pc.collection_id = c.id AND p.deleted_at IS NULL)
    FROM collections c";

fn from_row(r: &Row<'_>) -> rusqlite::Result<Collection> {
    Ok(Collection {
        id: r.get(0)?,
        name: r.get(1)?,
        color: r.get(2)?,
        platform: r.get(3)?,
        external_id: r.get(4)?,
        source_name: r.get(5)?,
        position: r.get(6)?,
        created_at: r.get(7)?,
        count: u64::try_from(r.get::<_, i64>(8)?).unwrap_or(0),
    })
}

/// The trimmed name, `None` when blank.
fn valid_name(name: &str) -> Result<Option<String>> {
    let name = js_trim(name);
    if name.is_empty() {
        return Ok(None);
    }
    if name.chars().count() > NAME_MAX_CHARS {
        return Err(RepoError::Invalid {
            field: "name",
            reason: "is longer than 200 characters",
        });
    }
    Ok(Some(name.to_owned()))
}

/// `#rgb` or `#rrggbb`, lowercased.
fn valid_color(color: &str) -> Result<String> {
    let hex = color.strip_prefix('#').unwrap_or("");
    if matches!(hex.len(), 3 | 6) && hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(format!("#{}", hex.to_ascii_lowercase()))
    } else {
        Err(RepoError::Invalid {
            field: "color",
            reason: "must be #rgb or #rrggbb",
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_are_validated_and_lowercased() {
        assert_eq!(valid_color("#3D5AFE").unwrap(), "#3d5afe");
        assert_eq!(valid_color("#FFF").unwrap(), "#fff");
        for bad in ["3d5afe", "#3d5af", "#ggg", "red", "#"] {
            assert!(valid_color(bad).is_err(), "{bad}");
        }
    }
}
