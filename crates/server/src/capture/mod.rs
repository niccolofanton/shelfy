//! Website capture queue entry and the shared service configuration.
pub mod client;
pub mod ingest;
pub mod protocol;

use clap::Args;
use rusqlite::{OptionalExtension as _, params};
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use shelfy_core::repo::{RepoError, posts};
use shelfy_core::web::captures;
use std::sync::Arc;
use tokio::sync::Mutex;
use url::{Host, Url};
use utoipa::ToSchema;

use crate::control::{jobs as rows, usage_daily, users};
use crate::error::{ApiError, ErrorCode};
use crate::events::model::ChangeReason;
use crate::jobs::{Enqueued, NewJob};
use crate::library::{self, Change};
use crate::state::{AppState, blocking};

#[derive(Clone, Args)]
pub struct CaptureArgs {
    /// Capture's internal authentication token; never returned to clients.
    #[arg(long, env = "SHELFY_INTERNAL_TOKEN", hide_env_values = true)]
    pub internal_token: Option<String>,
    /// Simultaneous website captures over all users (1–2).
    #[arg(long = "capture-sites-parallel", env = "SHELFY_CAPTURE_SITES_PARALLEL", default_value_t = 1, value_parser = clap::value_parser!(u8).range(1..=2))]
    pub sites_parallel: u8,
}

impl std::fmt::Debug for CaptureArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CaptureArgs")
            .field(
                "internal_token",
                &self.internal_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("sites_parallel", &self.sites_parallel)
            .finish()
    }
}

#[derive(Clone, Debug)]
pub struct CaptureConfig {
    pub internal_token: Option<SecretString>,
    pub sites_parallel: u8,
}
impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            internal_token: None,
            sites_parallel: 1,
        }
    }
}
impl From<CaptureArgs> for CaptureConfig {
    fn from(args: CaptureArgs) -> Self {
        Self {
            internal_token: args
                .internal_token
                .filter(|s| !s.is_empty())
                .map(SecretString::from),
            sites_parallel: args.sites_parallel,
        }
    }
}

