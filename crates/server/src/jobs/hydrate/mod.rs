//! `link.hydrate` (plan §2.12, §2.17; P2-11, L17): fills in a post that a
//! shared link created (`POST /links`) from the platform's public,
//! unauthenticated endpoints.
//!
//! | Platform | Routes, in order (SPIKE-9) | Module |
//! |---|---|---|
//! | Instagram | post page, then logged-out GraphQL; the extension (`hydrate_link`) for gated posts and while `instagram_web`'s breaker is open | [`instagram`] |
//! | X | `tweet-result`, then oEmbed | [`x`] |
//! | Pinterest | `PinResource`, then pidgets | [`pinterest`] |
//!
//! Requests follow [`fetch`]: one hydration at a time per host group, its
//! pace (1 per 3 s on Instagram, 1 per second elsewhere), its breaker, and
//! SPIKE-9's stop rule (the first block signal trips the breaker).
//! [`short_link`] resolves `pin.it` links for the route.
//!
//! **One job per link**: `{postKey}`, dedupe key `link.hydrate:<key>`; 2 at
//! once overall, 1 per user, 5 tries, a 2-minute lease (§2.12). A job finds
//! the post first: a post that is gone or already has media (a sync brought
//! them) needs nothing. Then, by verdict:
//!
//! | Verdict | The post | The job |
//! |---|---|---|
//! | data | merged like a capture item: [`sanitize_batch`], then [`upsert_batch`], then the archive state (`pending` with something to fetch) | succeeds; `archive.drain` is P2-10's to enqueue (the seam in [`after_merge`]) |
//! | gone (404, tombstone, deleted) | `failed` (C10) | fails, `not_found` |
//! | gated (Instagram) | `client`: the extension's `hydrate_link` (P2-14, P2-17) | succeeds |
//! | gated (X, Pinterest: no extension path) | `failed` | fails, `link_gated` |
//! | breaker open or tripped | Instagram: `client` while the breaker is open | requeued, without using a try, for when the breaker lets a probe through |
//! | no answer (5xx, timeouts, endpoints that changed) | unchanged; `failed` after the last try (out of tries, C10) | retried with the backoff of §2.12 (`unavailable`) |
//!
//! The verdicts `client` and `failed` stay until media arrive
//! ([`archive::state`] rule 3): a sync that brings the post, a share of the
//! same link again (it enqueues a new hydration), or the extension.
//!
//! [`sanitize_batch`]: shelfy_core::ingest::sanitize::sanitize_batch
//! [`upsert_batch`]: shelfy_core::ingest::merge::upsert_batch
//! [`archive::state`]: shelfy_core::ingest::archive::state

pub mod fetch;
pub mod instagram;
pub mod pinterest;
pub mod short_link;
pub mod x;

use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use shelfy_core::ingest::archive::{
    self, ArchiveModes, ArchivePolicy, ArchiveState, Scope, StateCounts,
};
use shelfy_core::ingest::merge::{UpsertOptions, upsert_batch};
use shelfy_core::ingest::sanitize::{MAX_POSTED_AT_AHEAD_MS, MIN_POSTED_AT_MS, sanitize_batch};
use shelfy_core::repo::{Platform, RepoError};
use tokio::time::Instant;

use self::fetch::Stop;
use super::{Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec, NewJob, Outcome};
use crate::error::ApiError;
use crate::events::model::ChangeReason;
use crate::ids::now_ms;
use crate::library;
use crate::state::AppState;

/// The kind's name.
pub const KIND: &str = "link.hydrate";

/// The job error of a post that is gone.
pub const GONE: &str = "not_found";
/// The job error of an X or Pinterest post that only signed-in users see.
pub const GATED: &str = "link_gated";
/// The job error of a hydration that got no answer.
pub const UNAVAILABLE: &str = "unavailable";

/// The kind, for the registry ([`super::kinds::registry`]): §2.12's row.
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(2)
            .per_user(1)
            .max_attempts(5)
            .lease(Duration::from_secs(120)),
        run,
    )
}

/// A job's payload.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    /// The post to hydrate.
    pub post_key: String,
}

