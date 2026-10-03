//! [`AiService`]: the one entry point of AI calls (plan §2.15, P3-09).
//!
//! A call goes: resolve a provider and model for the [`Task`](super::Task),
//! check consent, take the global and provider limits, check the breaker,
//! run `shelfy-ai`'s adapter, record usage and the metric, and update the
//! provider's status. The operator provider (the owner's node) is prebuilt
//! from the environment; its key never leaves `shelfy-ai`'s request.
//!
//! Limits (plan §2.15 "Reliability", G3-29):
//! - at most [`GLOBAL_IN_FLIGHT`] AI calls in flight over the whole server;
//! - the operator provider: `SHELFY_OPERATOR_AI_CONCURRENCY` (1 or 2) in
//!   flight to the node, with a 500 ms gap between the starts of its calls;
//! - a BYOK provider: `aiConcurrency` (1–8) per user.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use rusqlite::{OptionalExtension as _, params};
use serde::Serialize;
use serde_json::{Map, Value};
use shelfy_ai::{
    AiError, CallOptions, ChatRequest, ChatResponse, EgressPolicy, EmbedRequest, Embeddings,
    ErrorKind, Provider, ProviderConfig, ProviderKind, RetryPolicy, Source, TextCallback, Timeouts,
    TranscribeRequest, Transcript, Transport, Usage,
};
use shelfy_core::repo::RepoError;
use shelfy_core::repo::notifications::NewNotification;
use shelfy_core::repo::settings::{self, AiProvider, AiProviderKind, AiSettings};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;
use utoipa::ToSchema;

use super::breaker::{Admission, CallOutcome, OperatorBreaker, Transition, UserBreakers};
use super::operator::{CONNECT_TIMEOUT, OPERATOR_PROVIDER_ID, OperatorConfig};
use super::{AI_DRAIN_KIND, AiServiceError, Caller, ProviderRef, Task};
use crate::control::{audit, usage_daily};
use crate::error::ApiError;
use crate::events::model::{ProviderState, ProviderStatusEvent};
use crate::ids::now_ms;
use crate::outbound::ai::AiTransport;
use crate::outbound::{OriginAllowlist, Outbound};
use crate::state::{AppState, blocking};
use crate::telemetry::metrics;

/// AI calls in flight over the whole server (plan §2.15).
pub const GLOBAL_IN_FLIGHT: usize = 32;
/// The gap between the starts of two operator calls (G3-29).
pub const OPERATOR_GAP: Duration = Duration::from_millis(500);
/// How often an offline operator is re-probed.
pub const PROBE_INTERVAL: Duration = Duration::from_secs(60);
/// The chat first-token deadline.
const CHAT_FIRST_TOKEN: Duration = Duration::from_secs(20);

/// The resolved provider and model for one task.
#[derive(Clone, Debug)]
pub struct Route {
    /// Which provider serves it.
    pub provider: ProviderRef,
    /// The protocol.
    pub kind: ProviderKind,
    /// The model id to send (empty for whisper.cpp, which ignores it).
    pub model: String,
    /// The task.
    pub task: Task,
}

/// Per-call extras the caller supplies: cancellation and the streaming
/// callback. Timeouts, retries and the structured mode come from the service.
#[derive(Clone, Default)]
pub struct CallHints {
    /// Cancels the call.
    pub cancel: Option<CancellationToken>,
    /// Streamed text so far (chat).
    pub on_text: Option<TextCallback>,
    /// Explicit account-scoped provider override (chat search).
    pub provider_id: Option<String>,
}

impl CallHints {
    /// No cancellation, no callback.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Uses this provider if it belongs to the caller and serves the task.
    #[must_use]
    pub fn with_provider(mut self, id: String) -> Self {
        self.provider_id = Some(id);
        self
    }

    /// Cancelled by `token`.
    #[must_use]
    pub fn with_cancel(mut self, token: CancellationToken) -> Self {
        self.cancel = Some(token);
        self
    }

    /// Streamed text goes to `callback`.
    #[must_use]
    pub fn with_text_callback(mut self, callback: TextCallback) -> Self {
        self.on_text = Some(callback);
        self
    }
}

/// The model ids a provider offers, as `GET /me/providers` shows them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModels {
    /// The text model.
    #[schema(required = true)]
    pub text: Option<String>,
    /// The vision model (cataloging and QC).
    #[schema(required = true)]
    pub vision: Option<String>,
    /// The embedding model.
    #[schema(required = true)]
    pub embed: Option<String>,
}

/// One provider as `GET /me/providers` lists it: no key, and the operator
/// provider is `managed` (no edit or delete).
#[derive(Clone, Debug, PartialEq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSummary {
    /// The provider id (`operator`, or the user's id).
    pub id: String,
    /// The protocol (`operator`, `openai_compatible`, `anthropic`).
    pub kind: String,
    /// The display name.
    pub label: String,
    /// Whether the server manages it (the operator provider): no key, no edit.
    pub managed: bool,
    /// The models per use.
    pub models: ProviderModels,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_models: Option<super::providers::TaskModels>,
    /// Whether it transcribes (dictation).
    pub stt: bool,
    /// Its current state.
    pub status: ProviderState,
    pub configured: bool,
    pub consent_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last4: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prices: Option<super::providers::ProviderPrices>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consent: Option<super::providers::ProviderConsent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test: Option<super::providers::ProviderTest>,
}

