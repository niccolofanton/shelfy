//! The item-work state machine of social cataloging (plan §2.12, §2.15,
//! G3-5, G3-6, G3-9; P3-13): the lifecycle of a post's AI analysis in the
//! `posts.ai_*` columns, which the per-user `ai.drain` (the server job) works
//! through.
//!
//! The states (`ai_status`):
//!
//! - `NULL` — never analyzed ("unanalyzed", the carry-over from P1-10);
//! - `pending` — queued, due at `ai_next_at`;
//! - `analyzing` — claimed by a drain try, `ai_attempts` counting the try;
//! - `done` — analyzed (stamped `ai_analyzed_at`), the layer written;
//! - `error` — gave up, `ai_error` the code.
//!
//! The write primitives here are synchronous and offline (the core never
//! calls a provider): [`mark_pending`] enqueues, [`claim_due`] claims the
//! soonest item, [`apply`]/[`backoff`]/[`fail`]/[`release`] end a try while
//! it is still the row's current one (the guard: same `analyzing` attempt,
//! not trashed), and [`recover_interrupted`] cleans a dead try at start. A
//! guarded write that finds the row changed under it (a manual edit, a clear
//! or a trash) does nothing and reports it, so an AI write yields to the user
//! (plan lane rule 13).
//!
//! Analyzability (G3-9): a post is enqueued only when it has the inputs its
//! type needs — a media post needs a stored frame (a cover or a slide
//! object), a text post needs a caption. A post that needs media and has
//! none is not enqueued; the estimate counts it as waiting for media.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension as _, params, params_from_iter};

use super::catalog::CatalogKind;
use crate::repo::Result;
use crate::repo::posts::{self, AiPatch};
use crate::selector::Selector;

/// The social platforms whose posts social cataloging covers: everything but
/// websites (`platform = 'web'` or `media_type = 'website'`), which get the
/// web catalog (plan §1.2 #7) through P3-27's path. Files need an image preview.
///
/// On the `posts` alias `p`.
const SOCIAL: &str = "p.platform <> 'web' AND p.media_type <> 'website'";

/// Media types that need a stored frame to catalog (image and video posts);
/// a `text` post is cataloged from its caption alone.
const NEEDS_MEDIA: &str = "p.media_type IN ('image','images','carousel','video','file')";

/// Whether a post has the inputs its type needs (G3-9): a stored cover or
/// slide object, or — for a type that needs no media — a non-blank caption.
/// On the alias `p`.
const HAS_INPUTS: &str = "(p.cover_object IS NOT NULL \
     OR EXISTS (SELECT 1 FROM post_media m WHERE m.post_id = p.id AND (m.object_id IS NOT NULL OR m.video_object_id IS NOT NULL)) \
     OR (p.media_type NOT IN ('image','images','carousel','video','file') \
         AND p.caption IS NOT NULL AND trim(p.caption) <> ''))";

/// How far an analyze request reaches (plan §2.9 `analyze`): the posts of the
/// selector, filtered by their current AI state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Only the posts not analyzed yet (`ai_status IS NULL`).
    Missing,
    /// The explicitly selected posts, re-analyzing finished ones.
    Selected,
    /// Every post of the selector, re-analyzing finished ones.
    All,
}

impl Mode {
    /// The wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Selected => "selected",
            Self::All => "all",
        }
    }

    /// The `ai_status` condition that a post must meet to be (re-)enqueued in
    /// this mode: `missing` takes only the unanalyzed; `all` and `selected`
    /// also take finished (`done`) and failed (`error`) posts. A post already
    /// `pending` or `analyzing` is never re-enqueued (it is already queued).
    const fn enqueue_condition(self) -> &'static str {
        match self {
            Self::Missing => "p.ai_status IS NULL",
            Self::All | Self::Selected => {
                "(p.ai_status IS NULL OR p.ai_status = 'done' OR p.ai_status = 'error')"
            }
        }
    }
}

