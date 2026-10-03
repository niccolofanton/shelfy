//! The trash (plan §2.7 `deleted_at`, §2.9 Library, §2.13; P1-11): posts
//! taken out of the library for [`RETENTION_DAYS`] days, then purged.
//!
//! **Moving posts to the trash** ([`put`]) stamps `deleted_at` and drops their
//! rows from both search indexes, so lists, counts, search and its statistics
//! ignore them. Everything else stays: slides, tags, notes, the AI layer,
//! collection memberships, media references. [`restore`] clears the stamp and
//! indexes the posts again: they come back with their folders and their
//! search text exactly as before. A trashed post is still found by key
//! (`GET /posts/{key}`, `{keys}` selections) and is listed by [`page`], most
//! recently trashed first.
//!
//! **One stamp per operation, never reused.** Every post that one delete
//! moves gets the same `deleted_at`, the stamp of the delete, so the stamp
//! names the operation: [`Selector::TrashedAt`] selects its posts again,
//! which is the undo of that delete. Posts already in the trash keep their
//! stamp. A stamp is the delete's time in unix ms, made unique in the
//! library by [`new_stamp`]: later than every stamp a delete used or
//! reserved ([`reserve_stamp`], for a delete that a job runs later). So two
//! deletes in one millisecond, or a delete right after the undo of another,
//! never share one, and an undo never takes the posts of another delete
//! (P1-11 review L1).
//!
//! **When a post entered the trash.** The stamp is the undo key only: a job
//! may move a post long after the request that stamped it. A trashed post's
//! `updated_at` says when it really entered the trash, or was changed there
//! since: [`put`] sets it to the time of the move, and always past the cut of
//! every emptying requested before ([`emptying`]). Purges go by both (P1-11
//! review H1, M4):
//!
//! - **emptying the trash** deletes the posts that were in the trash when it
//!   was asked, and no other: [`emptying`] runs in the request's write
//!   transaction, so no move is halfway, and picks a cut `through` at or
//!   after the stamp and the `updated_at` of every post then in the trash.
//!   It records the cut, so a post that enters the trash later, such as one a
//!   running bulk delete moves after the request, gets an `updated_at` past
//!   it and stays;
//! - **the nightly retention** deletes the posts whose stamp and `updated_at`
//!   are both [`RETENTION_DAYS`] old ([`retention_cutoff`]): a post that a late
//!   job moved stays its full 30 days from the move.
//!
//! **Purging** ([`purge`]) deletes posts for good: their index rows, their
//! row and everything that cascades from it (slides, memberships, tags,
//! entities, captures). Media objects that lose their last reference get
//! `unreferenced_since`; deleting their files is the GC's (P4). Purging is
//! idempotent: a purged post is gone, so purging it again changes nothing.
//! A purge job works through [`purgeable`] chunks, oldest trash first.
//!
//! The library's `meta` table keeps the two values that make this exact:
//! the newest stamp used or reserved ([`LAST_STAMP_KEY`]) and the cut of the
//! last emptying ([`EMPTIED_THROUGH_KEY`]).
//!
//! [`Selector::TrashedAt`]: crate::selector::Selector::TrashedAt

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};

use crate::repo::posts::{self, PostSummary};
use crate::repo::{Result, id_list, media};
use crate::search::index;
use crate::selector::SelectorSql;

/// Days a post stays in the trash before the nightly purge deletes it (plan
/// §7.2).
pub const RETENTION_DAYS: u32 = 30;
/// [`RETENTION_DAYS`] in milliseconds.
pub const RETENTION_MS: i64 = RETENTION_DAYS as i64 * 86_400_000;
/// `meta` key of the cut of the last emptying of the trash ([`emptying`]).
pub const EMPTIED_THROUGH_KEY: &str = "trash.emptiedThrough";
/// `meta` key of the newest stamp a delete used or reserved ([`new_stamp`]).
pub const LAST_STAMP_KEY: &str = "trash.lastStamp";

/// The posts with these internal ids, as a condition for [`put`] and
/// [`restore`].
#[must_use]
pub fn by_ids(post_ids: &[i64]) -> SelectorSql {
    SelectorSql {
        condition: "p.id IN (SELECT value FROM json_each(?))".to_owned(),
        params: vec![Value::Text(id_list(post_ids))],
    }
}