/// The built operator provider.
struct OperatorRuntime {
    /// The OpenAI-compatible chat and embeddings provider.
    chat: Provider,
    /// The whisper.cpp provider, if `SHELFY_OPERATOR_STT_URL` is set.
    whisper: Option<Provider>,
    /// The text model id.
    text_model: String,
    /// The vision model id.
    vision_model: Option<String>,
    /// The embedding model id.
    embed_model: Option<String>,
    /// The display name.
    label: String,
    /// The per-call timeout.
    timeout: Duration,
}

/// The operator pacing gate: a concurrency semaphore and a 500 ms gap between
/// the starts of calls.
struct OperatorGate {
    sem: Arc<Semaphore>,
    gap: Duration,
    last_start: tokio::sync::Mutex<Option<Instant>>,
}

impl OperatorGate {
    fn new(concurrency: u8, gap: Duration) -> Self {
        Self {
            sem: Arc::new(Semaphore::new(usize::from(concurrency.max(1)))),
            gap,
            last_start: tokio::sync::Mutex::new(None),
        }
    }

    /// Holds a slot, then waits so this call starts at least `gap` after the
    /// previous one. The returned permit bounds the calls in flight.
    async fn acquire(&self) -> OwnedSemaphorePermit {
        let permit = Arc::clone(&self.sem)
            .acquire_owned()
            .await
            .expect("the operator gate semaphore is never closed");
        let wait = {
            let mut last = self.last_start.lock().await;
            let now = Instant::now();
            let start = last.map_or(now, |prev| (prev + self.gap).max(now));
            *last = Some(start);
            start.saturating_duration_since(now)
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        permit
    }
}

/// One semaphore survives concurrency edits, so old calls remain counted.
struct UserGate {
    sem: Arc<Semaphore>,
    reserved: tokio::sync::Mutex<Option<OwnedSemaphorePermit>>,
}
impl UserGate {
    fn new() -> Self {
        Self {
            sem: Arc::new(Semaphore::new(8)),
            reserved: tokio::sync::Mutex::new(None),
        }
    }
    async fn acquire(&self, concurrency: u8) -> OwnedSemaphorePermit {
        let desired = usize::from(8 - concurrency.clamp(1, 8));
        let mut reserved = self.reserved.lock().await;
        let current = reserved
            .as_ref()
            .map_or(0, OwnedSemaphorePermit::num_permits);
        if desired > current {
            let extra = Arc::clone(&self.sem)
                .acquire_many_owned(u32::try_from(desired - current).expect("at most 7 permits"))
                .await
                .expect("the user gate is never closed");
            if let Some(existing) = reserved.as_mut() {
                existing.merge(extra);
            } else {
                *reserved = Some(extra);
            }
        } else if desired < current {
            drop(
                reserved
                    .as_mut()
                    .expect("reserved slots exist")
                    .split(current - desired),
            );
        }
        drop(reserved);
        Arc::clone(&self.sem)
            .acquire_owned()
            .await
            .expect("the user gate is never closed")
    }
}

/// A held call: its provider and the permits it holds until it finishes.
struct CallGuard {
    provider: Provider,
    cancel: Option<CancellationToken>,
    _global: OwnedSemaphorePermit,
    _slot: OwnedSemaphorePermit,
}

struct Inner {
    operator: Option<OperatorRuntime>,
    operator_breaker: OperatorBreaker,
    user_breakers: UserBreakers,
    global: Arc<Semaphore>,
    operator_gate: OperatorGate,
    user_gates: Mutex<HashMap<String, Arc<UserGate>>>,
    /// (user, provider) pairs whose operator consent is recorded this process.
    consented: Mutex<std::collections::HashSet<(String, String)>>,
    user_statuses: Mutex<HashMap<(String, String), ProviderState>>,
    vault_on: bool,
    probe_interval: Duration,
    last_probe: Mutex<Option<Instant>>,
}

/// The AI service. Cheap to clone; everything is behind one `Arc`.
#[derive(Clone)]
pub struct AiService {
    inner: Arc<Inner>,
}

impl AiService {
    /// Builds the service from the operator settings, the egress allowlist
    /// (the operator endpoints' origins), the outbound client and whether the
    /// key vault is on.
    ///
    /// # Errors
    ///
    /// The operator endpoints are set but the egress guard refuses them (the
    /// allowlist and the operator URLs disagree): the start stops.
    pub fn new(
        config: &OperatorConfig,
        allow: &OriginAllowlist,
        outbound: &Outbound,
        vault_on: bool,
    ) -> anyhow::Result<Self> {
        let transport: Arc<dyn Transport> = Arc::new(AiTransport::new(outbound));
        let mut policy = EgressPolicy::new();
        for origin in allow.iter() {
            if let Ok(origin) = shelfy_ai::Origin::parse(&origin.to_string()) {
                policy = policy.allow(origin);
            }
        }
        let operator = Self::build_operator(config, &policy, &transport)?;
        let gate = OperatorGate::new(config.concurrency, OPERATOR_GAP);
        Ok(Self {
            inner: Arc::new(Inner {
                operator,
                operator_breaker: OperatorBreaker::new(),
                user_breakers: UserBreakers::new(),
                global: Arc::new(Semaphore::new(GLOBAL_IN_FLIGHT)),
                operator_gate: gate,
                user_gates: Mutex::new(HashMap::new()),
                consented: Mutex::new(std::collections::HashSet::new()),
                user_statuses: Mutex::new(HashMap::new()),
                vault_on,
                probe_interval: PROBE_INTERVAL,
                last_probe: Mutex::new(None),
            }),
        })
    }