/// What an analyze request would do, counted before it is confirmed (plan
/// §2.9 `analyze`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ScopeCounts {
    /// Posts that will be enqueued: in scope, in an enqueueable state, with
    /// the inputs their type needs.
    pub analyzable: u64,
    /// Posts in scope and in an enqueueable state but without the media their
    /// type needs; not enqueued (G3-9).
    pub waiting_for_media: u64,
    /// Posts in scope already `pending` or `analyzing`.
    pub already_queued: u64,
}

/// Counts of the whole library's AI states (the AI-status facet and the
/// admin's `ai-status`). Social posts only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StateCounts {
    /// `ai_status IS NULL`.
    pub unanalyzed: u64,
    /// `pending`.
    pub pending: u64,
    /// `analyzing`.
    pub analyzing: u64,
    /// `done`.
    pub done: u64,
    /// `error`.
    pub error: u64,
}

/// A claimed item, ready to catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Claim {
    /// The post's internal id.
    pub post_id: i64,
    /// Its public key.
    pub key: String,
    /// The try number this claim is (`ai_attempts` after the increment): the
    /// guard the drain passes back to [`apply`]/[`backoff`]/[`fail`].
    pub attempt: i64,
    /// Unique claim fence, prevents cancel/requeue ABA writes.
    pub token: String,
    /// Whether stored video keyframes were requested.
    pub deep: bool,
    /// Which catalog the post gets (social or web), from its platform and
    /// media type.
    pub kind: CatalogKind,
}

/// What a guarded end-of-try write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Guarded {
    /// The row was still this try's `analyzing`: the write landed.
    Applied,
    /// The row changed under the try (a manual edit, a clear, a trash, or a
    /// newer try): nothing was written (plan lane rule 13).
    Dropped,
}

impl Guarded {
    fn of(changed: bool) -> Self {
        if changed {
            Self::Applied
        } else {
            Self::Dropped
        }
    }

    /// Whether the write landed.
    #[must_use]
    pub fn applied(self) -> bool {
        self == Self::Applied
    }
}

/// What [`recover_interrupted`] cleaned.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Recovered {
    /// `analyzing` rows of a dead try sent back to `pending`.
    pub requeued: u64,
    /// `analyzing` rows failed `interrupted` after their last try.
    pub failed: u64,
}

/// The scope of a cancel or retry (plan §2.9, G3-6): specific posts, or every
/// item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reach {
    /// These posts, by key.
    Keys(Vec<String>),
    /// Every item (the queue's cancel-all / retry-all).
    All,
}

// ── Reading ────────────────────────────────────────────────────────────────

/// Counts what analyzing the posts of `selector` in `mode` would do, without
/// changing anything (plan §2.9 `analyze`, the first call).
///
/// # Errors
///
/// [`RepoError::Invalid`] when the selector is over its caps; database errors.
pub fn scope_counts(
    conn: &Connection,
    selector: &Selector,
    mode: Mode,
    now: i64,
) -> Result<ScopeCounts> {
    let _ = now;
    let scope = selector.sql()?;
    let count = |extra: &str| -> Result<u64> {
        let sql = format!(
            "SELECT count(*) FROM posts p WHERE {SOCIAL} AND p.deleted_at IS NULL \
             AND ({}) AND {extra}",
            scope.condition
        );
        let n: i64 = conn
            .prepare_cached(&sql)?
            .query_row(params_from_iter(scope.params.iter()), |r| r.get(0))?;
        Ok(u64::try_from(n).unwrap_or(0))
    };
    let analyzable = count(&format!("{} AND {HAS_INPUTS}", mode.enqueue_condition()))?;
    let waiting_for_media = count(&format!(
        "{} AND {NEEDS_MEDIA} AND NOT {HAS_INPUTS}",
        mode.enqueue_condition()
    ))?;
    let already_queued = count("p.ai_status IN ('pending','analyzing')")?;
    Ok(ScopeCounts {
        analyzable,
        waiting_for_media,
        already_queued,
    })
}

