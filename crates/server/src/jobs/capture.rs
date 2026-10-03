//! One durable site task; fresh work dirs, fences and replay receipts.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use rusqlite::{OptionalExtension as _, params};
use shelfy_core::repo::{RepoError, posts};
use shelfy_media::store::MediaStore;

use super::{CancelContext, JobContext, JobError, JobResult, Kind, KindSpec, Outcome};
use crate::capture::{
    self, Payload, ingest,
    protocol::{self, Line},
};
use crate::control::usage_daily::{self, Field};
use crate::events::model::ChangeReason;
use crate::library::{self, Change};
use crate::quota;

pub const KIND: &str = "capture.site";
const HOLD_MS: i64 = 15 * 60 * 1000;

pub fn kind(parallel: u8) -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(usize::from(parallel.clamp(1, 2)))
            .per_user(1)
            .max_attempts(2)
            .lease(Duration::from_secs(20 * 60)),
        run,
    )
    .with_cancel_hook(cancel)
}

struct WorkDir(PathBuf);
impl Drop for WorkDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn blocking<T, F>(f: F) -> Result<T, JobError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, JobError> + Send + 'static,
{
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| JobError::transient("internal"))?
}

async fn captured(ctx: &JobContext, payload: &Payload) -> Result<Option<i64>, JobError> {
    let (post, job) = (payload.post_id, ctx.id());
    ctx.user_db(move |db|db.read(|c| {
        c.query_row("SELECT id FROM web_captures WHERE post_id=?1 AND json_extract(meta_json,'$.capture.jobId')=?2 ORDER BY id DESC LIMIT 1",params![post,job],|r|r.get::<_,i64>(0)).optional().map_err(RepoError::from)
    }).map_err(JobError::from)).await
}

async fn count_success(ctx: &JobContext) -> Result<(), JobError> {
    let control = Arc::clone(ctx.state().control());
    let user = ctx.user_id().to_owned();
    let id = ctx.id();
    let now = ctx.jobs().clock().now_ms();
    blocking(move ||control.write(|tx| {
        // A receipt in the same control transaction as the daily counter:
        // recovery/retry never charges today's successful capture twice.
        let recorded:bool=tx.query_row("SELECT coalesce(json_extract(payload_json,'$.captureCounted'),0) FROM jobs WHERE id=?1 AND user_id=?2",params![id,user],|r|r.get(0))?;
        if !recorded {
            tx.execute("UPDATE jobs SET payload_json=json_set(payload_json,'$.captureCounted',1) WHERE id=?1 AND user_id=?2",params![id,user])?;
            usage_daily::bump(tx,&user,Field::Captures,1,now)?;
        }
        Ok::<_,RepoError>(())
    }).map_err(JobError::from)).await
}

async fn daily_dispatch_guard(ctx: &JobContext) -> Result<(), JobError> {
    let control = Arc::clone(ctx.state().control());
    let user = ctx.user_id().to_owned();
    let now = ctx.jobs().clock().now_ms();
    blocking(move || {
        control.read(|c| {
            let limits = crate::control::users::limits(c, &user)
                .map_err(JobError::from)?
                .ok_or_else(|| JobError::permanent("not_found"))?;
            let used = usage_daily::of_day(c, &user, now)
                .map_err(JobError::from)?
                .captures;
            // Generic /jobs/retry bypasses capture enqueue admission. Per-user
            // concurrency is one, so this check also fences retries across UTC days.
            if limits.capture_daily_limit > 0 && used >= limits.capture_daily_limit {
                return Err(JobError::permanent("capture_daily_limit"));
            }
            Ok(())
        })
    })
    .await
}