    fn build_operator(
        config: &OperatorConfig,
        policy: &EgressPolicy,
        transport: &Arc<dyn Transport>,
    ) -> anyhow::Result<Option<OperatorRuntime>> {
        let Some(url) = config.url.clone() else {
            return Ok(None);
        };
        let mut ai = ProviderConfig::new(ProviderKind::OpenAiCompatible, Source::Operator, url)
            .with_llama_health()
            .without_webp()
            // qwen thinks by default; off, the answer is about 5× faster. The
            // kwarg is ignored by templates that do not use it (P3-01).
            .with_extra(
                "chat_template_kwargs",
                serde_json::json!({ "enable_thinking": false }),
            );
        if let Some(key) = config.key.clone() {
            ai = ai.with_key(key);
        }
        let chat = Provider::new(ai, policy, Arc::clone(transport)).map_err(|e| {
            anyhow::anyhow!(
                "SHELFY_OPERATOR_AI_URL is refused by the egress guard; its origin must be in \
                 SHELFY_EGRESS_ALLOW_ORIGINS ({e})"
            )
        })?;
        let whisper = match config.stt_url.clone() {
            Some(stt_url) => {
                let mut w =
                    ProviderConfig::new(ProviderKind::WhisperCpp, Source::Operator, stt_url);
                if let Some(key) = config.stt_key.clone() {
                    w = w.with_key(key);
                }
                Some(
                    Provider::new(w, policy, Arc::clone(transport)).map_err(|e| {
                        anyhow::anyhow!(
                            "SHELFY_OPERATOR_STT_URL is refused by the egress guard ({e})"
                        )
                    })?,
                )
            }
            None => None,
        };
        Ok(Some(OperatorRuntime {
            chat,
            whisper,
            text_model: config.model.clone().unwrap_or_default(),
            vision_model: config.vision_model.clone(),
            embed_model: config.embed_model.clone(),
            label: config.label.clone(),
            timeout: config.timeout,
        }))
    }

    /// Whether AI is configured at all: the operator provider is on, or the
    /// vault is on (so a user may add a BYOK provider). Feeds `ai.tasks` in
    /// `GET /me` (G3-11).
    #[must_use]
    pub fn is_configured(&self) -> bool {
        self.inner.operator.is_some() || self.inner.vault_on
    }

    /// Whether the operator provider is configured.
    #[must_use]
    pub fn operator_configured(&self) -> bool {
        self.inner.operator.is_some()
    }

    /// The operator provider's current state, when it is configured.
    #[must_use]
    pub fn operator_state(&self) -> Option<ProviderState> {
        self.inner
            .operator
            .as_ref()
            .map(|_| self.inner.operator_breaker.state())
    }

    /// The providers `GET /me/providers` lists for `caller`: the operator
    /// provider (owner only, E4) and the user's BYOK providers (P3-19). No
    /// keys.
    ///
    /// # Errors
    ///
    /// The settings cannot be read.
    pub async fn list_providers(
        &self,
        state: &AppState,
        caller: Caller<'_>,
    ) -> Result<Vec<ProviderSummary>, ApiError> {
        let mut out = Vec::new();
        if let (true, Some(op)) = (caller.owner, self.inner.operator.as_ref()) {
            self.inner.operator_breaker.observe(caller.id);
            out.push(ProviderSummary {
                id: OPERATOR_PROVIDER_ID.to_owned(),
                kind: "operator".to_owned(),
                label: op.label.clone(),
                managed: true,
                models: ProviderModels {
                    text: Some(op.text_model.clone()),
                    vision: op.vision_model.clone(),
                    embed: op.embed_model.clone(),
                },
                task_models: None,
                stt: op.whisper.is_some(),
                status: self.inner.operator_breaker.state(),
                configured: true,
                consent_version: super::providers::CONSENT_VERSION.to_owned(),
                last4: None,
                base_url: None,
                prices: None,
                consent: None,
                test: None,
            });
        }
        let _lock = state.byok().lock(caller.id).await;
        let settings = self.read_ai_settings(state, caller.id).await;
        let now = Instant::now();
        for provider in &settings.providers {
            let control = Arc::clone(state.control());
            let user = caller.id.to_owned();
            let id = provider.id.clone();
            let sealed = blocking(move || {
                control.read(|c| crate::control::provider_keys::get(c, &user, &id))
            })
            .await?;
            out.push(ProviderSummary {
                id: provider.id.clone(),
                kind: match provider.kind {
                    AiProviderKind::OpenaiCompatible => "openai_compatible",
                    AiProviderKind::Anthropic => "anthropic",
                }
                .to_owned(),
                label: provider.label.clone(),
                managed: false,
                models: ProviderModels {
                    text: provider.models.chat.clone(),
                    vision: provider.models.catalog.clone(),
                    embed: provider.models.embed.clone(),
                },
                task_models: Some(provider.models.clone().into()),
                stt: provider.models.stt.is_some(),
                status: if self.inner.user_breakers.state(caller.id, &provider.id, now)
                    == ProviderState::Down
                {
                    ProviderState::Down
                } else {
                    self.inner
                        .user_statuses
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .get(&(caller.id.to_owned(), provider.id.clone()))
                        .copied()
                        .unwrap_or_else(|| {
                            provider
                                .test
                                .as_ref()
                                .map_or(ProviderState::Ok, super::providers::probe_status)
                        })
                },
                configured: sealed.is_some(),
                consent_version: super::providers::CONSENT_VERSION.to_owned(),
                last4: sealed.map(|k| k.sealed.last4),
                base_url: Some(provider.base_url.clone()),
                prices: provider.prices.map(Into::into),
                consent: provider.consent.clone().map(Into::into),
                test: provider.test.clone().map(Into::into),
            });
        }
        Ok(out)
    }

