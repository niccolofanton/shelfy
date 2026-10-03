//! How one call runs: timeouts, retries, cancellation and the streaming
//! callback (plan §2.15 "Reliability").
//!
//! Timeouts come from the caller, which knows the task: cloud cataloging
//! takes up to 120 s; chat wants its first token within 20 s and the whole
//! answer within 60 s; the operator provider has its own settings (P3-09).

use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::structured::StructuredMode;

/// The deadlines of a call and of each of its HTTP exchanges.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Timeouts {
    /// Opening the connection. Passing it is [`crate::ErrorKind::Offline`].
    pub connect: Duration,
    /// Streamed calls: the first token (text, reasoning or tool input) after
    /// sending, the response head included (llama.cpp sends it with the first
    /// token). Passing it is [`crate::ErrorKind::Transient`], never retried.
    pub first_token: Option<Duration>,
    /// One exchange, from sending to the last byte of the answer. Passing it
    /// is [`crate::ErrorKind::Transient`]. Each retry, the non-streaming retry
    /// and the repair call get their own.
    pub total: Duration,
    /// The whole call: every exchange, retry and backoff. Passing it is
    /// [`crate::ErrorKind::Transient`], never retried; no retry starts that
    /// would end after it.
    pub overall: Option<Duration>,
}

impl Timeouts {
    /// Cloud cataloging: 10 s to connect, 120 s per exchange.
    pub const CATALOG: Self = Self::new(Duration::from_secs(10), Duration::from_secs(120));
    /// Chat: 10 s to connect, the first token within 20 s, 60 s in all.
    pub const CHAT: Self = Self::new(Duration::from_secs(10), Duration::from_secs(60))
        .with_first_token(Duration::from_secs(20))
        .with_overall(Duration::from_secs(60));

    /// `connect` and `total`, without a first-token or call deadline.
    #[must_use]
    pub const fn new(connect: Duration, total: Duration) -> Self {
        Self {
            connect,
            first_token: None,
            total,
            overall: None,
        }
    }

    /// With a first-token deadline for streamed calls.
    #[must_use]
    pub const fn with_first_token(mut self, first_token: Duration) -> Self {
        self.first_token = Some(first_token);
        self
    }

    /// With a deadline for the whole call.
    #[must_use]
    pub const fn with_overall(mut self, overall: Duration) -> Self {
        self.overall = Some(overall);
        self
    }
}

/// Retries of transient and rate-limited errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Retries after the first try (3 by default).
    pub max_retries: u32,
    /// The first backoff; it doubles with each retry, with jitter.
    pub base_delay: Duration,
    /// The longest backoff.
    pub max_delay: Duration,
    /// The longest `Retry-After` honored. A provider asking for more gets no
    /// retry: the error goes back to the caller with the wait, to reschedule.
    pub retry_after_cap: Duration,
}

impl RetryPolicy {
    /// No retry at all (chat falls back at once).
    pub const NONE: Self = Self {
        max_retries: 0,
        base_delay: Duration::ZERO,
        max_delay: Duration::ZERO,
        retry_after_cap: Duration::ZERO,
    };
}

impl Default for RetryPolicy {
    /// 3 retries, 500 ms doubling up to 8 s, `Retry-After` up to 30 s.
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(8),
            retry_after_cap: Duration::from_secs(30),
        }
    }
}

/// Receives the answer's text so far, each time a streamed chunk adds to it
/// (the JSON so far for JSON answers). A retry starts the text over.
pub type TextCallback = Arc<dyn Fn(&str) + Send + Sync>;

/// How one call runs.
#[derive(Clone)]
pub struct CallOptions {
    /// The deadlines of each exchange.
    pub timeouts: Timeouts,
    /// Retries of transient and rate-limited errors.
    pub retry: RetryPolicy,
    /// Cancels the call: it ends with [`crate::ErrorKind::Cancelled`].
    pub cancel: Option<CancellationToken>,
    /// Streamed calls: the text so far.
    pub on_text: Option<TextCallback>,
    /// Overrides the provider's structured-output mode for this call.
    pub structured: Option<StructuredMode>,
}

impl CallOptions {
    /// `timeouts` with the default retries, no cancellation and no callback.
    #[must_use]
    pub fn new(timeouts: Timeouts) -> Self {
        Self {
            timeouts,
            retry: RetryPolicy::default(),
            cancel: None,
            on_text: None,
            structured: None,
        }
    }

    /// With `retry`.
    #[must_use]
    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
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
    pub fn with_text_callback(mut self, callback: impl Fn(&str) + Send + Sync + 'static) -> Self {
        self.on_text = Some(Arc::new(callback));
        self
    }

    /// With the structured-output mode `mode` for this call.
    #[must_use]
    pub fn with_structured(mut self, mode: StructuredMode) -> Self {
        self.structured = Some(mode);
        self
    }
}

impl fmt::Debug for CallOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CallOptions")
            .field("timeouts", &self.timeouts)
            .field("retry", &self.retry)
            .field("cancel", &self.cancel.is_some())
            .field("on_text", &self.on_text.is_some())
            .field("structured", &self.structured)
            .finish()
    }
}