/// The AI states of the library's social posts (the admin's `ai-status`,
/// the estimate's totals).
///
/// # Errors
///
/// Database errors.
pub fn state_counts(conn: &Connection) -> Result<StateCounts> {
    let mut counts = StateCounts::default();
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT p.ai_status, count(*) FROM posts p \
         WHERE {SOCIAL} AND p.deleted_at IS NULL GROUP BY p.ai_status"
    ))?;
    let rows = stmt.query_map([], |r| {
        Ok((r.get::<_, Option<String>>(0)?, r.get::<_, i64>(1)?))
    })?;
    for row in rows {
        let (status, n) = row?;
        let n = u64::try_from(n).unwrap_or(0);
        match status.as_deref() {
            None => counts.unanalyzed = n,
            Some("pending") => counts.pending = n,
            Some("analyzing") => counts.analyzing = n,
            Some("done") => counts.done = n,
            Some("error") => counts.error = n,
            Some(_) => {}
        }
    }
    Ok(counts)
}

/// The soonest time a `pending` social item is due, unix ms (the drain's
/// re-arm time and the sweep's wake time). `None` when none is pending.
///
/// # Errors
///
/// Database errors.
pub fn next_pending_at(conn: &Connection) -> Result<Option<i64>> {
    let at: Option<Option<i64>> = conn
        .prepare_cached(&format!(
            "SELECT min(coalesce(p.ai_next_at, 0)) FROM posts p \
             WHERE p.ai_status = 'pending' AND p.deleted_at IS NULL AND {SOCIAL}"
        ))?
        .query_row([], |r| r.get(0))
        .optional()?;
    Ok(at.flatten())
}

/// The age, in ms, of the oldest `pending` item already due at `now`, and how
/// many are due (the admin's "oldest due pending"). `None` when none is due.
///
/// # Errors
///
/// Database errors.
pub fn oldest_due_pending(conn: &Connection, now: i64) -> Result<Option<(i64, u64)>> {
    let row = conn
        .prepare_cached(&format!(
            "SELECT min(coalesce(p.ai_next_at, 0)), count(*) FROM posts p \
             WHERE p.ai_status = 'pending' AND p.deleted_at IS NULL AND {SOCIAL} \
             AND coalesce(p.ai_next_at, 0) <= ?1"
        ))?
        .query_row(params![now], |r| {
            Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, i64>(1)?))
        })
        .optional()?;
    Ok(match row {
        Some((Some(at), n)) if n > 0 => Some(((now - at).max(0), u64::try_from(n).unwrap_or(0))),
        _ => None,
    })
}

/// One item of the AI queue view (`GET /ai/queue`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Item {
    /// The post's internal id (the keyset cursor).
    pub id: i64,
    /// The post's public key.
    pub key: String,
    /// Its AI state (`pending`, `analyzing`, `error`, `done`).
    pub status: String,
    /// Tries spent.
    pub attempts: i64,
    /// Next attempt time, for a backed-off `pending` item.
    pub next_at: Option<i64>,
    /// The last error code, for an `error` item.
    pub error: Option<String>,
}

/// Lists the queue's items (`GET /ai/queue`), newest first, by `status` when
/// given (else every item with an `ai_status`), after `cursor` (an id), up to
/// `limit`. Returns the items and the next cursor. Social posts only.
///
/// # Errors
///
/// Database errors.
pub fn list(
    conn: &Connection,
    status: Option<&str>,
    cursor: Option<i64>,
    limit: u32,
) -> Result<(Vec<Item>, Option<i64>)> {
    let limit = limit.clamp(1, 200);
    let mut sql = format!(
        "SELECT p.id, p.key, p.ai_status, p.ai_attempts, p.ai_next_at, p.ai_error FROM posts p \
         WHERE p.ai_status IS NOT NULL AND p.deleted_at IS NULL AND {SOCIAL}"
    );
    let mut params: Vec<Value> = Vec::new();
    if let Some(status) = status {
        sql.push_str(" AND p.ai_status = ?");
        params.push(Value::Text(status.to_owned()));
    }
    if let Some(cursor) = cursor {
        sql.push_str(" AND p.id < ?");
        params.push(Value::Integer(cursor));
    }
    sql.push_str(" ORDER BY p.id DESC LIMIT ?");
    params.push(Value::Integer(i64::from(limit) + 1));
    let mut stmt = conn.prepare_cached(&sql)?;
    let rows = stmt.query_map(params_from_iter(params.iter()), |r| {
        Ok(Item {
            id: r.get(0)?,
            key: r.get(1)?,
            status: r.get(2)?,
            attempts: r.get(3)?,
            next_at: r.get(4)?,
            error: r.get(5)?,
        })
    })?;
    let mut items: Vec<Item> = rows.collect::<rusqlite::Result<_>>()?;
    let next = (items.len() > limit as usize).then(|| items[limit as usize - 1].id);
    items.truncate(limit as usize);
    Ok((items, next))
}

