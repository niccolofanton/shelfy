//! Fault injection: what the stub does instead of answering normally.

use serde::{Deserialize, Serialize};

/// The stub's endpoints, as faults and the request log name them.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Endpoint {
    /// OpenAI-compatible `…/chat/completions`.
    Chat,
    /// Anthropic `…/v1/messages`.
    Messages,
    /// OpenAI-compatible `…/embeddings`.
    Embeddings,
    /// OpenAI-compatible `…/audio/transcriptions`.
    Transcriptions,
    /// whisper.cpp `…/inference`.
    Inference,
    /// `…/models` (OpenAI-compatible or Anthropic, by the request's headers).
    Models,
    /// `/health`.
    Health,
    /// Any other path: 404.
    Unknown,
}

/// A misbehavior of one request. Status faults answer in the protocol's
/// error shape. Being offline (refused connections) is not a per-request
/// fault: [`super::Stub::set_offline`] closes the listener, and the admin
/// route `POST /_stub/offline` does it for a while.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Fault {
    /// 429 with `Retry-After` (whole seconds, rounded up) and `retry-after-ms`.
    RateLimited {
        /// The wait asked for.
        #[serde(default = "default_retry_after_ms")]
        retry_after_ms: u64,
    },
    /// 500.
    ServerError,
    /// 529 `overloaded_error` (Anthropic), 503 elsewhere.
    Overloaded,
    /// No answer at all: the request hangs until the client gives up.
    Timeout,
    /// The first token (or the whole answer, when not streamed) arrives late.
    SlowFirstToken {
        /// The delay.
        #[serde(default = "default_delay_ms")]
        delay_ms: u64,
    },
    /// 401.
    Unauthorized,
    /// 403.
    Forbidden,
    /// No quota left: 429 `insufficient_quota` (OpenAI-compatible), 402
    /// `billing_error` (Anthropic), 402 (whisper.cpp).
    QuotaExhausted,
    /// 400.
    BadRequest,
    /// 200 with a body (or stream chunks) that is not JSON.
    MalformedJson,
    /// 200 with JSON that does not match the request's schema.
    NonConformingJson,
    /// 200 with a stream that carries no content (or an empty answer).
    EmptyStream,
    /// The model refuses (`refusal` on OpenAI-compatible, `stop_reason:
    /// "refusal"` on Anthropic).
    Refusal,
    /// 302 to another path of the stub.
    Redirect,
    /// The connection drops in the middle of the answer.
    Reset,
}

const fn default_retry_after_ms() -> u64 {
    1000
}

const fn default_delay_ms() -> u64 {
    2000
}

impl Fault {
    /// The fault's name (its `kind`).
    #[must_use]
    pub fn name(&self) -> &'static str {
        match self {
            Self::RateLimited { .. } => "rate_limited",
            Self::ServerError => "server_error",
            Self::Overloaded => "overloaded",
            Self::Timeout => "timeout",
            Self::SlowFirstToken { .. } => "slow_first_token",
            Self::Unauthorized => "unauthorized",
            Self::Forbidden => "forbidden",
            Self::QuotaExhausted => "quota_exhausted",
            Self::BadRequest => "bad_request",
            Self::MalformedJson => "malformed_json",
            Self::NonConformingJson => "non_conforming_json",
            Self::EmptyStream => "empty_stream",
            Self::Refusal => "refusal",
            Self::Redirect => "redirect",
            Self::Reset => "reset",
        }
    }

    /// Parses `name` with default parameters (the binary's `--fault`).
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        Some(match name {
            "rate_limited" => Self::RateLimited {
                retry_after_ms: default_retry_after_ms(),
            },
            "server_error" => Self::ServerError,
            "overloaded" => Self::Overloaded,
            "timeout" => Self::Timeout,
            "slow_first_token" => Self::SlowFirstToken {
                delay_ms: default_delay_ms(),
            },
            "unauthorized" => Self::Unauthorized,
            "forbidden" => Self::Forbidden,
            "quota_exhausted" => Self::QuotaExhausted,
            "bad_request" => Self::BadRequest,
            "malformed_json" => Self::MalformedJson,
            "non_conforming_json" => Self::NonConformingJson,
            "empty_stream" => Self::EmptyStream,
            "refusal" => Self::Refusal,
            "redirect" => Self::Redirect,
            "reset" => Self::Reset,
            _ => return None,
        })
    }
}

/// A fault, how many times it applies, and to which endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaultRule {
    /// The fault.
    pub fault: Fault,
    /// How many requests it applies to; `None` until cleared.
    #[serde(default = "once")]
    pub times: Option<u32>,
    /// The endpoint it applies to; `None` for every endpoint.
    #[serde(default)]
    pub endpoint: Option<Endpoint>,
}

#[allow(clippy::unnecessary_wraps)]
const fn once() -> Option<u32> {
    Some(1)
}

impl FaultRule {
    /// `fault`, once, on any endpoint.
    #[must_use]
    pub fn new(fault: Fault) -> Self {
        Self {
            fault,
            times: once(),
            endpoint: None,
        }
    }

    /// For the next `times` matching requests.
    #[must_use]
    pub fn times(mut self, times: u32) -> Self {
        self.times = Some(times);
        self
    }

    /// Until cleared.
    #[must_use]
    pub fn always(mut self) -> Self {
        self.times = None;
        self
    }

    /// Only on `endpoint`.
    #[must_use]
    pub fn on(mut self, endpoint: Endpoint) -> Self {
        self.endpoint = Some(endpoint);
        self
    }
}

/// The pending rules: the first match applies and uses one of its times.
pub(crate) fn take(rules: &mut Vec<FaultRule>, endpoint: Endpoint) -> Option<Fault> {
    let index = rules
        .iter()
        .position(|rule| rule.endpoint.is_none_or(|wanted| wanted == endpoint))?;
    let rule = &mut rules[index];
    let fault = rule.fault.clone();
    if let Some(times) = rule.times.as_mut() {
        *times = times.saturating_sub(1);
        if *times == 0 {
            rules.remove(index);
        }
    }
    Some(fault)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_apply_in_order_and_run_out() {
        let mut rules = vec![
            FaultRule::new(Fault::ServerError)
                .on(Endpoint::Chat)
                .times(2),
            FaultRule::new(Fault::BadRequest).always(),
        ];
        assert_eq!(take(&mut rules, Endpoint::Models), Some(Fault::BadRequest));
        assert_eq!(take(&mut rules, Endpoint::Chat), Some(Fault::ServerError));
        assert_eq!(take(&mut rules, Endpoint::Chat), Some(Fault::ServerError));
        assert_eq!(take(&mut rules, Endpoint::Chat), Some(Fault::BadRequest));
        assert_eq!(rules.len(), 1);
    }

    #[test]
    fn rules_read_from_json() {
        let rule: FaultRule = serde_json::from_str(
            r#"{"fault": {"kind": "rate_limited", "retry_after_ms": 50}, "endpoint": "chat"}"#,
        )
        .unwrap();
        assert_eq!(rule.fault, Fault::RateLimited { retry_after_ms: 50 });
        assert_eq!(rule.times, Some(1));
        assert_eq!(rule.endpoint, Some(Endpoint::Chat));
        let rule: FaultRule =
            serde_json::from_str(r#"{"fault": {"kind": "timeout"}, "times": null}"#).unwrap();
        assert_eq!(rule.times, None);
        for name in ["rate_limited", "slow_first_token", "reset", "empty_stream"] {
            assert_eq!(Fault::from_name(name).unwrap().name(), name);
        }
    }
}
