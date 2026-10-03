//! What every adapter shares on the wire: endpoint URLs, headers, deadlines,
//! reading answers, and mapping failures to [`ErrorKind`]s.

use std::future::Future;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use futures_util::StreamExt as _;
use http::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use secrecy::{ExposeSecret, SecretString};
use serde_json::Value;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use url::Url;

use crate::error::{AiError, ErrorKind, RetryHint};
use crate::transport::{BodyStream, TransportError};

/// The `User-Agent` of every call.
pub(crate) const AGENT: &str = concat!("shelfy/", env!("CARGO_PKG_VERSION"));

/// The largest answer read whole (chat, embeddings, model lists): 32 MiB.
pub(crate) const MAX_ANSWER_BYTES: usize = 32 << 20;

/// The largest error body read for its message: 64 KiB.
const MAX_ERROR_BYTES: usize = 64 << 10;

/// `base` as configured, plus `suffix`: the base URL is used verbatim, so
/// Gemini's `…/v1beta/openai` keeps its path (plan §2.15). One trailing slash
/// of the base is dropped.
pub(crate) fn endpoint(base: &Url, suffix: &str) -> Result<Url, AiError> {
    let joined = format!("{}{suffix}", base.as_str().trim_end_matches('/'));
    Url::parse(&joined).map_err(|_| AiError::bad_request("the endpoint URL is not valid"))
}

/// Headers of a JSON exchange.
pub(crate) fn json_headers(stream: bool) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static(if stream {
            "text/event-stream"
        } else {
            "application/json"
        }),
    );
    headers.insert(USER_AGENT, HeaderValue::from_static(AGENT));
    headers
}

/// Adds `name: prefix + key`, marked sensitive (its `Debug` prints
/// `Sensitive`).
pub(crate) fn put_key(
    headers: &mut HeaderMap,
    name: HeaderName,
    prefix: &str,
    key: &SecretString,
) -> Result<(), AiError> {
    let mut value = HeaderValue::from_str(&format!("{prefix}{}", key.expose_secret()))
        .map_err(|_| AiError::bad_request("the key holds characters a header cannot carry"))?;
    value.set_sensitive(true);
    headers.insert(name, value);
    Ok(())
}

/// Runs `future` until `deadline`, unless `cancel` fires first.
pub(crate) async fn bounded<T>(
    future: impl Future<Output = T>,
    deadline: Instant,
    cancel: Option<&CancellationToken>,
    on_timeout: impl FnOnce() -> AiError,
) -> Result<T, AiError> {
    let timed = tokio::time::timeout_at(deadline, future);
    let result = match cancel {
        Some(token) => tokio::select! {
            biased;
            () = token.cancelled() => return Err(AiError::cancelled()),
            result = timed => result,
        },
        None => timed.await,
    };
    result.map_err(|_| on_timeout())
}

/// A transport failure as an AI error.
pub(crate) fn from_transport(error: TransportError) -> AiError {
    match error {
        TransportError::Blocked(guard) => AiError::new(
            ErrorKind::Refused,
            format!("the destination was refused: {guard}"),
        ),
        TransportError::Connect(failure) => {
            AiError::offline(format!("could not connect: {failure}"))
        }
        TransportError::Io(detail) => AiError::transient(format!("the exchange failed: {detail}")),
        TransportError::Unsupported(detail) => AiError::unsupported(detail),
    }
}

/// A moment an exchange must not pass, and the error it ends with if it does.
#[derive(Clone, Debug)]
pub(crate) struct Deadline {
    pub(crate) at: Instant,
    pub(crate) error: AiError,
}

impl Deadline {
    /// One exchange's own deadline: `total` after `sent_at`.
    pub(crate) fn total(sent_at: Instant, total: Duration) -> Self {
        Self {
            at: sent_at + total,
            error: AiError::transient(format!(
                "the answer took longer than {} ms",
                total.as_millis()
            )),
        }
    }