/// The terminal `error` codes in `ai_error`, with their counts (the admin's
/// "errors by code"). Social posts only.
///
/// # Errors
///
/// Database errors.
pub fn errors_by_code(conn: &Connection) -> Result<Vec<(String, u64)>> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT coalesce(p.ai_error, 'unknown'), count(*) FROM posts p \
         WHERE p.ai_status = 'error' AND p.deleted_at IS NULL AND {SOCIAL} \
         GROUP BY p.ai_error ORDER BY count(*) DESC, p.ai_error"
    ))?;
    let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?;
    rows.map(|row| {
        let (code, n) = row?;
        Ok((code, u64::try_from(n).unwrap_or(0)))
    })
    .collect()
}

// ── Writing ──────────────────────────────────────────────────────────────────

/// Enqueues the analyzable posts of `selector` in `mode`: sets them `pending`,
/// due now, with the attempt count and the error cleared. Returns how many
/// were newly enqueued. The confirm step of `POST /ai/analyze`.
///
/// # Errors
///
/// [`RepoError::Invalid`] when the selector is over its caps; database errors.
pub fn mark_pending(conn: &Connection, selector: &Selector, mode: Mode, now: i64) -> Result<u64> {
    let scope = selector.sql()?;
    let sql = format!(
        "UPDATE posts AS p SET ai_status = 'pending', ai_next_at = ?1, ai_attempts = 0, ai_error = NULL \
         WHERE id IN (SELECT p.id FROM posts p WHERE {SOCIAL} AND p.deleted_at IS NULL \
             AND ({}) AND {} AND {HAS_INPUTS})",
        scope.condition,
        mode.enqueue_condition()
    );
    let mut params = vec![Value::Integer(now)];
    params.extend(scope.params);
    let changed = conn
        .prepare_cached(&sql)?
        .execute(params_from_iter(params.iter()))?;
    Ok(changed as u64)
}

/// Forces the given posts `pending` (due now), whatever their current state,
/// unless they are trashed: the seam the capture ingest and a recapture use
/// (`ai::queue::enqueue(user, keys, …)`, the cross-phase dependency). Returns
/// how many changed.
///
/// # Errors
///
/// Database errors.
pub fn set_pending(conn: &Connection, post_ids: &[i64], now: i64) -> Result<u64> {
    if post_ids.is_empty() {
        return Ok(0);
    }
    let ids = Value::Text(serde_json::to_string(post_ids).expect("ids serialize"));
    let changed = conn
        .prepare_cached(
            "UPDATE posts AS p SET ai_status = 'pending', ai_next_at = ?2, ai_attempts = 0, \
             ai_error = NULL WHERE id IN (SELECT value FROM json_each(?1)) AND deleted_at IS NULL \
             AND ai_status IS NOT 'pending' AND ai_status IS NOT 'analyzing'",
        )?
        .execute(params![ids, now])?;
    Ok(changed as u64)
}

