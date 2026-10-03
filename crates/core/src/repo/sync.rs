//! Sync runs and sync sources (plan §2.16, §2.7; P2 contracts C4, C8; P2-09).
//!
//! A **sync run** records one walk of a platform listing by the browser
//! extension: its trigger, the listing it walked, the collection it mapped
//! into, the counters ingest raised (`scanned`, `inserted`, `updated`,
//! `known`), the pages it scanned, why it stopped, and the cursor a page cap
//! left for the next run. A **sync source** is the stable identity of a
//! listing across runs (`(platform, source_key)`): when its last full walk
//! reached the end of the feed (`last_full_at`), the next run is incremental
//! (P2-G1); a capped walk leaves a `resume_cursor` to continue from (P2-G2).
//!
//! The closed sets (trigger, listing kind, state, stop reason) are stored as
//! the text the API uses and validated there, by the typed wire enums of
//! `crates/server/src/routes/sync_runs.rs`; this module keeps them as strings
//! so the core carries no web types. Runs are never deleted: a library holds
//! one user's, so a run of another user is simply absent (a 404 at the API).

use rusqlite::{Connection, OptionalExtension, Row, params};

use super::{Platform, Result};

/// A run that is still going (`sync_runs.status`).
pub const STATE_RUNNING: &str = "running";

/// A new sync run, as [`insert_run`] stores it. The id is a ULID the server
/// mints; `status` starts [`STATE_RUNNING`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewRun<'a> {
    /// ULID, the run's public id.
    pub id: &'a str,
    /// The platform walked.
    pub platform: Platform,
    /// What started the run (`manual`, `web`, …).
    pub trigger: &'a str,
    /// The listing kind (`ig_saved`, `ig_collection`, `x_bookmarks`,
    /// `pin_board`).
    pub source_kind: &'a str,
    /// The source's stable identity, `(platform, source_key)`.
    pub source_key: &'a str,
    /// The folder or board id the listing names, when it has one.
    pub listing_external_id: Option<&'a str>,
    /// The folder or board name as the page showed it.
    pub listing_name: Option<&'a str>,
    /// The collection the run maps posts into.
    pub collection_id: Option<i64>,
    /// Whether this run may stop early on known items (P2-G1).
    pub incremental: bool,
    /// The consecutive-known threshold it uses.
    pub stop_after_known: i64,
    /// The cursor a previous capped walk left (P2-G2).
    pub resume_cursor: Option<&'a str>,
    /// The extension version that opened it.
    pub client_version: Option<&'a str>,
}

/// The end of a run, as [`patch_run`] applies it. Absent counters keep the
/// ones ingest raised.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RunPatch<'a> {
    /// The new state (`running`, `done`, `stopped`, `failed`).
    pub state: &'a str,
    /// Pages scanned, absolute; `None` keeps the stored value.
    pub pages: Option<i64>,
    /// Items scanned, absolute; `None` keeps the stored value.
    pub scanned: Option<i64>,
    /// Why it stopped.
    pub stop_reason: Option<&'a str>,
    /// The cursor to resume from; `None` leaves the stored one.
    pub resume_cursor: Option<&'a str>,
    /// The error code, when it failed.
    pub error_code: Option<&'a str>,
}

/// A sync run as the API returns it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRun {
    /// ULID.
    pub id: String,
    /// The platform.
    pub platform: Platform,
    /// The trigger.
    pub trigger: String,
    /// The listing kind.
    pub source_kind: String,
    /// The source identity.
    pub source_key: String,
    /// The folder or board id.
    pub listing_external_id: Option<String>,
    /// The folder or board name.
    pub listing_name: Option<String>,
    /// The mapped collection.
    pub collection_id: Option<i64>,
    /// The state (`sync_runs.status`).
    pub state: String,
    /// Items scanned.
    pub scanned: i64,
    /// New posts.
    pub inserted: i64,
    /// Known posts that changed.
    pub updated: i64,
    /// Known posts.
    pub known: i64,
    /// Pages scanned.
    pub pages: i64,
    /// Why it stopped.
    pub stop_reason: Option<String>,
    /// The resume cursor.
    pub resume_cursor: Option<String>,
    /// The error code.
    pub error_code: Option<String>,
    /// Whether it ran incrementally.
    pub incremental: bool,
    /// The consecutive-known threshold.
    pub stop_after_known: i64,
    /// When it started, unix ms.
    pub started_at: i64,
    /// When it ended, unix ms; `None` while running.
    pub finished_at: Option<i64>,
    /// Last activity, unix ms (create, ingest or patch).
    pub updated_at: i64,
}

