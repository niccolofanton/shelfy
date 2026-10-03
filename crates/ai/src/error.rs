//! The errors of an AI call (plan §2.15 "Reliability").
//!
//! Every failure maps to one [`ErrorKind`], which is what callers act on: the
//! AI service (P3-09) holds work on [`ErrorKind::Offline`], pauses a queue on
//! [`ErrorKind::InvalidKey`], reschedules on [`ErrorKind::RateLimited`], and so
//! on. The message is short and safe to log: it never holds a key, a prompt or
//! an answer.

use std::fmt;
use std::time::Duration;

use serde::Serialize;

/// What went wrong, as callers act on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    /// The provider cannot be reached: connection refused, no route, DNS
    /// failure, connect timeout. Never retried here: the caller holds the work.
    Offline,
    /// 401 or 403: the key is wrong, revoked or not allowed. Never retried.
    InvalidKey,
    /// 429: [`AiError::retry_after`] says when to come back.
    RateLimited,
    /// The account has no credit or quota left (402, `insufficient_quota`).
    /// Never retried.
    QuotaExhausted,
    /// 5xx, read timeouts, resets, an interrupted or malformed answer.
    Transient,
    /// The provider rejected the request (4xx), or answered with a redirect.
    BadRequest,
    /// Refused: the destination, by the egress guard ([`crate::guard`]), or
    /// the answer, by the model or the provider's filter.
    Refused,
    /// The answer does not match the JSON schema, even after one repair call.
    SchemaInvalid,
    /// The provider does not offer this call (embeddings or STT on Anthropic).
    Unsupported,
    /// The caller cancelled the call.
    Cancelled,
}

impl ErrorKind {
    /// The kind's stable name, as in problem details and metrics.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Offline => "offline",
            Self::InvalidKey => "invalid_key",
            Self::RateLimited => "rate_limited",
            Self::QuotaExhausted => "quota_exhausted",
            Self::Transient => "transient",
            Self::BadRequest => "bad_request",
            Self::Refused => "refused",
            Self::SchemaInvalid => "schema_invalid",
            Self::Unsupported => "unsupported",
            Self::Cancelled => "cancelled",
        }
    }

    /// Whether the adapters retry it (transient and rate-limited errors only).
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::Transient | Self::RateLimited)
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// How the retry loop treats an error, beyond its kind.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RetryHint {
    /// Retried when the kind is retryable.
    Normal,
    /// Never retried: a first-token timeout (a slow provider gets slower).
    Never,
    /// The stream broke after the answer started: the caller falls back to
    /// one non-streaming call instead of streaming again.
    StreamBroken,
}

/// An AI call that failed.
#[derive(Clone, PartialEq, Eq)]
pub struct AiError {
    kind: ErrorKind,
    message: String,
    status: Option<u16>,
    code: Option<String>,
    retry_after: Option<Duration>,
    hint: RetryHint,
}

/// The longest provider message an error keeps.
const MAX_MESSAGE: usize = 300;

impl AiError {
    /// An error of `kind`. `message` must hold no key and no content.
    #[must_use]
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: truncate(message.into()),
            status: None,
            code: None,
            retry_after: None,
            hint: RetryHint::Normal,
        }
    }

    /// With the provider's HTTP status.
    #[must_use]
    pub fn with_status(mut self, status: u16) -> Self {
        self.status = Some(status);
        self
    }

    /// With the provider's error code or type (`insufficient_quota`,
    /// `overloaded_error`…).
    #[must_use]
    pub fn with_code(mut self, code: impl Into<String>) -> Self {
        self.code = Some(truncate(code.into()));
        self
    }

    /// With the wait the provider asked for (`Retry-After`).
    #[must_use]
    pub fn with_retry_after(mut self, wait: Duration) -> Self {
        self.retry_after = Some(wait);
        self
    }

    pub(crate) fn with_hint(mut self, hint: RetryHint) -> Self {
        self.hint = hint;
        self
    }

    /// What went wrong.
    #[must_use]
    pub fn kind(&self) -> ErrorKind {
        self.kind
    }

    /// A short description, safe to log.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// The provider's HTTP status, when it answered.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        self.status
    }

    /// The provider's error code or type, when it sent one.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        self.code.as_deref()
    }

    /// How long the provider asked to wait (`retry-after-ms` or
    /// `Retry-After`), on [`ErrorKind::RateLimited`] and some
    /// [`ErrorKind::Transient`] answers.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after
    }

    pub(crate) fn hint(&self) -> RetryHint {
        self.hint
    }

    /// Whether the retry loop may try again.
    pub(crate) fn should_retry(&self) -> bool {
        self.kind.is_retryable() && self.hint == RetryHint::Normal
    }

    pub(crate) fn offline(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Offline, message)
    }

    pub(crate) fn transient(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Transient, message)
    }

    pub(crate) fn bad_request(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::BadRequest, message)
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(ErrorKind::Unsupported, message)
    }

    pub(crate) fn cancelled() -> Self {
        Self::new(ErrorKind::Cancelled, "the call was cancelled")
    }
}

/// Cuts `text` to [`MAX_MESSAGE`] bytes on a character boundary.
fn truncate(mut text: String) -> String {
    if text.len() > MAX_MESSAGE {
        let mut end = MAX_MESSAGE;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
        text.push('…');
    }
    text
}

impl fmt::Display for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.kind, self.message)?;
        if let Some(status) = self.status {
            write!(f, " (HTTP {status})")?;
        }
        if let Some(wait) = self.retry_after {
            write!(f, " (retry after {} ms)", wait.as_millis())?;
        }
        Ok(())
    }
}

impl fmt::Debug for AiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AiError")
            .field("kind", &self.kind)
            .field("message", &self.message)
            .field("status", &self.status)
            .field("code", &self.code)
            .field("retry_after", &self.retry_after)
            .finish_non_exhaustive()
    }
}

impl std::error::Error for AiError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_transient_and_rate_limited_errors_are_retryable() {
        let retryable: Vec<_> = [
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
        ]
        .into_iter()
        .filter(|kind| kind.is_retryable())
        .collect();
        assert_eq!(retryable, [ErrorKind::RateLimited, ErrorKind::Transient]);
    }

    #[test]
    fn long_messages_are_cut_on_a_character_boundary() {
        let error = AiError::transient("é".repeat(400));
        assert!(error.message().len() <= MAX_MESSAGE + '…'.len_utf8());
        assert!(error.message().ends_with('…'));
    }

    #[test]
    fn display_names_the_kind_status_and_wait() {
        let error = AiError::new(ErrorKind::RateLimited, "slow down")
            .with_status(429)
            .with_retry_after(Duration::from_millis(1500));
        assert_eq!(
            error.to_string(),
            "rate_limited: slow down (HTTP 429) (retry after 1500 ms)"
        );
    }
}