/// Claims the soonest due `pending` social item: marks it `analyzing` and
/// bumps `ai_attempts`. Returns `None` when none is due at `now`.
///
/// # Errors
///
/// Database errors.
pub fn claim_due(conn: &Connection, now: i64) -> Result<Option<Claim>> {
    // The soonest due item by its `ai_next_at`, then id, using `posts_ai`.
    let candidate = conn
        .prepare_cached(&format!(
            "SELECT p.id, p.key, p.ai_attempts, p.platform, p.media_type FROM posts p \
             WHERE p.ai_status = 'pending' AND p.deleted_at IS NULL AND {SOCIAL} \
             AND coalesce(p.ai_next_at, 0) <= ?1 \
             ORDER BY coalesce(p.ai_next_at, 0) ASC, p.id ASC LIMIT 1"
        ))?
        .query_row(params![now], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, String>(4)?,
            ))
        })
        .optional()?;
    let Some((post_id, key, attempts, platform, media_type)) = candidate else {
        return Ok(None);
    };
    // Claim it; another claimer that won the race leaves 0 rows changed.
    let claimed = conn
        .prepare_cached(
            "UPDATE posts AS p SET ai_status = 'analyzing', ai_attempts = ai_attempts + 1, \
             ai_next_at = NULL WHERE id = ?1 AND ai_status = 'pending'",
        )?
        .execute(params![post_id])?;
    if claimed == 0 {
        return Ok(None);
    }
    let key_hash = post_id.to_be_bytes();
    conn.execute("INSERT OR IGNORE INTO ai_cache(kind,key_hash,value_json,created_at) VALUES ('catalog.queue',?1,'{\"deep\":false}',?2)", params![key_hash.as_slice(), now])?;
    conn.execute("UPDATE ai_cache SET value_json = json_set(value_json, '$.claim', lower(hex(randomblob(16)))) WHERE kind='catalog.queue' AND key_hash=?1", params![key_hash.as_slice()])?;
    let (token, deep) = conn.query_row("SELECT json_extract(value_json,'$.claim'), coalesce(json_extract(value_json,'$.deep'),0) FROM ai_cache WHERE kind='catalog.queue' AND key_hash=?1", params![key_hash.as_slice()], |r| Ok((r.get(0)?,r.get(1)?)))?;
    Ok(Some(Claim {
        post_id,
        key,
        attempt: attempts + 1,
        token,
        deep,
        kind: CatalogKind::of(&platform, &media_type),
    }))
}

/// Whether this try still owns the row: `analyzing`, the same attempt, not
/// trashed. The guard of every end-of-try write.
fn is_current(conn: &Connection, post_id: i64, attempt: i64, token: &str) -> Result<bool> {
    let held = conn
        .prepare_cached(
            "SELECT 1 FROM posts WHERE id = ?1 AND ai_status = 'analyzing' \
             AND ai_attempts = ?2 AND deleted_at IS NULL AND EXISTS (SELECT 1 FROM ai_cache WHERE kind='catalog.queue' AND key_hash=?3 AND json_extract(value_json,'$.claim')=?4)",
        )?
        .query_row(params![post_id, attempt, post_id.to_be_bytes().as_slice(), token], |_| Ok(()))
        .optional()?;
    Ok(held.is_some())
}

/// Applies a finished analysis through [`posts::update_ai`], but only while
/// the row is still this try's `analyzing` (the guard). A manual edit, a clear
/// or a trash made meanwhile wins, and the result is dropped (plan lane rule
/// 13).
///
/// # Errors
///
/// Database errors.
pub fn apply(
    conn: &Connection,
    post_id: i64,
    attempt: i64,
    token: &str,
    patch: &AiPatch,
    now: i64,
) -> Result<Guarded> {
    if !is_current(conn, post_id, attempt, token)? {
        return Ok(Guarded::Dropped);
    }
    posts::update_ai(conn, post_id, patch, now)?;
    conn.execute(
        "DELETE FROM ai_cache WHERE kind='catalog.queue' AND key_hash=?1",
        [post_id.to_be_bytes().as_slice()],
    )?;
    Ok(Guarded::Applied)
}

/// Backs the item off to `pending`, due at `next_at`, recording `error` as the
/// last error, guarded as [`apply`]. The try is spent (the attempt stands).
///
/// # Errors
///
/// Database errors.
pub fn backoff(
    conn: &Connection,
    post_id: i64,
    attempt: i64,
    token: &str,
    next_at: i64,
    error: &str,
) -> Result<Guarded> {
    if !is_current(conn, post_id, attempt, token)? {
        return Ok(Guarded::Dropped);
    }
    let changed = conn
        .prepare_cached(
            "UPDATE posts AS p SET ai_status = 'pending', ai_next_at = ?3, ai_error = ?4 \
             WHERE id = ?1 AND ai_status = 'analyzing' AND ai_attempts = ?2 AND deleted_at IS NULL AND EXISTS (SELECT 1 FROM ai_cache WHERE kind='catalog.queue' AND key_hash=?5 AND json_extract(value_json,'$.claim')=?6)",
        )?
        .execute(params![post_id, attempt, next_at, error, post_id.to_be_bytes().as_slice(), token])?;
    Ok(Guarded::of(changed > 0))
}

