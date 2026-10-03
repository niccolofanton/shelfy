//! `usage_daily` (plan §2.6): what each user did on each UTC day, for daily
//! limits and for the operator.
//!
//! One row per user and day (`YYYY-MM-DD`, UTC), created by the first
//! [`bump`] of the day. The counters this phase keeps ([`Field`]):
//!
//! | Field | Counts | Bumped by |
//! |---|---|---|
//! | `captures` | site captures that succeeded | the capture job (P4-14), which also checks `users.capture_daily_limit` against it |
//! | `ingest_items` | items an ingest batch accepted | the ingest service (P2) |
//! | `bytes_in` | bytes stored in the user's media: each commit of a quota reservation ([`crate::quota`]) | [`crate::quota::Reservation::commit`] |
//! | `ai_calls`, `ai_in_tokens`, `ai_out_tokens` | AI calls and the tokens providers reported | the AI service ([`crate::ai`]), through [`record_ai`] |

use rusqlite::{Connection, OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, Result};

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// A counter of `usage_daily`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Field {
    /// Site captures.
    Captures,
    /// Items accepted by ingest.
    IngestItems,
    /// Bytes stored.
    BytesIn,
    /// AI calls (P3).
    AiCalls,
    /// Prompt tokens of AI calls (P3).
    AiInTokens,
    /// Answer tokens of AI calls (P3).
    AiOutTokens,
}

impl Field {
    /// The column.
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Captures => "captures",
            Self::IngestItems => "ingest_items",
            Self::BytesIn => "bytes_in",
            Self::AiCalls => "ai_calls",
            Self::AiInTokens => "ai_in_tokens",
            Self::AiOutTokens => "ai_out_tokens",
        }
    }
}

/// One user's counters of one day.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Daily {
    /// Site captures.
    pub captures: i64,
    /// Items accepted by ingest.
    pub ingest_items: i64,
    /// Bytes stored.
    pub bytes_in: i64,
    /// AI calls.
    pub ai_calls: i64,
    /// Prompt tokens of AI calls.
    pub ai_in_tokens: i64,
    /// Answer tokens of AI calls.
    pub ai_out_tokens: i64,
}

/// One day's AI usage, for `GET /me/usage/ai` (P3-09).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AiDay {
    /// The UTC day, `YYYY-MM-DD`.
    pub day: String,
    /// AI calls that day.
    pub calls: i64,
    /// Prompt tokens.
    pub in_tokens: i64,
    /// Answer tokens.
    pub out_tokens: i64,
}

