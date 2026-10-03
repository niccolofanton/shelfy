//! Non-streamed chips: ordinary AI routing/consent and durable 24h cache.
use super::{AiServiceError, CallHints, Caller, Task};
use crate::{
    error::{ApiError, ErrorCode},
    ids::now_ms,
    state::{AppState, blocking},
};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use shelfy_ai::{ChatRequest, JsonOutput, Message};
use shelfy_core::{ai::suggest as core, repo::posts::SourceBucket};
use utoipa::ToSchema;

#[derive(Clone, Debug, Serialize, Deserialize, ToSchema)]
pub struct SuggestResult {
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}
impl SuggestResult {
    fn empty(reason: &str) -> Self {
        Self {
            tags: Vec::new(),
            reason: Some(reason.into()),
        }
    }
}
fn unavailable(error: AiServiceError) -> SuggestResult {
    SuggestResult::empty(match error {
        AiServiceError::NotConfigured => "ai_not_configured",
        AiServiceError::ConsentRequired(_) => "ai_consent_required",
        AiServiceError::Call(ref e) => e.kind().as_str(),
    })
}

pub async fn suggest(
    state: &AppState,
    user: &str,
    query: String,
    source: Option<SourceBucket>,
) -> Result<SuggestResult, ApiError> {
    if core::is_blank(&query) {
        return Ok(SuggestResult::empty("empty_query"));
    }
    let owner = crate::jobs::ai_drain::is_owner(state, user).await?;
    let caller = Caller::new(user, owner);
    let route = match state.ai().route(state, caller, Task::Suggest).await {
        Ok(route) => route,
        Err(error) => return Ok(unavailable(error)),
    };
    // Check current consent/configuration and offline state before cache hits:
    // a revoked provider must not be treated as a usable suggestion route.
    let providers = state.ai().list_providers(state, caller).await?;
    let Some(provider) = providers
        .iter()
        .find(|p| p.id == route.provider.id() && p.configured)
    else {
        return Ok(SuggestResult::empty("ai_not_configured"));
    };
    if !provider.managed
        && !provider
            .consent
            .as_ref()
            .is_some_and(|c| c.version == super::providers::CONSENT_VERSION)
    {
        return Ok(SuggestResult::empty("ai_consent_required"));
    }
    if provider.status == crate::events::model::ProviderState::Offline {
        return Ok(SuggestResult::empty("provider_offline"));
    }
    let db = state.user_db(user).await?;
    let reader = db.clone();
    let text = query.clone();
    let (key, cached) = blocking(move || reader.read(|conn| {
        let key = core::cache_key(conn, &text, source)?;
        let raw: Option<String> = conn.query_row(
            "SELECT value_json FROM ai_cache WHERE kind='suggest' AND key_hash=?1 AND created_at>?2 AND created_at<=?3",
            params![key, now_ms().saturating_sub(core::TTL_MS), now_ms()], |r| r.get(0)
        ).optional()?;
        let cached = raw.and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
            .map(|tags| {
                let live: std::collections::HashSet<_> = core::intersect(conn, source, &tags)?
                    .into_iter().map(|tag| tag.trim().to_lowercase()).collect();
                // These are already canonical forms from the same vocabulary
                // generation. Revalidate membership without reformatting an
                // accepted alias's display form via identity resolution.
                Ok::<_,shelfy_core::repo::RepoError>(tags.into_iter()
                    .filter(|tag| live.contains(&tag.trim().to_lowercase()))
                    .take(core::MAX_TAGS).collect::<Vec<_>>())
            }).transpose()?;
        Ok::<_,shelfy_core::repo::RepoError>((key, cached))
    })).await?;
    if let Some(tags) = cached {
        return Ok(SuggestResult { tags, reason: None });
    }
    let prompt = core::request(&query).map_err(|_| ApiError::new(ErrorCode::Internal))?;
    let request = ChatRequest::new(&route.model, vec![Message::user_text(prompt.user)])
        .with_system(prompt.system)
        .with_temperature(prompt.temperature)
        .with_max_tokens(prompt.max_tokens)
        .with_json(
            JsonOutput::from_raw(prompt.schema.name, &prompt.schema.schema)
                .map_err(|_| ApiError::new(ErrorCode::Internal))?,
        );
    let cancel = state.shutdown_token().child_token();
    let mut hints = CallHints::new().with_cancel(cancel.clone());
    hints.provider_id = Some(route.provider.id().to_owned());
    let call = state
        .ai()
        .chat(state, caller, Task::Suggest, &request, hints);
    // Leave room below the standard route's 30s timeout to preserve the 200
    // empty-result contract even for an unreachable or occupied node.
    let result = tokio::select! {
        () = cancel.cancelled() => None,
        result = tokio::time::timeout(std::time::Duration::from_secs(20), call) => result.ok(),
    };
    cancel.cancel();
    let answer = match result {
        Some(Ok(answer)) => answer,
        Some(Err(error)) => return Ok(unavailable(error)),
        None => return Ok(SuggestResult::empty("provider_unavailable")),
    };
    let Some(candidates) = core::parse(&answer.text) else {
        return Ok(SuggestResult::empty("invalid_response"));
    };
    blocking(move || db.write(|conn| {
        let tags = core::intersect(conn, source, &candidates)?;
        // A post edit during the call must invalidate the old generation,
        // rather than cache this older answer under the edited vocabulary.
        if core::cache_key(conn, &query, source)? == key {
            conn.execute("DELETE FROM ai_cache WHERE kind='suggest' AND created_at<=?1", [now_ms().saturating_sub(core::TTL_MS)])?;
            conn.execute("INSERT INTO ai_cache(kind,key_hash,value_json,created_at) VALUES('suggest',?1,?2,?3)
                ON CONFLICT(kind,key_hash) DO UPDATE SET value_json=excluded.value_json,created_at=excluded.created_at",
                params![key, serde_json::to_string(&tags).expect("tags serialize"), now_ms()])?;
        }
        Ok::<_,shelfy_core::repo::RepoError>(SuggestResult { tags, reason: None })
    })).await
}
