//! The AI service and the operator provider (plan §2.15, §2.9, §2.10, §3.6,
//! §7.2; P3-09).
//!
//! Every AI call in the server goes through [`AiService`] (`state.ai()`): it
//! routes a [`Task`] to a provider, checks consent, holds the limits and the
//! circuit breaker, calls `shelfy-ai`'s adapter, records usage and metrics,
//! and keeps each provider's status. The operator provider — the owner's own
//! node (L15, L16) — is defined from the environment ([`operator`]); its key
//! is a [`shelfy_ai::secrecy::SecretString`] that never reaches a client, a
//! log, a metric or a vault.
//!
//! | Module | Contents |
//! |---|---|
//! | [`operator`] | the operator provider's env settings |
//! | [`service`] | [`AiService`]: routing, consent, limits, breaker, usage, metrics, status |
//! | `breaker` | the BYOK circuit breaker and the operator's reachability machine |
//!
//! [`vault`] seals BYOK keys (P3-02). Provider routes are P3-19; the service reads
//! their records through [`service::AiService`]'s seam, which returns nothing
//! until they land, so the operator provider is the only live one.

pub mod operator;
pub mod providers;
pub mod queue;
pub mod runs;
pub mod service;
pub mod vault;

mod breaker;

use crate::error::{ApiError, ErrorCode};

pub use operator::{OPERATOR_PROVIDER_ID, OperatorArgs, OperatorConfig};
pub use service::{AiService, CallHints, ProviderModels, ProviderSummary, Route};

/// The job kind of the social-cataloging drain (P3-13 registers it). The
/// service pauses it for a user when a provider refuses its key.
pub const AI_DRAIN_KIND: &str = "ai.drain";

/// What an AI call is for. Routing, timeouts and the metric label depend on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Task {
    /// Cataloging a social post (needs a vision model).
    Catalog,
    /// Screenshot quality control of a website capture (needs a vision model).
    Qc,
    /// Chat search (text, streamed).
    Chat,
    /// Suggestion chips (text).
    Suggest,
    /// A tag cluster refine run (text).
    Cluster,
    /// A tag alias run (text).
    Alias,
    /// Embeddings.
    Embed,
    /// Dictation, speech to text.
    Stt,
}

impl Task {
    /// Every task, in a stable order.
    pub const ALL: [Self; 8] = [
        Self::Catalog,
        Self::Qc,
        Self::Chat,
        Self::Suggest,
        Self::Cluster,
        Self::Alias,
        Self::Embed,
        Self::Stt,
    ];

    /// The task's stable name: the routing key and the `task` metric label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Catalog => "catalog",
            Self::Qc => "qc",
            Self::Chat => "chat",
            Self::Suggest => "suggest",
            Self::Cluster => "cluster",
            Self::Alias => "alias",
            Self::Embed => "embed",
            Self::Stt => "stt",
        }
    }

    /// Whether the task needs a vision-capable model (cataloging and QC).
    #[must_use]
    pub const fn needs_vision(self) -> bool {
        matches!(self, Self::Catalog | Self::Qc)
    }
}

/// Which provider serves a task.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderRef {
    /// The operator's node (L15).
    Operator,
    /// A BYOK provider the user configured, by its id.
    User(String),
}

impl ProviderRef {
    /// The provider id, as it appears in `GET /me/providers`, `provider.status`
    /// and the audit log.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Operator => OPERATOR_PROVIDER_ID,
            Self::User(id) => id,
        }
    }

    /// Whether this is the operator provider.
    #[must_use]
    pub fn is_operator(&self) -> bool {
        matches!(self, Self::Operator)
    }
}

/// Who is asking for an AI call.
#[derive(Clone, Copy, Debug)]
pub struct Caller<'a> {
    /// The user id.
    pub id: &'a str,
    /// Whether the user is the instance owner (the operator provider is
    /// owner-only, E4).
    pub owner: bool,
}

impl<'a> Caller<'a> {
    /// A caller.
    #[must_use]
    pub fn new(id: &'a str, owner: bool) -> Self {
        Self { id, owner }
    }
}