    /// Resolves the provider and model for `task`, honoring the user's routing
    /// override and falling back to the operator provider (plan §2.15).
    ///
    /// # Errors
    ///
    /// [`AiServiceError::NotConfigured`] when nothing can serve the task.
    pub async fn route(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        task: Task,
    ) -> Result<Route, AiServiceError> {
        let settings = self.read_ai_settings(state, caller.id).await;
        match settings.routing.for_task(task.as_str()) {
            Some(OPERATOR_PROVIDER_ID) => self
                .operator_route(caller, task)
                .ok_or(AiServiceError::NotConfigured),
            Some(id) => self
                .user_route(&settings, id, task)
                .ok_or(AiServiceError::NotConfigured),
            None => self
                .operator_route(caller, task)
                .or_else(|| self.first_user_route(&settings, task))
                .ok_or(AiServiceError::NotConfigured),
        }
    }

    /// Resolves an optional explicit provider, retaining ownership/model checks.
    pub async fn route_for_provider(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        task: Task,
        id: Option<&str>,
    ) -> Result<Route, AiServiceError> {
        match id {
            None => self.route(state, caller, task).await,
            Some(OPERATOR_PROVIDER_ID) => self
                .operator_route(caller, task)
                .ok_or(AiServiceError::NotConfigured),
            Some(id) => {
                let settings = self.read_ai_settings(state, caller.id).await;
                self.user_route(&settings, id, task)
                    .ok_or(AiServiceError::NotConfigured)
            }
        }
    }

    fn operator_route(&self, caller: Caller<'_>, task: Task) -> Option<Route> {
        if !caller.owner {
            return None; // the operator provider is owner-only (E4)
        }
        let op = self.inner.operator.as_ref()?;
        let (kind, model) = match task {
            // Cataloging and QC need a vision model, else no route (Q1).
            Task::Catalog | Task::Qc => (ProviderKind::OpenAiCompatible, op.vision_model.clone()?),
            Task::Embed => (ProviderKind::OpenAiCompatible, op.embed_model.clone()?),
            Task::Stt => {
                op.whisper.as_ref()?;
                (ProviderKind::WhisperCpp, String::new())
            }
            _ => (ProviderKind::OpenAiCompatible, op.text_model.clone()),
        };
        Some(Route {
            provider: ProviderRef::Operator,
            kind,
            model,
            task,
        })
    }

    fn user_route(&self, settings: &AiSettings, id: &str, task: Task) -> Option<Route> {
        let provider = settings.providers.iter().find(|p| p.id == id)?;
        self.descriptor_route(provider, task)
    }

    fn first_user_route(&self, settings: &AiSettings, task: Task) -> Option<Route> {
        settings
            .providers
            .iter()
            .find_map(|provider| self.descriptor_route(provider, task))
    }

    fn descriptor_route(&self, provider: &AiProvider, task: Task) -> Option<Route> {
        let model = match task {
            Task::Catalog => provider.models.catalog.clone()?,
            Task::Qc => provider
                .models
                .qc
                .clone()
                .or_else(|| provider.models.catalog.clone())?,
            Task::Chat => provider.models.chat.clone()?,
            Task::Suggest => provider.models.suggest.clone()?,
            Task::Cluster => provider
                .models
                .cluster
                .clone()
                .or_else(|| provider.models.chat.clone())
                .or_else(|| provider.models.suggest.clone())?,
            Task::Alias => provider
                .models
                .alias
                .clone()
                .or_else(|| provider.models.chat.clone())
                .or_else(|| provider.models.suggest.clone())?,
            Task::Embed => provider.models.embed.clone()?,
            Task::Stt => provider.models.stt.clone()?,
        };
        let kind = match provider.kind {
            AiProviderKind::OpenaiCompatible => ProviderKind::OpenAiCompatible,
            AiProviderKind::Anthropic => ProviderKind::Anthropic,
        };
        Some(Route {
            provider: ProviderRef::User(provider.id.clone()),
            kind,
            model,
            task,
        })
    }