/// Fails the item for good (`error`, with `ai_error`), guarded as [`apply`].
/// Bumps `updated_at` so the post's new AI state is seen on a refetch.
///
/// # Errors
///
/// Database errors.
pub fn fail(
    conn: &Connection,
    post_id: i64,
    attempt: i64,
    token: &str,
    error: &str,
    now: i64,
) -> Result<Guarded> {
    if !is_current(conn, post_id, attempt, token)? {
        return Ok(Guarded::Dropped);
    }
    let changed = conn
        .prepare_cached(
            "UPDATE posts AS p SET ai_status = 'error', ai_error = ?4, updated_at = ?3 \
             WHERE id = ?1 AND ai_status = 'analyzing' AND ai_attempts = ?2 AND deleted_at IS NULL AND EXISTS (SELECT 1 FROM ai_cache WHERE kind='catalog.queue' AND key_hash=?5 AND json_extract(value_json,'$.claim')=?6)",
        )?
        .execute(params![post_id, attempt, now, error, post_id.to_be_bytes().as_slice(), token])?;
    Ok(Guarded::of(changed > 0))
}

/// Releases the item back to `pending` without spending the try: undoes the
/// claim's attempt bump and re-arms it at `next_at`. Guarded as [`apply`].
/// Used when the provider is offline, paused or its breaker is open — the
/// work waits without a try (G3-25).
///
/// # Errors
///
/// Database errors.
pub fn release(
    conn: &Connection,
    post_id: i64,
    attempt: i64,
    token: &str,
    next_at: i64,
) -> Result<Guarded> {
    if !is_current(conn, post_id, attempt, token)? {
        return Ok(Guarded::Dropped);
    }
    let changed = conn
        .prepare_cached(
            "UPDATE posts AS p SET ai_status = 'pending', ai_attempts = ai_attempts - 1, \
             ai_next_at = ?3 WHERE id = ?1 AND ai_status = 'analyzing' AND ai_attempts = ?2 \
             AND deleted_at IS NULL",
        )?
        .execute(params![post_id, attempt, next_at])?;
    Ok(Guarded::of(changed > 0))
}

/// At the start of a drain, cleans the `analyzing` rows a dead try left: back
/// to `pending` to try again, or to `error` (`interrupted`) once the last try
/// is used. Social posts only.
///
/// # Errors
///
/// Database errors.
pub fn recover_interrupted(conn: &Connection, max_attempts: u32, now: i64) -> Result<Recovered> {
    let max = i64::from(max_attempts);
    let failed = conn
        .prepare_cached(&format!(
            "UPDATE posts AS p SET ai_status = 'error', ai_error = 'interrupted', updated_at = ?2 \
             WHERE ai_status = 'analyzing' AND deleted_at IS NULL AND {SOCIAL} AND ai_attempts >= ?1"
        ))?
        .execute(params![max, now])?;
    let requeued = conn
        .prepare_cached(&format!(
            "UPDATE posts AS p SET ai_status = 'pending', ai_next_at = ?1 \
             WHERE ai_status = 'analyzing' AND deleted_at IS NULL AND {SOCIAL}"
        ))?
        .execute(params![now])?;
    Ok(Recovered {
        requeued: requeued as u64,
        failed: failed as u64,
    })
}