pub async fn run(ctx: JobContext) -> JobResult {
    let deadline = std::time::Instant::now() + capture::client::DEADLINE;
    let mut payload: Payload = ctx.payload_as()?;
    // A user retry after first-capture cancellation may have no placeholder.
    // Repair only a missing identity, never resurrect a trashed existing post.
    let _gate = ctx.state().capture().gate.lock().await;
    let key = payload.post_key.clone();
    let first = payload.first;
    let placeholder =
        shelfy_core::web::captures::placeholder(&payload.url, ctx.jobs().clock().now_ms())
            .map_err(|_| JobError::permanent("invalid_input"))?;
    if placeholder.key != key {
        return Err(JobError::permanent("invalid_input"));
    }
    let fence = ctx.attempt_fence();
    let now = ctx.jobs().clock().now_ms();
    let fixed = ctx.user_db(move |db| db.write(|tx| {
        if !fence.is_current()? { return Err(JobError::cancelled()); }
        let found = tx.query_row("SELECT id,deleted_at IS NOT NULL FROM posts WHERE key=?1 AND platform='web'", [&key], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).optional().map_err(RepoError::from)?;
        match found {
            Some((_,true)) => Err(JobError::permanent("not_found")),
            Some((id,false)) => Ok(id),
            None if first => posts::insert(tx,&placeholder,now).map_err(JobError::from),
            None => Err(JobError::permanent("not_found")),
        }
    })).await?;
    if fixed != payload.post_id {
        let fence = ctx.attempt_fence();
        blocking(move || {
            if !fence.checkpoint(&serde_json::json!({"postId":fixed}))? {
                return Err(JobError::cancelled());
            }
            Ok(())
        })
        .await?;
        payload.post_id = fixed;
    }
    drop(_gate);
    let post = payload.post_id;
    if let Some(capture_id) = captured(&ctx, &payload).await? {
        count_success(&ctx).await?;
        crate::ai::queue::enqueue_web(ctx.state(), ctx.user_id(), &payload.post_key, capture_id)
            .await
            .map_err(JobError::from)?;
        return Ok(Outcome::Succeeded);
    }
    if ctx.should_yield() {
        return Ok(Outcome::Requeue { run_at: None });
    }
    daily_dispatch_guard(&ctx).await?;
    let reservation = quota::reserve(ctx.state(), ctx.user_id(), protocol::SITE_BYTES)
        .await
        .map_err(JobError::from)?;
    let id = crate::ids::new_ulid();
    let path = capture::work_root(ctx.state()).join(&id);
    let root = capture::work_root(ctx.state());
    let p = path.clone();
    blocking(move || {
        crate::config::create_private_dir(&root)
            .map_err(|_| JobError::transient("capture_storage"))?;
        std::fs::create_dir(&p).map_err(|_| JobError::transient("capture_storage"))?;
        Ok(())
    })
    .await?;
    let work = WorkDir(path.clone());
    let fence = ctx.attempt_fence();
    let work_id = id.clone();
    blocking(move || {
        if !fence.checkpoint(&serde_json::json!({"workCaptureId":work_id}))? {
            return Err(JobError::cancelled());
        }
        Ok(())
    })
    .await?;
    let result = capture::client::run(
        ctx.state(),
        &id,
        &payload.url,
        payload.options,
        ctx.token(),
        |line| {
            let ctx = ctx.clone();
            async move {
                ctx.heartbeat();
                match line {
                    Line::Event {
                        kind: _,
                        code,
                        params,
                    } if code == "stage" => {
                        ctx.progress(
                            params.get("frac").and_then(serde_json::Value::as_f64),
                            params.get("stage").and_then(serde_json::Value::as_str),
                        )
                        .await
                    }
                    Line::Event { kind, code, params } => ctx.state().events().capture_event(
                        ctx.user_id(),
                        &crate::events::model::CaptureEvent {
                            job_id: ctx.id(),
                            post_key: ctx.post_key().unwrap_or("").into(),
                            kind,
                            code,
                            params,
                        },
                    ),
                    Line::Done {
                        duration_ms,
                        peak_rss_bytes,
                        bytes,
                        ..
                    } => crate::telemetry::metrics::capture_report(
                        "done",
                        duration_ms,
                        peak_rss_bytes,
                        bytes,
                    ),
                    _ => {}
                }
            }
        },
    )
    .await;
    let blocked = match result {
        Ok(blocked) => blocked,
        Err(err) if err.code() == "capture_unavailable" && err.is_transient() => {
            let now = ctx.jobs().clock().now_ms();
            let since = payload.waiting_since.unwrap_or(now);
            if now.saturating_sub(since) >= HOLD_MS {
                return Err(err);
            }
            let fence = ctx.attempt_fence();
            blocking(move || {
                fence.checkpoint(&serde_json::json!({"waitingSince":since}))?;
                Ok(())
            })
            .await?;
            ctx.progress(None, Some("waiting_capture")).await;
            return Ok(Outcome::Requeue {
                run_at: Some(now.saturating_add(10_000)),
            });
        }
        Err(err) => return Err(err),
    };
    if ctx.should_yield() {
        return Ok(Outcome::Requeue { run_at: None });
    }
    let p = path.clone();
    let url = payload.url.clone();
    let opts = payload.options;
    let validate_ctx = ctx.clone();
    let validated = blocking(move || {
        if blocked && !p.join("manifest.json").exists() {
            return Err(JobError::permanent("capture_blocked"));
        }
        ingest::validate_checked(&p, &url, opts, blocked, || {
            validate_ctx.heartbeat();
            if validate_ctx.should_yield() {
                return Err(JobError::cancelled());
            }
            if std::time::Instant::now() >= deadline {
                return Err(JobError::transient("capture_timeout"));
            }
            Ok(())
        })
    })
    .await;
    let validated = match validated {
        Ok(value) => value,
        Err(err) => {
            if ctx.should_yield() {
                return Ok(Outcome::Requeue { run_at: None });
            }
            if err.code() == "capture_blocked" {
                mark_blocked(&ctx, &payload).await?;
            }
            return Err(err);
        }
    };
    let media = MediaStore::new(ctx.state().config().data_dir.users_dir())
        .user(ctx.user_id())
        .map_err(|_| JobError::permanent("capture_storage"))?;
    let prep_media = media.clone();
    let prepared = blocking(move || ingest::prepare(&prep_media, validated, opts)).await?;
    let fence = ctx.attempt_fence();
    let token = ctx.token().clone();
    let now = ctx.jobs().clock().now_ms();
    let job = ctx.id();
    let key = payload.post_key.clone();
    let caches = Arc::clone(ctx.state().user_dbs());
    let events = ctx.state().events().clone();
    let user = ctx.user_id().to_owned();
    let capture_id = ctx.user_db(move |db| {
        let capture_id = db.write(|tx| {
            if token.is_cancelled() || !fence.is_current()? { return Err(JobError::cancelled()); }
            let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM posts WHERE id=?1 AND key=?2 AND platform='web' AND deleted_at IS NULL)",params![post,key],|r|r.get(0)).map_err(RepoError::from)?;
            if !valid { return Err(JobError::permanent("not_found")); }
            let (capture_id,added)=ingest::commit(tx,&media,post,job,prepared,now)?;
            reservation.commit(added)?;
            Ok::<_,JobError>(capture_id)
        })?;
        library::committed(&caches,&events,&user,db,ChangeReason::Capture,Some(vec![key]));
        Ok(capture_id)
    }).await?;
    drop(work);
    count_success(&ctx).await?;
    // Outside the ingest transaction, also retried from the durable receipt.
    crate::ai::queue::enqueue_web(ctx.state(), ctx.user_id(), &payload.post_key, capture_id)
        .await
        .map_err(JobError::from)?;
    Ok(Outcome::Succeeded)
}