/// Enqueues the hydration of `post_key` for `user_id`; an active one for
/// the same post is returned instead.
///
/// # Errors
///
/// The control database failed.
pub async fn enqueue(jobs: &Jobs, user_id: &str, post_key: &str) -> Result<Enqueued, ApiError> {
    let payload = serde_json::to_value(Payload {
        post_key: post_key.to_owned(),
    })
    .map_err(ApiError::internal)?;
    jobs.enqueue(
        NewJob::new(user_id, KIND)
            .payload(payload)
            .dedupe(format!("{KIND}:{post_key}")),
    )
    .await
}

/// The archive modes the state rule uses: the archive's current ones
/// (`SHELFY_ARCHIVE_MODE_<PLATFORM>`, with a platform whose CDN breaker is
/// open handed to the extension; P2-10).
#[must_use]
pub fn archive_modes(state: &AppState) -> ArchiveModes {
    super::archive::modes(state)
}

/// The post a hydration works on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// Internal id.
    pub id: i64,
    /// Public key.
    pub key: String,
    /// Platform.
    pub platform: Platform,
    /// Native id: the pk, the tweet id, the pin id.
    pub native_id: String,
    /// Instagram's code.
    pub shortcode: Option<String>,
    /// The link to the post.
    pub post_url: String,
}

/// What a platform's endpoints said about a post.
#[derive(Debug)]
pub enum Verdict {
    /// Its data.
    Found(Found),
    /// Deleted, or never existed.
    Gone,
    /// Only signed-in users see it (private, age-restricted).
    Gated,
    /// No route answered: the code of the last failure.
    Failed(&'static str),
}

/// A post's data, as an extension item (the input of `sanitize_batch`), and
/// its publication time, which the endpoints give in their own formats.
#[derive(Debug)]
pub struct Found {
    /// The item: `id`, `postUrl`, `profileUrl`, `authorUsername`,
    /// `authorName`, `text`, `mediaType`, `thumbnailUrl`, `media`.
    pub item: Value,
    /// The publication time, unix ms.
    pub posted_at: Option<i64>,
}

/// A post's row as [`target`] reads it: id, platform, native id, code, link.
type TargetRow = (i64, Platform, String, Option<String>, Option<String>);

/// Whether a post needs hydration, and what to hydrate it with.
fn target(conn: &Connection, key: &str) -> Result<Option<Target>, RepoError> {
    let row: Option<TargetRow> = conn
        .prepare_cached(
            "SELECT id, platform, native_id, shortcode, post_url FROM posts WHERE key = ?1",
        )?
        .query_row([key], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
        })
        .optional()?;
    let Some((id, platform, native_id, shortcode, post_url)) = row else {
        return Ok(None);
    };
    let facts = archive::load(conn, Scope::Posts(&[id]))?;
    let needs = facts
        .first()
        .is_some_and(|(_, post)| archive::needs_hydration(post));
    if !needs {
        return Ok(None);
    }
    let post_url = post_url.unwrap_or_else(|| canonical_url(platform, &native_id));
    Ok(Some(Target {
        id,
        key: key.to_owned(),
        platform,
        native_id,
        shortcode,
        post_url,
    }))
}

/// A post's link from its native id, for a post saved without one.
#[must_use]
pub fn canonical_url(platform: Platform, native_id: &str) -> String {
    match platform {
        Platform::Instagram => format!(
            "https://www.instagram.com/p/{}/",
            shelfy_core::ids::ig::MediaPk::parse_decimal(native_id)
                .map(|pk| pk.to_shortcode())
                .unwrap_or_default()
        ),
        Platform::Twitter => format!("https://x.com/i/status/{native_id}"),
        Platform::Pinterest => format!("https://www.pinterest.com/pin/{native_id}/"),
        Platform::Web | Platform::Manual => String::new(),
    }
}

/// Whether the post `key` still waits for its hydration: it exists, it is
/// a social post without media, and it is not handed to the extension
/// (`client`). `POST /links` re-enqueues such a post.
///
/// # Errors
///
/// Database errors.
pub fn waits_for_server(conn: &Connection, key: &str) -> Result<bool, RepoError> {
    let Some(target) = target(conn, key)? else {
        return Ok(false);
    };
    let state: ArchiveState = conn
        .prepare_cached("SELECT archive_state FROM posts WHERE id = ?1")?
        .query_row([target.id], |r| r.get(0))?;
    Ok(state != ArchiveState::Client)
}

