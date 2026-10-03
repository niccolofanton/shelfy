//! Per-user social catalog worker. Item state is durable in the library;
//! provider outages hold work without spending an item attempt.
use std::sync::{Arc, Mutex};
use std::time::Duration;

use shelfy_ai::{ChatRequest, ErrorKind, Image, ImageType, JsonOutput, Message, Part};
use shelfy_core::ai::{catalog, inputs, normalize, queue};
use shelfy_core::repo::notifications::NewNotification;
use shelfy_media::{
    digest::Digest,
    kind::MediaKind,
    name::Rendition,
    pool::ImagePool,
    render::RenderSpec,
    store::{MediaStore, UserMedia},
    video::{VideoTools, VideoToolsConfig},
};
use tokio::time::Instant;

use super::{
    Backoff, CancelContext, Enqueued, JobContext, JobError, JobResult, Jobs, Kind, KindSpec,
    NewJob, Outcome, SweepContext,
};
use crate::ai::{AiServiceError, CallHints, Caller, Task};
use crate::control::users::{self, Role};
use crate::error::ApiError;
use crate::events::{self, model::ChangeReason};
use crate::library::{self, Change};
use crate::state::{AppState, blocking};

/// Stable queue kind, also its dedupe key.
pub const KIND: &str = crate::ai::AI_DRAIN_KIND;
/// Tries per item. Provider holds spend none.
pub const ITEM_TRIES: u32 = 3;
/// A bounded turn keeps another user's backlog moving.
pub const ITEMS_PER_TURN: usize = 25;
const HOLD_MS: i64 = 60_000;
const ITEM_BACKOFF: Backoff = Backoff::new(Duration::from_secs(5), Duration::from_secs(300));

/// Registers the worker, recovery sweep and library cancel hook.
#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(32)
            .per_user(1)
            .lease(Duration::from_secs(600)),
        run,
    )
    .with_sweep(pending)
    .with_cancel_hook(cancel)
}

/// Arms one drain per user; a delayed active drain becomes due now.
pub async fn enqueue(jobs: &Jobs, user_id: &str) -> Result<Enqueued, ApiError> {
    jobs.enqueue(NewJob::new(user_id, KIND).dedupe(KIND)).await
}

/// Whether a caller may use the owner-only operator route.
pub async fn is_owner(state: &AppState, user_id: &str) -> Result<bool, ApiError> {
    let control = Arc::clone(state.control());
    let id = user_id.to_owned();
    Ok(blocking(move || control.read(|conn| users::get(conn, &id)))
        .await?
        .is_some_and(|u| u.role == Role::Owner))
}

async fn pending(ctx: SweepContext) -> Result<Option<i64>, JobError> {
    ctx.user_db(|db| {
        db.read(|conn| {
            let counts = queue::state_counts(conn)?;
            // A crashed item needs a new drain even with no pending row.
            Ok::<_, shelfy_core::repo::RepoError>(if counts.analyzing > 0 {
                Some(0)
            } else {
                queue::next_pending_at(conn)?
            })
        })
        .map_err(JobError::from)
    })
    .await
}

async fn cancel(ctx: CancelContext) -> Result<u64, JobError> {
    let now = ctx.now_ms();
    Ok(
        library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
            Ok(Change {
                value: queue::cancel(tx, &queue::Reach::All, now)?,
                keys: None,
            })
        })
        .await?
        .value,
    )
}

async fn next_at(ctx: &JobContext) -> Result<Option<i64>, JobError> {
    ctx.user_db(|db| db.read(queue::next_pending_at).map_err(JobError::from))
        .await
}

