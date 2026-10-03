//! `users`: the accounts (plan §2.6). E4 keeps the instance owner-only: one
//! `owner`, created by `admin create-owner`. `admin create-user` (E6) adds
//! `member` accounts for tests, such as the live host's mock account; the
//! instance otherwise stays owner-only (no invite redemption route yet).
//!
//! Besides the account itself: what the user accepted ([`Consent`], `POST
//! /me/consent`), their storage ([`Usage`]: counted by the `usage.recompute`
//! job, moved by the commits and releases of [`crate::quota`], read by `GET
//! /me/usage`) and their [`Limits`] (`admin user limits`).

use rusqlite::{Connection, OptionalExtension as _, Row, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use crate::telemetry::redact::Redacted;

/// `users.role`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// The instance owner: admin routes, unlimited quota.
    Owner,
    /// An invited user.
    Member,
}

impl Role {
    /// The stored value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owner => "owner",
            Self::Member => "member",
        }
    }

    /// The role stored as `value`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "owner" => Some(Self::Owner),
            "member" => Some(Self::Member),
            _ => None,
        }
    }
}

/// `users.status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    /// Can sign in.
    Active,
    /// Blocked by the owner.
    Disabled,
    /// Being purged.
    Deleting,
}

impl Status {
    /// The status stored as `value`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "active" => Some(Self::Active),
            "disabled" => Some(Self::Disabled),
            "deleting" => Some(Self::Deleting),
            _ => None,
        }
    }
}

/// An account. The email is [`Redacted`] so the struct can be debug-printed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// ULID; names the user's directory under `users/`.
    pub id: String,
    /// Login email, lowercase.
    pub email: Redacted<String>,
    /// Display name, if the account was given one.
    pub display_name: Option<String>,
    /// Role.
    pub role: Role,
    /// Status.
    pub status: Status,
    /// Storage quota in bytes; 0 means unlimited (the owner).
    pub quota_bytes: i64,
    /// Creation time, unix ms.
    pub created_at: i64,
}

/// A new account.
#[derive(Clone, Copy, Debug)]
pub struct NewUser<'a> {
    /// ULID.
    pub id: &'a str,
    /// Email, as returned by [`normalize_email`].
    pub email: &'a str,
    /// Display name, if any.
    pub display_name: Option<&'a str>,
    /// Role.
    pub role: Role,
    /// Quota in bytes; 0 = unlimited.
    pub quota_bytes: i64,
}

/// A member's default storage quota when none is given (plan §4.2: "Default
/// quota 5 GiB per member (owner unlimited)"). `admin create-user` (E6) uses
/// it; P4-07 (quotas) may later make it configurable.
pub const DEFAULT_MEMBER_QUOTA_BYTES: i64 = 5 * 1024 * 1024 * 1024;

const COLUMNS: &str = "id, email, display_name, role, status, quota_bytes, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    let role: String = row.get(3)?;
    let status: String = row.get(4)?;
    Ok(User {
        id: row.get(0)?,
        email: Redacted(row.get(1)?),
        display_name: row.get(2)?,
        // The schema's CHECK constraints admit only these values.
        role: Role::parse(&role).unwrap_or(Role::Member),
        status: Status::parse(&status).unwrap_or(Status::Disabled),
        quota_bytes: row.get(5)?,
        created_at: row.get(6)?,
    })
}

/// Inserts an account.
///
/// # Errors
///
/// [`RepoError::Conflict`] when the id or the email is taken.
pub fn insert(conn: &Connection, user: &NewUser<'_>, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO users (id, email, display_name, role, quota_bytes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            user.id,
            user.email,
            user.display_name,
            user.role.as_str(),
            user.quota_bytes,
            now
        ],
    )
    .map_err(|e| conflict_on_unique(e, "email"))?;
    Ok(())
}