async fn run(ctx: JobContext) -> JobResult {
    let Payload { post_key } = ctx.payload_as()?;
    let key = post_key.clone();
    let Some(target) = ctx
        .user_db(move |db| db.read(|conn| target(conn, &key)).map_err(JobError::from))
        .await?
    else {
        return Ok(Outcome::Succeeded);
    };
    ctx.progress(Some(0.0), Some("fetch")).await;
    let outbound = ctx.state().outbound().clone();
    let verdict = match target.platform {
        Platform::Instagram => instagram::hydrate(&outbound, &target).await,
        Platform::Twitter => x::hydrate(&outbound, &target).await,
        Platform::Pinterest => pinterest::hydrate(&outbound, &target).await,
        Platform::Web | Platform::Manual => return Ok(Outcome::Succeeded),
    };
    if ctx.is_cancelled() {
        return Err(JobError::cancelled());
    }
    let instagram = target.platform == Platform::Instagram;
    match verdict {
        Ok(Verdict::Found(found)) => {
            merge(&ctx, &target, found).await?;
            Ok(Outcome::Succeeded)
        }
        Ok(Verdict::Gone) => {
            set_state(&ctx, &target, ArchiveState::Failed).await?;
            Err(JobError::permanent(GONE))
        }
        Ok(Verdict::Gated) if instagram => {
            set_state(&ctx, &target, ArchiveState::Client).await?;
            Ok(Outcome::Succeeded)
        }
        Ok(Verdict::Gated) => {
            set_state(&ctx, &target, ArchiveState::Failed).await?;
            Err(JobError::permanent(GATED))
        }
        Ok(Verdict::Failed(code)) => {
            if ctx.attempt() >= ctx.max_attempts() {
                set_state(&ctx, &target, ArchiveState::Failed).await?;
            }
            Err(JobError::transient(UNAVAILABLE).with_detail(code))
        }
        Err(stop) => {
            let until = match stop {
                Stop::BreakerOpen(until) => until,
                Stop::Blocked(_) => fetch::reopens(&outbound, group_of(target.platform)),
            };
            if instagram {
                // Handed to the extension while the breaker is open (C10).
                set_state(&ctx, &target, ArchiveState::Client).await?;
            }
            let wait = until.saturating_duration_since(Instant::now());
            let wait_ms = i64::try_from(wait.as_millis()).unwrap_or(i64::MAX);
            Ok(Outcome::Requeue {
                run_at: Some(now_ms().saturating_add(wait_ms)),
            })
        }
    }
}

/// The hydration host group of a platform.
const fn group_of(platform: Platform) -> crate::outbound::HostGroup {
    use crate::outbound::HostGroup;
    match platform {
        Platform::Instagram => HostGroup::InstagramWeb,
        Platform::Twitter => HostGroup::XWeb,
        _ => HostGroup::PinterestWeb,
    }
}

/// Merges the data into the post, derives its state, and announces it.
async fn merge(ctx: &JobContext, target: &Target, found: Found) -> Result<(), JobError> {
    let now = now_ms();
    let modes = archive_modes(ctx.state());
    let target_owned = target.clone();
    let counts = ctx
        .user_db(move |db| {
            db.write(|tx| -> Result<Option<StateCounts>, RepoError> {
                let target = target_owned;
                let batch = sanitize_batch(target.platform, &[found.item], now)
                    .map_err(|err| invalid(err.to_string()))?;
                let Some(mut post) = batch.posts.into_iter().next() else {
                    return Err(invalid("the item was rejected".to_owned()));
                };
                if post.key != target.key {
                    return Err(invalid("the item names another post".to_owned()));
                }
                if post.post_url.is_none() {
                    post.post_url = Some(target.post_url.clone());
                }
                if post.shortcode.is_none() {
                    post.shortcode.clone_from(&target.shortcode);
                }
                if let Some(at) = found.posted_at
                    && (MIN_POSTED_AT_MS..=now.saturating_add(MAX_POSTED_AT_AHEAD_MS)).contains(&at)
                {
                    post.posted_at = Some(at);
                }
                let summary = upsert_batch(tx, &[post], UpsertOptions::default(), now)?;
                if !summary.posts.first().is_some_and(|p| p.changed) {
                    return Ok(None);
                }
                let policy = ArchivePolicy::read(tx, modes)?;
                let refreshed =
                    archive::refresh_states(tx, Scope::Posts(&[target.id]), &policy, now)?;
                Ok(Some(refreshed.counts))
            })
            .map_err(JobError::from)
        })
        .await?;
    if let Some(counts) = counts {
        library::announce(
            ctx.state().events(),
            ctx.user_id(),
            ChangeReason::Ingest,
            Some(vec![target.key.clone()]),
        );
        after_merge(ctx, counts).await;
    }
    Ok(())
}