/// Serializes placeholder/enqueue and cancellation cleanup, so a newly saved
/// placeholder cannot be removed by an old queue cancellation.
#[derive(Default)]
pub struct CaptureService {
    pub(crate) gate: Mutex<()>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Options {
    #[serde(default = "default_pages")]
    #[schema(minimum = 1, maximum = 8, default = 6)]
    pub max_pages: u8,
    #[serde(default)]
    pub single_page: bool,
}
const fn default_pages() -> u8 {
    6
}
impl Default for Options {
    fn default() -> Self {
        Self {
            max_pages: 6,
            single_page: false,
        }
    }
}
impl Options {
    pub fn validate(self) -> Result<Self, ApiError> {
        if !(1..=8).contains(&self.max_pages) {
            return Err(ApiError::invalid_field("maxPages", "must be 1–8"));
        }
        Ok(self)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Payload {
    pub post_key: String,
    pub post_id: i64,
    pub url: String,
    pub options: Options,
    pub first: bool,
    #[serde(default)]
    pub waiting_since: Option<i64>,
}

pub fn configured(state: &AppState) -> bool {
    state.config().outbound.capture.is_some() && state.config().capture.internal_token.is_some()
}

pub fn validate_url(raw: &str) -> Result<String, ApiError> {
    let fail =
        || ApiError::invalid_field("url", "must be a public http(s) URL up to 2048 characters");
    if raw.chars().count() > 2048 {
        return Err(fail());
    }
    let mut url = Url::parse(raw.trim()).map_err(|_| fail())?;
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.port_or_known_default(), Some(80 | 443))
    {
        return Err(fail());
    }
    match url.host().ok_or_else(fail)? {
        Host::Domain(domain)
            if domain.contains('.')
                && !domain.ends_with('.')
                && domain != "localhost"
                && !domain.ends_with(".localhost") => {}
        Host::Ipv4(ip) if crate::outbound::is_public(ip.into()) => {}
        Host::Ipv6(ip) if crate::outbound::is_public(ip.into()) => {}
        _ => return Err(fail()),
    }
    url.set_fragment(None);
    if url.as_str().len() > 2048 {
        return Err(fail());
    }
    Ok(url.to_string())
}

fn active(
    conn: &rusqlite::Connection,
    user: &str,
    key: &str,
) -> Result<Option<rows::JobRow>, RepoError> {
    let id = conn.query_row("SELECT id FROM jobs WHERE user_id=?1 AND kind='capture.site' AND dedupe_key=?2 AND state IN ('queued','running')", params![user,key], |r|r.get::<_,i64>(0)).optional()?;
    id.map(|id| rows::get(conn, user, id))
        .transpose()
        .map(Option::flatten)
}

/// Creates the existing/new placeholder and one active task for its identity.
/// The final daily-limit check and job insert share the control transaction.
pub async fn enqueue_site(
    state: &AppState,
    user_id: &str,
    raw_url: &str,
    opts: Options,
) -> Result<(String, Enqueued), ApiError> {
    let opts = opts.validate()?;
    let url = validate_url(raw_url)?;
    if !configured(state) {
        return Err(ApiError::new(ErrorCode::CaptureUnavailable));
    }
    let _gate = state.capture().gate.lock().await;
    let now = state.jobs().clock().now_ms();
    let placeholder = captures::placeholder(&url, now)
        .map_err(|_| ApiError::invalid_field("url", "invalid website identity"))?;
    let key = placeholder.key.clone();
    let ckey = key.clone();
    let control = Arc::clone(state.control());
    let user = user_id.to_owned();
    if let Some(job) = blocking(move || control.read(|c| active(c, &user, &ckey))).await? {
        // Also arm an existing durable row after the insert/notification crash gap.
        let value = serde_json::from_str(&job.payload_json)
            .map_err(|_| ApiError::new(ErrorCode::Internal))?;
        let queued = state
            .jobs()
            .enqueue(
                NewJob::new(user_id, crate::jobs::capture::KIND)
                    .dedupe(&key)
                    .payload(value),
            )
            .await?;
        return Ok((key, queued));
    }
    let control = Arc::clone(state.control());
    let user = user_id.to_owned();
    blocking(move || control.read(|c| daily_available(c, &user, now))).await?;
    let write_key = key.clone();
    let written=library::write(state,user_id,ChangeReason::Capture,move |tx| {
        let found=tx.query_row("SELECT id,current_capture_id,deleted_at IS NOT NULL FROM posts WHERE key=?1 AND platform='web'",[&write_key],|r| Ok((r.get::<_,i64>(0)?,r.get::<_,Option<i64>>(1)?,r.get::<_,bool>(2)?))).optional()?;
        let (id,first,inserted)=if let Some((id,current,deleted))=found {
            if deleted { return Err(RepoError::NotFound); } (id,current.is_none(),false)
        } else { (posts::insert(tx,&placeholder,now)?,true,true) };
        Ok(Change {value:(id,first,inserted),keys:Some(vec![write_key])})
    }).await?;
    let (post_id, first, inserted) = written.value;
    let payload = Payload {
        post_key: key.clone(),
        post_id,
        url,
        options: opts,
        first,
        waiting_since: None,
    };
    let value = serde_json::to_value(&payload).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let encoded = value.to_string();
    let control = Arc::clone(state.control());
    let user = user_id.to_owned();
    let ckey = key.clone();
    let insertion = blocking(move || {
        control.write(|tx| {
            if let Some(job) = active(tx, &user, &ckey)? {
                return Ok(rows::Inserted::Existing(job));
            }
            daily_available(tx, &user, now)?;
            rows::insert(
                tx,
                &rows::NewJobRow {
                    user_id: &user,
                    kind: crate::jobs::capture::KIND,
                    dedupe_key: Some(&ckey),
                    priority: 100,
                    payload_json: &encoded,
                    max_attempts: 2,
                    run_at: now,
                },
                now,
            )
            .map_err(ApiError::from)
        })
    })
    .await;
    let insertion = match insertion {
        Ok(value) => value,
        Err(error) => {
            if inserted {
                let _ = library::write(state, user_id, ChangeReason::Capture, move |tx| {
                    posts::purge(tx, &[post_id], now)?;
                    Ok(Change {
                        value: (),
                        keys: None,
                    })
                })
                .await;
            }
            return Err(error);
        }
    };
    let (job, created) = match insertion {
        rows::Inserted::Created(row) => (row, true),
        rows::Inserted::Existing(row) => (row, false),
    };
    // Wake the scheduler via its normal entry; the unique active key returns
    // the just-inserted row, including after a crash before this notification.
    state
        .jobs()
        .enqueue(
            NewJob::new(user_id, crate::jobs::capture::KIND)
                .dedupe(&key)
                .payload(value),
        )
        .await?;
    if created {
        state.events().job_updated(
            user_id,
            crate::events::model::JobUpdatedEvent {
                id: job.id,
                kind: job.kind.clone(),
                state: job.state,
                progress: None,
                stage: None,
                post_key: Some(key.clone()),
                error_code: None,
            },
        );
    }
    Ok((key, Enqueued { job, created }))
}

fn daily_available(conn: &rusqlite::Connection, user: &str, now: i64) -> Result<(), ApiError> {
    let limits = users::limits(conn, user)?.ok_or(RepoError::NotFound)?;
    let active:i64=conn.query_row("SELECT count(*) FROM jobs WHERE user_id=?1 AND kind='capture.site' AND state IN ('queued','running') AND coalesce(json_extract(payload_json,'$.captureCounted'),0)=0",[user],|r|r.get(0)).map_err(RepoError::from)?;
    let today = usage_daily::of_day(conn, user, now)?.captures;
    if limits.capture_daily_limit > 0 && today.saturating_add(active) >= limits.capture_daily_limit
    {
        return Err(ApiError::new(ErrorCode::CaptureDailyLimit));
    }
    Ok(())
}

pub fn work_root(state: &AppState) -> std::path::PathBuf {
    state.config().data_dir.root().join("work/capture")
}