/// A sync source (`GET /extension/sources`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Source {
    /// The platform.
    pub platform: Platform,
    /// The source identity.
    pub source_key: String,
    /// The listing kind.
    pub source_kind: Option<String>,
    /// The folder or board id.
    pub external_id: Option<String>,
    /// The folder or board name when last synced.
    pub source_name: Option<String>,
    /// The collection it maps into.
    pub collection_id: Option<i64>,
    /// When a run of this source last ran, unix ms.
    pub last_run_at: Option<i64>,
    /// When its last full walk reached the end of the feed, unix ms.
    pub last_full_at: Option<i64>,
    /// The cursor a capped walk left.
    pub resume_cursor: Option<String>,
}

/// What [`upsert_source`] records when a run opens.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceUpdate<'a> {
    /// The platform.
    pub platform: Platform,
    /// The source identity.
    pub source_key: &'a str,
    /// The listing kind.
    pub source_kind: &'a str,
    /// The folder or board id.
    pub external_id: Option<&'a str>,
    /// The folder or board name.
    pub source_name: Option<&'a str>,
    /// The collection it maps into.
    pub collection_id: Option<i64>,
}

/// A keyset cursor over the runs list: a run's `(started_at, id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunCursor {
    /// The run's start time.
    pub started_at: i64,
    /// The run's id (ties break by it).
    pub id: String,
}

const RUN_COLUMNS: &str = "id, platform, trigger, source_kind, source_key, listing_external_id,
    listing_name, collection_id, status, scanned, inserted, updated, known, pages, stop_reason,
    resume_cursor, error_code, incremental, stop_after_known, started_at, finished_at, updated_at";

fn run_from_row(r: &Row<'_>) -> rusqlite::Result<SyncRun> {
    Ok(SyncRun {
        id: r.get(0)?,
        platform: r.get(1)?,
        trigger: r.get(2)?,
        source_kind: r.get(3)?,
        source_key: r.get(4)?,
        listing_external_id: r.get(5)?,
        listing_name: r.get(6)?,
        collection_id: r.get(7)?,
        state: r.get(8)?,
        scanned: r.get(9)?,
        inserted: r.get(10)?,
        updated: r.get(11)?,
        known: r.get(12)?,
        pages: r.get(13)?,
        stop_reason: r.get(14)?,
        resume_cursor: r.get(15)?,
        error_code: r.get(16)?,
        incremental: r.get::<_, i64>(17)? != 0,
        stop_after_known: r.get(18)?,
        started_at: r.get(19)?,
        finished_at: r.get(20)?,
        updated_at: r.get(21)?,
    })
}

/// Opens a run (`status = 'running'`, `started_at = now`).
///
/// # Errors
///
/// Database errors (a duplicate id, a `collection_id` that does not exist).
pub fn insert_run(conn: &Connection, new: &NewRun<'_>, now: i64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO sync_runs
           (id, platform, trigger, source_kind, source_key, listing_external_id, listing_name,
            collection_id, status, scanned, inserted, updated, known, pages, incremental,
            stop_after_known, resume_cursor, client_version, started_at, finished_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'running', 0, 0, 0, 0, 0, ?9, ?10, ?11, ?12, ?13,
                 NULL, ?13)",
    )?
    .execute(params![
        new.id,
        new.platform,
        new.trigger,
        new.source_kind,
        new.source_key,
        new.listing_external_id,
        new.listing_name,
        new.collection_id,
        i64::from(new.incremental),
        new.stop_after_known,
        new.resume_cursor,
        new.client_version,
        now,
    ])?;
    Ok(())
}

/// One run by id; `None` when the library has no such run (another user's, or
/// an unknown id).
///
/// # Errors
///
/// Database errors.
pub fn get_run(conn: &Connection, id: &str) -> Result<Option<SyncRun>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM sync_runs WHERE id = ?1"
        ))?
        .query_row([id], run_from_row)
        .optional()?)
}

/// Raises a running run's counters by what an ingest batch accepted and marks
/// it active (`updated_at = now`). Returns whether the run exists and runs.
///
/// # Errors
///
/// Database errors.
pub fn add_counts(
    conn: &Connection,
    id: &str,
    scanned: i64,
    inserted: i64,
    updated: i64,
    known: i64,
    now: i64,
) -> Result<bool> {
    let changed = conn
        .prepare_cached(
            "UPDATE sync_runs
               SET scanned = scanned + ?2, inserted = inserted + ?3, updated = updated + ?4,
                   known = known + ?5, updated_at = ?6
             WHERE id = ?1 AND status = 'running'",
        )?
        .execute(params![id, scanned, inserted, updated, known, now])?;
    Ok(changed > 0)
}

