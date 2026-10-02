//! Sliding-window limits on sign-in requests (plan §2.9, §2.11):
//!
//! - per client IP: 10 per minute on the sign-in routes, from
//!   `CF-Connecting-IP` (trusted: the tunnel is the only ingress);
//! - per email address: 3 sign-in emails per hour, counted for every address,
//!   known or not, so a 429 says nothing about whether an account exists.
//!
//! Keys are SHA-256 digests, so the limiter holds no address or IP in clear.
//! Memory is bounded: an idle key is dropped after one window, and the cache
//! holds at most [`MAX_KEYS`] keys. P1-15 brings the general rate limits and
//! may fold these in.

use std::collections::VecDeque;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::http::{HeaderMap, HeaderName};
use moka::sync::Cache;
use sha2::{Digest, Sha256};

/// Header with the client IP, set by Cloudflare.
pub const CLIENT_IP_HEADER: HeaderName = HeaderName::from_static("cf-connecting-ip");

/// Most keys one limiter tracks.
pub const MAX_KEYS: u64 = 10_000;

/// At most `max` hits per `window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RateLimit {
    /// Hits allowed in one window.
    pub max: u32,
    /// The window.
    pub window: Duration,
}

/// A digest naming what is limited.
pub type Key = [u8; 32];

/// A [`RateLimit`] per key.
pub struct RateLimiter {
    limit: RateLimit,
    hits: Cache<Key, Arc<Mutex<VecDeque<i64>>>>,
}

impl RateLimiter {
    /// A limiter for `limit`.
    #[must_use]
    pub fn new(limit: RateLimit) -> Self {
        Self {
            limit,
            hits: Cache::builder()
                .max_capacity(MAX_KEYS)
                .time_to_idle(limit.window.max(Duration::from_secs(1)))
                .build(),
        }
    }

    /// Counts a hit on `key` at `now` (unix ms). Over the limit, the hit is
    /// refused (and not counted) with the seconds until a slot frees up.
    ///
    /// # Errors
    ///
    /// The retry delay in seconds, at least 1.
    pub fn hit(&self, key: &Key, now: i64) -> Result<(), u32> {
        let window = i64::try_from(self.limit.window.as_millis()).unwrap_or(i64::MAX);
        let hits = self.hits.get_with(*key, Arc::default);
        let mut hits = hits.lock().unwrap_or_else(PoisonError::into_inner);
        while hits
            .front()
            .is_some_and(|&at| at <= now.saturating_sub(window))
        {
            hits.pop_front();
        }
        let max = usize::try_from(self.limit.max).unwrap_or(usize::MAX);
        if hits.len() < max {
            hits.push_back(now);
            return Ok(());
        }
        let oldest = hits.front().copied().unwrap_or(now);
        let wait_ms = oldest.saturating_add(window).saturating_sub(now).max(1);
        let seconds = wait_ms.div_euclid(1000) + i64::from(wait_ms.rem_euclid(1000) > 0);
        Err(u32::try_from(seconds).unwrap_or(u32::MAX).max(1))
    }
}

/// The key of `value` in the `kind` namespace (`ip`, `email`).
#[must_use]
pub fn key(kind: &str, value: &str) -> Key {
    let mut hasher = Sha256::new();
    hasher.update(kind.as_bytes());
    hasher.update([0]);
    hasher.update(value.as_bytes());
    hasher.finalize().into()
}

/// The client IP from `CF-Connecting-IP`, if present and valid.
#[must_use]
pub fn client_ip(headers: &HeaderMap) -> Option<IpAddr> {
    headers
        .get(CLIENT_IP_HEADER)?
        .to_str()
        .ok()?
        .trim()
        .parse()
        .ok()
}

/// The per-IP key of a request. Without `CF-Connecting-IP` (a local run, or a
/// proxy that does not set it) every request shares one key, which keeps the
/// limit strict instead of open.
#[must_use]
pub fn ip_key(headers: &HeaderMap) -> Key {
    match client_ip(headers) {
        Some(ip) => key("ip", &ip.to_string()),
        None => key("ip", "unknown"),
    }
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    const MINUTE: i64 = 60_000;

    #[test]
    fn a_window_admits_max_hits_then_slides() {
        let limiter = RateLimiter::new(RateLimit {
            max: 3,
            window: Duration::from_secs(3600),
        });
        let a = key("email", "owner@example.test");
        let b = key("email", "other@example.test");
        let t0 = 1_790_899_200_000;
        assert_eq!(limiter.hit(&a, t0), Ok(()));
        assert_eq!(limiter.hit(&a, t0 + 10 * MINUTE), Ok(()));
        assert_eq!(limiter.hit(&a, t0 + 20 * MINUTE), Ok(()));
        // The 4th within the hour waits until the first leaves the window.
        assert_eq!(limiter.hit(&a, t0 + 30 * MINUTE), Err(30 * 60));
        assert_eq!(limiter.hit(&a, t0 + 60 * MINUTE - 500), Err(1));
        // Refused hits were not counted: one slot frees at t0 + 60 min.
        assert_eq!(
            limiter.hit(&b, t0 + 30 * MINUTE),
            Ok(()),
            "keys are separate"
        );
        assert_eq!(limiter.hit(&a, t0 + 60 * MINUTE), Ok(()));
        assert_eq!(limiter.hit(&a, t0 + 61 * MINUTE), Err(9 * 60));
    }

    #[test]
    fn keys_are_namespaced_digests() {
        assert_ne!(key("ip", "1.2.3.4"), key("email", "1.2.3.4"));
        assert_eq!(key("ip", "1.2.3.4"), key("ip", "1.2.3.4"));
    }

    #[test]
    fn the_client_ip_comes_from_cloudflare() {
        let mut headers = HeaderMap::new();
        assert_eq!(client_ip(&headers), None);
        assert_eq!(ip_key(&headers), key("ip", "unknown"));
        headers.insert(CLIENT_IP_HEADER, HeaderValue::from_static(" 203.0.113.7 "));
        assert_eq!(client_ip(&headers), Some("203.0.113.7".parse().unwrap()));
        assert_eq!(ip_key(&headers), key("ip", "203.0.113.7"));
        headers.insert(CLIENT_IP_HEADER, HeaderValue::from_static("2001:DB8::1"));
        assert_eq!(ip_key(&headers), key("ip", "2001:db8::1"), "canonical form");
        headers.insert(CLIENT_IP_HEADER, HeaderValue::from_static("not-an-ip"));
        assert_eq!(ip_key(&headers), key("ip", "unknown"));
    }
}