/// Cancels queued work (plan §2.9, G3-6): `pending` and `analyzing` items go
/// back to `done` when they were analyzed before (`ai_analyzed_at` set), else
/// to `NULL`; `ai_next_at`, `ai_error` and `ai_attempts` are cleared. Returns
/// how many changed.
///
/// # Errors
///
/// Database errors.
pub fn cancel(conn: &Connection, reach: &Reach, now: i64) -> Result<u64> {
    let _ = now;
    let set = "ai_status = CASE WHEN ai_analyzed_at IS NOT NULL THEN 'done' ELSE NULL END, \
               ai_next_at = NULL, ai_error = NULL, ai_attempts = 0";
    let n = match reach {
        Reach::All => conn
            .prepare_cached(&format!(
                "UPDATE posts AS p SET {set} WHERE ai_status IN ('pending','analyzing') \
                 AND deleted_at IS NULL AND {SOCIAL}"
            ))?
            .execute([])?,
        Reach::Keys(keys) => {
            if keys.is_empty() {
                return Ok(0);
            }
            let ids = Value::Text(serde_json::to_string(keys).expect("keys serialize"));
            conn.prepare_cached(&format!(
                "UPDATE posts AS p SET {set} WHERE ai_status IN ('pending','analyzing') \
                 AND deleted_at IS NULL AND key IN (SELECT value FROM json_each(?1))"
            ))?
            .execute(params![ids])?
        }
    };
    Ok(n as u64)
}

/// Retries failed items (plan §2.9, G3-6): `error` items go back to `pending`,
/// due now, with the attempt count and the error cleared. Returns how many
/// changed.
///
/// # Errors
///
/// Database errors.
pub fn retry(conn: &Connection, reach: &Reach, now: i64) -> Result<u64> {
    let set = "ai_status = 'pending', ai_next_at = ?1, ai_attempts = 0, ai_error = NULL";
    let n = match reach {
        Reach::All => conn
            .prepare_cached(&format!(
                "UPDATE posts AS p SET {set} WHERE ai_status = 'error' AND deleted_at IS NULL AND {SOCIAL}"
            ))?
            .execute(params![now])?,
        Reach::Keys(keys) => {
            if keys.is_empty() {
                return Ok(0);
            }
            let ids = Value::Text(serde_json::to_string(keys).expect("keys serialize"));
            conn.prepare_cached(&format!(
                "UPDATE posts AS p SET {set} WHERE ai_status = 'error' AND deleted_at IS NULL \
                 AND key IN (SELECT value FROM json_each(?2))"
            ))?
            .execute(params![now, ids])?
        }
    };
    Ok(n as u64)
}

/// Exact eligible IDs at estimate time; confirmation cannot grow its selection.
pub fn eligible_ids(conn: &Connection, selector: &Selector, mode: Mode) -> Result<Vec<i64>> {
    let scope = selector.sql()?;
    let mut stmt = conn.prepare(&format!("SELECT p.id FROM posts p WHERE {SOCIAL} AND p.deleted_at IS NULL AND ({}) AND {} AND {HAS_INPUTS} ORDER BY p.id", scope.condition, mode.enqueue_condition()))?;
    Ok(stmt
        .query_map(params_from_iter(scope.params.iter()), |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Saves the per-item deep choice without storing a caption, media or prompt.
pub fn set_deep(conn: &Connection, ids: &[i64], deep: bool, now: i64) -> Result<()> {
    for id in ids {
        conn.execute("INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES ('catalog.queue',?1,json_object('deep',?2),?3) ON CONFLICT(kind,key_hash) DO UPDATE SET value_json=excluded.value_json,created_at=excluded.created_at", params![id.to_be_bytes().as_slice(),deep,now])?;
    }
    Ok(())
}

/// Latest provider availability observed by the drain (aggregate diagnostics).
pub fn provider_status(conn: &Connection) -> Result<Option<String>> {
    Ok(conn.query_row("SELECT json_extract(value_json,'$.state') FROM ai_cache WHERE kind='catalog.status' AND key_hash=X'00'",[],|r|r.get(0)).optional()?)
}
/// Updates the aggregate provider snapshot only on a transition.
pub fn set_provider_status(conn: &Connection, status: &str, now: i64) -> Result<()> {
    if provider_status(conn)?.as_deref() != Some(status) {
        conn.execute("INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES ('catalog.status',X'00',json_object('state',?1),?2) ON CONFLICT(kind,key_hash) DO UPDATE SET value_json=excluded.value_json,created_at=excluded.created_at",rusqlite::params![status,now])?;
    }
    Ok(())
}