/// After a merge: the archive drain fetches what the post now has (P2-10;
/// should the enqueue fail, the drain's 10-minute sweep re-arms the user).
async fn after_merge(ctx: &JobContext, counts: StateCounts) {
    if counts.server_work() > 0
        && let Err(err) = super::archive::enqueue(ctx.jobs(), ctx.user_id()).await
    {
        tracing::warn!(job_id = ctx.id(), error = %err, "cannot enqueue the archive drain");
    }
}

/// Sets the hydration's verdict on the post, then derives its state again:
/// the rule keeps the verdict while the post has no media, and replaces it
/// when media arrived meanwhile.
async fn set_state(
    ctx: &JobContext,
    target: &Target,
    verdict: ArchiveState,
) -> Result<(), JobError> {
    let now = now_ms();
    let modes = archive_modes(ctx.state());
    let id = target.id;
    let changed = ctx
        .user_db(move |db| {
            db.write(|tx| -> Result<bool, RepoError> {
                let set = tx
                    .prepare_cached(
                        "UPDATE posts SET archive_state = ?2 WHERE id = ?1 AND archive_state IS NOT ?2",
                    )?
                    .execute(params![id, verdict])?;
                let policy = ArchivePolicy::read(tx, modes)?;
                let refreshed = archive::refresh_states(tx, Scope::Posts(&[id]), &policy, now)?;
                Ok(set > 0 || refreshed.changed > 0)
            })
            .map_err(JobError::from)
        })
        .await?;
    if changed {
        library::announce(
            ctx.state().events(),
            ctx.user_id(),
            ChangeReason::Ingest,
            Some(vec![target.key.clone()]),
        );
        if verdict == ArchiveState::Client {
            // The extension's `hydrate_link` (P2-14).
            crate::extension::tasks::wake(ctx.state(), ctx.user_id());
        }
    }
    Ok(())
}

fn invalid(reason: String) -> RepoError {
    tracing::warn!(reason = %reason, "a hydrated item was refused");
    RepoError::Invalid {
        field: "item",
        reason: "the hydrated post does not fit the link",
    }
}

// ── Helpers of the platform modules ─────────────────────────────────────────

/// Depth-first search for the first object accepted by `accept`, within a
/// node budget (SPIKE-9's `findObject`).
#[must_use]
pub fn find_object(
    root: &Value,
    accept: impl Fn(&Map<String, Value>) -> bool,
) -> Option<&Map<String, Value>> {
    let mut stack = vec![root];
    let mut budget = 300_000_u32;
    while let Some(node) = stack.pop() {
        budget = budget.checked_sub(1)?;
        match node {
            Value::Object(map) => {
                if accept(map) {
                    return Some(map);
                }
                stack.extend(map.values().rev().filter(|v| v.is_object() || v.is_array()));
            }
            Value::Array(items) => {
                stack.extend(items.iter().rev().filter(|v| v.is_object() || v.is_array()));
            }
            _ => {}
        }
    }
    None
}

/// A non-empty string member of `value`.
#[must_use]
pub fn str_at<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value[key].as_str().filter(|s| !s.trim().is_empty())
}

/// The HTML entities of oEmbed's markup: the named ones it uses and numeric
/// references.
#[must_use]
pub fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest[1..].find(';').map(|e| e + 1).filter(|e| *e <= 10) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            "mdash" => Some('—'),
            "ndash" => Some('–'),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|hex| u32::from_str_radix(hex, 16).ok())
                .or_else(|| entity.strip_prefix('#').and_then(|d| d.parse().ok()))
                .and_then(char::from_u32),
        };
        match decoded {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];