/// An integer of the library's `meta` table.
fn meta_i64(conn: &Connection, key: &str) -> Result<Option<i64>> {
    let value: Option<String> = conn
        .prepare_cached("SELECT value FROM meta WHERE key = ?1")?
        .query_row([key], |r| r.get(0))
        .optional()?;
    Ok(value.and_then(|v| v.parse().ok()))
}

/// Raises the integer `key` of `meta` to `value`; it never goes down.
fn raise_meta(conn: &Connection, key: &str, value: i64) -> Result<()> {
    if meta_i64(conn, key)?.is_some_and(|current| current >= value) {
        return Ok(());
    }
    conn.prepare_cached(
        "INSERT INTO meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
    )?
    .execute(params![key, value.to_string()])?;
    Ok(())
}

/// The stamp of a delete at `now`: `now`, or later when a delete already
/// used or reserved that time or a later one (module docs). Reads only: the
/// stamp is recorded once [`put`] moves posts with it, or by
/// [`reserve_stamp`]. Call it in the delete's write transaction.
///
/// # Errors
///
/// Database errors.
pub fn new_stamp(conn: &Connection, now: i64) -> Result<i64> {
    let last = meta_i64(conn, LAST_STAMP_KEY)?;
    let newest: Option<i64> = conn
        .prepare_cached("SELECT max(deleted_at) FROM posts WHERE deleted_at IS NOT NULL")?
        .query_row([], |r| r.get(0))?;
    Ok([last, newest]
        .into_iter()
        .flatten()
        .fold(now, |stamp, used| stamp.max(used.saturating_add(1))))
}

/// The stamp of a delete that a job runs later ([`new_stamp`]), recorded now
/// so that no other delete takes it meanwhile. Call it in a write
/// transaction.
///
/// # Errors
///
/// Database errors.
pub fn reserve_stamp(conn: &Connection, now: i64) -> Result<i64> {
    let stamp = new_stamp(conn, now)?;
    raise_meta(conn, LAST_STAMP_KEY, stamp)?;
    Ok(stamp)
}

/// Moves the posts `which` selects to the trash, stamped `at`, and drops
/// their index rows. `at` is the delete's stamp ([`new_stamp`], or the one
/// its job reserved), recorded as used once posts move with it. `now` is the
/// time of the move: the posts' `updated_at`, past the cut of every emptying
/// requested so far (module docs). Posts already in the trash keep their
/// stamp and their `updated_at`. Returns the ids of the posts moved,
/// ascending.
///
/// # Errors
///
/// Database errors.
pub fn put(conn: &Connection, which: &SelectorSql, at: i64, now: i64) -> Result<Vec<i64>> {
    let entered = meta_i64(conn, EMPTIED_THROUGH_KEY)?
        .map_or(now, |through| now.max(through.saturating_add(1)));
    let sql = format!(
        "UPDATE posts SET deleted_at = ?, updated_at = ?
         WHERE deleted_at IS NULL AND id IN (SELECT p.id FROM posts p WHERE {})
         RETURNING id",
        which.condition
    );
    let times = [Value::Integer(at), Value::Integer(entered)];
    let mut moved: Vec<i64> = conn
        .prepare_cached(&sql)?
        .query_map(params_from_iter(times.iter().chain(&which.params)), |r| {
            r.get(0)
        })?
        .collect::<rusqlite::Result<_>>()?;
    if moved.is_empty() {
        return Ok(moved);
    }
    moved.sort_unstable();
    for &id in &moved {
        index::remove_post(conn, id)?;
    }
    raise_meta(conn, LAST_STAMP_KEY, at)?;
    Ok(moved)
}

