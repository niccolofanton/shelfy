//! Queries on the control database (`control.sqlite`, plan §2.6).
//!
//! The control database is server-only state (accounts, sessions, tokens,
//! jobs), so its queries live in the server, while `shelfy-core` keeps the
//! schema and the connection handling ([`shelfy_core::db::ControlDb`]). Like
//! the core repositories, these are plain functions over a `&Connection` that
//! run inside `ControlDb::read` / `ControlDb::write` and take `now` as an
//! argument. Errors are [`RepoError`]s, so they map onto problems the same way.
//!
//! [`RepoError`]: shelfy_core::repo::RepoError

pub mod audit;
pub mod invites;
pub mod users;

use rusqlite::ErrorCode;
use shelfy_core::repo::RepoError;

/// Maps a UNIQUE or PRIMARY KEY violation to [`RepoError::Conflict`].
fn conflict_on_unique(err: rusqlite::Error, what: &'static str) -> RepoError {
    match err.sqlite_error() {
        Some(e)
            if e.code == ErrorCode::ConstraintViolation
                && matches!(
                    e.extended_code,
                    rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
                        | rusqlite::ffi::SQLITE_CONSTRAINT_PRIMARYKEY
                ) =>
        {
            RepoError::Conflict(what)
        }
        _ => err.into(),
    }
}
