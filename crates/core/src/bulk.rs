//! Actions on many posts at once (plan §2.9 "Bulk selector"; P1-11): move
//! them to the trash and back, add them to collections or take them out of
//! one, clear their AI description or AI tags (desktop `db:deletePosts`,
//! `analyze:clearDescriptions`, `analyze:clearTags`). The posts are a
//! [`Selector`]: keys, a filter minus exceptions, or the posts one delete
//! moved to the trash.
//!
//! **Two ways to run.** [`apply`] runs an action over a whole selection in
//! the caller's transaction, in a few statements built on the selector's
//! condition ([`Selector::sql`]). The API does that for selections of up to
//! [`MAX_INLINE`] posts, inside the request. A larger selection becomes a
//! job, which cuts it into [`next_chunk`]s of at most [`CHUNK`] posts, in id
//! order, and applies the action to each chunk in a transaction of its own,
//! so that no transaction holds the library's writer for long. A chunk is
//! the [`Selector::Keys`] of its posts, so a job runs exactly the inline
//! path, one chunk at a time.
//!
//! **A job sees the library as it goes.** Each chunk evaluates the selector
//! anew: a post that starts or stops matching a filter while the job runs is
//! included or not by the chunk that reaches its id, and the id order means
//! no post is visited twice.
//!
//! **What each action changes**, among the selected posts:
//!
//! | [`Action`] | Changes |
//! |---|---|
//! | `Delete` | the posts outside the trash go to it, all stamped with the action's time ([`crate::trash::put`]) |
//! | `Restore` | the posts in the trash come back ([`crate::trash::restore`]) |
//! | `AddToCollections` | the posts outside the trash join each collection; members stay as they are |
//! | `RemoveFromCollection` | the members leave the collection, in the trash or not |
//! | `ClearAiDescription` | `ai_description` and `ai_status` become `NULL`: the post counts as not analyzed again (desktop `clearAiDescriptions`) |
//! | `ClearAiTags` | `ai_tags_json`, the AI tag rows and `ai_status` are cleared; manual tags stay (desktop `clearAiTags`) |
//!
//! A [`Selector::Keys`] selection reaches the trash, so the AI actions apply
//! to trashed posts given by key, as `PATCH /posts/{key}` does; a filter
//! reaches the trash only with `trash` set. Each action is idempotent on
//! each post: applying it again changes nothing, so a job that stopped
//! halfway can start over from the first chunk.

use rusqlite::types::Value;
use rusqlite::{Connection, params_from_iter};

use crate::repo::posts::{self, AiPatch};
use crate::repo::{RepoError, Result, collections};
use crate::selector::{self, MAX_KEYS, Selector, SelectorSql};
use crate::trash;

/// The largest selection the API runs inline, in the request (plan §2.9: up
/// to 500 posts run inline). It is also the most keys a selector takes.
pub const MAX_INLINE: u64 = MAX_KEYS as u64;
/// Posts per chunk of a job: the inline limit, so a chunk is one inline run.
pub const CHUNK: usize = MAX_KEYS;
/// Most collections one `AddToCollections` adds to.
pub const MAX_COLLECTIONS: usize = 50;

/// What a bulk action does to each selected post (module docs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Action {
    /// Move the posts to the trash.
    Delete,
    /// Bring the posts back from the trash.
    Restore,
    /// Add the posts to every one of these collections, by id.
    AddToCollections(Vec<i64>),
    /// Take the posts out of this collection, by id.
    RemoveFromCollection(i64),
    /// Clear the AI description (and the AI status).
    ClearAiDescription,
    /// Clear the AI tags (and the AI status).
    ClearAiTags,
}

impl Action {
    /// Refuses an action whose arguments cannot work: no collection, too
    /// many, or one twice.
    ///
    /// # Errors
    ///
    /// [`RepoError::Invalid`] naming `collectionIds`.
    pub fn validate(&self) -> Result<()> {
        if let Self::AddToCollections(ids) = self {
            let invalid = |reason| {
                Err(RepoError::Invalid {
                    field: "collectionIds",
                    reason,
                })
            };
            if ids.is_empty() {
                return invalid("needs at least one collection");
            }
            if ids.len() > MAX_COLLECTIONS {
                return invalid("has more than 50 collections");
            }
            if ids.iter().enumerate().any(|(i, id)| ids[..i].contains(id)) {
                return invalid("names a collection twice");
            }
        }
        Ok(())
    }

    /// Checks that the collections the action names exist, so it fails
    /// before it changes anything.
    ///
    /// # Errors
    ///
    /// [`RepoError::NotFound`] for a collection that does not exist;
    /// [`RepoError::Invalid`] from [`Action::validate`].
    pub fn check(&self, conn: &Connection) -> Result<()> {
        self.validate()?;
        let ids: &[i64] = match self {
            Self::AddToCollections(ids) => ids,
            Self::RemoveFromCollection(id) => std::slice::from_ref(id),
            _ => &[],
        };
        for &id in ids {
            if collections::get(conn, id)?.is_none() {
                return Err(RepoError::NotFound);
            }
        }
        Ok(())
    }
}

/// What [`apply`] did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Applied {
    /// Posts the selector selected (unknown keys are not counted).
    pub selected: u64,
    /// Internal ids of the posts the action changed, ascending.
    pub changed: Vec<i64>,
}

/// How many posts `selector` selects.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a selector over its caps; database errors.
pub fn count(conn: &Connection, selector: &Selector) -> Result<u64> {
    selector::count(conn, selector)
}