async fn mark_blocked(ctx: &JobContext, payload: &Payload) -> Result<(), JobError> {
    let post = payload.post_id;
    let key = payload.post_key.clone();
    let fence = ctx.attempt_fence();
    library::write(ctx.state(),ctx.user_id(),ChangeReason::Capture,move |tx| {
        if !fence.is_current().map_err(|_|RepoError::Conflict("capture attempt ended"))? {return Ok(Change {value:(),keys:Some(vec![])});}
        tx.execute("UPDATE posts SET archive_state='failed',cover_fetch_error='capture_blocked' WHERE id=?1 AND key=?2 AND current_capture_id IS NULL",params![post,key])?;
        Ok(Change{value:(),keys:Some(vec![key])})
    }).await.map_err(JobError::from)?;
    Ok(())
}

async fn cancel(ctx: CancelContext) -> Result<u64, JobError> {
    let _gate = ctx.state().capture().gate.lock().await;
    let control = Arc::clone(ctx.state().control());
    let user = ctx.user_id().to_owned();
    let payloads=blocking(move ||control.read(|c| {
        let mut statement=c.prepare("SELECT payload_json FROM jobs WHERE user_id=?1 AND kind='capture.site' AND state='cancelled' AND NOT EXISTS(SELECT 1 FROM jobs active WHERE active.user_id=jobs.user_id AND active.kind=jobs.kind AND active.dedupe_key=jobs.dedupe_key AND active.state IN ('queued','running'))")?;
        let rows=statement.query_map([&user],|r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok::<_,RepoError>(rows.into_iter().filter_map(|s|serde_json::from_str::<Payload>(&s).ok()).filter(|p|p.first).collect::<Vec<_>>())
    }).map_err(JobError::from)).await?;
    let now = ctx.now_ms();
    let written=library::write(ctx.state(),ctx.user_id(),ChangeReason::Capture,move |tx| {
        let mut ids=Vec::new(); let mut keys=Vec::new();
        for p in payloads {
            let empty:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM posts WHERE id=?1 AND key=?2 AND platform='web' AND current_capture_id IS NULL AND coalesce(trim(user_note),'')='' AND coalesce(json_array_length(user_tags_json),0)=0 AND NOT EXISTS(SELECT 1 FROM post_collections WHERE post_id=posts.id))",params![p.post_id,p.post_key],|r|r.get(0))?;
            if empty {ids.push(p.post_id); keys.push(p.post_key);}
        }
        let changed=posts::purge(tx,&ids,now)? as u64;
        Ok(Change {value:changed,keys:Some(keys)})
    }).await.map_err(JobError::from)?;
    Ok(written.value)
}

/// Hourly orphan cleanup; generated ULID directories only, older than 24 hours.
pub async fn sweep(state: &crate::state::AppState, now: SystemTime) {
    let root = capture::work_root(state);
    let control = Arc::clone(state.control());
    let _ = tokio::task::spawn_blocking(move || {
        let active=control.read(|c| {
            let mut s=c.prepare("SELECT json_extract(payload_json,'$.workCaptureId') FROM jobs WHERE kind='capture.site' AND state='running'")?;
            let values=s.query_map([],|r|r.get::<_,Option<String>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok::<_,RepoError>(values.into_iter().flatten().collect::<std::collections::HashSet<_>>())
        });
        let Ok(active)=active else {return;};
        let Ok(entries) = std::fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if active.contains(name) {continue;}
            if name.len() != 26
                || !name
                    .bytes()
                    .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
            {
                continue;
            }
            let Ok(meta) = std::fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir()
                && meta
                    .modified()
                    .ok()
                    .and_then(|stamp| now.duration_since(stamp).ok())
                    .is_some_and(|age| age > Duration::from_secs(86400))
            {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    })
    .await;
}