async fn run(ctx: JobContext) -> JobResult {
    let now = ctx.jobs().clock().now_ms();
    library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
        Ok(Change {
            value: queue::recover_interrupted(tx, ITEM_TRIES, now)?,
            keys: None,
        })
    })
    .await?;
    let owner = is_owner(ctx.state(), ctx.user_id()).await?;
    let caller = Caller::new(ctx.user_id(), owner);
    let mut done = ctx
        .payload()
        .get("done")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let mut errors = ctx
        .payload()
        .get("errors")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    let vocabulary = tokio::sync::Mutex::new((None, Vec::<String>::new()));
    let concurrency = match ctx
        .state()
        .ai()
        .route(ctx.state(), caller, Task::Catalog)
        .await
    {
        Ok(route) if route.provider.is_operator() => {
            usize::from(ctx.state().config().operator.concurrency.max(1))
        }
        Ok(_) => usize::from(
            ctx.user_db(|db| {
                db.read(shelfy_core::repo::settings::read)
                    .map_err(JobError::from)
            })
            .await?
            .ai
            .concurrency
            .clamp(1, 8),
        ),
        Err(_) => 1,
    };
    let mut processed = 0;
    while processed < ITEMS_PER_TURN {
        let mut batch = Vec::new();
        for _ in 0..concurrency.min(ITEMS_PER_TURN - processed) {
            if ctx.should_yield() {
                if batch.is_empty() {
                    return Ok(Outcome::Requeue { run_at: None });
                }
                break;
            }
            let now = ctx.jobs().clock().now_ms();
            let claim = library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
                let claim = queue::claim_due(tx, now)?;
                Ok(Change {
                    keys: claim.as_ref().map(|c| vec![c.key.clone()]),
                    value: claim,
                })
            })
            .await?
            .value;
            let Some(claim) = claim else {
                break;
            };
            batch.push(claim);
        }
        if batch.is_empty() {
            break;
        }
        processed += batch.len();
        ctx.progress(None, Some("catalog")).await;
        let results = futures_util::future::join_all(batch.into_iter().map(|claim| async {
            let result = catalog_item(&ctx, caller, &claim, &vocabulary).await;
            (claim, result)
        }))
        .await;
        let mut batch_hold = false;
        let mut batch_pause = false;
        for (claim, result) in results {
            let provider_status = match &result {
                Ok(_) => "ok",
                Err(ItemError::Provider(AiServiceError::Call(e))) => match e.kind() {
                    ErrorKind::Offline => "offline",
                    ErrorKind::InvalidKey => "invalid_key",
                    ErrorKind::QuotaExhausted => "quota_exhausted",
                    ErrorKind::Transient if e.code() == Some("provider_held") => "down",
                    _ => "degraded",
                },
                Err(ItemError::Provider(_)) => "not_configured",
                _ => "ok",
            };
            let now = ctx.jobs().clock().now_ms();
            let mut hold = false;
            let mut pause = false;
            let mut finished = false;
            let mut failed = false;
            let fence = ctx.attempt_fence();
            let c = claim.clone();
            let action = match result {
                Ok(patch) => {
                    finished = true;
                    Action::Apply(Box::new(patch))
                }
                Err(ItemError::Media) => {
                    failed = true;
                    Action::Fail("media_unreadable")
                }
                Err(ItemError::Provider(error)) => match error {
                    AiServiceError::Call(e) => match e.kind() {
                        ErrorKind::Transient if e.code() == Some("provider_held") => {
                            hold = true;
                            Action::Release(now + HOLD_MS)
                        }
                        ErrorKind::Offline | ErrorKind::Cancelled => {
                            hold = true;
                            Action::Release(now + HOLD_MS)
                        }
                        ErrorKind::InvalidKey | ErrorKind::QuotaExhausted => {
                            hold = true;
                            pause = true;
                            Action::Release(now)
                        }
                        ErrorKind::Transient | ErrorKind::RateLimited
                            if claim.attempt < i64::from(ITEM_TRIES) =>
                        {
                            let wait = ITEM_BACKOFF
                                .jittered(claim.attempt as u32, getrandom::u64().unwrap_or(0))
                                .max(
                                    e.retry_after()
                                        .unwrap_or_default()
                                        .min(Duration::from_secs(3600)),
                                );
                            Action::Backoff(
                                now.saturating_add(wait.as_millis() as i64),
                                e.kind().as_str(),
                            )
                        }
                        kind => {
                            failed = true;
                            Action::Fail(kind.as_str())
                        }
                    },
                    AiServiceError::NotConfigured | AiServiceError::ConsentRequired(_) => {
                        hold = true;
                        pause = true;
                        Action::Release(now)
                    }
                },
                Err(ItemError::Schema) => {
                    failed = true;
                    Action::Fail("schema_invalid")
                }
            };
            let applied = library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
                // The scheduler fence makes a cancelled drain unable to apply a result.
                if !fence
                    .is_current()
                    .map_err(|_| shelfy_core::repo::RepoError::Invalid {
                        field: "claim",
                        reason: "lost job lease",
                    })?
                {
                    return Ok(Change {
                        value: false,
                        keys: Some(vec![]),
                    });
                }
                queue::set_provider_status(tx, provider_status, now)?;
                let guard = match action {
                    Action::Apply(patch) => {
                        queue::apply(tx, c.post_id, c.attempt, &c.token, &patch, now)?
                    }
                    Action::Fail(code) => {
                        queue::fail(tx, c.post_id, c.attempt, &c.token, code, now)?
                    }
                    Action::Backoff(at, code) => {
                        queue::backoff(tx, c.post_id, c.attempt, &c.token, at, code)?
                    }
                    Action::Release(at) => queue::release(tx, c.post_id, c.attempt, &c.token, at)?,
                };
                Ok(Change {
                    value: guard.applied(),
                    keys: Some(vec![c.key]),
                })
            })
            .await?
            .value;
            if applied {
                done += u64::from(finished);
                errors += u64::from(failed);
            }
            ctx.attempt_fence()
                .checkpoint(&serde_json::json!({"done": done, "errors": errors}))?;
            batch_hold |= hold;
            batch_pause |= pause;
        }
        if batch_pause {
            ctx.jobs().pause(ctx.user_id(), KIND).await?;
        }
        if batch_hold {
            return Ok(Outcome::Requeue {
                run_at: Some(ctx.jobs().clock().now_ms() + HOLD_MS),
            });
        }
    }
    if let Some(at) = next_at(&ctx).await? {
        return Ok(Outcome::Requeue { run_at: Some(at) });
    }
    if !ctx.should_yield() {
        events::notify(
            ctx.state(),
            ctx.user_id(),
            NewNotification {
                kind: "ai".into(),
                code: "ai.analysis_finished".into(),
                params: serde_json::json!({"done": done, "errors": errors})
                    .as_object()
                    .unwrap()
                    .clone(),
                target: Some("/ai/queue".into()),
            },
        )
        .await?;
    }
    Ok(Outcome::Succeeded)
}