    /// Runs a chat call for `task`: route, consent, limits, breaker, adapter,
    /// usage, metric, status.
    ///
    /// # Errors
    ///
    /// [`AiServiceError`]: the task is not routable, needs consent, or the
    /// call failed (or was held because the provider is offline or its breaker
    /// is open).
    pub async fn chat(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        task: Task,
        request: &ChatRequest,
        hints: CallHints,
    ) -> Result<ChatResponse, AiServiceError> {
        let route = self
            .route_for_provider(state, caller, task, hints.provider_id.as_deref())
            .await?;
        let guard = self
            .begin_cancellable(state, caller, &route, hints.cancel.as_ref())
            .await?;
        let options = self.call_options(&route, hints);
        let mut request = request.clone();
        request.model.clone_from(&route.model);
        let result = until_cancelled(
            guard.cancel.as_ref(),
            guard.provider.chat(&request, &options),
        )
        .await;
        let generation = guard.cancel.clone();
        drop(guard);
        let usage = result.as_ref().ok().and_then(|r| r.usage);
        self.finish(
            state,
            caller,
            &route,
            outcome_of(&result),
            usage,
            generation,
        )
        .await;
        result.map_err(AiServiceError::Call)
    }

    /// Runs an embeddings call ([`Task::Embed`]).
    ///
    /// # Errors
    ///
    /// As [`AiService::chat`].
    pub async fn embed(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        request: &EmbedRequest,
        hints: CallHints,
    ) -> Result<Embeddings, AiServiceError> {
        let route = self.route(state, caller, Task::Embed).await?;
        let guard = self
            .begin_cancellable(state, caller, &route, hints.cancel.as_ref())
            .await?;
        let options = self.call_options(&route, hints);
        let result = until_cancelled(
            guard.cancel.as_ref(),
            guard.provider.embed(request, &options),
        )
        .await;
        let generation = guard.cancel.clone();
        drop(guard);
        let usage = result.as_ref().ok().and_then(|r| r.usage);
        self.finish(
            state,
            caller,
            &route,
            outcome_of(&result),
            usage,
            generation,
        )
        .await;
        result.map_err(AiServiceError::Call)
    }

    /// Runs a transcription call ([`Task::Stt`]).
    ///
    /// # Errors
    ///
    /// As [`AiService::chat`].
    pub async fn transcribe(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        request: &TranscribeRequest,
        hints: CallHints,
    ) -> Result<Transcript, AiServiceError> {
        let route = self.route(state, caller, Task::Stt).await?;
        let guard = self
            .begin_cancellable(state, caller, &route, hints.cancel.as_ref())
            .await?;
        let options = self.call_options(&route, hints);
        let result = until_cancelled(
            guard.cancel.as_ref(),
            guard.provider.transcribe(request, &options),
        )
        .await;
        let generation = guard.cancel.clone();
        drop(guard);
        self.finish(state, caller, &route, outcome_of(&result), None, generation)
            .await;
        result.map_err(AiServiceError::Call)
    }

    async fn begin_cancellable(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        route: &Route,
        cancel: Option<&CancellationToken>,
    ) -> Result<CallGuard, AiServiceError> {
        if let Some(cancel) = cancel {
            tokio::select! { biased; _ = cancel.cancelled() => Err(AiServiceError::Call(AiError::new(ErrorKind::Cancelled, "the AI call was cancelled"))), result = self.begin(state, caller, route) => result }
        } else {
            self.begin(state, caller, route).await
        }
    }

    pub(crate) async fn acquire_probe(
        &self,
        user: &str,
        concurrency: u8,
    ) -> (OwnedSemaphorePermit, OwnedSemaphorePermit) {
        let global = self.acquire_global().await;
        let slot = self.user_gate(user).acquire(concurrency).await;
        (global, slot)
    }

    async fn begin(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        route: &Route,
    ) -> Result<CallGuard, AiServiceError> {
        match &route.provider {
            ProviderRef::Operator => self.begin_operator(state, caller, route).await,
            ProviderRef::User(id) => self.begin_user(state, caller, route, id).await,
        }
    }

    async fn begin_operator(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        route: &Route,
    ) -> Result<CallGuard, AiServiceError> {
        let op = self
            .inner
            .operator
            .as_ref()
            .ok_or(AiServiceError::NotConfigured)?;
        self.inner.operator_breaker.observe(caller.id);
        // Held when offline or the key was refused: the work waits without a
        // provider call, so no try is spent (G3-25).
        match self.inner.operator_breaker.state() {
            ProviderState::Offline => {
                metrics::record_ai_request(
                    metrics::ai_provider_kind::OPERATOR,
                    route.task.as_str(),
                    ErrorKind::Offline.as_str(),
                    0,
                    0,
                );
                return Err(AiServiceError::Call(held_error(ErrorKind::Offline)));
            }
            ProviderState::InvalidKey => {
                metrics::record_ai_request(
                    metrics::ai_provider_kind::OPERATOR,
                    route.task.as_str(),
                    ErrorKind::InvalidKey.as_str(),
                    0,
                    0,
                );
                self.inner.operator_breaker.mark_paused(caller.id);
                let _ = state.jobs().pause(caller.id, AI_DRAIN_KIND).await;
                return Err(AiServiceError::Call(held_error(ErrorKind::InvalidKey)));
            }
            _ => {}
        }
        self.ensure_operator_consent(state, caller).await;
        let global = self.acquire_global().await;
        let slot = self.inner.operator_gate.acquire().await;
        let provider = if route.task == Task::Stt {
            op.whisper.clone().ok_or(AiServiceError::NotConfigured)?
        } else {
            op.chat.clone()
        };
        Ok(CallGuard {
            provider,
            cancel: None,
            _global: global,
            _slot: slot,
        })
    }

