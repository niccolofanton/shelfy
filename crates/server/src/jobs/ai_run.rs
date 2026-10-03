//! Cancellable cluster refinement and alias proposal jobs. Offline providers
//! requeue without spending a try; prior chunks and their cursor commit together.
use std::time::Duration;

use shelfy_ai::{ChatRequest, EmbedRequest, ErrorKind, JsonOutput, Message};
use shelfy_core::ai::taxonomy_prompt::{self, TaxonomyRequest};
use shelfy_core::repo::{
    RepoError,
    notifications::{self, NewNotification},
};
use shelfy_core::tags::{
    aliases,
    clusters::{self, RefinedGroup},
    embeddings, graph,
};

use super::{JobContext, JobError, JobResult, Kind, KindSpec, Outcome, ai_drain};
use crate::ai::{
    AiServiceError, CallHints, Caller, Task,
    runs::{self, Plan, RunKind},
};
use crate::events::model::{ChangeReason, Notification};
use crate::library::{self, Change};

pub const KIND: &str = "ai.run";
const HOLD_MS: i64 = 60_000;

#[must_use]
pub fn kind() -> Kind {
    Kind::new(
        KindSpec::new(KIND)
            .global(4)
            .per_user(1)
            .max_attempts(1)
            .lease(Duration::from_secs(1800)),
        run,
    )
}

fn held(error: &AiServiceError) -> bool {
    matches!(error, AiServiceError::Call(e) if e.kind()==ErrorKind::Offline || e.code()==Some("provider_held"))
}
fn hold(ctx: &JobContext) -> Outcome {
    Outcome::Requeue {
        run_at: Some(ctx.jobs().clock().now_ms() + HOLD_MS),
    }
}
fn lost() -> RepoError {
    RepoError::Invalid {
        field: "job",
        reason: "lost job lease",
    }
}
async fn progress(ctx: &JobContext, stage: &str, n: usize, m: usize) {
    ctx.progress(
        Some(if m == 0 { 1.0 } else { n as f64 / m as f64 }),
        Some(&format!("{stage}:{n}/{m}")),
    )
    .await;
}

async fn persist(ctx: &JobContext, plan: Plan) -> Result<(), JobError> {
    let fence = ctx.attempt_fence();
    let id = ctx.id();
    let now = ctx.jobs().clock().now_ms();
    library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
        if !fence.is_current().map_err(|_| lost())? {
            return Err(lost());
        }
        runs::save(tx, id, &plan, now)?;
        Ok(Change::collections(()))
    })
    .await?;
    Ok(())
}

