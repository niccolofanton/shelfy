//! BYOK descriptors in the library, sealed credentials in control, and consent.
//! Mutations and credential snapshots share a per-user lock. Endpoint/key
//! changes cancel the old generation before changing either database; interrupted
//! cross-database writes may leave an orphaned sealed key, never a callable
//! descriptor with a credential intended for a different endpoint.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use secrecy::{ExposeSecret as _, SecretString};
use serde::{Deserialize, Serialize};
use shelfy_ai::{
    AiError, CallOptions, ChatRequest, EgressPolicy, ErrorKind, Image, ImageType, JsonOutput,
    Message, Part, Provider, ProviderConfig, ProviderKind, RetryPolicy, Source, Timeouts,
};
use shelfy_core::repo::{
    RepoError,
    settings::{
        self, AiModels, AiPrices, AiProbeResult, AiProvider, AiProviderConsent, AiProviderKind,
        AiProviderTest, SettingsChange,
    },
};
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio_util::sync::CancellationToken;
use url::{Host, Url};
use utoipa::ToSchema;

use super::OPERATOR_PROVIDER_ID;
use crate::control::{audit, provider_keys};
use crate::error::{ApiError, ErrorCode};
use crate::ids::now_ms;
use crate::outbound::ai::AiTransport;
use crate::state::{AppState, blocking};

pub const CONSENT_VERSION: &str = "ai-provider-v1";
pub const MAX_PROVIDERS: usize = 8;