    async fn begin_user(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        _route: &Route,
        id: &str,
    ) -> Result<CallGuard, AiServiceError> {
        let lock = state.byok().lock(caller.id).await;
        let settings = self.read_ai_settings(state, caller.id).await;
        let descriptor = settings
            .providers
            .iter()
            .find(|p| p.id == id)
            .ok_or(AiServiceError::NotConfigured)?;
        if !descriptor
            .consent
            .as_ref()
            .is_some_and(|c| c.version == super::providers::CONSENT_VERSION)
        {
            return Err(AiServiceError::ConsentRequired(id.to_owned()));
        }
        let key = super::providers::key(state, caller.id, id)
            .await
            .map_err(|_| AiServiceError::Call(held_error(ErrorKind::InvalidKey)))?
            .ok_or(AiServiceError::NotConfigured)?;
        if self
            .inner
            .user_breakers
            .admit(caller.id, id, Instant::now())
            == Admission::Refuse
        {
            return Err(AiServiceError::Call(held_error(ErrorKind::Transient)));
        }
        let provider =
            super::providers::adapter(state, descriptor, key).map_err(AiServiceError::Call)?;
        let cancel = state.byok().generation(caller.id, id);
        drop(lock);
        let global = until_cancelled(Some(&cancel), async { Ok(self.acquire_global().await) })
            .await
            .map_err(AiServiceError::Call)?;
        let slot = until_cancelled(Some(&cancel), async {
            Ok(self
                .user_gate(caller.id)
                .acquire(settings.concurrency)
                .await)
        })
        .await
        .map_err(AiServiceError::Call)?;
        Ok(CallGuard {
            provider,
            cancel: Some(cancel),
            _global: global,
            _slot: slot,
        })
    }

    async fn acquire_global(&self) -> OwnedSemaphorePermit {
        Arc::clone(&self.inner.global)
            .acquire_owned()
            .await
            .expect("the global AI semaphore is never closed")
    }

    fn user_gate(&self, user: &str) -> Arc<UserGate> {
        let mut gates = self.lock_gates();
        Arc::clone(
            gates
                .entry(user.to_owned())
                .or_insert_with(|| Arc::new(UserGate::new())),
        )
    }

    fn call_options(&self, route: &Route, hints: CallHints) -> CallOptions {
        let mut options = CallOptions::new(self.timeouts(route));
        options.cancel = hints.cancel;
        options.on_text = hints.on_text;
        options
    }

    fn timeouts(&self, route: &Route) -> Timeouts {
        match &route.provider {
            ProviderRef::Operator => {
                let total = self
                    .inner
                    .operator
                    .as_ref()
                    .map_or(Duration::from_secs(60), |op| op.timeout);
                if route.task == Task::Chat {
                    Timeouts::new(CONNECT_TIMEOUT, total)
                        .with_first_token(CHAT_FIRST_TOKEN)
                        .with_overall(total)
                } else {
                    Timeouts::new(CONNECT_TIMEOUT, total)
                }
            }
            ProviderRef::User(_) => match route.task {
                Task::Chat | Task::Suggest => Timeouts::CHAT,
                _ => Timeouts::CATALOG,
            },
        }
    }

    async fn finish(
        &self,
        state: &AppState,
        caller: Caller<'_>,
        route: &Route,
        outcome: CallOutcome,
        usage: Option<Usage>,
        generation: Option<CancellationToken>,
    ) {
        let outcome_label = match outcome {
            CallOutcome::Ok => metrics::ai_outcome::OK,
            CallOutcome::Failed(kind) => kind.as_str(),
        };
        let (in_tokens, out_tokens) = usage.map_or((0, 0), |u| (u.input_tokens, u.output_tokens));
        metrics::record_ai_request(
            self.provider_kind_label(route),
            route.task.as_str(),
            outcome_label,
            in_tokens,
            out_tokens,
        );
        self.record_usage(state, caller.id, in_tokens, out_tokens)
            .await;
        match &route.provider {
            ProviderRef::Operator => self.finish_operator(state, caller, outcome).await,
            ProviderRef::User(id) => {
                let _lock = state.byok().lock(caller.id).await;
                if generation
                    .as_ref()
                    .is_some_and(CancellationToken::is_cancelled)
                {
                    return;
                }
                self.inner
                    .user_breakers
                    .record(caller.id, id, &outcome, Instant::now());
                let status = match outcome {
                    CallOutcome::Ok => ProviderState::Ok,
                    CallOutcome::Failed(ErrorKind::Cancelled) => return,
                    CallOutcome::Failed(ErrorKind::InvalidKey) => ProviderState::InvalidKey,
                    CallOutcome::Failed(ErrorKind::Offline) => ProviderState::Offline,
                    CallOutcome::Failed(_) => {
                        if self
                            .inner
                            .user_breakers
                            .state(caller.id, id, Instant::now())
                            == ProviderState::Down
                        {
                            ProviderState::Down
                        } else {
                            ProviderState::Degraded
                        }
                    }
                };
                self.record_user_status(state, caller.id, id, status);
            }
        }
    }