async fn run(ctx: JobContext) -> JobResult {
    let kind: RunKind = serde_json::from_value(ctx.payload()["runKind"].clone())
        .map_err(|_| JobError::permanent("invalid_payload"))?;
    let id = ctx.id();
    let incarnation = ctx.incarnation().to_owned();
    let mut plan = ctx
        .user_db(move |db| {
            db.read(|conn| {
                runs::load_for(conn, id, &incarnation)?
                    .map_or_else(|| runs::snapshot(conn, kind), Ok)
            })
            .map_err(JobError::from)
        })
        .await?;
    plan.incarnation = Some(ctx.incarnation().to_owned());
    if plan.kind != kind {
        return Err(JobError::permanent("invalid_payload"));
    }
    if plan.finished {
        return Ok(Outcome::Succeeded);
    }
    if ctx.should_yield() {
        return Ok(Outcome::Requeue { run_at: None });
    }
    persist(&ctx, plan.clone()).await?;
    let owner = ai_drain::is_owner(ctx.state(), ctx.user_id()).await?;
    let caller = Caller::new(ctx.user_id(), owner);
    if kind == RunKind::Clusters && plan.groups.is_none() {
        match prepare_groups(&ctx, caller, &plan).await {
            Ok(Some(groups)) => {
                plan.groups = Some(groups);
                persist(&ctx, plan.clone()).await?;
            }
            Ok(None) => return Ok(Outcome::Requeue { run_at: None }),
            Err(StepError::Provider(error)) if held(&error) => return Ok(hold(&ctx)),
            Err(StepError::Provider(error)) => {
                return Err(crate::error::ApiError::from(error).into());
            }
            Err(StepError::Job(error)) => return Err(error),
        }
    }
    let total = match kind {
        RunKind::Clusters => plan.groups.as_ref().expect("prepared groups").len(),
        RunKind::Aliases => plan.tags.len().div_ceil(40),
    };
    while plan.next < total {
        if ctx.should_yield() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        let stage = match kind {
            RunKind::Clusters => "refine",
            RunKind::Aliases => "aliases",
        };
        progress(&ctx, stage, plan.next, total).await;
        let request = match kind {
            RunKind::Clusters => taxonomy_prompt::refine(&plan.groups.as_ref().unwrap()[plan.next]),
            RunKind::Aliases => taxonomy_prompt::aliases(
                &plan.tags[plan.next * 40..((plan.next + 1) * 40).min(plan.tags.len())],
                &plan.vocabulary,
            ),
        }
        .map_err(|_| JobError::permanent("invalid_payload"))?;
        let result = chat(&ctx, caller, kind.task(), request).await;
        if ctx.should_yield() {
            return Ok(Outcome::Requeue { run_at: None });
        }
        if let Err(error) = &result
            && held(error)
        {
            return Ok(hold(&ctx));
        }
        let (groups, pairs) = match kind {
            RunKind::Clusters => {
                let group = &plan.groups.as_ref().unwrap()[plan.next];
                let groups = match result {
                    Ok(value) => clusters::validate_refined_groups(&group.tags, &value),
                    Err(AiServiceError::Call(e)) if e.kind() == ErrorKind::Cancelled => {
                        return Err(JobError::cancelled());
                    }
                    Err(AiServiceError::Call(_)) => vec![RefinedGroup {
                        label: group.tags[0].clone(),
                        tags: group.tags.clone(),
                    }],
                    Err(error) => return Err(crate::error::ApiError::from(error).into()),
                };
                (groups, vec![])
            }
            RunKind::Aliases => {
                let value = result.map_err(|e| JobError::from(crate::error::ApiError::from(e)))?;
                let batch = &plan.tags[plan.next * 40..((plan.next + 1) * 40).min(plan.tags.len())];
                (
                    vec![],
                    aliases::validate_pairs(batch, &plan.vocabulary, &value),
                )
            }
        };
        let fence = ctx.attempt_fence();
        let mut updated = plan.clone();
        let now = ctx.jobs().clock().now_ms();
        plan = library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
            if !fence.is_current().map_err(|_| lost())? {
                return Err(lost());
            }
            if kind == RunKind::Clusters && !updated.initialized {
                clusters::clear_proposals(tx, now)?;
            }
            updated.proposed += match kind {
                RunKind::Clusters => clusters::append_proposals(tx, &groups, id, now)?.count,
                RunKind::Aliases => aliases::save_proposals(tx, &pairs, now)?,
            };
            updated.initialized = true;
            updated.next += 1;
            runs::save(tx, id, &updated, now)?;
            Ok(Change::collections(updated))
        })
        .await?
        .value;
        progress(&ctx, stage, plan.next, total).await;
    }
    if ctx.should_yield() {
        return Ok(Outcome::Requeue { run_at: None });
    }
    let fence = ctx.attempt_fence();
    let now = ctx.jobs().clock().now_ms();
    let notification = library::write(ctx.state(), ctx.user_id(), ChangeReason::Ai, move |tx| {
        if !fence.is_current().map_err(|_| lost())? {
            return Err(lost());
        }
        if kind == RunKind::Clusters && !plan.initialized {
            clusters::clear_proposals(tx, now)?;
        }
        let notification = notifications::create(
            tx,
            &NewNotification {
                kind: "ai".into(),
                code: "ai.run_finished".into(),
                params: serde_json::json!({"kind":kind.name(),"proposed":plan.proposed})
                    .as_object()
                    .unwrap()
                    .clone(),
                target: Some("/jobs?kind=ai.run".into()),
            },
            now,
        )?;
        plan.finished = true;
        // Retain only the tiny completed marker so retry/recovery is idempotent.
        plan.tags.clear();
        plan.vocabulary.clear();
        plan.groups = Some(vec![]);
        runs::save(tx, id, &plan, now)?;
        Ok(Change::collections(notification))
    })
    .await?
    .value;
    ctx.state()
        .events()
        .notification(ctx.user_id(), &Notification::from(notification));
    progress(&ctx, "complete", total, total).await;
    Ok(Outcome::Succeeded)
}

