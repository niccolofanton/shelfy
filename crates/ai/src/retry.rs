//! Retries with backoff and jitter (plan §2.15 "Reliability"): up to
//! [`RetryPolicy::max_retries`] more tries on transient and rate-limited
//! errors, honoring the provider's `Retry-After` up to a cap. Offline, an
//! invalid key, an exhausted quota and every other kind end the call at once.

use std::future::Future;
use std::time::Duration;

use tokio_util::sync::CancellationToken;

use crate::error::AiError;
use crate::options::RetryPolicy;

/// Runs `attempt` until it succeeds or may not be retried. Returns its result
/// and how many tries ran.
pub(crate) async fn retrying<T, F, Fut>(
    policy: &RetryPolicy,
    cancel: Option<&CancellationToken>,
    mut attempt: F,
) -> (Result<T, AiError>, u32)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, AiError>>,
{
    let mut tries = 0;
    loop {
        tries += 1;
        let error = match attempt().await {
            Ok(value) => return (Ok(value), tries),
            Err(error) => error,
        };
        if !error.should_retry() || tries > policy.max_retries {
            return (Err(error), tries);
        }
        let wait = match error.retry_after() {
            Some(wait) if wait > policy.retry_after_cap => return (Err(error), tries),
            Some(wait) => wait,
            None => backoff(policy, tries - 1),
        };
        tracing::debug!(
            kind = %error.kind(),
            status = error.status(),
            retry = tries,
            wait_ms = u64::try_from(wait.as_millis()).unwrap_or(u64::MAX),
            "retrying an AI call",
        );
        if let Err(cancelled) = sleep(wait, cancel).await {
            return (Err(cancelled), tries);
        }
    }
}

/// Sleeps `wait`, unless `cancel` fires first.
pub(crate) async fn sleep(
    wait: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<(), AiError> {
    match cancel {
        Some(token) => tokio::select! {
            biased;
            () = token.cancelled() => Err(AiError::cancelled()),
            () = tokio::time::sleep(wait) => Ok(()),
        },
        None => {
            tokio::time::sleep(wait).await;
            Ok(())
        }
    }
}

/// The wait before retry `n` (0-based): the base delay doubled `n` times, at
/// most the maximum, then between half and all of it at random.
fn backoff(policy: &RetryPolicy, n: u32) -> Duration {
    let full = policy
        .base_delay
        .saturating_mul(1 << n.min(16))
        .min(policy.max_delay);
    let half = full / 2;
    half + jitter(full - half)
}

/// A random duration in `0..=max`.
fn jitter(max: Duration) -> Duration {
    let nanos = u64::try_from(max.as_nanos()).unwrap_or(u64::MAX);
    if nanos == 0 {
        return Duration::ZERO;
    }
    let random = getrandom::u64().unwrap_or(0);
    Duration::from_nanos(random % nanos.saturating_add(1))
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::error::{ErrorKind, RetryHint};

    fn fast() -> RetryPolicy {
        RetryPolicy {
            max_retries: 3,
            base_delay: Duration::from_millis(1),
            max_delay: Duration::from_millis(4),
            retry_after_cap: Duration::from_millis(50),
        }
    }

    async fn run(policy: RetryPolicy, errors: Vec<AiError>) -> (Result<u32, AiError>, u32) {
        let calls = AtomicU32::new(0);
        retrying(&policy, None, || {
            let n = calls.fetch_add(1, Ordering::SeqCst) as usize;
            let next = errors.get(n).cloned();
            async move { next.map_or(Ok(7), Err) }
        })
        .await
    }

    #[tokio::test]
    async fn transient_and_rate_limited_errors_are_retried() {
        let errors = vec![
            AiError::transient("500"),
            AiError::new(ErrorKind::RateLimited, "429").with_retry_after(Duration::from_millis(5)),
        ];
        let (result, tries) = run(fast(), errors).await;
        assert_eq!(result, Ok(7));
        assert_eq!(tries, 3);
    }

    #[tokio::test]
    async fn retries_stop_after_the_maximum() {
        let errors = vec![AiError::transient("500"); 10];
        let (result, tries) = run(fast(), errors).await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Transient);
        assert_eq!(tries, 4);
    }

    #[tokio::test]
    async fn offline_invalid_key_and_quota_are_never_retried() {
        for kind in [
            ErrorKind::Offline,
            ErrorKind::InvalidKey,
            ErrorKind::QuotaExhausted,
            ErrorKind::BadRequest,
        ] {
            let (result, tries) = run(fast(), vec![AiError::new(kind, "x")]).await;
            assert_eq!(result.unwrap_err().kind(), kind);
            assert_eq!(tries, 1, "{kind}");
        }
    }

    #[tokio::test]
    async fn a_retry_after_past_the_cap_goes_back_to_the_caller() {
        let error =
            AiError::new(ErrorKind::RateLimited, "429").with_retry_after(Duration::from_secs(60));
        let (result, tries) = run(fast(), vec![error]).await;
        assert_eq!(
            result.unwrap_err().retry_after(),
            Some(Duration::from_secs(60))
        );
        assert_eq!(tries, 1);
    }

    #[tokio::test]
    async fn hinted_errors_are_not_retried() {
        for hint in [RetryHint::Never, RetryHint::StreamBroken] {
            let (_, tries) = run(fast(), vec![AiError::transient("x").with_hint(hint)]).await;
            assert_eq!(tries, 1);
        }
    }

    #[tokio::test]
    async fn cancelling_ends_the_wait() {
        let token = CancellationToken::new();
        token.cancel();
        let slow = RetryPolicy {
            base_delay: Duration::from_secs(60),
            max_delay: Duration::from_secs(60),
            ..fast()
        };
        let (result, tries) = retrying(&slow, Some(&token), || async {
            Err::<(), _>(AiError::transient("500"))
        })
        .await;
        assert_eq!(result.unwrap_err().kind(), ErrorKind::Cancelled);
        assert_eq!(tries, 1);
    }

    #[test]
    fn backoff_doubles_up_to_the_maximum_with_jitter() {
        let policy = RetryPolicy {
            max_retries: 10,
            base_delay: Duration::from_millis(100),
            max_delay: Duration::from_millis(800),
            retry_after_cap: Duration::from_secs(1),
        };
        for (n, full) in [(0, 100), (1, 200), (2, 400), (3, 800), (9, 800)] {
            let wait = backoff(&policy, n);
            assert!(wait >= Duration::from_millis(full / 2), "{n}: {wait:?}");
            assert!(wait <= Duration::from_millis(full), "{n}: {wait:?}");
        }
    }
}