    async fn finish_operator(&self, state: &AppState, caller: Caller<'_>, outcome: CallOutcome) {
        let (transition, resumed) = match outcome {
            CallOutcome::Ok => self.inner.operator_breaker.reachable(),
            CallOutcome::Failed(kind) => (
                self.inner.operator_breaker.failed(kind, caller.id),
                Vec::new(),
            ),
        };
        if let Transition::Changed(now) = transition {
            self.publish_operator_status(state, now);
            if now == ProviderState::InvalidKey {
                self.pause_and_notify(state, caller.id).await;
            }
        }
        for user in resumed {
            if let Err(err) = state.jobs().resume(&user, AI_DRAIN_KIND).await {
                tracing::debug!(error = %err, "resuming the ai.drain queue failed");
            }
        }
    }

    /// Checks an observed operator node, no more than once a minute. Health
    /// never generates content or wakes the node. Called by maintenance.
    pub async fn maintain(&self, state: &AppState) {
        if self.inner.operator.is_none()
            || (!self.inner.operator_breaker.is_blocked()
                && self.inner.operator_breaker.observers().is_empty())
        {
            return;
        }
        {
            let mut last = self.lock_probe();
            let now = Instant::now();
            if last.is_some_and(|prev| now.duration_since(prev) < self.inner.probe_interval) {
                return;
            }
            *last = Some(now);
        }
        self.operator_probe_once(state).await;
    }

    /// Probes the operator node once, ignoring the interval, and updates its
    /// reachability state. A health probe runs no generation; the models probe
    /// checks the key.
    pub async fn operator_probe_once(&self, state: &AppState) {
        let Some(op) = self.inner.operator.as_ref() else {
            return;
        };
        let options = CallOptions::new(Timeouts::new(CONNECT_TIMEOUT, Duration::from_secs(10)))
            .with_retry(RetryPolicy::NONE);
        let result = match self.inner.operator_breaker.state() {
            ProviderState::InvalidKey => op.chat.models(&options).await.map(|_| ()),
            _ => op.chat.health(&options).await,
        };
        if let Err(error) = result {
            // A health probe has no caller to pause, so only reachability failures
            // affect the breaker. Key refusal is learned by a real call/models probe.
            if matches!(
                error.kind(),
                ErrorKind::Offline | ErrorKind::Transient | ErrorKind::RateLimited
            ) && let Transition::Changed(status) =
                self.inner.operator_breaker.failed(error.kind(), "")
            {
                self.publish_operator_status(state, status);
            }
        } else {
            let (transition, resumed) = self.inner.operator_breaker.reachable();
            if let Transition::Changed(now) = transition {
                self.publish_operator_status(state, now);
            }
            for user in resumed {
                if let Err(err) = state.jobs().resume(&user, AI_DRAIN_KIND).await {
                    tracing::debug!(error = %err, "resuming the ai.drain queue failed");
                }
            }
        }
    }

    fn publish_operator_status(&self, state: &AppState, status: ProviderState) {
        let event = ProviderStatusEvent {
            provider_id: OPERATOR_PROVIDER_ID.to_owned(),
            state: status,
        };
        for user in self.inner.operator_breaker.observers() {
            state.events().provider_status(&user, &event);
        }
    }

    async fn pause_and_notify(&self, state: &AppState, user: &str) {
        if let Err(err) = state.jobs().pause(user, AI_DRAIN_KIND).await {
            // The kind is registered by P3-13; until then there is no queue.
            tracing::debug!(error = %err, "pausing the ai.drain queue failed");
        }
        let mut params = Map::new();
        params.insert(
            "providerId".to_owned(),
            Value::String(OPERATOR_PROVIDER_ID.to_owned()),
        );
        let notification = NewNotification {
            kind: "ai".to_owned(),
            code: "ai.provider_key_invalid".to_owned(),
            params,
            target: Some("/settings".to_owned()),
        };
        if let Err(err) = crate::events::notify(state, user, notification).await {
            tracing::warn!(error = %err, "posting the ai.provider_key_invalid notification failed");
        }
    }

    async fn record_usage(&self, state: &AppState, user: &str, in_tokens: u64, out_tokens: u64) {
        let user = user.to_owned();
        let control = Arc::clone(state.control());
        let in_i = i64::try_from(in_tokens).unwrap_or(i64::MAX);
        let out_i = i64::try_from(out_tokens).unwrap_or(i64::MAX);
        let now = now_ms();
        let recorded = blocking(move || {
            control.write(|conn| usage_daily::record_ai(conn, &user, in_i, out_i, now))
        })
        .await;
        if let Err(err) = recorded {
            tracing::warn!(error = %err, "recording AI usage failed");
        }
    }