    /// The first token's deadline: `wait` after `sent_at`. Never retried.
    pub(crate) fn first_token(sent_at: Instant, wait: Duration) -> Self {
        Self {
            at: sent_at + wait,
            error: AiError::transient(format!("no token arrived within {} ms", wait.as_millis()))
                .with_hint(RetryHint::Never),
        }
    }

    /// The whole call's deadline: `overall` after `started`. Never retried.
    pub(crate) fn overall(started: Instant, overall: Duration) -> Self {
        Self {
            at: started + overall,
            error: AiError::transient(format!(
                "the call took longer than {} ms",
                overall.as_millis()
            ))
            .with_hint(RetryHint::Never),
        }
    }

    /// The earlier of `self` and `other`.
    #[must_use]
    pub(crate) fn min(self, other: Option<Self>) -> Self {
        match other {
            Some(other) if other.at < self.at => other,
            _ => self,
        }
    }

    /// Runs `future` until this deadline, unless `cancel` fires first.
    pub(crate) async fn run<T>(
        &self,
        future: impl Future<Output = T>,
        cancel: Option<&CancellationToken>,
    ) -> Result<T, AiError> {
        bounded(future, self.at, cancel, || self.error.clone()).await
    }
}

/// An answer whose status and headers arrived.
pub(crate) struct Opened {
    pub(crate) headers: HeaderMap,
    body: BodyStream,
    pub(crate) sent_at: Instant,
    deadline: Deadline,
}

impl Opened {
    /// The answer of an exchange sent at `sent_at`, whose body must end by
    /// `deadline`.
    pub(crate) fn new(
        headers: HeaderMap,
        body: BodyStream,
        sent_at: Instant,
        deadline: Deadline,
    ) -> Self {
        Self {
            headers,
            body,
            sent_at,
            deadline,
        }
    }

    /// The next chunk, or `None` at the end. It must arrive before the
    /// exchange's deadline, and before `early` when given (the first-token
    /// deadline). A failure here broke an answer that had started.
    pub(crate) async fn next_chunk(
        &mut self,
        early: Option<Deadline>,
        cancel: Option<&CancellationToken>,
    ) -> Result<Option<Bytes>, AiError> {
        let deadline = self.deadline.clone().min(early);
        match deadline.run(self.body.next(), cancel).await? {
            None => Ok(None),
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(error)) => Err(from_transport(error).with_hint(RetryHint::StreamBroken)),
        }
    }

    /// The whole body, at most `cap` bytes.
    pub(crate) async fn read_all(
        mut self,
        cap: usize,
        cancel: Option<&CancellationToken>,
    ) -> Result<Bytes, AiError> {
        let mut body = BytesMut::new();
        // A body that breaks off is an ordinary transient failure here: the
        // whole exchange can be retried.
        let unbroken = |error: AiError| match error.hint() {
            RetryHint::StreamBroken => error.with_hint(RetryHint::Normal),
            _ => error,
        };
        while let Some(chunk) = self.next_chunk(None, cancel).await.map_err(unbroken)? {
            if body.len() + chunk.len() > cap {
                // The same answer would come back: no retry.
                return Err(
                    AiError::transient(format!("the answer is larger than {cap} bytes"))
                        .with_hint(RetryHint::Never),
                );
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body.freeze())
    }

    /// The error answer of `status`, read as far as a short deadline and
    /// [`MAX_ERROR_BYTES`] allow.
    pub(crate) async fn into_error(
        mut self,
        status: StatusCode,
        cancel: Option<&CancellationToken>,
        key: Option<&SecretString>,
    ) -> AiError {
        let mut body = BytesMut::new();
        let early = Deadline {
            at: Instant::now() + Duration::from_secs(5),
            error: AiError::transient("the error answer was slow"),
        };
        while body.len() < MAX_ERROR_BYTES {
            match self.next_chunk(Some(early.clone()), cancel).await {
                Ok(Some(chunk)) => body.extend_from_slice(&chunk),
                Err(error) if error.kind() == ErrorKind::Cancelled => return error,
                Ok(None) | Err(_) => break,
            }
        }
        body.truncate(MAX_ERROR_BYTES);
        from_status(status, &self.headers, &body, key)
    }
}