enum Action {
    Apply(Box<shelfy_core::repo::posts::AiPatch>),
    Fail(&'static str),
    Backoff(i64, &'static str),
    Release(i64),
}
enum ItemError {
    Provider(AiServiceError),
    Media,
    Schema,
}

async fn catalog_item(
    ctx: &JobContext,
    caller: Caller<'_>,
    claim: &queue::Claim,
    vocabulary: &tokio::sync::Mutex<(Option<shelfy_core::generation::Generation>, Vec<String>)>,
) -> Result<shelfy_core::repo::posts::AiPatch, ItemError> {
    let route = ctx
        .state()
        .ai()
        .route(ctx.state(), caller, Task::Catalog)
        .await
        .map_err(ItemError::Provider)?;
    let post_id = claim.post_id;
    let generation = ctx
        .state()
        .user_db(ctx.user_id())
        .await
        .map_err(|_| ItemError::Media)?
        .generation();
    let mut vocabulary = vocabulary.lock().await;
    let refresh_vocabulary = vocabulary.0 != Some(generation);
    let (input, hints) = ctx
        .user_db(move |db| {
            db.read(|conn| {
                Ok::<_, shelfy_core::repo::RepoError>((
                    inputs::select(conn, post_id)?,
                    if refresh_vocabulary {
                        Some(inputs::vocabulary(conn, inputs::VOCABULARY_SIZE)?)
                    } else {
                        None
                    },
                ))
            })
            .map_err(JobError::from)
        })
        .await
        .map_err(|_| ItemError::Media)?;
    if let Some(hints) = hints {
        *vocabulary = (Some(generation), hints);
    }
    let hints = vocabulary.1.clone();
    drop(vocabulary);
    let input = input.ok_or(ItemError::Media)?;
    let images = frames(ctx, &input, claim.deep).await?;
    if ctx.should_yield() {
        return Err(ItemError::Provider(AiServiceError::Call(
            shelfy_ai::AiError::new(ErrorKind::Cancelled, "cataloging was cancelled"),
        )));
    }
    if matches!(
        input.media_type.as_str(),
        "image" | "images" | "carousel" | "video" | "file"
    ) && images.is_empty()
    {
        return Err(ItemError::Media);
    }
    let prompt = catalog::request(
        input.kind,
        input.caption.as_deref(),
        &hints,
        !images.is_empty(),
    )
    .map_err(|_| ItemError::Schema)?;
    let mut parts = vec![Part::Text(prompt.user)];
    parts.extend(images.into_iter().map(|data| {
        Part::Image(Image {
            media_type: ImageType::Jpeg,
            data: data.into(),
        })
    }));
    let request = ChatRequest::new(&route.model, vec![Message::user(parts)])
        .with_system(prompt.system)
        .with_temperature(prompt.temperature)
        .with_max_tokens(prompt.max_tokens)
        .with_json(
            JsonOutput::from_raw(prompt.schema.name, &prompt.schema.schema)
                .map_err(|_| ItemError::Schema)?,
        )
        .streamed(true);
    let events = ctx.state().events().clone();
    let user = ctx.user_id().to_owned();
    let key = claim.key.clone();
    let last = Mutex::new(None::<Instant>);
    let callback = Arc::new(move |text: &str| {
        let mut last = last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let now = Instant::now();
        if last.is_none_or(|at| now.duration_since(at) >= Duration::from_millis(250)) {
            events.ai_stream(&user, &key, text.to_owned());
            *last = Some(now);
        }
    });
    let started = Instant::now();
    let response = ctx
        .state()
        .ai()
        .chat(
            ctx.state(),
            caller,
            Task::Catalog,
            &request,
            CallHints::new()
                .with_cancel(ctx.token().clone())
                .with_text_callback(callback),
        )
        .await
        .map_err(ItemError::Provider)?;
    crate::ai::queue::record_duration(started.elapsed().as_millis() as u64);
    let catalog =
        normalize::parse_catalog(input.kind, &response.text).map_err(|_| ItemError::Schema)?;
    Ok(catalog.into_patch(route.provider.id(), &route.model))
}

fn object_path(
    store: &UserMedia,
    object: &inputs::FrameObject,
    grid: bool,
) -> Result<std::path::PathBuf, ItemError> {
    let digest = Digest::from_slice(&object.sha256).ok_or(ItemError::Media)?;
    if grid && object.variants & Rendition::G480.bit() != 0 {
        return Ok(store.rendition_path(&digest, Rendition::G480));
    }
    let kind = MediaKind::from_ext(&object.ext).ok_or(ItemError::Media)?;
    Ok(store.object_path(&digest, kind))
}

async fn frames(
    ctx: &JobContext,
    input: &inputs::PostInputs,
    deep: bool,
) -> Result<Vec<Vec<u8>>, ItemError> {
    let store = MediaStore::new(ctx.state().config().data_dir.users_dir())
        .user(ctx.user_id())
        .map_err(|_| ItemError::Media)?;
    let tools = VideoTools::new(VideoToolsConfig::new(
        ctx.state().config().video_tools.clone(),
        ctx.state().config().data_dir.root().join("ai-scratch"),
        None,
    ));
    let mut images = Vec::new();
    // Larger frames preserve titles/credits and other small on-screen text.
    let side = if deep { 1024 } else { 480 };
    for frame in &input.frames {
        if ctx.should_yield() {
            break;
        }
        match frame {
            inputs::Frame::Image(obj) => {
                if let Ok(path) = object_path(&store, obj, !deep)
                    && let Ok(image) = ImagePool::shared().jpeg_file(path, side, 85).await
                {
                    images.push(image);
                }
            }
            inputs::Frame::Video { video, poster } => {
                if let Some(obj) = poster
                    && let Ok(path) = object_path(&store, obj, !deep)
                    && let Ok(image) = ImagePool::shared().jpeg_file(path, side, 85).await
                {
                    images.push(image);
                }
                if (deep || poster.is_none())
                    && let Some(video) = video
                {
                    let path = object_path(&store, video, false)?;
                    let remaining = inputs::MAX_FRAMES.saturating_sub(images.len());
                    if remaining > 0
                        && let Ok(frames) = tools
                            .keyframes(
                                &path,
                                remaining.min(4),
                                RenderSpec {
                                    max_side: 1024,
                                    quality: 85.0,
                                },
                                ctx.token(),
                            )
                            .await
                    {
                        for frame in frames {
                            if let Ok(jpeg) = ImagePool::shared()
                                .jpeg_bytes(frame.image.webp.into(), 1024, 85)
                                .await
                            {
                                images.push(jpeg);
                            }
                        }
                    }
                }
            }
        }
        if images.len() >= inputs::MAX_FRAMES {
            images.truncate(inputs::MAX_FRAMES);
            break;
        }
    }
    Ok(images)
}