/// Takes the posts `which` selects out of the trash and indexes them again.
/// Posts outside the trash are left alone. Returns the ids of the posts
/// restored, ascending.
///
/// # Errors
///
/// Database errors.
pub fn restore(conn: &Connection, which: &SelectorSql, now: i64) -> Result<Vec<i64>> {
    let sql = format!(
        "UPDATE posts SET deleted_at = NULL, updated_at = ?
         WHERE deleted_at IS NOT NULL AND id IN (SELECT p.id FROM posts p WHERE {})
         RETURNING id",
        which.condition
    );
    let mut restored: Vec<i64> = conn
        .prepare_cached(&sql)?
        .query_map(
            params_from_iter(std::iter::once(&Value::Integer(now)).chain(&which.params)),
            |r| r.get(0),
        )?
        .collect::<rusqlite::Result<_>>()?;
    restored.sort_unstable();
    for &id in &restored {
        index::reindex_post(conn, id)?;
    }
    Ok(restored)
}

/// Deletes posts for good, in or out of the trash: their index rows, the
/// post rows and everything that cascades from them (slides, memberships,
/// tags, entities, captures). Objects left without references are stamped
/// for the GC. Returns the keys of the posts deleted; unknown ids are
/// skipped.
///
/// # Errors
///
/// Database errors.
pub fn purge(conn: &Connection, post_ids: &[i64], now: i64) -> Result<Vec<String>> {
    if post_ids.is_empty() {
        return Ok(Vec::new());
    }
    let ids = id_list(post_ids);
    let objects: Vec<i64> = conn
        .prepare_cached(
            "WITH doomed(id) AS (SELECT value FROM json_each(?1))
             SELECT cover_object FROM posts WHERE id IN doomed AND cover_object IS NOT NULL
             UNION SELECT object_id FROM post_media
                   WHERE post_id IN doomed AND object_id IS NOT NULL
             UNION SELECT video_object_id FROM post_media
                   WHERE post_id IN doomed AND video_object_id IS NOT NULL
             UNION SELECT hero_object FROM web_captures
                   WHERE post_id IN doomed AND hero_object IS NOT NULL
             UNION SELECT favicon_object FROM web_captures
                   WHERE post_id IN doomed AND favicon_object IS NOT NULL
             UNION SELECT a.object_id FROM web_capture_assets a
                   JOIN web_captures c ON c.id = a.capture_id WHERE c.post_id IN doomed",
        )?
        .query_map([&ids], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    // Index rows first: `posts.id` is a plain rowid that a later insert may
    // reuse.
    for &id in post_ids {
        index::remove_post(conn, id)?;
    }
    let mut keys: Vec<String> = conn
        .prepare_cached(
            "DELETE FROM posts WHERE id IN (SELECT value FROM json_each(?1)) RETURNING key",
        )?
        .query_map([&ids], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    keys.sort_unstable();
    media::mark_unreferenced(conn, &objects, now)?;
    Ok(keys)
}

/// Posts in the trash.
///
/// # Errors
///
/// Database errors.
pub fn count(conn: &Connection) -> Result<u64> {
    let n: i64 = conn
        .prepare_cached("SELECT count(*) FROM posts WHERE deleted_at IS NOT NULL")?
        .query_row([], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// A place in the trash list: the last post of the previous page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Position {
    /// Its `deleted_at`.
    pub deleted_at: i64,
    /// Its internal id.
    pub id: i64,
}

/// One page of the trash.
#[derive(Clone, Debug, PartialEq)]
pub struct TrashPage {
    /// The posts, most recently trashed first (then by id, descending).
    pub items: Vec<PostSummary>,
    /// Where the next page starts; `None` on the last page.
    pub next: Option<Position>,
}

/// One page of the trash, most recently trashed first, after `after` (keyset
/// paging: a post trashed or restored between two pages never shifts the
/// others). `limit` is clamped to `1..=MAX_PAGE_SIZE`.
///
/// # Errors
///
/// Database errors.
pub fn page(conn: &Connection, limit: u32, after: Option<Position>) -> Result<TrashPage> {
    let limit = limit.clamp(1, posts::MAX_PAGE_SIZE);
    let row = |r: &rusqlite::Row<'_>| -> rusqlite::Result<(String, i64, i64)> {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    };
    let take = i64::from(limit) + 1;
    let mut rows: Vec<(String, i64, i64)> = match after {
        None => conn
            .prepare_cached(
                "SELECT p.key, p.deleted_at, p.id FROM posts p WHERE p.deleted_at IS NOT NULL
                 ORDER BY p.deleted_at DESC, p.id DESC LIMIT ?1",
            )?
            .query_map([take], row)?
            .collect::<rusqlite::Result<_>>()?,
        // `deleted_at <= ?1` is the range of `posts_trash`, which holds
        // `(deleted_at, id)`; the rest of the keyset condition filters it.
        Some(p) => conn
            .prepare_cached(
                "SELECT p.key, p.deleted_at, p.id FROM posts p
                 WHERE p.deleted_at IS NOT NULL AND p.deleted_at <= ?1
                   AND (p.deleted_at < ?1 OR p.id < ?2)
                 ORDER BY p.deleted_at DESC, p.id DESC LIMIT ?3",
            )?
            .query_map(params![p.deleted_at, p.id, take], row)?
            .collect::<rusqlite::Result<_>>()?,
    };
    let more = rows.len() > limit as usize;
    rows.truncate(limit as usize);
    let next = rows
        .last()
        .filter(|_| more)
        .map(|&(_, deleted_at, id)| Position { deleted_at, id });
    let keys: Vec<String> = rows.into_iter().map(|(key, _, _)| key).collect();
    let items = posts::get_many(conn, &keys)?;
    Ok(TrashPage { items, next })
}

/// Ids of the trashed posts a purge through `through` deletes, oldest trash
/// first, at most `limit`: the next chunk of a purge. A post qualifies when
/// both its stamp and its `updated_at` (when it entered the trash, or was
/// changed there) are at or before `through` (module docs).
///
/// # Errors
///
/// Database errors.
pub fn purgeable(conn: &Connection, through: i64, limit: usize) -> Result<Vec<i64>> {
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let ids = conn
        .prepare_cached(
            "SELECT id FROM posts
             WHERE deleted_at IS NOT NULL AND deleted_at <= ?1 AND updated_at <= ?1
             ORDER BY deleted_at, id LIMIT ?2",
        )?
        .query_map(params![through, limit], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// How many posts a purge through `through` deletes ([`purgeable`]).
///
/// # Errors
///
/// Database errors.
pub fn count_purgeable(conn: &Connection, through: i64) -> Result<u64> {
    let n: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM posts
             WHERE deleted_at IS NOT NULL AND deleted_at <= ?1 AND updated_at <= ?1",
        )?
        .query_row([through], |r| r.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// What emptying the trash at `now` deletes ([`emptying`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Emptying {
    /// The cut: a purge through it deletes every post in the trash now.
    pub through: i64,
    /// Posts in the trash now.
    pub posts: u64,
}

/// Starts emptying the trash at `now`: the cut `through` of the purge, at or
/// after `now` and the stamp and `updated_at` of every post in the trash, so
/// that the purge deletes exactly the posts in the trash now. The cut is
/// recorded, and every post moved to the trash afterwards gets an
/// `updated_at` past it ([`put`]), so the purge leaves it alone. Call it in
/// the write transaction of the request: no move is then halfway.
///
/// # Errors
///
/// Database errors.
pub fn emptying(conn: &Connection, now: i64) -> Result<Emptying> {
    let (posts, newest): (i64, Option<i64>) = conn
        .prepare_cached(
            "SELECT count(*), max(max(deleted_at, updated_at)) FROM posts
             WHERE deleted_at IS NOT NULL",
        )?
        .query_row([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    let through = newest.map_or(now, |newest| now.max(newest));
    raise_meta(conn, EMPTIED_THROUGH_KEY, through)?;
    Ok(Emptying {
        through,
        posts: u64::try_from(posts).unwrap_or(0),
    })
}

/// The `through` of the nightly purge at `now`: the posts whose stamp and
/// `updated_at` are at least [`RETENTION_MS`] old.
#[must_use]
pub const fn retention_cutoff(now: i64) -> i64 {
    now.saturating_sub(RETENTION_MS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retention_is_thirty_days() {
        assert_eq!(RETENTION_MS, 2_592_000_000);
        assert_eq!(retention_cutoff(RETENTION_MS + 5), 5);
        assert_eq!(retention_cutoff(i64::MIN), i64::MIN);
    }
}
