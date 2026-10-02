//! The time of the job system.
//!
//! Job rows store unix milliseconds (`run_at`, `lease_until`, …), while the
//! scheduler's timers are tokio timers. In production both follow the
//! machine: [`Clock::System`] reads the wall clock. Tests use
//! [`Clock::tokio`], whose milliseconds advance with tokio's clock, so paused
//! time (`tokio::time::pause`, `advance`) drives backoffs, leases, delayed
//! jobs and the nightly schedule deterministically.

use std::time::Duration;

use tokio::time::Instant;

use crate::ids::now_ms;

/// Milliseconds in a day.
const DAY_MS: i64 = 86_400_000;

/// Where the job system reads the time.
#[derive(Clone, Copy, Debug, Default)]
pub enum Clock {
    /// The wall clock.
    #[default]
    System,
    /// Tokio's clock, reading `start_ms` at `start`.
    Tokio {
        /// Unix milliseconds at `start`.
        start_ms: i64,
        /// The tokio instant of `start_ms`.
        start: Instant,
    },
}

impl Clock {
    /// A clock that reads `start_ms` now and then advances with tokio's
    /// clock (paused or not).
    #[must_use]
    pub fn tokio(start_ms: i64) -> Self {
        Self::Tokio {
            start_ms,
            start: Instant::now(),
        }
    }

    /// The current time, unix ms.
    #[must_use]
    pub fn now_ms(&self) -> i64 {
        match *self {
            Self::System => now_ms(),
            Self::Tokio { start_ms, start } => {
                let elapsed = i64::try_from(start.elapsed().as_millis()).unwrap_or(i64::MAX);
                start_ms.saturating_add(elapsed)
            }
        }
    }

    /// The tokio instant when this clock reads `at_ms`; now if that passed.
    #[must_use]
    pub fn instant_at(&self, at_ms: i64) -> Instant {
        let wait = at_ms.saturating_sub(self.now_ms()).max(0);
        Instant::now() + Duration::from_millis(u64::try_from(wait).unwrap_or(0))
    }
}

/// The first time after `now_ms` that is `offset` past a UTC midnight (the
/// nightly schedule: 03:00 UTC).
#[must_use]
pub fn next_daily(now_ms: i64, offset: Duration) -> i64 {
    let offset = i64::try_from(offset.as_millis()).unwrap_or(0) % DAY_MS;
    let today = now_ms.div_euclid(DAY_MS) * DAY_MS + offset;
    if today > now_ms {
        today
    } else {
        today + DAY_MS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 2026-10-02T00:00:00Z.
    const MIDNIGHT: i64 = 1_790_899_200_000;
    const THREE: Duration = Duration::from_secs(3 * 3600);

    #[test]
    fn the_next_three_am_utc() {
        let three = MIDNIGHT + 3 * 3_600_000;
        assert_eq!(next_daily(MIDNIGHT, THREE), three);
        assert_eq!(next_daily(three - 1, THREE), three);
        assert_eq!(next_daily(three, THREE), three + DAY_MS, "strictly after");
        assert_eq!(next_daily(three + 1, THREE), three + DAY_MS);
        assert_eq!(next_daily(MIDNIGHT - 1, THREE), three);
    }

    #[tokio::test(start_paused = true)]
    async fn the_tokio_clock_follows_paused_time() {
        let clock = Clock::tokio(MIDNIGHT);
        assert_eq!(clock.now_ms(), MIDNIGHT);
        tokio::time::advance(Duration::from_millis(1_500)).await;
        assert_eq!(clock.now_ms(), MIDNIGHT + 1_500);
        let at = clock.instant_at(MIDNIGHT + 2_000);
        assert_eq!(at - Instant::now(), Duration::from_millis(500));
        assert_eq!(
            clock.instant_at(MIDNIGHT),
            Instant::now(),
            "the past is now"
        );
    }
}