/// Runtime coordination; no key is cached here.
#[derive(Default)]
pub struct ProviderStore {
    locks: Mutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    generations: Mutex<HashMap<(String, String), CancellationToken>>,
}
impl ProviderStore {
    pub async fn lock(&self, user: &str) -> OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self
                .locks
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(user).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(AsyncMutex::new(()));
                locks.insert(user.to_owned(), Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }
    pub fn generation(&self, user: &str, id: &str) -> CancellationToken {
        self.generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .entry((user.to_owned(), id.to_owned()))
            .or_default()
            .clone()
    }
    pub fn invalidate(&self, user: &str, id: &str) {
        if let Some(token) = self
            .generations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&(user.to_owned(), id.to_owned()))
        {
            token.cancel();
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum UserProviderKind {
    OpenaiCompatible,
    Anthropic,
}
impl From<UserProviderKind> for AiProviderKind {
    fn from(kind: UserProviderKind) -> Self {
        match kind {
            UserProviderKind::OpenaiCompatible => Self::OpenaiCompatible,
            UserProviderKind::Anthropic => Self::Anthropic,
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct TaskModels {
    pub catalog: Option<String>,
    pub chat: Option<String>,
    pub suggest: Option<String>,
    pub qc: Option<String>,
    pub cluster: Option<String>,
    pub alias: Option<String>,
    pub embed: Option<String>,
    pub stt: Option<String>,
}
impl From<TaskModels> for AiModels {
    fn from(m: TaskModels) -> Self {
        Self {
            catalog: m.catalog,
            chat: m.chat,
            suggest: m.suggest,
            qc: m.qc,
            cluster: m.cluster,
            alias: m.alias,
            embed: m.embed,
            stt: m.stt,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderPrices {
    pub input_per_million_usd: f64,
    pub output_per_million_usd: f64,
}
impl From<AiPrices> for ProviderPrices {
    fn from(p: AiPrices) -> Self {
        Self {
            input_per_million_usd: p.input_per_million_usd,
            output_per_million_usd: p.output_per_million_usd,
        }
    }
}
impl From<ProviderPrices> for AiPrices {
    fn from(p: ProviderPrices) -> Self {
        Self {
            input_per_million_usd: p.input_per_million_usd,
            output_per_million_usd: p.output_per_million_usd,
        }
    }
}

/// The key can be supplied only on writes, never serialized or debug-printed.
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct ProviderInput {
    pub kind: UserProviderKind,
    pub label: String,
    pub base_url: String,
    #[serde(default)]
    pub models: TaskModels,
    #[serde(default)]
    pub prices: Option<ProviderPrices>,
    /// Omit to keep the current credential. Required on creation or a target change.
    #[serde(default, deserialize_with = "secret")]
    #[schema(value_type = Option<String>, write_only = true)]
    pub key: Option<SecretString>,
}
fn secret<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<SecretString>, D::Error> {
    Option::<String>::deserialize(deserializer).map(|key| key.map(SecretString::from))
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderConsent {
    pub version: String,
    pub accepted_at: i64,
}
impl From<AiProviderConsent> for ProviderConsent {
    fn from(c: AiProviderConsent) -> Self {
        Self {
            version: c.version,
            accepted_at: c.accepted_at,
        }
    }
}
#[derive(Debug, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConsentRequest {
    pub version: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub ok: bool,
    pub skipped: bool,
    pub error: Option<String>,
}
impl From<AiProbeResult> for ProbeResult {
    fn from(p: AiProbeResult) -> Self {
        Self {
            ok: p.ok,
            skipped: p.skipped,
            error: p.error,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, ToSchema)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTest {
    pub tested_at: i64,
    pub models: ProbeResult,
    pub text: ProbeResult,
    pub vision: ProbeResult,
    pub schema: ProbeResult,
}
impl From<AiProviderTest> for ProviderTest {
    fn from(p: AiProviderTest) -> Self {
        Self {
            tested_at: p.tested_at,
            models: p.models.into(),
            text: p.text.into(),
            vision: p.vision.into(),
            schema: p.schema.into(),
        }
    }
}

pub fn validate_id(id: &str) -> Result<(), ApiError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
        || id.eq_ignore_ascii_case(OPERATOR_PROVIDER_ID)
    {
        return Err(ApiError::invalid_field(
            "id",
            "use 1-64 letters, digits, underscores or hyphens; operator is reserved",
        ));
    }
    Ok(())
}

pub fn policy(state: &AppState) -> EgressPolicy {
    let mut policy = EgressPolicy::new().allow_loopback(state.config().ai_allow_loopback);
    for origin in state.config().outbound.allow_origins.iter() {
        if let Ok(origin) = shelfy_ai::Origin::parse(&origin.to_string()) {
            policy = policy.allow(origin);
        }
    }
    policy
}

async fn validated_url(state: &AppState, raw: &str) -> Result<Url, ApiError> {
    let invalid = || {
        ApiError::invalid_field(
            "baseUrl",
            "use HTTPS at a public address, never an operator host",
        )
    };
    if raw.len() > 4096 {
        return Err(invalid());
    }
    let mut url = Url::parse(raw).map_err(|_| invalid())?;
    let policy = policy(state);
    let route = policy.check_user_url(&url).map_err(|_| invalid())?;
    if route == shelfy_ai::Egress::Loopback {
        // No local DNS request or resolver exception: normalize special-use
        // names and mapped loopback to the literal-only development transport.
        if matches!(url.host(), Some(Host::Domain(_))) {
            url.set_host(Some("127.0.0.1")).map_err(|_| invalid())?;
        }
        if let Some(Host::Ipv6(ip)) = url.host()
            && let Some(v4) = ip.to_ipv4_mapped()
        {
            url.set_ip_host(v4.into()).map_err(|_| invalid())?;
        }
        policy.check_user_url(&url).map_err(|_| invalid())?;
    } else {
        if !matches!(url.port_or_known_default(), Some(80 | 443)) {
            return Err(invalid());
        }
        if let Some(Host::Domain(host)) = url.host() {
            let answers = tokio::time::timeout(
                Duration::from_secs(5),
                state.config().outbound.lookup.lookup(host),
            )
            .await
            .map_err(|_| invalid())?
            .map_err(|_| invalid())?;
            if answers.is_empty()
                || answers
                    .iter()
                    .any(|ip| !shelfy_ai::guard::is_public(*ip) || !crate::outbound::is_public(*ip))
            {
                return Err(invalid());
            }
        }
    }
    Ok(url)
}

fn validate_models(models: &TaskModels) -> Result<(), ApiError> {
    for model in [
        &models.catalog,
        &models.chat,
        &models.suggest,
        &models.qc,
        &models.cluster,
        &models.alias,
        &models.embed,
        &models.stt,
    ]
    .into_iter()
    .flatten()
    {
        if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
            return Err(ApiError::invalid_field(
                "models",
                "model ids must be 1-256 characters without control characters",
            ));
        }
    }
    Ok(())
}

pub async fn put(
    state: &AppState,
    user: &str,
    id: &str,
    input: ProviderInput,
) -> Result<(), ApiError> {
    validate_id(id)?;
    if !state.vault().enabled() {
        return Err(ApiError::new(ErrorCode::AiVaultDisabled));
    }
    if input.label.trim().is_empty()
        || input.label.chars().count() > 128
        || input.label.chars().any(char::is_control)
    {
        return Err(ApiError::invalid_field(
            "label",
            "use a nonempty label of at most 128 characters",
        ));
    }
    validate_models(&input.models)?;
    if let Some(prices) = input.prices {
        for price in [prices.input_per_million_usd, prices.output_per_million_usd] {
            if !price.is_finite() || !(0.0..=1_000_000.0).contains(&price) {
                return Err(ApiError::invalid_field(
                    "prices",
                    "prices must be finite, nonnegative dollars per million tokens",
                ));
            }
        }
    }
    if let Some(key) = &input.key
        && (key.expose_secret().is_empty()
            || key.expose_secret().len() > 16_384
            || key.expose_secret().chars().any(char::is_control))
    {
        return Err(ApiError::invalid_field(
            "key",
            "use a nonempty credential without control characters, at most 16384 bytes",
        ));
    }
    let url = validated_url(state, &input.base_url).await?;
    let _lock = state.byok().lock(user).await;
    let db = state.user_db(user).await?;
    let mut stored = blocking({
        let db = Arc::clone(&db);
        move || db.read(settings::read)
    })
    .await?
    .ai;
    let old = stored.providers.iter().find(|p| p.id == id).cloned();
    if old.is_none() && stored.providers.len() >= MAX_PROVIDERS {
        return Err(ApiError::invalid_field(
            "providers",
            "at most 8 providers are allowed",
        ));
    }
    let kind = AiProviderKind::from(input.kind);
    let changed_target = old
        .as_ref()
        .is_none_or(|p| p.kind != kind || p.base_url != url.as_str());
    if changed_target && input.key.is_none() {
        return Err(ApiError::invalid_field(
            "key",
            "a new credential is required when creating or changing the provider endpoint or protocol",
        ));
    }
    let changed_key = input.key.is_some();
    let models = AiModels::from(input.models);
    let changed_models = old.as_ref().is_none_or(|p| p.models != models);
    let descriptor = AiProvider {
        id: id.to_owned(),
        kind,
        label: input.label.trim().to_owned(),
        base_url: url.to_string(),
        models,
        prices: input.prices.map(Into::into),
        consent: old
            .as_ref()
            .filter(|_| !changed_target && !changed_key)
            .and_then(|p| p.consent.clone()),
        test: old
            .as_ref()
            .filter(|_| !changed_target && !changed_key && !changed_models)
            .and_then(|p| p.test.clone()),
    };
    let sealed = input
        .key
        .as_ref()
        .map(|key| state.vault().seal(user, id, key))
        .transpose()?;
    state.byok().invalidate(user, id);
    state.ai().reset_user_provider(user, id);
    stored.providers.retain(|p| p.id != id);
    // Remove the callable descriptor before changing a credential in another DB.
    if changed_target || changed_key {
        let change = SettingsChange {
            ai_providers: Some(stored.providers.clone()),
            ..SettingsChange::default()
        };
        blocking({
            let db = Arc::clone(&db);
            move || db.write(|tx| settings::update(tx, &change, now_ms()))
        })
        .await?;
    }
    if let Some(sealed) = sealed {
        let control = Arc::clone(state.control());
        let user = user.to_owned();
        let id = id.to_owned();
        blocking(move || control.write(|tx| provider_keys::put(tx, &user, &id, &sealed, now_ms())))
            .await?;
    }
    stored.providers.push(descriptor);
    let change = SettingsChange {
        ai_providers: Some(stored.providers),
        ..SettingsChange::default()
    };
    blocking(move || db.write(|tx| settings::update(tx, &change, now_ms()))).await?;
    Ok(())
}

pub async fn delete(state: &AppState, user: &str, id: &str) -> Result<(), ApiError> {
    validate_id(id)?;
    let _lock = state.byok().lock(user).await;
    state.byok().invalidate(user, id);
    state.ai().reset_user_provider(user, id);
    let db = state.user_db(user).await?;
    let id_owned = id.to_owned();
    // Descriptor and routing disappear atomically before the sealed row.
    blocking(move || {
        db.write(|tx| {
            let mut settings = settings::read(tx)?.ai;
            settings.providers.retain(|p| p.id != id_owned);
            settings
                .routing
                .0
                .retain(|_, provider| provider != &id_owned);
            settings::update(
                tx,
                &SettingsChange {
                    ai_providers: Some(settings.providers),
                    ai_routing: Some(settings.routing),
                    ..SettingsChange::default()
                },
                now_ms(),
            )
        })
    })
    .await?;
    let control = Arc::clone(state.control());
    let user = user.to_owned();
    let id = id.to_owned();
    blocking(move || control.write(|tx| provider_keys::delete(tx, &user, &id))).await?;
    Ok(())
}

pub async fn consent(
    state: &AppState,
    user: &str,
    id: &str,
    version: &str,
) -> Result<(), ApiError> {
    validate_id(id)?;
    if version != CONSENT_VERSION {
        return Err(ApiError::invalid_field(
            "version",
            "the provider consent version is not current",
        ));
    }
    let _lock = state.byok().lock(user).await;
    let db = state.user_db(user).await?;
    let id_owned = id.to_owned();
    let now = now_ms();
    // Record the audit first. A stopped operation grants no consent until the
    // library commit succeeds; retries can repeat the audit safely.
    let descriptor = blocking({
        let db = Arc::clone(&db);
        let id = id_owned.clone();
        move || {
            db.read(|c| {
                settings::read(c)?
                    .ai
                    .providers
                    .into_iter()
                    .find(|p| p.id == id)
                    .ok_or(RepoError::NotFound)
            })
        }
    })
    .await?;
    if descriptor
        .consent
        .as_ref()
        .is_some_and(|c| c.version == CONSENT_VERSION)
    {
        return Ok(());
    }
    let control = Arc::clone(state.control());
    let user = user.to_owned();
    let target = id.to_owned();
    blocking(move || {
        control.write(|tx| {
            audit::record(
                tx,
                &audit::Entry {
                    action: audit::AI_PROVIDER_CONSENT,
                    actor_user_id: Some(&user),
                    target: Some(&target),
                    meta: Some(&serde_json::json!({"version": CONSENT_VERSION})),
                },
                now,
            )
        })
    })
    .await?;
    blocking(move || {
        db.write(|tx| {
            let mut stored = settings::read(tx)?.ai.providers;
            let provider = stored
                .iter_mut()
                .find(|p| p.id == id_owned)
                .ok_or(RepoError::NotFound)?;
            provider.consent = Some(AiProviderConsent {
                version: CONSENT_VERSION.to_owned(),
                accepted_at: now,
            });
            settings::update(
                tx,
                &SettingsChange {
                    ai_providers: Some(stored),
                    ..SettingsChange::default()
                },
                now,
            )
        })
    })
    .await?;
    Ok(())
}

/// Caller holds the per-user provider lock while combining descriptor and key.
pub async fn key(state: &AppState, user: &str, id: &str) -> Result<Option<SecretString>, ApiError> {
    let control = Arc::clone(state.control());
    let user_owned = user.to_owned();
    let id_owned = id.to_owned();
    let row =
        blocking(move || control.read(|conn| provider_keys::get(conn, &user_owned, &id_owned)))
            .await?;
    let Some(row) = row else { return Ok(None) };
    let key = state.vault().open(user, id, &row.sealed)?;
    let control = Arc::clone(state.control());
    let user = user.to_owned();
    let id = id.to_owned();
    blocking(move || control.write(|conn| provider_keys::touch(conn, &user, &id, now_ms())))
        .await?;
    Ok(Some(key))
}

/// Builds a per-call adapter. The plaintext is dropped with this adapter.
pub fn adapter(
    state: &AppState,
    descriptor: &AiProvider,
    key: SecretString,
) -> Result<Provider, AiError> {
    let kind = match descriptor.kind {
        AiProviderKind::OpenaiCompatible => ProviderKind::OpenAiCompatible,
        AiProviderKind::Anthropic => ProviderKind::Anthropic,
    };
    let url = Url::parse(&descriptor.base_url)
        .map_err(|_| AiError::new(ErrorKind::Refused, "the provider URL is not valid"))?;
    let config = ProviderConfig::new(kind, Source::User, url).with_key(key);
    let transport = if state.config().ai_allow_loopback {
        AiTransport::with_local_loopback(state.outbound())
    } else {
        AiTransport::new(state.outbound())
    };
    Provider::new(config, &policy(state), Arc::new(transport))
}

fn outcome<T>(result: &Result<T, AiError>, optional: bool) -> AiProbeResult {
    let error = result.as_ref().err().map(AiError::kind);
    let skipped = optional
        && result.as_ref().err().is_some_and(|e| {
            e.kind() == ErrorKind::Unsupported || matches!(e.status(), Some(404 | 405))
        });
    AiProbeResult {
        ok: result.is_ok(),
        skipped,
        error: (!skipped)
            .then(|| error.map(|e| e.as_str().to_owned()))
            .flatten(),
    }
}
fn skip() -> AiProbeResult {
    AiProbeResult {
        ok: false,
        skipped: true,
        error: None,
    }
}

pub async fn test(state: &AppState, user: &str, id: &str) -> Result<ProviderTest, ApiError> {
    validate_id(id)?;
    let (descriptor, provider, cancel, concurrency) = {
        let _lock = state.byok().lock(user).await;
        let db = state.user_db(user).await?;
        let id_owned = id.to_owned();
        let settings = blocking(move || db.read(settings::read)).await?.ai;
        let descriptor = settings
            .providers
            .into_iter()
            .find(|p| p.id == id_owned)
            .ok_or_else(|| ApiError::new(ErrorCode::NotFound))?;
        let key = key(state, user, id)
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::AiNotConfigured))?;
        let provider = adapter(state, &descriptor, key)
            .map_err(|e| ApiError::from(super::AiServiceError::Call(e)))?;
        (
            descriptor,
            provider,
            state.byok().generation(user, id),
            settings.concurrency,
        )
    };
    let _permits = tokio::select! { biased; _ = cancel.cancelled() => return Err(ApiError::new(ErrorCode::Conflict)), slots = state.ai().acquire_probe(user, concurrency) => slots };
    let mut options = CallOptions::new(
        Timeouts::new(Duration::from_secs(3), Duration::from_secs(5))
            .with_overall(Duration::from_secs(6)),
    );
    options.retry = RetryPolicy::NONE;
    options.cancel = Some(cancel.clone());
    let shutdown = state.shutdown_token();
    let result = tokio::select! {
        _ = shutdown.cancelled() => return Err(ApiError::new(ErrorCode::ProviderUnavailable)),
        _ = cancel.cancelled() => return Err(ApiError::new(ErrorCode::Conflict)),
        result = synthetic_probes(&provider, &descriptor.models, &options) => result,
    };
    // Do not let an old test's results survive an edit/delete or cancellation.
    let _lock = state.byok().lock(user).await;
    if cancel.is_cancelled() {
        return Err(ApiError::new(ErrorCode::Conflict));
    }
    state
        .ai()
        .record_user_status(state, user, id, probe_status(&result));
    let db = state.user_db(user).await?;
    let id = id.to_owned();
    let result_copy = result.clone();
    blocking(move || {
        db.write(|tx| {
            let mut stored = settings::read(tx)?.ai.providers;
            let provider = stored
                .iter_mut()
                .find(|p| p.id == id)
                .ok_or(RepoError::NotFound)?;
            provider.test = Some(result_copy);
            settings::update(
                tx,
                &SettingsChange {
                    ai_providers: Some(stored),
                    ..SettingsChange::default()
                },
                now_ms(),
            )
        })
    })
    .await?;
    Ok(result.into())
}

async fn synthetic_probes(
    provider: &Provider,
    models: &AiModels,
    options: &CallOptions,
) -> AiProviderTest {
    let offered = provider.models(options).await;
    let models_result = outcome(&offered, true);
    let text_model = models
        .chat
        .as_ref()
        .or(models.suggest.as_ref())
        .or(models.catalog.as_ref());
    let text = if let Some(model) = text_model {
        outcome(
            &provider
                .chat(
                    &ChatRequest::new(
                        model,
                        vec![Message::user_text(
                            "Synthetic connectivity check. Reply with OK.",
                        )],
                    ),
                    options,
                )
                .await,
            false,
        )
    } else {
        skip()
    };
    // Valid, synthetic 1x1 PNG; no image or caption is taken from the library.
    let vision = if let Some(model) = &models.catalog {
        let bytes = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mP8/x8AAwMCAO+aYyQAAAAASUVORK5CYII=").expect("fixed synthetic PNG");
        let image = Part::Image(Image {
            media_type: ImageType::Png,
            data: bytes.into(),
        });
        outcome(
            &provider
                .chat(
                    &ChatRequest::new(
                        model,
                        vec![Message::user(vec![
                            Part::text("Describe this synthetic image briefly."),
                            image,
                        ])],
                    ),
                    options,
                )
                .await,
            false,
        )
    } else {
        skip()
    };
    let schema = if let Some(model) = text_model {
        let schema = JsonOutput::new("connectivity", serde_json::json!({"type":"object","additionalProperties":false,"properties":{"ok":{"type":"boolean"}},"required":["ok"]})).expect("fixed strict schema");
        outcome(
            &provider
                .chat(
                    &ChatRequest::new(
                        model,
                        vec![Message::user_text(
                            "Synthetic schema check. Return an object with ok true.",
                        )],
                    )
                    .with_json(schema),
                    options,
                )
                .await,
            false,
        )
    } else {
        skip()
    };
    AiProviderTest {
        tested_at: now_ms(),
        models: models_result,
        text,
        vision,
        schema,
    }
}

pub(crate) fn probe_status(test: &AiProviderTest) -> crate::events::model::ProviderState {
    use crate::events::model::ProviderState;
    let errors: Vec<_> = [&test.models, &test.text, &test.vision, &test.schema]
        .into_iter()
        .filter_map(|r| r.error.as_deref())
        .collect();
    if errors.contains(&"invalid_key") {
        ProviderState::InvalidKey
    } else if errors.contains(&"offline") {
        ProviderState::Offline
    } else if errors.is_empty() {
        ProviderState::Ok
    } else {
        ProviderState::Degraded
    }
}