/// The error of a non-2xx answer.
pub(crate) fn from_status(
    status: StatusCode,
    headers: &HeaderMap,
    body: &[u8],
    key: Option<&SecretString>,
) -> AiError {
    let detail = ProviderError::parse(body);
    let code = status.as_u16();
    let quota = detail.is_quota(Some(code));
    let kind = match code {
        300..=399 => ErrorKind::BadRequest,
        401 | 403 => ErrorKind::InvalidKey,
        402 => ErrorKind::QuotaExhausted,
        408 | 409 | 425 => ErrorKind::Transient,
        _ if quota && (400..=499).contains(&code) => ErrorKind::QuotaExhausted,
        429 => ErrorKind::RateLimited,
        400..=499 => ErrorKind::BadRequest,
        _ => ErrorKind::Transient,
    };
    let message = if (300..=399).contains(&code) {
        "the provider answered with a redirect, which is not followed".to_owned()
    } else {
        detail
            .message
            .as_deref()
            .map(|message| scrub(message, key))
            .unwrap_or_else(|| format!("the provider answered HTTP {code}"))
    };
    let mut error = AiError::new(kind, message).with_status(code);
    if let Some(code) = detail.code {
        error = error.with_code(scrub(&code, key));
    }
    if matches!(kind, ErrorKind::RateLimited | ErrorKind::Transient)
        && let Some(wait) = retry_after(headers)
    {
        error = error.with_retry_after(wait);
    }
    error
}

/// An error a provider sent inside an answer (a stream event, a 200 body).
pub(crate) fn from_error_value(error: &Value, key: Option<&SecretString>) -> AiError {
    let detail = ProviderError::from_value(error);
    let status = error
        .get("code")
        .and_then(Value::as_u64)
        .or_else(|| error.get("status").and_then(Value::as_u64))
        .and_then(|code| u16::try_from(code).ok());
    let kind = match (status, detail.code.as_deref()) {
        _ if detail.is_quota(status) => ErrorKind::QuotaExhausted,
        (Some(401 | 403), _) | (_, Some("authentication_error" | "permission_error")) => {
            ErrorKind::InvalidKey
        }
        (Some(429), _) | (_, Some("rate_limit_error" | "rate_limit_exceeded")) => {
            ErrorKind::RateLimited
        }
        (Some(400..=428 | 430..=499), _) | (_, Some("invalid_request_error")) => {
            ErrorKind::BadRequest
        }
        _ => ErrorKind::Transient,
    };
    let message = detail.message.as_deref().map_or_else(
        || "the provider sent an error".to_owned(),
        |message| scrub(message, key),
    );
    let mut error = AiError::new(kind, message);
    if let Some(status) = status {
        error = error.with_status(status);
    }
    if let Some(code) = detail.code {
        error = error.with_code(scrub(&code, key));
    }
    error
}

/// `message` without the key, should a provider echo it.
fn scrub(message: &str, key: Option<&SecretString>) -> String {
    match key.map(ExposeSecret::expose_secret) {
        Some(key) if !key.is_empty() => message.replace(key, "[redacted]"),
        _ => message.to_owned(),
    }
}

/// The wait a provider asked for: `retry-after-ms` (OpenAI), else
/// `Retry-After` in seconds. HTTP dates are ignored.
pub(crate) fn retry_after(headers: &HeaderMap) -> Option<Duration> {
    let seconds = |name: &str, scale: f64| {
        headers
            .get(name)?
            .to_str()
            .ok()?
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|value| value.is_finite() && *value >= 0.0)
            .map(|value| Duration::from_secs_f64((value / scale).min(86_400.0)))
    };
    seconds("retry-after-ms", 1000.0).or_else(|| seconds("retry-after", 1.0))
}