/// Days from 1970-01-01 to a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// A month's number from its English name or abbreviation.
fn month_number(name: &str) -> Option<i64> {
    let lower = name.to_ascii_lowercase();
    let short = lower.get(..3)?;
    MONTHS
        .iter()
        .position(|m| *m == short)
        .and_then(|i| i64::try_from(i + 1).ok())
}

fn valid_date(year: i64, month: i64, day: i64) -> bool {
    (1970..=2200).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day)
}

/// `Thu, 01 Oct 2026 12:00:00 +0000` (RFC 2822, Pinterest's `created_at`)
/// in unix ms.
#[must_use]
pub fn rfc2822_ms(text: &str) -> Option<i64> {
    let text = text.trim();
    let text = text.split_once(',').map_or(text, |(_, rest)| rest).trim();
    let parts: Vec<&str> = text.split_whitespace().collect();
    let [day, month, year, time, zone] = parts.as_slice() else {
        return None;
    };
    let (day, month, year): (i64, i64, i64) =
        (day.parse().ok()?, month_number(month)?, year.parse().ok()?);
    if !valid_date(year, month, day) {
        return None;
    }
    let clock: Vec<i64> = time
        .split(':')
        .map(|p| p.parse().ok())
        .collect::<Option<_>>()?;
    let [h, m, s] = clock.as_slice() else {
        return None;
    };
    let offset = match *zone {
        "GMT" | "UT" | "UTC" | "Z" => 0,
        zone if zone.len() == 5 => {
            let sign = match &zone[..1] {
                "+" => 1,
                "-" => -1,
                _ => return None,
            };
            let hours: i64 = zone[1..3].parse().ok()?;
            let minutes: i64 = zone[3..5].parse().ok()?;
            sign * (hours * 60 + minutes)
        }
        _ => return None,
    };
    let seconds = days_from_civil(year, month, day) * 86_400 + h * 3_600 + m * 60 + s - offset * 60;
    Some(seconds * 1_000)
}

/// `October 1, 2026` (oEmbed's date) as midnight UTC, in unix ms.
#[must_use]
pub fn month_day_year_ms(text: &str) -> Option<i64> {
    let cleaned = text.replace(',', " ");
    let parts: Vec<&str> = cleaned.split_whitespace().collect();
    let [month, day, year] = parts.as_slice() else {
        return None;
    };
    let (month, day, year): (i64, i64, i64) =
        (month_number(month)?, day.parse().ok()?, year.parse().ok()?);
    valid_date(year, month, day).then(|| days_from_civil(year, month, day) * 86_400_000)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn dates_in_the_endpoints_formats() {
        assert_eq!(
            rfc2822_ms("Thu, 01 Oct 2026 12:00:00 +0000"),
            Some(1_790_856_000_000)
        );
        assert_eq!(
            rfc2822_ms("Thu, 01 Oct 2026 14:00:00 +0200"),
            Some(1_790_856_000_000)
        );
        assert_eq!(
            rfc2822_ms("01 Oct 2026 12:00:00 GMT"),
            Some(1_790_856_000_000)
        );
        assert_eq!(rfc2822_ms("yesterday"), None);
        assert_eq!(
            month_day_year_ms("October 1, 2026"),
            Some(1_790_812_800_000)
        );
        assert_eq!(month_day_year_ms("Oct 1, 2026"), Some(1_790_812_800_000));
        assert_eq!(month_day_year_ms("1 October 2026"), None);
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }

    #[test]
    fn entities_and_objects() {
        assert_eq!(
            decode_entities("a &amp; b &#39;c&#x27; &lt;d&gt; &bogus; & e"),
            "a & b 'c' <d> &bogus; & e"
        );
        let json = json!({ "a": [{ "b": 1 }, { "xig": { "c": 2 } }] });
        let found = find_object(&json, |o| o.contains_key("xig")).unwrap();
        assert_eq!(found["xig"]["c"], 2);
        assert!(find_object(&json, |o| o.contains_key("nope")).is_none());
    }
}
