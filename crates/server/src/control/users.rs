//! `users`: the accounts (plan §2.6). E4 keeps the instance owner-only: one
//! `owner`, created by `admin create-owner`.

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

    fn parse(value: &str) -> Option<Self> {
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
    fn parse(value: &str) -> Option<Self> {
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
    /// Role.
    pub role: Role,
    /// Quota in bytes; 0 = unlimited.
    pub quota_bytes: i64,
}

const COLUMNS: &str = "id, email, role, status, quota_bytes, created_at";

fn from_row(row: &Row<'_>) -> rusqlite::Result<User> {
    let role: String = row.get(2)?;
    let status: String = row.get(3)?;
    Ok(User {
        id: row.get(0)?,
        email: Redacted(row.get(1)?),
        // The schema's CHECK constraints admit only these values.
        role: Role::parse(&role).unwrap_or(Role::Member),
        status: Status::parse(&status).unwrap_or(Status::Disabled),
        quota_bytes: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// Inserts an account.
///
/// # Errors
///
/// [`RepoError::Conflict`] when the id or the email is taken.
pub fn insert(conn: &Connection, user: &NewUser<'_>, now: i64) -> Result<()> {
    conn.execute(
        "INSERT INTO users (id, email, role, quota_bytes, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            user.id,
            user.email,
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