/// Runs `action` on every post `selector` selects, at time `now` (the
/// trash stamp of a delete, and every post's new `updated_at`), in the
/// caller's transaction. Checks the action first ([`Action::check`]).
///
/// # Errors
///
/// [`RepoError::NotFound`] for a collection that does not exist;
/// [`RepoError::Invalid`] for a selector over its caps or a bad action;
/// database errors.
pub fn apply(conn: &Connection, selector: &Selector, action: &Action, now: i64) -> Result<Applied> {
    action.check(conn)?;
    let which = selector.sql()?;
    let selected = count_of(conn, &which)?;
    let mut changed = match action {
        Action::Delete => trash::put(conn, &which, now)?,
        Action::Restore => trash::restore(conn, &which, now)?,
        Action::AddToCollections(ids) => {
            let mut added = Vec::new();
            for &id in ids {
                added.extend(add_to_collection(conn, &which, id, now)?);
            }
            added
        }
        Action::RemoveFromCollection(id) => remove_from_collection(conn, &which, *id)?,
        Action::ClearAiDescription => clear_ai(conn, &which, clear_description(), now)?,
        Action::ClearAiTags => clear_ai(conn, &which, clear_tags(), now)?,
    };
    changed.sort_unstable();
    changed.dedup();
    Ok(Applied { selected, changed })
}

/// The next part of a job's selection: at most `limit` posts.
#[derive(Clone, Debug, PartialEq)]
pub struct Chunk {
    /// Its posts, by key, for [`apply`].
    pub selector: Selector,
    /// How many posts it holds.
    pub len: usize,
    /// The largest internal id in it: the `after` of the next chunk.
    pub last: i64,
}

/// The next `limit` posts `selector` selects whose internal id is above
/// `after` (0 for the first chunk), in id order; `None` once there are no
/// more.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a selector over its caps; database errors.
pub fn next_chunk(
    conn: &Connection,
    selector: &Selector,
    after: i64,
    limit: usize,
) -> Result<Option<Chunk>> {
    let which = selector.sql()?;
    let sql = format!(
        "SELECT p.id, p.key FROM posts p WHERE ({}) AND p.id > ? ORDER BY p.id LIMIT ?",
        which.condition
    );
    let tail = [
        Value::Integer(after),
        Value::Integer(i64::try_from(limit.min(CHUNK)).unwrap_or(0)),
    ];
    let rows: Vec<(i64, String)> = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(which.params.iter().chain(&tail)), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let Some(&(last, _)) = rows.last() else {
        return Ok(None);
    };
    let len = rows.len();
    Ok(Some(Chunk {
        selector: Selector::Keys(rows.into_iter().map(|(_, key)| key).collect()),
        len,
        last,
    }))
}

fn count_of(conn: &Connection, which: &SelectorSql) -> Result<u64> {
    let n: i64 = conn
        .prepare_cached(&format!(
            "SELECT count(*) FROM posts p WHERE {}",
            which.condition
        ))?
        .query_row(params_from_iter(which.params.iter()), |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Adds the selected posts outside the trash to collection `id`, in one
/// statement; returns the posts added (members are skipped).
fn add_to_collection(
    conn: &Connection,
    which: &SelectorSql,
    id: i64,
    now: i64,
) -> Result<Vec<i64>> {
    let sql = format!(
        "INSERT OR IGNORE INTO post_collections (post_id, collection_id, added_at)
         SELECT p.id, ?, ? FROM posts p WHERE p.deleted_at IS NULL AND ({})
         RETURNING post_id",
        which.condition
    );
    let head = [Value::Integer(id), Value::Integer(now)];
    let added = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(head.iter().chain(&which.params)), |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(added)
}

/// Takes the selected posts out of collection `id`, in one statement;
/// returns the posts that were members.
fn remove_from_collection(conn: &Connection, which: &SelectorSql, id: i64) -> Result<Vec<i64>> {
    let sql = format!(
        "DELETE FROM post_collections
         WHERE collection_id = ? AND post_id IN (SELECT p.id FROM posts p WHERE {})
         RETURNING post_id",
        which.condition
    );
    let removed = conn
        .prepare_cached(&sql)?
        .query_map(
            params_from_iter(std::iter::once(&Value::Integer(id)).chain(&which.params)),
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    Ok(removed)
}

/// The desktop's `clearAiDescriptions`: no description, and not analyzed.
fn clear_description() -> AiPatch {
    AiPatch {
        description: Some(None),
        status: Some(None),
        ..AiPatch::default()
    }
}

/// The desktop's `clearAiTags`: no AI tags, and not analyzed.
fn clear_tags() -> AiPatch {
    AiPatch {
        tags: Some(None),
        status: Some(None),
        ..AiPatch::default()
    }
}

/// Applies `patch` to each selected post ([`posts::update_ai`]: the tag
/// rows and the search index follow); returns the posts it changed.
fn clear_ai(conn: &Connection, which: &SelectorSql, patch: AiPatch, now: i64) -> Result<Vec<i64>> {
    let ids: Vec<i64> = conn
        .prepare_cached(&format!(
            "SELECT p.id FROM posts p WHERE {} ORDER BY p.id",
            which.condition
        ))?
        .query_map(params_from_iter(which.params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    let mut changed = Vec::new();
    for id in ids {
        if posts::update_ai(conn, id, &patch, now)? {
            changed.push(id);
        }
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collection_lists_are_validated() {
        assert!(Action::AddToCollections(vec![1, 2]).validate().is_ok());
        for bad in [
            Vec::new(),
            vec![3, 3],
            (0..=MAX_COLLECTIONS as i64).collect(),
        ] {
            assert!(
                matches!(
                    Action::AddToCollections(bad.clone()).validate(),
                    Err(RepoError::Invalid {
                        field: "collectionIds",
                        ..
                    })
                ),
                "{bad:?}"
            );
        }
        assert!(Action::RemoveFromCollection(-1).validate().is_ok());
        assert!(Action::Delete.validate().is_ok());
    }
}
