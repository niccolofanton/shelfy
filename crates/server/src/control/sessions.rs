//! `sessions` (plan §2.6, §2.11): opaque server-side sessions.
//!
//! A row is keyed by the SHA-256 of the session cookie's value, so the
//! database never holds a usable session. `expires_at` is the absolute expiry
//! (90 days after sign-in); the idle expiry (30 days) follows `last_seen_at`,
//! which [`touch`] moves forward. `reauth_at` is the last time the user proved
//! who they are (sign-in now, re-authentication from P1-13).

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

use super::conflict_on_unique;
use super::users::{Role, Status};
use crate::tokens::TokenHash;

/// Longest `user_agent` kept, in bytes.
pub const USER_AGENT_MAX_BYTES: usize = 256;

/// A new session.
#[derive(Clone, Copy, Debug)]
pub struct NewSession<'a> {
    /// SHA-256 of the cookie value.
    pub id_hash: &'a TokenHash,
    /// The signed-in user.
    pub user_id: &'a str,
    /// Absolute expiry, unix ms.
    pub expires_at: i64,
    /// The browser's `User-Agent`, for the session list (P1-17); truncated.
    pub user_agent: Option<&'a str>,
}

/// A stored session with the state of its user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Session {
    /// The user.
    pub user_id: String,
    /// The user's role.
    pub role: Role,
    /// The user's status: only `active` users have usable sessions.
    pub user_status: Status,
    /// Sign-in time, unix ms.
    pub created_at: i64,
    /// Absolute expiry, unix ms.
    pub expires_at: i64,
    /// Last use (moved forward by [`touch`]), unix ms.
    pub last_seen_at: i64,
    /// Last proof of identity (sign-in or re-authentication), unix ms.
    pub reauth_at: Option<i64>,
}

impl Session {
    /// Whether the session still signs its user in at `now`: the user is
    /// active and neither the absolute nor the idle expiry (`idle_ms` after
    /// the last use) has passed.
    #[must_use]
    pub fn is_usable(&self, now: i64, idle_ms: i64) -> bool {
        self.user_status == Status::Active
            && now < self.expires_at
            && now < self.last_seen_at.saturating_add(idle_ms)
    }
}

/// Stores a session; `created_at`, `last_seen_at` and `reauth_at` are `now`.
///
/// # Errors
///
/// [`RepoError::Conflict`] if the hash exists (a token collision), or the
/// insert failed.
pub fn insert(conn: &Connection, session: &NewSession<'_>, now: i64) -> Result<()> {
    let user_agent = session.user_agent.map(truncate_user_agent);
    conn.execute(
        "INSERT INTO sessions (id_hash, user_id, created_at, expires_at, last_seen_at, reauth_at, \
         user_agent) VALUES (?1, ?2, ?3, ?4, ?3, ?3, ?5)",
        params![
            session.id_hash.as_slice(),
            session.user_id,
            now,
            session.expires_at,
            user_agent
        ],
    )
    .map_err(|e| conflict_on_unique(e, "session"))?;
    Ok(())
}

/// The session whose cookie hashes to `id_hash`, usable or not.
///
/// # Errors
///
/// The query failed.
pub fn find(conn: &Connection, id_hash: &TokenHash) -> Result<Option<Session>> {
    conn.query_row(
        "SELECT s.user_id, u.role, u.status, s.created_at, s.expires_at, s.last_seen_at, \
         s.reauth_at FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.id_hash = ?1",
        [id_hash.as_slice()],
        |row| {
            let role: String = row.get(1)?;
            let status: String = row.get(2)?;
            Ok(Session {
                user_id: row.get(0)?,
                // The schema's CHECK constraints admit only known values.
                role: Role::parse(&role).unwrap_or(Role::Member),
                user_status: Status::parse(&status).unwrap_or(Status::Disabled),
                created_at: row.get(3)?,
                expires_at: row.get(4)?,
                last_seen_at: row.get(5)?,
                reauth_at: row.get(6)?,
            })
        },
    )
    .optional()
    .map_err(RepoError::from)
}