/// The account with `id`.
///
/// # Errors
///
/// The query failed.
pub fn get(conn: &Connection, id: &str) -> Result<Option<User>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM users WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// The owner (the first one, should there ever be several).
///
/// # Errors
///
/// The query failed.
pub fn find_owner(conn: &Connection) -> Result<Option<User>> {
    conn.query_row(
        &format!(
            "SELECT {COLUMNS} FROM users WHERE role = 'owner' ORDER BY created_at, id LIMIT 1"
        ),
        [],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// The account with `email` (case-insensitive, like the column).
///
/// # Errors
///
/// The query failed.
pub fn find_by_email(conn: &Connection, email: &str) -> Result<Option<User>> {
    conn.query_row(
        &format!("SELECT {COLUMNS} FROM users WHERE email = ?1"),
        [email],
        from_row,
    )
    .optional()
    .map_err(RepoError::from)
}

/// What a user accepted (plan §2.11, §7.2): the versions of the disclaimer
/// and of the privacy notice, and when. `None` until accepted.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Consent {
    /// The disclaimer's version.
    pub disclaimer_version: Option<String>,
    /// When the disclaimer was accepted, unix ms.
    pub disclaimer_accepted_at: Option<i64>,
    /// The privacy notice's version.
    pub privacy_version: Option<String>,
    /// When the privacy notice was accepted, unix ms.
    pub privacy_accepted_at: Option<i64>,
}

/// The consent of `user_id`; `None` when there is no such user.
///
/// # Errors
///
/// The query failed.
pub fn consent(conn: &Connection, user_id: &str) -> Result<Option<Consent>> {
    conn.query_row(
        "SELECT disclaimer_version, disclaimer_accepted_at, privacy_version, \
         privacy_accepted_at FROM users WHERE id = ?1",
        [user_id],
        |row| {
            Ok(Consent {
                disclaimer_version: row.get(0)?,
                disclaimer_accepted_at: row.get(1)?,
                privacy_version: row.get(2)?,
                privacy_accepted_at: row.get(3)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records that `user_id` accepted the disclaimer `disclaimer_version` and
/// the privacy notice `privacy_version` at `now`. Returns whether the user
/// exists.
///
/// # Errors
///
/// The update failed.
pub fn set_consent(
    conn: &Connection,
    user_id: &str,
    disclaimer_version: &str,
    privacy_version: &str,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE users SET disclaimer_version = ?2, disclaimer_accepted_at = ?4, \
         privacy_version = ?3, privacy_accepted_at = ?4 WHERE id = ?1",
        params![user_id, disclaimer_version, privacy_version, now],
    )?;
    Ok(changed > 0)
}

/// A user's storage (plan §2.13): the quota, and the use as the
/// `usage.recompute` job last counted it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Usage {
    /// The quota in bytes; 0 means unlimited (the owner).
    pub quota_bytes: i64,
    /// Media plus database, in bytes.
    pub used_bytes: i64,
    /// The stored media objects, in bytes.
    pub media_bytes: i64,
    /// The library database file, in bytes.
    pub db_bytes: i64,
    /// When the use was counted, unix ms; `None` if it never was.
    pub updated_at: Option<i64>,
}

/// The storage of `user_id`; `None` when there is no such user.
///
/// # Errors
///
/// The query failed.
pub fn usage(conn: &Connection, user_id: &str) -> Result<Option<Usage>> {
    conn.query_row(
        "SELECT quota_bytes, usage_bytes, usage_media_bytes, usage_db_bytes, usage_updated_at \
         FROM users WHERE id = ?1",
        [user_id],
        |row| {
            Ok(Usage {
                quota_bytes: row.get(0)?,
                used_bytes: row.get(1)?,
                media_bytes: row.get(2)?,
                db_bytes: row.get(3)?,
                updated_at: row.get(4)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records the use of `user_id` counted at `now`: `media_bytes` of media
/// and a `db_bytes` database. Returns whether the user exists.
///
/// # Errors
///
/// The update failed.
pub fn set_usage(
    conn: &Connection,
    user_id: &str,
    media_bytes: i64,
    db_bytes: i64,
    now: i64,
) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE users SET usage_bytes = ?2 + ?3, usage_media_bytes = ?2, usage_db_bytes = ?3, \
         usage_updated_at = ?4 WHERE id = ?1",
        params![user_id, media_bytes, db_bytes, now],
    )?;
    Ok(changed > 0)
}

/// Adds `bytes` of media to the use of `user_id` (a quota commit,
/// [`crate::quota`]); `usage_bytes` stays media plus database and
/// `usage_updated_at` stays the time of the last count. Returns whether the
/// user exists.
///
/// # Errors
///
/// [`RepoError::Invalid`] for negative `bytes`; the update failed.
pub fn add_media_usage(conn: &Connection, user_id: &str, bytes: i64) -> Result<bool> {
    if bytes < 0 {
        return Err(RepoError::Invalid {
            field: "bytes",
            reason: "must not be negative",
        });
    }
    // SET reads the row as it was, so both sums start from the old media.
    let changed = conn
        .prepare_cached(
            "UPDATE users SET usage_media_bytes = usage_media_bytes + ?2, \
             usage_bytes = usage_media_bytes + ?2 + usage_db_bytes WHERE id = ?1",
        )?
        .execute(params![user_id, bytes])?;
    Ok(changed > 0)
}

/// Takes `bytes` of media off the use of `user_id`, never below 0 (a quota
/// release after deleted objects, [`crate::quota`]). Returns whether the
/// user exists.
///
/// # Errors
///
/// [`RepoError::Invalid`] for negative `bytes`; the update failed.
pub fn remove_media_usage(conn: &Connection, user_id: &str, bytes: i64) -> Result<bool> {
    if bytes < 0 {
        return Err(RepoError::Invalid {
            field: "bytes",
            reason: "must not be negative",
        });
    }
    let changed = conn
        .prepare_cached(
            "UPDATE users SET usage_media_bytes = max(0, usage_media_bytes - ?2), \
             usage_bytes = max(0, usage_media_bytes - ?2) + usage_db_bytes WHERE id = ?1",
        )?
        .execute(params![user_id, bytes])?;
    Ok(changed > 0)
}

/// A user's limits (plan §2.6, §2.13; P4-07).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// The storage quota in bytes, media plus database; 0 means unlimited.
    pub quota_bytes: i64,
    /// Site captures a day (UTC); 0 means unlimited.
    pub capture_daily_limit: i64,
}

/// The limits of `user_id`; `None` when there is no such user.
///
/// # Errors
///
/// The query failed.
pub fn limits(conn: &Connection, user_id: &str) -> Result<Option<Limits>> {
    conn.query_row(
        "SELECT quota_bytes, capture_daily_limit FROM users WHERE id = ?1",
        [user_id],
        |row| {
            Ok(Limits {
                quota_bytes: row.get(0)?,
                capture_daily_limit: row.get(1)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Sets the limits of `user_id` that are given, and keeps the others.
/// Returns the limits now in force; `None` when there is no such user.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a negative limit; the update failed.
pub fn set_limits(
    conn: &Connection,
    user_id: &str,
    quota_bytes: Option<i64>,
    capture_daily_limit: Option<i64>,
) -> Result<Option<Limits>> {
    if quota_bytes.is_some_and(|q| q < 0) {
        return Err(RepoError::Invalid {
            field: "quotaBytes",
            reason: "must not be negative (0 means unlimited)",
        });
    }
    if capture_daily_limit.is_some_and(|c| c < 0) {
        return Err(RepoError::Invalid {
            field: "captureDailyLimit",
            reason: "must not be negative (0 means unlimited)",
        });
    }
    conn.execute(
        "UPDATE users SET quota_bytes = coalesce(?2, quota_bytes), \
         capture_daily_limit = coalesce(?3, capture_daily_limit) WHERE id = ?1",
        params![user_id, quota_bytes, capture_daily_limit],
    )?;
    limits(conn, user_id)
}

/// Trims and lowercases an email address and checks its shape: one `@`, a
/// local part of 1–64 and a dotted domain, at most 254 characters, no
/// whitespace or control characters. Deliverability is not checked.
///
/// # Errors
///
/// [`RepoError::Invalid`] on field `email`.
pub fn normalize_email(raw: &str) -> Result<String> {
    let invalid = |reason| RepoError::Invalid {
        field: "email",
        reason,
    };
    let email = raw.trim().to_ascii_lowercase();
    if email.is_empty() || email.len() > 254 {
        return Err(invalid("must be 1 to 254 characters"));
    }
    if email.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(invalid("must not contain spaces or control characters"));
    }
    let Some((local, domain)) = email.split_once('@') else {
        return Err(invalid("must contain an @"));
    };
    if domain.contains('@') {
        return Err(invalid("must contain a single @"));
    }
    if local.is_empty() || local.len() > 64 {
        return Err(invalid("the part before the @ must be 1 to 64 characters"));
    }
    let labels_ok = domain
        .split('.')
        .all(|label| !label.is_empty() && !label.starts_with('-') && !label.ends_with('-'));
    if !domain.contains('.') || !labels_ok {
        return Err(invalid("the domain must be a dotted host name"));
    }
    Ok(email)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    #[test]
    fn consent_is_recorded_per_user() {
        let (db, owner, member) = control_with_users();
        let read = |id: &str| db.read(|conn| consent(conn, id)).unwrap();
        assert_eq!(read(&owner), Some(Consent::default()));
        assert!(
            db.write(|tx| set_consent(tx, &owner, "2026-10", "1", NOW))
                .unwrap()
        );
        assert_eq!(
            read(&owner),
            Some(Consent {
                disclaimer_version: Some("2026-10".into()),
                disclaimer_accepted_at: Some(NOW),
                privacy_version: Some("1".into()),
                privacy_accepted_at: Some(NOW),
            })
        );
        assert_eq!(read(&member), Some(Consent::default()));
        assert_eq!(read("01NOBODY000000000000000000"), None);
        assert!(
            !db.write(|tx| set_consent(tx, "01NOBODY000000000000000000", "a", "b", NOW))
                .unwrap()
        );
    }

    #[test]
    fn usage_is_media_plus_database() {
        let (db, owner, _member) = control_with_users();
        let read = || db.read(|conn| usage(conn, &owner)).unwrap().unwrap();
        assert_eq!(read(), Usage::default(), "never counted, unlimited quota");
        assert!(
            db.write(|tx| set_usage(tx, &owner, 7_000, 300, NOW))
                .unwrap()
        );
        assert_eq!(
            read(),
            Usage {
                quota_bytes: 0,
                used_bytes: 7_300,
                media_bytes: 7_000,
                db_bytes: 300,
                updated_at: Some(NOW),
            }
        );
    }

    #[test]
    fn usage_deltas_keep_media_plus_database() {
        let (db, owner, member) = control_with_users();
        let read = || db.read(|conn| usage(conn, &owner)).unwrap().unwrap();
        db.write(|tx| set_usage(tx, &owner, 1_000, 300, NOW))
            .unwrap();
        assert!(db.write(|tx| add_media_usage(tx, &owner, 500)).unwrap());
        let after = read();
        assert_eq!(
            (after.media_bytes, after.db_bytes, after.used_bytes),
            (1_500, 300, 1_800)
        );
        assert_eq!(after.updated_at, Some(NOW), "a commit is not a count");
        assert!(db.write(|tx| remove_media_usage(tx, &owner, 200)).unwrap());
        assert_eq!(read().used_bytes, 1_600);
        assert!(
            db.write(|tx| remove_media_usage(tx, &owner, 9_999))
                .unwrap()
        );
        let floor = read();
        assert_eq!(
            (floor.media_bytes, floor.used_bytes),
            (0, 300),
            "never below 0"
        );
        assert!(
            db.write(|tx| add_media_usage(tx, &owner, -1)).is_err(),
            "negative"
        );
        assert!(
            db.write(|tx| remove_media_usage(tx, &owner, -1)).is_err(),
            "negative"
        );
        assert!(
            !db.write(|tx| add_media_usage(tx, "01NOBODY000000000000000000", 1))
                .unwrap()
        );
        let untouched = db.read(|conn| usage(conn, &member)).unwrap().unwrap();
        assert_eq!(untouched, Usage::default());
    }

    #[test]
    fn limits_change_only_where_given() {
        let (db, owner, member) = control_with_users();
        let read = |id: &str| db.read(|conn| limits(conn, id)).unwrap();
        assert_eq!(
            read(&member),
            Some(Limits {
                quota_bytes: 0,
                capture_daily_limit: 20,
            }),
            "the schema's defaults"
        );
        let set = db
            .write(|tx| set_limits(tx, &member, Some(5 << 30), None))
            .unwrap();
        assert_eq!(
            set,
            Some(Limits {
                quota_bytes: 5 << 30,
                capture_daily_limit: 20,
            })
        );
        let set = db
            .write(|tx| set_limits(tx, &member, None, Some(0)))
            .unwrap();
        assert_eq!(set.unwrap().capture_daily_limit, 0);
        assert_eq!(read(&member).unwrap().quota_bytes, 5 << 30);
        assert_eq!(read(&owner).unwrap().quota_bytes, 0, "another user");
        for (quota, captures) in [(Some(-1), None), (None, Some(-1))] {
            let err = db
                .write(|tx| set_limits(tx, &member, quota, captures))
                .unwrap_err();
            assert!(matches!(err, RepoError::Invalid { .. }), "{err}");
        }
        assert_eq!(
            db.write(|tx| set_limits(tx, "01NOBODY000000000000000000", Some(1), None))
                .unwrap(),
            None
        );
    }

    #[test]
    fn emails_are_trimmed_and_lowercased() {
        assert_eq!(
            normalize_email("  Owner@Example.TEST ").unwrap(),
            "owner@example.test"
        );
        assert_eq!(
            normalize_email("a.b+tag@mail.example.co").unwrap(),
            "a.b+tag@mail.example.co"
        );
    }

    #[test]
    fn malformed_emails_are_refused() {
        for bad in [
            "",
            "owner",
            "@example.test",
            "owner@",
            "owner@localhost",
            "own er@example.test",
            "a@b@example.test",
            "owner@.example.test",
            "owner@example..test",
            "owner@-example.test",
        ] {
            let err = normalize_email(bad).unwrap_err();
            assert!(
                matches!(err, RepoError::Invalid { field: "email", .. }),
                "{bad:?}: {err}"
            );
        }
        let long_local = format!("{}@example.test", "a".repeat(65));
        assert!(normalize_email(&long_local).is_err());
    }
}
