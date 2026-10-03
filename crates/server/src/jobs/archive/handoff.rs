//! The breaker handoff (plan §2.13, D14; contract C10): while a CDN host
//! group's breaker is open, the server stops fetching that platform and its
//! `pending` items become `client` (extension uploads, P2-14); when the
//! breaker leaves the open state, the items whose URLs are still valid come
//! back to `pending`.
//!
//! The states are derived by the core rule with the archive's current modes
//! ([`super::modes`]: a platform whose breaker is open counts as `client`),
//! so the handoff is a derivation of that platform's posts in every active
//! user's library:
//!
//! - **Opens** (`Closed` or `HalfOpen` → `Open`): the platform's posts are
//!   derived again; what the server would fetch becomes `client`.
//! - **Leaves the open state** (`Open` → `HalfOpen`, after its 30 minutes,
//!   or → `Closed`): derived again with the configured modes, and each user
//!   with server work gets their drain. The first fetch of a half-open
//!   breaker is its probe ([`crate::outbound::breaker`]); the other drains
//!   wait for its verdict (`BreakerOpen` → deferred). A blocked probe opens
//!   the breaker again, and the next check hands the platform over again.
//!
//! An extension upload still in flight when the items come back completes
//! as usual: the store finds the slot filled and the CAS dedupes the bytes.
//!
//! [`spawn_watcher`] runs the checks ([`sync_breakers`]) whenever a breaker
//! opens or closes ([`crate::outbound::Cdn::subscribe`]), when an open
//! breaker's time is up, and every [`WATCH_INTERVAL`] (a reopening after a
//! probe sends no change). At start it derives every user's library once,
//! since the breakers of a new process start closed: what a previous
//! process handed over comes back, and the drains start. The drains also
//! hand their own user's platform over as soon as they meet an open
//! breaker, so a handoff never waits for the watcher.

use std::time::Duration;

use shelfy_core::ingest::archive::{ArchivePolicy, Scope, refresh_states};
use shelfy_core::repo::{Platform, RepoError};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::control::jobs as job_rows;
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::library::{self, Change};
use crate::outbound::{BreakerState, HostGroup};
use crate::state::{AppState, blocking};

/// How often the watcher looks at the breakers without a change.
pub const WATCH_INTERVAL: Duration = Duration::from_secs(30);

/// Checks the CDN breakers and hands over, or takes back, the platforms
/// whose breaker opened or left the open state since the last check (see
/// the module docs). Returns those transitions: the platform, and whether
/// its breaker is open now.
///
/// # Errors
///
/// Listing the users failed. A library that cannot be derived (locked for
/// a restore) is skipped and logged.
pub async fn sync_breakers(state: &AppState) -> Result<Vec<(Platform, bool)>, ApiError> {
    let transitions = state.archive().observe_breakers(state.outbound());
    if transitions.is_empty() {
        return Ok(transitions);
    }
    for (platform, open) in &transitions {
        tracing::info!(
            platform = platform.as_str(),
            open,
            "archive: {} the platform's fetches",
            if *open {
                "the extension takes over"
            } else {
                "the server takes back"
            }
        );
    }
    let platforms: Vec<Platform> = transitions.iter().map(|(platform, _)| *platform).collect();
    let returned: Vec<Platform> = transitions
        .iter()
        .filter(|(_, open)| !open)
        .map(|(platform, _)| *platform)
        .collect();
    rederive_all(state, Some(&platforms), &returned, !returned.is_empty()).await?;
    Ok(transitions)
}