/// Records a use of the session at `now` (sliding idle expiry), and of its
/// user. Returns whether the session exists.
///
/// # Errors
///
/// The update failed.
pub fn touch(conn: &Connection, id_hash: &TokenHash, now: i64) -> Result<bool> {
    let user_id: Option<String> = conn
        .query_row(
            "UPDATE sessions SET last_seen_at = MAX(last_seen_at, ?2) WHERE id_hash = ?1 \
             RETURNING user_id",
            params![id_hash.as_slice(), now],
            |row| row.get(0),
        )
        .optional()?;
    let Some(user_id) = user_id else {
        return Ok(false);
    };
    conn.execute(
        "UPDATE users SET last_seen_at = ?2 WHERE id = ?1 \
         AND (last_seen_at IS NULL OR last_seen_at < ?2)",
        params![user_id, now],
    )?;
    Ok(true)
}

/// Records a proof of identity at `now` (the re-authentication seam, P1-13).
/// Returns whether the session exists.
///
/// # Errors
///
/// The update failed.
pub fn set_reauth(conn: &Connection, id_hash: &TokenHash, now: i64) -> Result<bool> {
    let changed = conn.execute(
        "UPDATE sessions SET reauth_at = ?2 WHERE id_hash = ?1",
        params![id_hash.as_slice(), now],
    )?;
    Ok(changed > 0)
}

/// Deletes one session (sign-out, rotation). Returns its user when it existed.
///
/// # Errors
///
/// The delete failed.
pub fn delete(conn: &Connection, id_hash: &TokenHash) -> Result<Option<String>> {
    conn.query_row(
        "DELETE FROM sessions WHERE id_hash = ?1 RETURNING user_id",
        [id_hash.as_slice()],
        |row| row.get(0),
    )
    .optional()
    .map_err(RepoError::from)
}

/// Deletes every session of `user_id` (sign-out everywhere); returns how many.
///
/// # Errors
///
/// The delete failed.
pub fn delete_for_user(conn: &Connection, user_id: &str) -> Result<usize> {
    Ok(conn.execute("DELETE FROM sessions WHERE user_id = ?1", [user_id])?)
}

/// One session of a user, for the account's session list (P1-17).
#[derive(Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// SHA-256 of the cookie value: the row's key.
    pub id_hash: TokenHash,
    /// Sign-in time, unix ms.
    pub created_at: i64,
    /// Absolute expiry, unix ms.
    pub expires_at: i64,
    /// Last use, unix ms.
    pub last_seen_at: i64,
    /// The browser's `User-Agent` at sign-in, truncated.
    pub user_agent: Option<String>,
}

impl std::fmt::Debug for SessionRow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionRow")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .field("last_seen_at", &self.last_seen_at)
            .finish_non_exhaustive()
    }
}

