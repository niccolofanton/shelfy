//! Opt-in screenshot loading-state QC. No capture is blocked by a provider
//! outage, missing route, malformed response or corrupt image.
use super::{CallHints, Caller, Task};
use crate::state::{AppState, blocking};
use serde::Serialize;
use shelfy_ai::{ChatRequest, Image, ImageType, JsonOutput, Message, Part};
use shelfy_core::ai::{
    prompts::{self, Task as PromptTask},
    sanitize,
};
use shelfy_media::{pool::ImagePool, render};
use std::sync::Arc;
#[derive(Clone, Debug, Serialize)]
pub struct Assessment {
    pub ok: bool,
    pub status: String,
    pub reason: Option<String>,
    pub ready: bool,
}
impl Assessment {
    fn open(ready: bool) -> Self {
        Self {
            ok: true,
            status: "unknown".into(),
            reason: None,
            ready,
        }
    }
}
/// Assesses the top square. The feature flag and vision route are checked
/// here, so capture callers can invoke this seam unconditionally.
pub async fn assess(state: &AppState, user_id: &str, image: Arc<[u8]>) -> Assessment {
    assess_inner(state, user_id, image)
        .await
        .unwrap_or_else(|| Assessment::open(true))
}
async fn assess_inner(state: &AppState, user_id: &str, image: Arc<[u8]>) -> Option<Assessment> {
    let db = state.user_db(user_id).await.ok()?;
    let enabled = blocking(move || db.read(shelfy_core::repo::settings::read))
        .await
        .ok()?
        .ai
        .vision_qc;
    if !enabled {
        return Some(Assessment::open(false));
    }
    let owner = crate::jobs::ai_drain::is_owner(state, user_id).await.ok()?;
    let caller = Caller::new(user_id, owner);
    let route = match state.ai().route(state, caller, Task::Qc).await {
        Ok(route) => route,
        Err(_) => return Some(Assessment::open(false)),
    };
    let jpeg = ImagePool::shared()
        .run(move || render::jpeg_top_square_bytes(&image, 768, 85))
        .await
        .ok()?
        .ok()?;
    let task = PromptTask::Qc;
    let schema = prompts::response_schema(task)?;
    let request = ChatRequest::new(
        route.model,
        vec![Message::user(vec![
            Part::Text(prompts::user_prompt(task, &[]).ok()?),
            Part::Image(Image {
                media_type: ImageType::Jpeg,
                data: jpeg.into(),
            }),
        ])],
    )
    .with_system(prompts::system_prompt(task, &[]).ok()?)
    .with_temperature(prompts::spec(task).temperature)
    .with_max_tokens(prompts::max_tokens(task, 0))
    .with_json(JsonOutput::from_raw(schema.name, &schema.schema).ok()?);
    let response = state
        .ai()
        .chat(state, caller, Task::Qc, &request, CallHints::new())
        .await
        .ok()?;
    let value = response
        .json
        .or_else(|| serde_json::from_str(&response.text).ok())?;
    let status = value.get("status").and_then(serde_json::Value::as_str)?;
    if !["ok", "black", "blank", "loading", "partial"].contains(&status) {
        return None;
    }
    Some(Assessment {
        ok: status == "ok",
        status: status.into(),
        reason: value
            .get("reason")
            .and_then(serde_json::Value::as_str)
            .map(|s| sanitize::text(s, 240)),
        ready: true,
    })
}
