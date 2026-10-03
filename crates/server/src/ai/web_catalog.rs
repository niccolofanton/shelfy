//! Website catalog calls: the current capture's hero, bands and inert digest.
use super::{AiServiceError, CallHints, Caller, Task};
use crate::jobs::{JobContext, JobError};
use shelfy_ai::{ChatRequest, ErrorKind, Image, ImageType, JsonOutput, Message, Part};
use shelfy_core::ai::{
    catalog::CatalogRequest,
    prompts::{self, Task as PromptTask},
    template::Var,
    web_design, web_inputs,
};
use shelfy_core::repo::posts::AiPatch;
use shelfy_media::{digest::Digest, kind::MediaKind, pool::ImagePool, store::MediaStore};

pub enum CatalogError {
    Provider(AiServiceError),
    Media,
    Schema,
}
/// Builds from the shared template/schema, using P3-27's 8k page-digest budget
/// rather than the social/desktop caption's 1,200-unit limit.
fn prompt(
    input: &web_inputs::WebInputs,
    frames: bool,
) -> Result<CatalogRequest, prompts::PromptError> {
    let task = PromptTask::WebDesign;
    let schema = prompts::response_schema(task).expect("web design schema");
    let ground = web_design::ground(&input.post);
    Ok(CatalogRequest {
        system: prompts::system_prompt(task, &[])?,
        user: prompts::user_prompt(
            task,
            &[
                ("frames", Var::Flag(frames)),
                ("digest", Var::Text(&input.digest)),
                ("ground", Var::Text(&ground.to_string())),
            ],
        )?,
        schema,
        temperature: prompts::spec(task).temperature,
        max_tokens: prompts::max_tokens(task, 0),
    })
}
/// Runs one website item. The core queue guards the write against recaptures.
pub async fn catalog(
    ctx: &JobContext,
    post_id: i64,
    caller: Caller<'_>,
) -> Result<AiPatch, CatalogError> {
    let input = ctx
        .user_db(move |db| {
            db.read(|c| web_inputs::select(c, post_id))
                .map_err(JobError::from)
        })
        .await
        .map_err(|_| CatalogError::Media)?
        .ok_or(CatalogError::Media)?;
    let store = MediaStore::new(ctx.state().config().data_dir.users_dir())
        .user(ctx.user_id())
        .map_err(|_| CatalogError::Media)?;
    let mut images = vec![];
    for obj in &input.frames {
        if ctx.should_yield() {
            return Err(CatalogError::Provider(AiServiceError::Call(
                shelfy_ai::AiError::new(ErrorKind::Cancelled, "cataloging was cancelled"),
            )));
        }
        let Some(digest) = Digest::from_slice(&obj.sha256) else {
            continue;
        };
        let Some(kind) = MediaKind::from_ext(&obj.ext) else {
            continue;
        };
        if let Ok(jpeg) = ImagePool::shared()
            .jpeg_file(store.object_path(&digest, kind), 768, 85)
            .await
        {
            images.push(jpeg);
        }
    }
    if images.is_empty() && input.digest.is_empty() {
        return Err(CatalogError::Media);
    }
    let route = ctx
        .state()
        .ai()
        .route(ctx.state(), caller, Task::Catalog)
        .await
        .map_err(CatalogError::Provider)?;
    let prompt = prompt(&input, !images.is_empty()).map_err(|_| CatalogError::Schema)?;
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
                .map_err(|_| CatalogError::Schema)?,
        );
    let started = tokio::time::Instant::now();
    let response = ctx
        .state()
        .ai()
        .chat(
            ctx.state(),
            caller,
            Task::Catalog,
            &request,
            CallHints::new().with_cancel(ctx.token().clone()),
        )
        .await
        .map_err(CatalogError::Provider)?;
    super::queue::record_duration(started.elapsed().as_millis() as u64);
    let raw = response
        .json
        .or_else(|| serde_json::from_str(&response.text).ok())
        .ok_or(CatalogError::Schema)?;
    if !web_design::valid(&raw) {
        return Err(CatalogError::Schema);
    }
    Ok(web_design::patch(
        &web_design::map(&raw, &input.post, &route.model),
        route.provider.id(),
        &route.model,
    ))
}