/// Every stored session of `user_id`, usable or not, most recently used
/// first.
///
/// # Errors
///
/// The query failed.
pub fn list_for_user(conn: &Connection, user_id: &str) -> Result<Vec<SessionRow>> {
    let mut statement = conn.prepare_cached(
        "SELECT id_hash, created_at, expires_at, last_seen_at, user_agent FROM sessions \
         WHERE user_id = ?1 ORDER BY last_seen_at DESC, created_at DESC",
    )?;
    let rows = statement
        .query_map([user_id], |row| {
            let id_hash: Vec<u8> = row.get(0)?;
            Ok(SessionRow {
                // Keys are SHA-256 digests; anything else never matches a cookie.
                id_hash: id_hash.try_into().unwrap_or([0; 32]),
                created_at: row.get(1)?,
                expires_at: row.get(2)?,
                last_seen_at: row.get(3)?,
                user_agent: row.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Deletes session `id_hash` if it belongs to `user_id`; returns whether it
/// did.
///
/// # Errors
///
/// The delete failed.
pub fn delete_of_user(conn: &Connection, user_id: &str, id_hash: &TokenHash) -> Result<bool> {
    let deleted = conn.execute(
        "DELETE FROM sessions WHERE id_hash = ?1 AND user_id = ?2",
        params![id_hash.as_slice(), user_id],
    )?;
    Ok(deleted > 0)
}

/// Deletes the sessions that expired by `now`, absolutely or after `idle_ms`
/// without use; returns how many.
///
/// # Errors
///
/// The delete failed.
pub fn prune(conn: &Connection, now: i64, idle_ms: i64) -> Result<usize> {
    Ok(conn.execute(
        "DELETE FROM sessions WHERE expires_at <= ?1 OR last_seen_at <= ?1 - ?2",
        params![now, idle_ms],
    )?)
}

/// `user_agent` cut to [`USER_AGENT_MAX_BYTES`] on a character boundary.
fn truncate_user_agent(user_agent: &str) -> &str {
    if user_agent.len() <= USER_AGENT_MAX_BYTES {
        return user_agent;
    }
    let mut end = USER_AGENT_MAX_BYTES;
    while !user_agent.is_char_boundary(end) {
        end -= 1;
    }
    &user_agent[..end]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};
    use crate::tokens::SecretToken;

    const DAY: i64 = 86_400_000;
    const IDLE: i64 = 30 * DAY;

    #[test]
    fn a_session_round_trips_and_slides() {
        let (db, owner, _member) = control_with_users();
        let hash = SecretToken::generate().hash();
        db.write(|tx| {
            insert(
                tx,
                &NewSession {
                    id_hash: &hash,
                    user_id: &owner,
                    expires_at: NOW + 90 * DAY,
                    user_agent: Some("Mozilla/5.0"),
                },
                NOW,
            )
        })
        .unwrap();
        let session = db.read(|conn| find(conn, &hash)).unwrap().unwrap();
        assert_eq!(session.user_id, owner);
        assert_eq!(session.role, Role::Owner);
        assert_eq!(
            (session.created_at, session.last_seen_at, session.reauth_at),
            (NOW, NOW, Some(NOW))
        );
        assert!(session.is_usable(NOW + IDLE - 1, IDLE));
        assert!(!session.is_usable(NOW + IDLE, IDLE), "idle for 30 days");

        // A use moves the idle expiry, never the absolute one.
        let later = NOW + 20 * DAY;
        assert!(db.write(|tx| touch(tx, &hash, later)).unwrap());
        let session = db.read(|conn| find(conn, &hash)).unwrap().unwrap();
        assert_eq!(session.last_seen_at, later);
        assert!(session.is_usable(NOW + 40 * DAY, IDLE));
        assert!(!session.is_usable(NOW + 90 * DAY, IDLE), "absolute expiry");
        // A late, out-of-order touch never moves it back.
        db.write(|tx| touch(tx, &hash, NOW)).unwrap();
        let session = db.read(|conn| find(conn, &hash)).unwrap().unwrap();
        assert_eq!(session.last_seen_at, later);
        let user_seen: i64 = db
            .read(|conn| {
                conn.query_row(
                    "SELECT last_seen_at FROM users WHERE id = ?1",
                    [&owner],
                    |row| row.get(0),
                )
                .map_err(RepoError::from)
            })
            .unwrap();
        assert_eq!(user_seen, later);

        assert!(db.write(|tx| set_reauth(tx, &hash, later)).unwrap());
        let session = db.read(|conn| find(conn, &hash)).unwrap().unwrap();
        assert_eq!(session.reauth_at, Some(later));

        assert_eq!(db.write(|tx| delete(tx, &hash)).unwrap(), Some(owner));
        assert_eq!(db.write(|tx| delete(tx, &hash)).unwrap(), None);
        assert!(!db.write(|tx| touch(tx, &hash, later)).unwrap());
        assert_eq!(db.read(|conn| find(conn, &hash)).unwrap(), None);
    }

    #[test]
    fn disabled_users_have_no_usable_session() {
        let (db, _owner, member) = control_with_users();
        let hash = SecretToken::generate().hash();
        db.write(|tx| {
            insert(
                tx,
                &NewSession {
                    id_hash: &hash,
                    user_id: &member,
                    expires_at: NOW + DAY,
                    user_agent: None,
                },
                NOW,
            )?;
            tx.execute(
                "UPDATE users SET status = 'disabled' WHERE id = ?1",
                [&member],
            )?;
            Ok::<_, RepoError>(())
        })
        .unwrap();
        let session = db.read(|conn| find(conn, &hash)).unwrap().unwrap();
        assert_eq!(session.user_status, Status::Disabled);
        assert!(!session.is_usable(NOW, IDLE));
    }

    #[test]
    fn sign_out_everywhere_and_pruning() {
        let (db, owner, member) = control_with_users();
        let hashes: Vec<TokenHash> = (0..4).map(|_| SecretToken::generate().hash()).collect();
        db.write(|tx| {
            // owner: fresh, idle for too long, past its absolute expiry; member: fresh.
            for (hash, user, created, expires) in [
                (&hashes[0], &owner, NOW, NOW + 90 * DAY),
                (&hashes[1], &owner, NOW - IDLE - 1, NOW + 60 * DAY),
                (&hashes[2], &owner, NOW - 90 * DAY, NOW),
                (&hashes[3], &member, NOW, NOW + 90 * DAY),
            ] {
                let session = NewSession {
                    id_hash: hash,
                    user_id: user,
                    expires_at: expires,
                    user_agent: None,
                };
                insert(tx, &session, created)?;
            }
            Ok::<_, RepoError>(())
        })
        .unwrap();
        assert_eq!(db.write(|tx| prune(tx, NOW, IDLE)).unwrap(), 2);
        assert!(db.read(|conn| find(conn, &hashes[0])).unwrap().is_some());
        assert!(db.read(|conn| find(conn, &hashes[1])).unwrap().is_none());
        assert!(db.read(|conn| find(conn, &hashes[2])).unwrap().is_none());

        assert_eq!(db.write(|tx| delete_for_user(tx, &owner)).unwrap(), 1);
        assert!(db.read(|conn| find(conn, &hashes[0])).unwrap().is_none());
        assert!(
            db.read(|conn| find(conn, &hashes[3])).unwrap().is_some(),
            "other users keep their sessions"
        );
    }

    #[test]
    fn a_user_lists_and_deletes_their_own_sessions() {
        let (db, owner, member) = control_with_users();
        let hashes: Vec<TokenHash> = (0..3).map(|_| SecretToken::generate().hash()).collect();
        db.write(|tx| {
            for (hash, user, at, agent) in [
                (&hashes[0], &owner, NOW, Some("Firefox")),
                (&hashes[1], &owner, NOW + 5, None),
                (&hashes[2], &member, NOW, None),
            ] {
                let session = NewSession {
                    id_hash: hash,
                    user_id: user,
                    expires_at: at + 90 * DAY,
                    user_agent: agent,
                };
                insert(tx, &session, at)?;
            }
            Ok::<_, RepoError>(())
        })
        .unwrap();
        let rows = db.read(|conn| list_for_user(conn, &owner)).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id_hash, hashes[1], "most recently used first");
        assert_eq!(rows[1].user_agent.as_deref(), Some("Firefox"));
        assert_eq!(
            (rows[1].created_at, rows[1].last_seen_at, rows[1].expires_at),
            (NOW, NOW, NOW + 90 * DAY)
        );
        assert!(!format!("{:?}", rows[0]).contains("id_hash"));

        assert!(
            !db.write(|tx| delete_of_user(tx, &owner, &hashes[2]))
                .unwrap(),
            "another user's session"
        );
        assert!(
            db.write(|tx| delete_of_user(tx, &owner, &hashes[0]))
                .unwrap()
        );
        assert!(
            !db.write(|tx| delete_of_user(tx, &owner, &hashes[0]))
                .unwrap()
        );
        assert_eq!(
            db.read(|conn| list_for_user(conn, &owner)).unwrap().len(),
            1
        );
        assert_eq!(
            db.read(|conn| list_for_user(conn, &member)).unwrap().len(),
            1
        );
    }

    #[test]
    fn user_agents_are_truncated_on_a_char_boundary() {
        assert_eq!(truncate_user_agent("short"), "short");
        let long = "é".repeat(200); // 400 bytes
        let cut = truncate_user_agent(&long);
        assert!(cut.len() <= USER_AGENT_MAX_BYTES);
        assert_eq!(cut.len(), USER_AGENT_MAX_BYTES);
        assert!(cut.chars().all(|c| c == 'é'));
    }
}