/// Why an AI call could not run, or how it failed. The HTTP routes turn it
/// into an [`ApiError`]; the drain and chat read the [`shelfy_ai::ErrorKind`]
/// of [`AiServiceError::Call`] to decide between a backoff, a hold and a pause.
#[derive(Debug)]
pub enum AiServiceError {
    /// No provider can serve the task on this server (the operator provider
    /// is off, or it lacks the model the task needs, and the user has no
    /// provider that fits).
    NotConfigured,
    /// The provider needs consent before content is sent to it.
    ConsentRequired(String),
    /// The provider call failed (or was held because the provider is offline
    /// or its breaker is open).
    Call(shelfy_ai::AiError),
}

impl std::fmt::Display for AiServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotConfigured => f.write_str("no AI provider is configured for this task"),
            Self::ConsentRequired(id) => write!(f, "provider {id} needs consent"),
            Self::Call(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for AiServiceError {}

impl From<AiServiceError> for ApiError {
    fn from(error: AiServiceError) -> Self {
        use shelfy_ai::ErrorKind;
        match error {
            AiServiceError::NotConfigured => Self::new(ErrorCode::AiNotConfigured),
            AiServiceError::ConsentRequired(_) => Self::new(ErrorCode::AiConsentRequired),
            AiServiceError::Call(error) => {
                let retry_after = error
                    .retry_after()
                    .map(|wait| u32::try_from(wait.as_secs().max(1)).unwrap_or(u32::MAX));
                let code = match error.kind() {
                    ErrorKind::Offline => ErrorCode::ProviderOffline,
                    ErrorKind::InvalidKey => ErrorCode::ProviderKeyInvalid,
                    ErrorKind::QuotaExhausted => ErrorCode::ProviderQuotaExhausted,
                    ErrorKind::RateLimited | ErrorKind::Transient | ErrorKind::SchemaInvalid => {
                        ErrorCode::ProviderUnavailable
                    }
                    ErrorKind::Unsupported => ErrorCode::NotAvailable,
                    ErrorKind::BadRequest | ErrorKind::Refused | ErrorKind::Cancelled => {
                        ErrorCode::BadRequest
                    }
                };
                let api = Self::new(code);
                // A held or retryable provider suggests when to come back.
                match code {
                    ErrorCode::ProviderOffline => api.with_retry_after(retry_after.unwrap_or(60)),
                    ErrorCode::ProviderUnavailable => {
                        api.with_retry_after(retry_after.unwrap_or(5))
                    }
                    _ => api,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use axum::http::header::RETRY_AFTER;
    use axum::response::IntoResponse as _;
    use shelfy_ai::{AiError, ErrorKind};

    use super::*;

    /// The code and `Retry-After` of a failed call of `kind`.
    fn api(kind: ErrorKind) -> (ErrorCode, Option<String>) {
        let error = ApiError::from(AiServiceError::Call(AiError::new(kind, "x")));
        let code = error.code();
        let response = error.into_response();
        let retry = response
            .headers()
            .get(RETRY_AFTER)
            .map(|value| value.to_str().unwrap().to_owned());
        (code, retry)
    }

    #[test]
    fn every_call_error_kind_has_its_problem() {
        // Offline is held (come back in a minute); rate limits and transient
        // errors are backed off; an invalid key or no quota is not retried. A connect
        // timeout is `Offline` since F15. A new kind fails to compile here.
        let kinds = [
            ErrorKind::Offline,
            ErrorKind::InvalidKey,
            ErrorKind::RateLimited,
            ErrorKind::QuotaExhausted,
            ErrorKind::Transient,
            ErrorKind::BadRequest,
            ErrorKind::Refused,
            ErrorKind::SchemaInvalid,
            ErrorKind::Unsupported,
            ErrorKind::Cancelled,
        ];
        for kind in kinds {
            let expected = match kind {
                ErrorKind::Offline => (ErrorCode::ProviderOffline, Some("60")),
                ErrorKind::InvalidKey => (ErrorCode::ProviderKeyInvalid, None),
                ErrorKind::QuotaExhausted => (ErrorCode::ProviderQuotaExhausted, None),
                ErrorKind::RateLimited | ErrorKind::Transient | ErrorKind::SchemaInvalid => {
                    (ErrorCode::ProviderUnavailable, Some("5"))
                }
                ErrorKind::Unsupported => (ErrorCode::NotAvailable, None),
                ErrorKind::BadRequest | ErrorKind::Refused | ErrorKind::Cancelled => {
                    (ErrorCode::BadRequest, None)
                }
            };
            let (code, retry) = api(kind);
            assert_eq!((code, retry.as_deref()), (expected.0, expected.1), "{kind}");
        }
    }
}
