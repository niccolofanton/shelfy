//! `idempotency` (plan §2.6, §2.9): the first response to each request that
//! carried an `Idempotency-Key`, kept 24 hours, so a repeat gets it back
//! instead of acting twice ([`crate::jobs::idempotency`]).
//!
//! A row is reserved (status [`PENDING`]) before the request runs and
//! completed with its response afterwards; a request that fails with a
//! server error releases it. `body` is opaque here: the middleware stores
//! the request's fingerprint and the response in it.

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::Result;

/// `status` of a reserved row whose request is still running.
pub const PENDING: u16 = 0;

/// A stored row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    /// The response status, or [`PENDING`].
    pub status: u16,
    /// What the middleware stored.
    pub body: Vec<u8>,
}

/// What [`reserve`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reservation {
    /// No usable row existed: one is now reserved for this request.
    Reserved,
    /// The key is taken: by a finished request (its response), or by one
    /// still running ([`PENDING`]).
    Taken(Stored),
}

/// Reserves `key` of `user_id` for a request, storing `body` with it.
///
/// A row created before `expired_before` is gone (the 24-hour window), and a
/// reservation made before `stale_before` belongs to a request that died
/// (a crash): both are replaced.
///
/// # Errors
///
/// The query failed.
pub fn reserve(
    conn: &Connection,
    user_id: &str,
    key: &str,
    body: &[u8],
    now: i64,
    expired_before: i64,
    stale_before: i64,
) -> Result<Reservation> {
    let existing = conn
        .query_row(
            "SELECT status, body, created_at FROM idempotency WHERE user_id = ?1 AND key = ?2",
            params![user_id, key],
            |row| {
                Ok((
                    row.get::<_, u16>(0)?,
                    row.get::<_, Option<Vec<u8>>>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((status, body, created_at)) = existing {
        let usable =
            created_at >= expired_before && (status != PENDING || created_at >= stale_before);
        if usable {
            return Ok(Reservation::Taken(Stored {
                status,
                body: body.unwrap_or_default(),
            }));
        }
    }
    conn.execute(
        "INSERT INTO idempotency (user_id, key, status, body, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5) \
         ON CONFLICT (user_id, key) DO UPDATE SET status = excluded.status, \
         body = excluded.body, created_at = excluded.created_at",
        params![user_id, key, PENDING, body, now],
    )?;
    Ok(Reservation::Reserved)
}

/// Stores the response of the request that reserved `key`.
///
/// # Errors
///
/// The query failed.
pub fn complete(
    conn: &Connection,
    user_id: &str,
    key: &str,
    status: u16,
    body: &[u8],
) -> Result<()> {
    conn.execute(
        "UPDATE idempotency SET status = ?3, body = ?4 \
         WHERE user_id = ?1 AND key = ?2 AND status = ?5",
        params![user_id, key, status, body, PENDING],
    )?;
    Ok(())
}

/// Drops the reservation of `key`, so the request can be sent again.
///
/// # Errors
///
/// The query failed.
pub fn release(conn: &Connection, user_id: &str, key: &str) -> Result<()> {
    conn.execute(
        "DELETE FROM idempotency WHERE user_id = ?1 AND key = ?2 AND status = ?3",
        params![user_id, key, PENDING],
    )?;
    Ok(())
}

/// Deletes the rows created before `before`; returns how many.
///
/// # Errors
///
/// The query failed.
pub fn prune(conn: &Connection, before: i64) -> Result<u64> {
    let deleted = conn.execute("DELETE FROM idempotency WHERE created_at < ?1", [before])?;
    Ok(deleted as u64)
}

#[cfg(test)]
mod tests {
    use shelfy_core::repo::RepoError;

    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    const DAY: i64 = 86_400_000;

    #[test]
    fn a_key_is_reserved_once_then_replays_its_response() {
        let (db, owner, member) = control_with_users();
        db.write(|tx| {
            let reserve_at =
                |user: &str, now: i64| reserve(tx, user, "k1", b"fp", now, now - DAY, now - 60_000);
            assert_eq!(reserve_at(&owner, NOW)?, Reservation::Reserved);
            assert_eq!(
                reserve_at(&owner, NOW + 1)?,
                Reservation::Taken(Stored {
                    status: PENDING,
                    body: b"fp".to_vec()
                }),
                "still running"
            );
            // Keys are per user.
            assert_eq!(reserve_at(&member, NOW)?, Reservation::Reserved);

            complete(tx, &owner, "k1", 200, b"response")?;
            let done = Reservation::Taken(Stored {
                status: 200,
                body: b"response".to_vec(),
            });
            assert_eq!(reserve_at(&owner, NOW + 2)?, done);
            // A completed row is never overwritten by a late completion.
            complete(tx, &owner, "k1", 500, b"late")?;
            assert_eq!(reserve_at(&owner, NOW + 3)?, done);
            // After 24 hours the key is new again.
            assert_eq!(reserve_at(&owner, NOW + DAY + 1)?, Reservation::Reserved);
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }

    #[test]
    fn stale_and_released_reservations_free_the_key() {
        let (db, owner, _) = control_with_users();
        db.write(|tx| {
            let reserve_at = |now: i64| reserve(tx, &owner, "k", b"", now, now - DAY, now - 60_000);
            assert_eq!(reserve_at(NOW)?, Reservation::Reserved);
            // The request died without completing: after a minute the key is free.
            assert!(matches!(reserve_at(NOW + 59_000)?, Reservation::Taken(_)));
            assert_eq!(reserve_at(NOW + 61_000)?, Reservation::Reserved);
            release(tx, &owner, "k")?;
            assert_eq!(reserve_at(NOW + 61_001)?, Reservation::Reserved);

            complete(tx, &owner, "k", 201, b"r")?;
            release(tx, &owner, "k")?;
            assert!(
                matches!(
                    reserve_at(NOW + 61_002)?,
                    Reservation::Taken(Stored { status: 201, .. })
                ),
                "release keeps a completed row"
            );
            assert_eq!(prune(tx, NOW + 61_001)?, 0);
            assert_eq!(prune(tx, NOW + 61_002)?, 1);
            Ok::<_, RepoError>(())
        })
        .unwrap();
    }
}