/// The message and code of an error body, in any of the shapes providers
/// use: `{"error": {"message", "type", "code"}}` (OpenAI, Anthropic and most
/// compatibles), `{"error": "…"}` (whisper.cpp, llama.cpp), `[{"error": …}]`
/// (Gemini), `{"message"}` or `{"detail"}`.
#[derive(Debug, Default)]
struct ProviderError {
    message: Option<String>,
    code: Option<String>,
    codes: Vec<String>,
}

impl ProviderError {
    fn parse(body: &[u8]) -> Self {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return Self::default();
        };
        let value = match &value {
            Value::Array(items) => items.first().cloned().unwrap_or(Value::Null),
            _ => value,
        };
        match value.get("error") {
            Some(error) if !error.is_null() => Self::from_value(error),
            _ => Self::from_value(&value),
        }
    }

    fn from_value(value: &Value) -> Self {
        let text = |name: &str| value.get(name).and_then(Value::as_str).map(str::to_owned);
        let codes: Vec<String> = ["code", "type", "status"]
            .iter()
            .filter_map(|name| match value.get(*name) {
                Some(Value::String(code)) if !code.is_empty() => Some(code.clone()),
                _ => None,
            })
            .collect();
        let message = match value {
            Value::String(message) => Some(message.clone()),
            _ => text("message")
                .or_else(|| text("detail"))
                .or_else(|| text("error")),
        };
        Self {
            message: message.filter(|message| !message.trim().is_empty()),
            code: codes.first().cloned(),
            codes,
        }
    }

    /// Whether the error says the account has no credit or quota left. A 429
    /// counts only with an explicit code (OpenAI's `insufficient_quota`):
    /// Gemini's per-minute limits read "You exceeded your current quota" too,
    /// and they pass.
    fn is_quota(&self, status: Option<u16>) -> bool {
        const CODES: [&str; 4] = [
            "insufficient_quota",
            "billing_error",
            "billing_hard_limit_reached",
            "insufficient_credits",
        ];
        const PHRASES: [&str; 3] = [
            "exceeded your current quota",
            "credit balance is too low",
            "insufficient credits",
        ];
        let by_code = self.codes.iter().any(|code| CODES.contains(&code.as_str()));
        if status == Some(429) {
            return by_code;
        }
        by_code
            || self.message.as_deref().is_some_and(|message| {
                let message = message.to_ascii_lowercase();
                PHRASES.iter().any(|phrase| message.contains(phrase))
            })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn status_error(code: u16, body: &str, headers: &[(&str, &str)]) -> AiError {
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            map.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_str(value).unwrap(),
            );
        }
        from_status(
            StatusCode::from_u16(code).unwrap(),
            &map,
            body.as_bytes(),
            None,
        )
    }

    #[test]
    fn statuses_map_to_kinds() {
        for (code, kind) in [
            (301, ErrorKind::BadRequest),
            (302, ErrorKind::BadRequest),
            (400, ErrorKind::BadRequest),
            (401, ErrorKind::InvalidKey),
            (402, ErrorKind::QuotaExhausted),
            (403, ErrorKind::InvalidKey),
            (404, ErrorKind::BadRequest),
            (408, ErrorKind::Transient),
            (413, ErrorKind::BadRequest),
            (422, ErrorKind::BadRequest),
            (429, ErrorKind::RateLimited),
            (500, ErrorKind::Transient),
            (502, ErrorKind::Transient),
            (503, ErrorKind::Transient),
            (529, ErrorKind::Transient),
        ] {
            assert_eq!(status_error(code, "", &[]).kind(), kind, "{code}");
        }
    }

    #[test]
    fn quota_answers_are_not_rate_limits() {
        let openai = r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#;
        let error = status_error(429, openai, &[("retry-after", "20")]);
        assert_eq!(error.kind(), ErrorKind::QuotaExhausted);
        assert_eq!(error.code(), Some("insufficient_quota"));
        assert_eq!(error.retry_after(), None);
        let anthropic = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Your credit balance is too low to access the Anthropic API."}}"#;
        assert_eq!(
            status_error(400, anthropic, &[]).kind(),
            ErrorKind::QuotaExhausted
        );
        // Gemini's per-minute limits name the quota too: a 429 needs a code.
        for message in [
            "Resource has been exhausted (e.g. check quota).",
            "You exceeded your current quota, please check your plan and billing details.",
        ] {
            let gemini = json!([{"error": {"code": 429, "message": message, "status": "RESOURCE_EXHAUSTED"}}]);
            let error = status_error(429, &gemini.to_string(), &[("retry-after", "7")]);
            assert_eq!(error.kind(), ErrorKind::RateLimited, "{message}");
            assert_eq!(error.code(), Some("RESOURCE_EXHAUSTED"));
            assert_eq!(error.retry_after(), Some(Duration::from_secs(7)));
            let in_stream = json!({"code": 429, "message": message});
            assert_eq!(
                from_error_value(&in_stream, None).kind(),
                ErrorKind::RateLimited
            );
        }
        let openrouter = r#"{"error":{"code":402,"message":"Insufficient credits"}}"#;
        assert_eq!(
            status_error(402, openrouter, &[]).kind(),
            ErrorKind::QuotaExhausted
        );
    }

    #[test]
    fn retry_after_prefers_milliseconds() {
        let error = status_error(
            429,
            "{}",
            &[("retry-after-ms", "250"), ("retry-after", "3")],
        );
        assert_eq!(error.retry_after(), Some(Duration::from_millis(250)));
        let error = status_error(503, "{}", &[("retry-after", "2")]);
        assert_eq!(error.retry_after(), Some(Duration::from_secs(2)));
        let error = status_error(
            429,
            "{}",
            &[("retry-after", "Wed, 21 Oct 2015 07:28:00 GMT")],
        );
        assert_eq!(error.retry_after(), None);
    }

    #[test]
    fn messages_come_from_every_shape_and_never_hold_the_key() {
        assert_eq!(
            status_error(400, r#"{"error":"no 'file' field"}"#, &[]).message(),
            "no 'file' field"
        );
        assert_eq!(
            status_error(400, r#"{"detail":"bad"}"#, &[]).message(),
            "bad"
        );
        assert_eq!(
            status_error(500, "<html>oops</html>", &[]).message(),
            "the provider answered HTTP 500"
        );
        let key = SecretString::from("sk-planted-0123456789");
        let body = r#"{"error":{"message":"Incorrect API key provided: sk-planted-0123456789.","code":"invalid_api_key"}}"#;
        let error = from_status(
            StatusCode::UNAUTHORIZED,
            &HeaderMap::new(),
            body.as_bytes(),
            Some(&key),
        );
        assert!(!error.to_string().contains("sk-planted-0123456789"));
        assert!(error.message().contains("[redacted]"));
    }

    #[test]
    fn errors_inside_answers_map_by_code_or_type() {
        let overloaded = json!({"type": "overloaded_error", "message": "Overloaded"});
        assert_eq!(
            from_error_value(&overloaded, None).kind(),
            ErrorKind::Transient
        );
        let limited = json!({"type": "rate_limit_error", "message": "slow"});
        assert_eq!(
            from_error_value(&limited, None).kind(),
            ErrorKind::RateLimited
        );
        let openrouter = json!({"code": 402, "message": "Insufficient credits"});
        assert_eq!(
            from_error_value(&openrouter, None).kind(),
            ErrorKind::QuotaExhausted
        );
        let invalid = json!({"code": 401, "message": "No auth credentials found"});
        assert_eq!(
            from_error_value(&invalid, None).kind(),
            ErrorKind::InvalidKey
        );
    }

    #[test]
    fn endpoints_keep_the_base_path() {
        let base = Url::parse("https://generativelanguage.googleapis.com/v1beta/openai/").unwrap();
        assert_eq!(
            endpoint(&base, "/chat/completions").unwrap().as_str(),
            "https://generativelanguage.googleapis.com/v1beta/openai/chat/completions"
        );
        let root = Url::parse("https://api.anthropic.com").unwrap();
        assert_eq!(
            endpoint(&root, "/v1/messages").unwrap().as_str(),
            "https://api.anthropic.com/v1/messages"
        );
    }
}
