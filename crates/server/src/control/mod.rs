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

pub mod api_tokens;
pub mod audit;
pub mod invites;
pub mod magic_links;
pub mod sessions;
pub mod uploads;
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

/// A control database on a temporary directory, for the unit tests of the
/// query modules.
#[cfg(test)]
pub(crate) mod testing {
    use std::ops::Deref;

    use shelfy_core::db::{ControlDb, ControlDbConfig};
    use tempfile::TempDir;

    use super::users::{self, NewUser, Role};

    /// A fixed "now" (2026-10-02), unix ms.
    pub const NOW: i64 = 1_790_899_200_000;

    /// The database and the directory that holds it.
    pub struct TestControl {
        db: ControlDb,
        _dir: TempDir,
    }

    impl Deref for TestControl {
        type Target = ControlDb;

        fn deref(&self) -> &ControlDb {
            &self.db
        }
    }

    /// A fresh control database with an owner and a member; returns it and
    /// their ids.
    pub fn control_with_users() -> (TestControl, String, String) {
        let dir = tempfile::tempdir().expect("temp dir");
        let config = ControlDbConfig {
            readers: 1,
            ..ControlDbConfig::default()
        };
        let db = ControlDb::open(dir.path().join("control.sqlite"), &config).expect("open");
        let owner = "01OWNER0000000000000000000".to_owned();
        let member = "01MEMBER000000000000000000".to_owned();
        db.write(|tx| {
            for (id, email, role) in [
                (&owner, "owner@example.test", Role::Owner),
                (&member, "member@example.test", Role::Member),
            ] {
                let user = NewUser {
                    id,
                    email,
                    role,
                    quota_bytes: 0,
                };
                users::insert(tx, &user, NOW)?;
            }
            Ok::<_, shelfy_core::repo::RepoError>(())
        })
        .expect("users");
        (TestControl { db, _dir: dir }, owner, member)
    }
}
