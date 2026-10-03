//! Streaming parser and cancellable, resumable metadata batch writer.
use super::{checkpoint, save};
use crate::control::{
    uploads::{self, Found, UploadPurpose},
    users,
};
use crate::error::{ApiError, ErrorCode};
use crate::events::model::ChangeReason;
use crate::jobs::import::Payload;
use crate::jobs::{JobContext, JobError};
use crate::{library, quota};
use serde_json::{Value, json};
use shelfy_core::import::{
    self, collections,
    v1::{self, Record},
};
use shelfy_core::repo::{
    RepoError,
    notifications::{self, NewNotification},
};
use std::collections::BTreeMap;
use std::fs::File;
use std::sync::Arc;
use tokio::sync::mpsc;

impl From<v1::Error> for JobError {
    fn from(e: v1::Error) -> Self {
        match e {
            v1::Error::Io(e) => Self::transient("unavailable").with_detail(e.to_string()),
            v1::Error::Format => Self::permanent(ErrorCode::ImportFormatUnknown.as_str()),
        }
    }
}
enum Message {
    Definitions {
        defs: BTreeMap<String, collections::Definition>,
        total: u64,
    },
    Batch {
        records: Vec<(u64, Value)>,
        next: u64,
    },
}
/// Runs only claimed, server-named files. A complete prepass validates the
/// envelope and collects bounded definitions, independent of property order.
/// Neither remote URLs nor desktop local paths are fetched or published.
pub async fn run(ctx: &JobContext) -> Result<crate::jobs::Outcome, JobError> {
    let payload: Payload = ctx.payload_as()?;
    let control = Arc::clone(ctx.state().control());
    let user = ctx.user_id().to_owned();
    let upload_id = payload.upload_id.clone();
    let upload =
        crate::state::blocking(move || control.read(|c| uploads::get(c, &user, &upload_id)))
            .await?
            .filter(|u| {
                u.purpose == Some(UploadPurpose::IMPORT)
                    && u.is_complete()
                    && u.meta.consumed_at.is_some()
            })
            .ok_or_else(|| JobError::permanent("invalid_payload"))?;
    if upload.found() != Some(Found::Json) {
        return Err(JobError::permanent(ErrorCode::ImportFormatUnknown.as_str()));
    }
    let saved = ctx
        .user_db({
            let id = ctx.id();
            move |db| db.read(|c| checkpoint(c, id)).map_err(JobError::from)
        })
        .await?;
    if saved.complete {
        return Ok(crate::jobs::Outcome::Succeeded);
    }
    quota::check(ctx.state(), ctx.user_id(), 0).await?;
    let path = uploads::file_path(
        &ctx.state().config().data_dir.uploads_dir(),
        &upload.id,
        true,
    );
    let mut definitions_done = saved.definitions_done;
    let token = ctx.token().clone();
    let now = ctx.jobs().clock().now_ms();
    let (tx, mut rx) = mpsc::channel(1);
    let parser = tokio::task::spawn_blocking(move || -> Result<(), JobError> {
        let mut defs = BTreeMap::new();
        let mut definition_weight = 0;
        let mut seen = 0;
        let mut recognized = 0;
        v1::read(
            File::open(&path).map_err(v1::Error::from)?,
            |r| -> Result<(), JobError> {
                if token.is_cancelled() {
                    return Err(JobError::cancelled());
                }
                match r {
                    Record::Collection(v) => {
                        definition_weight += v1::weight(&v);
                        if defs.len() >= 5000 || definition_weight > v1::BATCH_BYTES {
                            return Err(v1::Error::Format.into());
                        }
                        let d = collections::definition(&v)
                            .map_err(|_| JobError::permanent("validation_failed"))?;
                        // First definition wins, matching the desktop's ensure map.
                        defs.entry(d.key.clone()).or_insert(d);
                    }
                    Record::Post { value, .. } => {
                        seen += 1;
                        if import::normalize::post(&value, now).is_ok()
                            || value
                                .get("platform")
                                .and_then(Value::as_str)
                                .is_some_and(|p| {
                                    matches!(
                                        p,
                                        "instagram" | "twitter" | "pinterest" | "web" | "manual"
                                    )
                                })
                            || value.get("shortcode").is_some()
                            || value.get("text").is_some() && value.get("authorUsername").is_some()
                        {
                            recognized += 1;
                        }
                    }
                }
                Ok(())
            },
        )?;
        if seen > 0 && recognized == 0 {
            return Err(v1::Error::Format.into());
        }
        tx.blocking_send(Message::Definitions { defs, total: seen })
            .map_err(|_| JobError::cancelled())?;
        let mut records = Vec::new();
        let mut weight = 0;
        let mut next = saved.next;
        v1::read(
            File::open(&path).map_err(v1::Error::from)?,
            |r| -> Result<(), JobError> {
                if token.is_cancelled() {
                    return Err(JobError::cancelled());
                }
                if let Record::Post { index, value, .. } = r {
                    if index < saved.next {
                        return Ok(());
                    }
                    let charge = v1::weight(&value);
                    if !records.is_empty()
                        && (records.len() == v1::BATCH_ITEMS || weight + charge > v1::BATCH_BYTES)
                    {
                        tx.blocking_send(Message::Batch {
                            records: std::mem::take(&mut records),
                            next,
                        })
                        .map_err(|_| JobError::cancelled())?;
                        weight = 0;
                    }
                    weight += charge;
                    next = index + 1;
                    records.push((index, value));
                }
                Ok(())
            },
        )?;
        if !records.is_empty() {
            tx.blocking_send(Message::Batch { records, next })
                .map_err(|_| JobError::cancelled())?;
        }
        Ok(())
    });
    let mut defs = Arc::new(BTreeMap::new());
    let mut total = 0;
    while let Some(message) = rx.recv().await {
        if ctx.is_cancelled() {
            return Err(JobError::cancelled());
        }
        if ctx.should_yield() {
            return Ok(crate::jobs::Outcome::Requeue { run_at: None });
        } // scheduler pause resumes cursor on retry
        let (records, next, is_definitions) = match message {
            Message::Definitions { defs: d, total: n } => {
                total = n;
                defs = Arc::new(d);
                if definitions_done {
                    continue;
                }
                definitions_done = true;
                (Vec::new(), 0, true)
            }
            Message::Batch { records, next } => (records, next, false),
        };
        let defs = Arc::clone(&defs);
        let state = ctx.state().clone();
        let user = ctx.user_id().to_owned();
        let fence = ctx.attempt_fence();
        let id = ctx.id();
        let now = ctx.jobs().clock().now_ms();
        let saved = ctx
            .user_db(move |db| {
                let (saved, keys) = db.write(|c| -> Result<_, JobError> {
                    if !fence.is_current()? {
                        return Err(JobError::cancelled());
                    }
                    let mut saved = checkpoint(c, id)?;
                    let mut keys = Vec::new();
                    if is_definitions && !saved.definitions_done {
                        for d in defs.values() {
                            let (_, created) = collections::ensure(c, d, now)?;
                            saved.report.collections += u64::from(created);
                        }
                        saved.definitions_done = true;
                    } else if !is_definitions && next > saved.next {
                        keys = import::apply(c, &records, &defs, &mut saved.report, now)?;
                        saved.next = next;
                    }
                    save(c, id, &saved)?;
                    check_quota(&state, &user, c)?;
                    Ok((saved, keys))
                })?;
                if !keys.is_empty() || is_definitions && saved.report.collections > 0 {
                    library::committed(
                        state.user_dbs(),
                        state.events(),
                        &user,
                        db,
                        ChangeReason::Import,
                        if keys.len() <= 200 { Some(keys) } else { None },
                    );
                }
                Ok(saved)
            })
            .await?;
        ctx.progress(
            Some(if total == 0 {
                1.0
            } else {
                saved.next as f64 / total as f64
            }),
            Some("posts"),
        )
        .await;
        tracing::debug!(
            job_id = ctx.id(),
            processed = saved.next,
            "import batch committed"
        );
    }
    parser
        .await
        .map_err(|e| JobError::transient("internal").with_detail(e.to_string()))??;
    finish(ctx).await?;
    // Keep the consumed upload bytes until housekeeping expires them: generic
    // cancelled/failed job retry must still be able to resume its checkpoint.
    Ok(crate::jobs::Outcome::Succeeded)
}
fn check_quota(
    state: &crate::state::AppState,
    user: &str,
    c: &rusqlite::Connection,
) -> Result<(), JobError> {
    let pages: i64 = c
        .query_row("PRAGMA page_count", [], |r| r.get(0))
        .map_err(RepoError::from)?;
    let size: i64 = c
        .query_row("PRAGMA page_size", [], |r| r.get(0))
        .map_err(RepoError::from)?;
    let media: i64 = c
        .query_row(
            "SELECT coalesce(sum(bytes),0) FROM media_objects",
            [],
            |r| r.get(0),
        )
        .map_err(RepoError::from)?;
    let db = pages.saturating_mul(size);
    let usage = state
        .control()
        .read(|c| users::usage(c, user))?
        .ok_or_else(|| JobError::permanent("unauthorized"))?;
    let used = u64::try_from(media.saturating_add(db)).unwrap_or(u64::MAX);
    if usage.quota_bytes > 0
        && used.saturating_add(state.quota().reserved(user))
            > u64::try_from(usage.quota_bytes).unwrap_or(0)
    {
        return Err(ApiError::new(ErrorCode::QuotaExceeded).into());
    }
    state.quota().record_count(user, media, db)?;
    Ok(())
}
async fn finish(ctx: &JobContext) -> Result<(), JobError> {
    let id = ctx.id();
    let fence = ctx.attempt_fence();
    let state = ctx.state().clone();
    let user = ctx.user_id().to_owned();
    let now = ctx.jobs().clock().now_ms();
    ctx.user_db(move|db|{
        let notice=db.write(|c|->Result<_,JobError>{
            if !fence.is_current()? {return Err(JobError::cancelled());}
            let mut saved=checkpoint(c,id)?;
            if saved.complete {return Ok(None);}
            let params=json!({"jobId":id,"imported":saved.report.imported,"updated":saved.report.updated,"skipped":saved.report.skipped,"rejected":saved.report.rejected_count});
            let n=notifications::create(c,&NewNotification{kind:"import".into(),code:"import.done".into(),params:params.as_object().cloned().unwrap_or_default(),target:Some("/library".into())},now)?;
            saved.complete=true;save(c,id,&saved)?;
            check_quota(&state,&user,c)?;
            Ok(Some(n))
        })?;
        if let Some(n)=notice {state.events().notification(&user,&n.into());}
        Ok(())
    }).await?;
    ctx.progress(Some(1.0), Some("done")).await;
    Ok(())
}