    async fn ensure_operator_consent(&self, state: &AppState, caller: Caller<'_>) {
        let key = (caller.id.to_owned(), OPERATOR_PROVIDER_ID.to_owned());
        if self.lock_consent().contains(&key) {
            return;
        }
        let user = caller.id.to_owned();
        let control = Arc::clone(state.control());
        let now = now_ms();
        let recorded = blocking(move || {
            control.write(|conn| -> Result<(), RepoError> {
                let exists = conn
                    .query_row(
                        "SELECT 1 FROM audit_log WHERE action = ?1 AND actor_user_id = ?2 \
                         AND target = ?3 LIMIT 1",
                        params![audit::AI_PROVIDER_CONSENT, user, OPERATOR_PROVIDER_ID],
                        |_| Ok(()),
                    )
                    .optional()
                    .map_err(RepoError::from)?
                    .is_some();
                if !exists {
                    audit::record(
                        conn,
                        &audit::Entry {
                            action: audit::AI_PROVIDER_CONSENT,
                            actor_user_id: Some(&user),
                            target: Some(OPERATOR_PROVIDER_ID),
                            meta: None,
                        },
                        now,
                    )?;
                }
                Ok(())
            })
        })
        .await;
        match recorded {
            Ok(()) => {
                self.lock_consent().insert(key);
            }
            Err(err) => tracing::warn!(error = %err, "recording operator consent failed"),
        }
    }

    pub(crate) fn reset_user_provider(&self, user: &str, id: &str) {
        self.inner
            .user_statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&(user.to_owned(), id.to_owned()));
        self.inner.user_breakers.remove(user, id);
    }

    pub(crate) fn record_user_status(
        &self,
        state: &AppState,
        user: &str,
        id: &str,
        status: ProviderState,
    ) {
        let previous = self
            .inner
            .user_statuses
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert((user.to_owned(), id.to_owned()), status);
        if previous != Some(status) {
            state.events().provider_status(
                user,
                &ProviderStatusEvent {
                    provider_id: id.to_owned(),
                    state: status,
                },
            );
        }
    }

    async fn read_ai_settings(&self, state: &AppState, user: &str) -> AiSettings {
        let Ok(db) = state.user_db(user).await else {
            return AiSettings::default();
        };
        match blocking(move || db.read(settings::read)).await {
            Ok(settings) => settings.ai,
            Err(err) => {
                tracing::warn!(error = %err, "reading AI settings failed; using defaults");
                AiSettings::default()
            }
        }
    }

    fn provider_kind_label(&self, route: &Route) -> &'static str {
        match &route.provider {
            ProviderRef::Operator => metrics::ai_provider_kind::OPERATOR,
            ProviderRef::User(_) => match route.kind {
                ProviderKind::OpenAiCompatible => metrics::ai_provider_kind::OPENAI_COMPATIBLE,
                ProviderKind::Anthropic => metrics::ai_provider_kind::ANTHROPIC,
                ProviderKind::WhisperCpp => metrics::ai_provider_kind::WHISPER_CPP,
            },
        }
    }

    fn lock_gates(&self) -> MutexGuard<'_, HashMap<String, Arc<UserGate>>> {
        self.inner
            .user_gates
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_consent(&self) -> MutexGuard<'_, std::collections::HashSet<(String, String)>> {
        self.inner
            .consented
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn lock_probe(&self) -> MutexGuard<'_, Option<Instant>> {
        self.inner
            .last_probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// A call's result as a breaker/metric outcome.
fn outcome_of<T>(result: &Result<T, AiError>) -> CallOutcome {
    CallOutcome::of(result.as_ref().map(|_| ()).map_err(AiError::kind))
}

/// The error for a call the service held without reaching the provider.
fn held_error(kind: ErrorKind) -> AiError {
    let message = match kind {
        ErrorKind::Offline => "the operator node is offline",
        ErrorKind::InvalidKey => "the operator node refused the key",
        _ => "the provider is cooling down after repeated failures",
    };
    AiError::new(kind, message).with_code("provider_held")
}

async fn until_cancelled<T>(
    cancel: Option<&CancellationToken>,
    call: impl std::future::Future<Output = Result<T, AiError>>,
) -> Result<T, AiError> {
    if let Some(cancel) = cancel {
        tokio::select! { biased; _ = cancel.cancelled() => Err(AiError::new(ErrorKind::Cancelled, "the provider configuration changed")), result = call => result }
    } else {
        call.await
    }
}

#[cfg(test)]
mod user_gate_tests {
    use super::UserGate;
    use tokio::time::{Duration, timeout};
    #[tokio::test]
    async fn changing_concurrency_counts_existing_calls() {
        let gate = UserGate::new();
        let first = gate.acquire(1).await;
        let second = gate.acquire(2).await;
        assert!(
            timeout(Duration::from_millis(20), gate.acquire(2))
                .await
                .is_err()
        );
        drop(first);
        let third = gate.acquire(2).await;
        drop(third);
        assert!(
            timeout(Duration::from_millis(20), gate.acquire(1))
                .await
                .is_err()
        );
        drop(second);
        let fourth = gate.acquire(1).await;
        assert!(
            timeout(Duration::from_millis(20), gate.acquire(1))
                .await
                .is_err()
        );
        drop(fourth);
    }
}