async fn chat(
    ctx: &JobContext,
    caller: Caller<'_>,
    task: Task,
    prompt: TaxonomyRequest,
) -> Result<serde_json::Value, AiServiceError> {
    let route = ctx.state().ai().route(ctx.state(), caller, task).await?;
    let request = ChatRequest::new(route.model, vec![Message::user_text(prompt.user)])
        .with_system(prompt.system)
        .with_temperature(prompt.temperature)
        .with_max_tokens(prompt.max_tokens)
        .with_json(
            JsonOutput::from_raw(prompt.schema.name, &prompt.schema.schema)
                .map_err(AiServiceError::Call)?,
        );
    let response = ctx
        .state()
        .ai()
        .chat(
            ctx.state(),
            caller,
            task,
            &request,
            CallHints::default().with_cancel(ctx.token().clone()),
        )
        .await?;
    Ok(response.json.unwrap_or_else(|| {
        clusters::parse_refine_response(&serde_json::Value::String(response.text))
    }))
}

enum StepError {
    Provider(AiServiceError),
    Job(JobError),
}
impl From<AiServiceError> for StepError {
    fn from(e: AiServiceError) -> Self {
        Self::Provider(e)
    }
}
impl From<JobError> for StepError {
    fn from(e: JobError) -> Self {
        Self::Job(e)
    }
}

async fn prepare_groups(
    ctx: &JobContext,
    caller: Caller<'_>,
    plan: &Plan,
) -> Result<Option<Vec<graph::CandidateGroup>>, StepError> {
    let tags = plan.tags.iter().map(|t| t.norm.clone()).collect::<Vec<_>>();
    let route = ctx
        .state()
        .ai()
        .route(ctx.state(), caller, Task::Embed)
        .await;
    let mut vectors = None;
    if let Ok(route) = route {
        let model = serde_json::json!([route.provider.id(), route.model]).to_string();
        let key = model.clone();
        let input = tags.clone();
        let mut cached = ctx
            .user_db(move |db| {
                db.read(|conn| embeddings::cached(conn, &key, &input))
                    .map_err(JobError::from)
            })
            .await?;
        let missing = tags
            .iter()
            .filter(|t| !cached.contains_key(*t))
            .cloned()
            .collect::<Vec<_>>();
        let mut failed = false;
        for (i, batch) in missing.chunks(32).enumerate() {
            if ctx.should_yield() {
                return Ok(None);
            }
            progress(ctx, "embed", i * 32, missing.len()).await;
            let result = ctx
                .state()
                .ai()
                .embed(
                    ctx.state(),
                    caller,
                    &EmbedRequest {
                        model: route.model.clone(),
                        input: batch.to_vec(),
                        dimensions: None,
                    },
                    CallHints::default().with_cancel(ctx.token().clone()),
                )
                .await;
            if ctx.should_yield() {
                return Ok(None);
            }
            match result {
                Err(error) if held(&error) => return Err(error.into()),
                Err(_) => {
                    failed = true;
                    break;
                }
                Ok(response) => {
                    let fence = ctx.attempt_fence();
                    let key = model.clone();
                    let input = batch.to_vec();
                    let saved = ctx
                        .user_db(move |db| {
                            db.write(|tx| {
                                if !fence.is_current().map_err(|_| lost())? {
                                    return Err(lost());
                                }
                                embeddings::save(tx, &key, &input, &response.vectors)
                            })
                            .map_err(JobError::from)
                        })
                        .await;
                    if let Err(error) = saved {
                        if error.code() == "validation_failed" {
                            failed = true;
                            break;
                        }
                        return Err(error.into());
                    }
                }
            }
        }
        if !failed {
            let key = model.clone();
            let input = tags.clone();
            cached = ctx
                .user_db(move |db| {
                    db.read(|conn| embeddings::cached(conn, &key, &input))
                        .map_err(JobError::from)
                })
                .await?;
            vectors = Some(cached);
        }
    } else if let Err(error) = route
        && !matches!(error, AiServiceError::NotConfigured)
    {
        return Err(error.into());
    }
    progress(ctx, "group", 0, 1).await;
    let groups = ctx
        .user_db(move |db| {
            db.read(|conn| {
                graph::candidate_groups(conn, vectors.as_ref(), graph::Options::default())
            })
            .map_err(JobError::from)
        })
        .await?;
    Ok(Some(groups))
}