/// Derives again the archive states of every active user's library, with
/// the current modes: the posts of `platforms`, or all of them (`None`).
/// The items of the `returned` platforms that their breaker blocked are due
/// at once (their tries stay counted). With `enqueue`, each user left with
/// server work gets their drain.
///
/// # Errors
///
/// Listing the users failed.
pub async fn rederive_all(
    state: &AppState,
    platforms: Option<&[Platform]>,
    returned: &[Platform],
    enqueue: bool,
) -> Result<(), ApiError> {
    let control = std::sync::Arc::clone(state.control());
    let users = blocking(move || control.read(job_rows::active_users)).await?;
    for user in users {
        let library = state.config().data_dir.library_db(&user);
        if !tokio::fs::try_exists(&library).await.unwrap_or(false) {
            continue;
        }
        let modes = super::modes(state);
        let now = state.jobs().clock().now_ms();
        let platforms = platforms.map(<[Platform]>::to_vec);
        let returned = returned.to_vec();
        let written = library::write(state, &user, ChangeReason::Archive, move |tx| {
            for platform in &returned {
                unblock(tx, *platform)?;
            }
            let policy = ArchivePolicy::read(tx, modes)?;
            match &platforms {
                Some(platforms) => {
                    for platform in platforms {
                        refresh_states(tx, Scope::Platform(*platform), &policy, now)?;
                    }
                }
                None => {
                    refresh_states(tx, Scope::All, &policy, now)?;
                }
            }
            Ok::<_, RepoError>(Change {
                value: server_work(tx)?,
                keys: None,
            })
        })
        .await;
        if written.is_ok() {
            // Posts may have gone to `client`: the extension's tasks (P2-14).
            crate::extension::tasks::wake(state, &user);
        }
        match written {
            Ok(written) if enqueue && written.value > 0 => {
                if let Err(err) = super::enqueue(state.jobs(), &user).await {
                    tracing::warn!(user_id = %user, error = %err, "archive: cannot enqueue the drain");
                }
            }
            Ok(_) => {}
            Err(err) => {
                tracing::warn!(user_id = %user, error = %err, "archive: cannot derive the archive states");
            }
        }
    }
    Ok(())
}

/// Makes the items of `platform` that a block delayed due now.
fn unblock(conn: &rusqlite::Connection, platform: Platform) -> Result<(), RepoError> {
    conn.prepare_cached(
        "UPDATE post_media SET fetch_next_at = NULL
         WHERE fetch_error = 'blocked' AND fetch_next_at IS NOT NULL
           AND post_id IN (SELECT id FROM posts WHERE platform = ?1)",
    )?
    .execute([platform.as_str()])?;
    conn.prepare_cached(
        "UPDATE posts SET cover_fetch_next_at = NULL
         WHERE platform = ?1 AND cover_fetch_error = 'blocked'
           AND cover_fetch_next_at IS NOT NULL",
    )?
    .execute([platform.as_str()])?;
    Ok(())
}

/// The library's posts the server has something to fetch for.
fn server_work(conn: &rusqlite::Connection) -> Result<u64, RepoError> {
    let n: i64 = conn
        .prepare_cached(
            "SELECT count(*) FROM posts WHERE deleted_at IS NULL
               AND archive_state IN ('pending', 'partial')
               AND platform IN ('instagram', 'twitter', 'pinterest')",
        )?
        .query_row([], |row| row.get(0))?;
    Ok(u64::try_from(n).unwrap_or(0))
}

/// Starts the watcher (see the module docs); it stops when `token` fires.
#[must_use]
pub fn spawn_watcher(state: AppState, token: CancellationToken) -> JoinHandle<()> {
    tokio::spawn(async move {
        if let Err(err) = rederive_all(&state, None, &[], true).await {
            tracing::warn!(error = %err, "archive: the start-up derivation failed");
        }
        let mut changes = state.outbound().cdn().subscribe();
        loop {
            let wait = next_check(&state);
            tokio::select! {
                () = token.cancelled() => return,
                changed = changes.changed() => if changed.is_err() { return },
                () = tokio::time::sleep(wait) => {}
            }
            if let Err(err) = sync_breakers(&state).await {
                tracing::warn!(error = %err, "archive: the breaker check failed");
            }
        }
    })
}

/// When to look again: when the first open breaker's time is up, at most
/// [`WATCH_INTERVAL`] from now.
fn next_check(state: &AppState) -> Duration {
    let now = Instant::now();
    HostGroup::CDN
        .into_iter()
        .filter_map(|group| match state.outbound().cdn().breaker_state(group) {
            BreakerState::Open { until } => Some(until.saturating_duration_since(now)),
            BreakerState::Closed | BreakerState::HalfOpen => None,
        })
        .map(|wait| wait + Duration::from_millis(10))
        .fold(WATCH_INTERVAL, Duration::min)
}