/// Applies the end of a run and returns it; `None` when the run is gone. A
/// terminal state (anything but `running`) stamps `finished_at`.
///
/// # Errors
///
/// Database errors.
pub fn patch_run(
    conn: &Connection,
    id: &str,
    patch: &RunPatch<'_>,
    now: i64,
) -> Result<Option<SyncRun>> {
    let terminal = patch.state != STATE_RUNNING;
    let changed = conn
        .prepare_cached(
            "UPDATE sync_runs SET
               status = ?2,
               pages = COALESCE(?3, pages),
               scanned = COALESCE(?4, scanned),
               stop_reason = ?5,
               resume_cursor = COALESCE(?6, resume_cursor),
               error_code = ?7,
               finished_at = CASE WHEN ?8 THEN ?9 ELSE finished_at END,
               updated_at = ?9
             WHERE id = ?1",
        )?
        .execute(params![
            id,
            patch.state,
            patch.pages,
            patch.scanned,
            patch.stop_reason,
            patch.resume_cursor,
            patch.error_code,
            terminal,
            now,
        ])?;
    if changed == 0 {
        return Ok(None);
    }
    get_run(conn, id)
}

/// A page of runs, newest first, filtered by platform and state, after
/// `before`. Reads `limit + 1` to tell whether a next page exists; returns at
/// most `limit` and the cursor to continue from.
///
/// # Errors
///
/// Database errors.
pub fn list_runs(
    conn: &Connection,
    platform: Option<Platform>,
    state: Option<&str>,
    before: Option<&RunCursor>,
    limit: usize,
) -> Result<(Vec<SyncRun>, Option<RunCursor>)> {
    let mut sql = format!("SELECT {RUN_COLUMNS} FROM sync_runs WHERE 1 = 1");
    let mut args: Vec<rusqlite::types::Value> = Vec::new();
    if let Some(platform) = platform {
        args.push(platform.as_str().to_owned().into());
        sql.push_str(&format!(" AND platform = ?{}", args.len()));
    }
    if let Some(state) = state {
        args.push(state.to_owned().into());
        sql.push_str(&format!(" AND status = ?{}", args.len()));
    }
    if let Some(cursor) = before {
        args.push(cursor.started_at.into());
        let started = args.len();
        args.push(cursor.id.clone().into());
        let id = args.len();
        sql.push_str(&format!(
            " AND (started_at < ?{started} OR (started_at = ?{started} AND id < ?{id}))"
        ));
    }
    let want = limit.saturating_add(1);
    args.push(i64::try_from(want).unwrap_or(i64::MAX).into());
    sql.push_str(&format!(
        " ORDER BY started_at DESC, id DESC LIMIT ?{}",
        args.len()
    ));
    let mut runs: Vec<SyncRun> = conn
        .prepare_cached(&sql)?
        .query_map(rusqlite::params_from_iter(args), run_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    let next = (runs.len() == want).then(|| {
        runs.pop();
        let last = runs.last().expect("limit is at least 1");
        RunCursor {
            started_at: last.started_at,
            id: last.id.clone(),
        }
    });
    Ok((runs, next))
}

/// Stops the running runs that have seen no activity for `idle_ms`
/// (`updated_at` older than `now - idle_ms`), with stop reason `error`.
/// Returns how many it stopped.
///
/// # Errors
///
/// Database errors.
pub fn sweep_idle(conn: &Connection, idle_ms: i64, now: i64) -> Result<usize> {
    let cutoff = now.saturating_sub(idle_ms);
    let stopped = conn
        .prepare_cached(
            "UPDATE sync_runs
               SET status = 'stopped', stop_reason = 'idle', finished_at = ?2, updated_at = ?2
             WHERE status = 'running' AND updated_at < ?1",
        )?
        .execute(params![cutoff, now])?;
    Ok(stopped)
}

const SOURCE_COLUMNS: &str = "platform, source_key, source_kind, external_id, source_name,
    collection_id, last_run_at, last_full_at, resume_cursor";

fn source_from_row(r: &Row<'_>) -> rusqlite::Result<Source> {
    Ok(Source {
        platform: r.get(0)?,
        source_key: r.get(1)?,
        source_kind: r.get(2)?,
        external_id: r.get(3)?,
        source_name: r.get(4)?,
        collection_id: r.get(5)?,
        last_run_at: r.get(6)?,
        last_full_at: r.get(7)?,
        resume_cursor: r.get(8)?,
    })
}

/// One source by identity.
///
/// # Errors
///
/// Database errors.
pub fn get_source(
    conn: &Connection,
    platform: Platform,
    source_key: &str,
) -> Result<Option<Source>> {
    Ok(conn
        .prepare_cached(&format!(
            "SELECT {SOURCE_COLUMNS} FROM sync_sources WHERE platform = ?1 AND source_key = ?2"
        ))?
        .query_row(params![platform, source_key], source_from_row)
        .optional()?)
}

/// Records that a run of `update`'s source opened now: the listing it stands
/// for, its collection and `last_run_at`. Keeps `last_full_at` and the resume
/// cursor.
///
/// # Errors
///
/// Database errors.
pub fn upsert_source(conn: &Connection, update: &SourceUpdate<'_>, now: i64) -> Result<()> {
    conn.prepare_cached(
        "INSERT INTO sync_sources
           (platform, source_key, source_kind, external_id, source_name, collection_id, last_run_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (platform, source_key) DO UPDATE SET
           source_kind = excluded.source_kind,
           external_id = excluded.external_id,
           source_name = COALESCE(excluded.source_name, sync_sources.source_name),
           collection_id = excluded.collection_id,
           last_run_at = excluded.last_run_at",
    )?
    .execute(params![
        update.platform,
        update.source_key,
        update.source_kind,
        update.external_id,
        update.source_name,
        update.collection_id,
        now,
    ])?;
    Ok(())
}

/// Records that a source's full walk reached the end of the feed: its next
/// run is incremental, and the resume cursor is cleared (P2-G1, P2-G2).
///
/// # Errors
///
/// Database errors.
pub fn set_last_full(
    conn: &Connection,
    platform: Platform,
    source_key: &str,
    now: i64,
) -> Result<()> {
    conn.prepare_cached(
        "UPDATE sync_sources SET last_full_at = ?3, resume_cursor = NULL
         WHERE platform = ?1 AND source_key = ?2",
    )?
    .execute(params![platform, source_key, now])?;
    Ok(())
}

/// Stores (or clears) the cursor a capped walk left, to resume from next run.
///
/// # Errors
///
/// Database errors.
pub fn set_resume_cursor(
    conn: &Connection,
    platform: Platform,
    source_key: &str,
    cursor: Option<&str>,
) -> Result<()> {
    conn.prepare_cached(
        "UPDATE sync_sources SET resume_cursor = ?3 WHERE platform = ?1 AND source_key = ?2",
    )?
    .execute(params![platform, source_key, cursor])?;
    Ok(())
}

/// Every source, newest run first.
///
/// # Errors
///
/// Database errors.
pub fn list_sources(conn: &Connection) -> Result<Vec<Source>> {
    let rows = conn
        .prepare_cached(&format!(
            "SELECT {SOURCE_COLUMNS} FROM sync_sources
             ORDER BY last_run_at DESC, platform, source_key"
        ))?
        .query_map([], source_from_row)?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;
    use crate::schema::{self, Kind};

    fn library() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        schema::migrate(&mut conn, Kind::Library).unwrap();
        conn
    }

    fn new_run<'a>(id: &'a str, platform: Platform, key: &'a str) -> NewRun<'a> {
        NewRun {
            id,
            platform,
            trigger: "manual",
            source_kind: "ig_saved",
            source_key: key,
            listing_external_id: None,
            listing_name: None,
            collection_id: None,
            incremental: false,
            stop_after_known: 10,
            resume_cursor: None,
            client_version: Some("0.2.0"),
        }
    }

    #[test]
    fn a_run_is_opened_counted_and_ended() {
        let conn = library();
        insert_run(
            &conn,
            &new_run("01A", Platform::Instagram, "ig_saved"),
            1_000,
        )
        .unwrap();
        let run = get_run(&conn, "01A").unwrap().unwrap();
        assert_eq!(run.state, "running");
        assert_eq!(run.started_at, 1_000);
        assert_eq!(run.updated_at, 1_000);
        assert_eq!(run.finished_at, None);
        assert!(!run.incremental);

        assert!(add_counts(&conn, "01A", 12, 3, 1, 8, 2_000).unwrap());
        let run = get_run(&conn, "01A").unwrap().unwrap();
        assert_eq!(
            (run.scanned, run.inserted, run.updated, run.known),
            (12, 3, 1, 8)
        );
        assert_eq!(run.updated_at, 2_000);

        let patched = patch_run(
            &conn,
            "01A",
            &RunPatch {
                state: "done",
                pages: Some(2),
                scanned: Some(12),
                stop_reason: Some("end_of_feed"),
                resume_cursor: None,
                error_code: None,
            },
            3_000,
        )
        .unwrap()
        .unwrap();
        assert_eq!(patched.state, "done");
        assert_eq!(patched.pages, 2);
        assert_eq!(patched.finished_at, Some(3_000));
        assert_eq!(patched.stop_reason.as_deref(), Some("end_of_feed"));

        // A counter bump no longer lands on a finished run.
        assert!(!add_counts(&conn, "01A", 1, 0, 0, 0, 4_000).unwrap());
        assert!(get_run(&conn, "02X").unwrap().is_none());
    }

    #[test]
    fn runs_page_newest_first() {
        let conn = library();
        for (n, id) in ["01A", "01B", "01C"].iter().enumerate() {
            insert_run(
                &conn,
                &new_run(id, Platform::Twitter, "x_bookmarks"),
                1_000 + n as i64,
            )
            .unwrap();
        }
        let (first, cursor) = list_runs(&conn, None, None, None, 2).unwrap();
        assert_eq!(
            first.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["01C", "01B"]
        );
        let cursor = cursor.unwrap();
        let (second, next) = list_runs(&conn, None, None, Some(&cursor), 2).unwrap();
        assert_eq!(
            second.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(),
            ["01A"]
        );
        assert!(next.is_none());

        let filtered = list_runs(&conn, Some(Platform::Instagram), None, None, 10)
            .unwrap()
            .0;
        assert!(filtered.is_empty(), "another platform");
    }

    #[test]
    fn idle_running_runs_are_swept() {
        let conn = library();
        insert_run(&conn, &new_run("01A", Platform::Instagram, "ig_saved"), 0).unwrap();
        insert_run(&conn, &new_run("01B", Platform::Instagram, "ig_folder"), 0).unwrap();
        add_counts(&conn, "01B", 1, 0, 0, 1, 10_000_000).unwrap(); // recently active
        assert_eq!(sweep_idle(&conn, 7_200_000, 10_000_000).unwrap(), 1);
        assert_eq!(get_run(&conn, "01A").unwrap().unwrap().state, "stopped");
        assert_eq!(get_run(&conn, "01B").unwrap().unwrap().state, "running");
    }

    #[test]
    fn a_source_remembers_its_full_walk_and_cursor() {
        let conn = library();
        let update = SourceUpdate {
            platform: Platform::Instagram,
            source_key: "ig_collection:42",
            source_kind: "ig_collection",
            external_id: Some("42"),
            source_name: Some("Lighting"),
            collection_id: None,
        };
        upsert_source(&conn, &update, 1_000).unwrap();
        let source = get_source(&conn, Platform::Instagram, "ig_collection:42")
            .unwrap()
            .unwrap();
        assert_eq!(source.source_name.as_deref(), Some("Lighting"));
        assert_eq!(source.last_run_at, Some(1_000));
        assert_eq!(source.last_full_at, None);

        set_resume_cursor(
            &conn,
            Platform::Instagram,
            "ig_collection:42",
            Some("page-7"),
        )
        .unwrap();
        assert_eq!(
            get_source(&conn, Platform::Instagram, "ig_collection:42")
                .unwrap()
                .unwrap()
                .resume_cursor
                .as_deref(),
            Some("page-7")
        );
        set_last_full(&conn, Platform::Instagram, "ig_collection:42", 2_000).unwrap();
        let source = get_source(&conn, Platform::Instagram, "ig_collection:42")
            .unwrap()
            .unwrap();
        assert_eq!(source.last_full_at, Some(2_000));
        assert_eq!(source.resume_cursor, None, "a full walk clears it");

        // A rename on the platform keeps the row's name only when given.
        upsert_source(
            &conn,
            &SourceUpdate {
                source_name: None,
                ..update
            },
            3_000,
        )
        .unwrap();
        let source = get_source(&conn, Platform::Instagram, "ig_collection:42")
            .unwrap()
            .unwrap();
        assert_eq!(source.source_name.as_deref(), Some("Lighting"), "kept");
        assert_eq!(source.last_run_at, Some(3_000));
        assert_eq!(list_sources(&conn).unwrap().len(), 1);
    }
}