/// The UTC day of `unix_ms`, as stored: `YYYY-MM-DD`.
#[must_use]
pub fn day_of(unix_ms: i64) -> String {
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = unix_ms.div_euclid(DAY_MS) + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

/// Adds `n` to `field` of `user_id`'s row for the day of `now` (creating
/// it). `n` may be 0, which only creates the row; a negative `n` is refused.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a negative `n`; database errors.
pub fn bump(conn: &Connection, user_id: &str, field: Field, n: i64, now: i64) -> Result<()> {
    if n < 0 {
        return Err(RepoError::Invalid {
            field: "n",
            reason: "a daily counter only grows",
        });
    }
    let column = field.column();
    conn.prepare_cached(&format!(
        "INSERT INTO usage_daily (user_id, day, {column}) VALUES (?1, ?2, ?3)
         ON CONFLICT (user_id, day) DO UPDATE SET {column} = {column} + excluded.{column}"
    ))?
    .execute(params![user_id, day_of(now), n])?;
    Ok(())
}

/// The counters of `user_id` for the day of `now`; zeros when the day has
/// no row yet.
///
/// # Errors
///
/// The query failed.
pub fn of_day(conn: &Connection, user_id: &str, now: i64) -> Result<Daily> {
    Ok(conn
        .prepare_cached(
            "SELECT captures, ingest_items, bytes_in, ai_calls, ai_in_tokens, ai_out_tokens \
             FROM usage_daily WHERE user_id = ?1 AND day = ?2",
        )?
        .query_row(params![user_id, day_of(now)], |row| {
            Ok(Daily {
                captures: row.get(0)?,
                ingest_items: row.get(1)?,
                bytes_in: row.get(2)?,
                ai_calls: row.get(3)?,
                ai_in_tokens: row.get(4)?,
                ai_out_tokens: row.get(5)?,
            })
        })
        .optional()?
        .unwrap_or_default())
}

/// Adds one AI call and its tokens to `user_id`'s row for the day of `now`
/// (creating it), in one statement. Token counts may be 0 when the provider
/// reported none. Negative counts are refused.
///
/// # Errors
///
/// [`RepoError::Invalid`] for a negative count; database errors.
pub fn record_ai(
    conn: &Connection,
    user_id: &str,
    in_tokens: i64,
    out_tokens: i64,
    now: i64,
) -> Result<()> {
    if in_tokens < 0 || out_tokens < 0 {
        return Err(RepoError::Invalid {
            field: "tokens",
            reason: "a daily counter only grows",
        });
    }
    conn.prepare_cached(
        "INSERT INTO usage_daily (user_id, day, ai_calls, ai_in_tokens, ai_out_tokens) \
         VALUES (?1, ?2, 1, ?3, ?4) \
         ON CONFLICT (user_id, day) DO UPDATE SET \
         ai_calls = ai_calls + 1, \
         ai_in_tokens = ai_in_tokens + excluded.ai_in_tokens, \
         ai_out_tokens = ai_out_tokens + excluded.ai_out_tokens",
    )?
    .execute(params![user_id, day_of(now), in_tokens, out_tokens])?;
    Ok(())
}

/// The AI usage of `user_id` over the last `days` days up to the day of `now`,
/// newest day first, skipping days with no row. `days` is clamped to 1..=365.
///
/// # Errors
///
/// The query failed.
pub fn ai_recent(conn: &Connection, user_id: &str, days: u32, now: i64) -> Result<Vec<AiDay>> {
    let days = i64::from(days.clamp(1, 365));
    let since = day_of(now - (days - 1) * DAY_MS);
    let mut statement = conn.prepare_cached(
        "SELECT day, ai_calls, ai_in_tokens, ai_out_tokens FROM usage_daily \
         WHERE user_id = ?1 AND day >= ?2 AND day <= ?3 \
         AND (ai_calls > 0 OR ai_in_tokens > 0 OR ai_out_tokens > 0) \
         ORDER BY day DESC",
    )?;
    let rows = statement
        .query_map(params![user_id, since, day_of(now)], |row| {
            Ok(AiDay {
                day: row.get(0)?,
                calls: row.get(1)?,
                in_tokens: row.get(2)?,
                out_tokens: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::testing::{NOW, control_with_users};

    #[test]
    fn days_are_utc_dates() {
        assert_eq!(day_of(0), "1970-01-01");
        assert_eq!(day_of(NOW), "2026-10-02");
        assert_eq!(
            day_of(NOW - 1),
            "2026-10-01",
            "the last ms of the day before"
        );
        assert_eq!(day_of(NOW + DAY_MS - 1), "2026-10-02");
        assert_eq!(day_of(951_782_400_000), "2000-02-29");
        assert_eq!(day_of(-1), "1969-12-31");
    }

    #[test]
    fn counters_grow_per_user_and_day() {
        let (db, owner, member) = control_with_users();
        let add = |user: &str, field, n, at| db.write(|tx| bump(tx, user, field, n, at));
        add(&owner, Field::Captures, 1, NOW).unwrap();
        add(&owner, Field::Captures, 2, NOW + 3_600_000).unwrap();
        add(&owner, Field::BytesIn, 4_096, NOW).unwrap();
        add(&owner, Field::IngestItems, 50, NOW).unwrap();
        add(&owner, Field::Captures, 1, NOW + DAY_MS).unwrap();
        add(&member, Field::BytesIn, 7, NOW).unwrap();

        let read = |user: &str, at| db.read(|conn| of_day(conn, user, at)).unwrap();
        assert_eq!(
            read(&owner, NOW),
            Daily {
                captures: 3,
                ingest_items: 50,
                bytes_in: 4_096,
                ..Daily::default()
            }
        );
        assert_eq!(
            read(&owner, NOW + DAY_MS),
            Daily {
                captures: 1,
                ..Daily::default()
            }
        );
        assert_eq!(read(&member, NOW).bytes_in, 7);
        assert_eq!(read(&member, NOW + DAY_MS), Daily::default(), "no row");

        let err = add(&owner, Field::Captures, -1, NOW).unwrap_err();
        assert!(matches!(err, RepoError::Invalid { .. }), "{err}");
        assert_eq!(read(&owner, NOW).captures, 3);
    }

    #[test]
    fn ai_calls_and_tokens_accumulate_and_list_by_day() {
        let (db, owner, _member) = control_with_users();
        let record = |in_t, out_t, at| db.write(|tx| record_ai(tx, &owner, in_t, out_t, at));
        record(1_200, 300, NOW).unwrap();
        record(900, 150, NOW + 3_600_000).unwrap();
        record(1_000, 200, NOW + DAY_MS).unwrap();

        let today = db.read(|conn| of_day(conn, &owner, NOW)).unwrap();
        assert_eq!(today.ai_calls, 2);
        assert_eq!(today.ai_in_tokens, 2_100);
        assert_eq!(today.ai_out_tokens, 450);

        // Two days have AI rows; newest first, and days without AI are skipped.
        let rows = db
            .read(|conn| ai_recent(conn, &owner, 7, NOW + DAY_MS))
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].day, day_of(NOW + DAY_MS));
        assert_eq!(
            (rows[0].calls, rows[0].in_tokens, rows[0].out_tokens),
            (1, 1_000, 200)
        );
        assert_eq!(rows[1].calls, 2);

        // A one-day window shows only today.
        let one = db.read(|conn| ai_recent(conn, &owner, 1, NOW)).unwrap();
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].day, day_of(NOW));

        // A capture-only day never shows up as AI usage.
        db.write(|tx| bump(tx, &owner, Field::Captures, 1, NOW + 2 * DAY_MS))
            .unwrap();
        let rows = db
            .read(|conn| ai_recent(conn, &owner, 7, NOW + 2 * DAY_MS))
            .unwrap();
        assert!(rows.iter().all(|r| r.day != day_of(NOW + 2 * DAY_MS)));

        let err = db
            .write(|tx| record_ai(tx, &owner, -1, 0, NOW))
            .unwrap_err();
        assert!(matches!(err, RepoError::Invalid { .. }), "{err}");
    }
}
