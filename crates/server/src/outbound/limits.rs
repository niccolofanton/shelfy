//! Host groups and their limits (§2.13, SPIKE-2, SPIKE-9, P2 lane rule 4).
//!
//! A host group is a platform's set of hosts that share one budget and one
//! breaker ([`super::breaker`]):
//!
//! | Group | Hosts | Rate | Concurrency | Jitter | Used by |
//! |---|---|---|---|---|---|
//! | `instagram` | `*.cdninstagram.com`, `*.fbcdn.net` | 2 req/s, `SHELFY_ARCHIVE_RATE_INSTAGRAM` | 4 | 120–400 ms before a fetch | CDN fetches (P2-04, P2-10, P4-16) |
//! | `x` | `pbs.twimg.com`, `video.twimg.com` | 2 req/s, `SHELFY_ARCHIVE_RATE_X` | 8 | — | CDN fetches |
//! | `pinterest` | `*.pinimg.com` | 2 req/s, `SHELFY_ARCHIVE_RATE_PINTEREST` | 4 | 120–400 ms before a fetch | CDN fetches |
//! | `instagram_web` | `www.instagram.com` | 1 per 3 s | 1 | 0–250 ms on each spacing | link hydration (P2-11): post page, GraphQL |
//! | `x_web` | `cdn.syndication.twimg.com`, `publish.x.com` | 1 req/s | 1 | 0–250 ms on each spacing | link hydration: `tweet-result`, oEmbed |
//! | `pinterest_web` | `www.pinterest.com`, `widgets.pinterest.com` | 1 req/s | 1 | 0–250 ms on each spacing | link hydration: PinResource, pidgets |
//!
//! - **Rate.** A pacer per group: the GCRA of P1-15's limiter
//!   ([`crate::rate_limit`]) with a burst of 1. Each request reserves the
//!   next slot, at least `1/rate` after the previous one (plus the group's
//!   [`GroupLimits::spread`]), and waits for it, so requests to a group never
//!   start closer than that, also after an idle spell. Call
//!   [`HostLimits::pace`] before every request, a fallback's included.
//! - **Concurrency.** A fetch holds its group's [`GroupSlot`] from its first
//!   request until its body is stored ([`HostLimits::acquire`]).
//! - **Jitter.** Two kinds, each as its spike measured it: the CDN groups
//!   wait a random 120–400 ms at the start of a fetch (DL-14), and the
//!   hydration groups add 0–250 ms to every spacing (SPIKE-9). Neither lets a
//!   group go faster than its rate.
//!
//! Time is tokio's: paused-time tests (`tokio::time::pause`, `advance`)
//! drive the pacers and the jitter deterministically.

use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;
use url::{Host, Url};

use super::resolve::HostSet;

/// A platform's hosts that share a budget and a breaker.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HostGroup {
    /// The Instagram CDN: `*.cdninstagram.com`, `*.fbcdn.net`.
    Instagram,
    /// The X CDN: `pbs.twimg.com`, `video.twimg.com`.
    X,
    /// The Pinterest CDN: `*.pinimg.com`.
    Pinterest,
    /// Instagram's web hosts for hydration: `www.instagram.com`.
    InstagramWeb,
    /// X's public embed hosts for hydration: `cdn.syndication.twimg.com`,
    /// `publish.x.com`.
    XWeb,
    /// Pinterest's public hosts for hydration: `www.pinterest.com`,
    /// `widgets.pinterest.com`.
    PinterestWeb,
}

static GROUP_HOSTS: LazyLock<[Arc<HostSet>; 6]> =
    LazyLock::new(|| HostGroup::ALL.map(|group| Arc::new(HostSet::new(group.patterns()))));

impl HostGroup {
    /// Every group, in [`HostGroup::index`] order.
    pub const ALL: [Self; 6] = [
        Self::Instagram,
        Self::X,
        Self::Pinterest,
        Self::InstagramWeb,
        Self::XWeb,
        Self::PinterestWeb,
    ];

    /// The CDN groups, the only ones [`super::Cdn::fetch`] fetches from.
    pub const CDN: [Self; 3] = [Self::Instagram, Self::X, Self::Pinterest];

    /// The `host_group` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::X => "x",
            Self::Pinterest => "pinterest",
            Self::InstagramWeb => "instagram_web",
            Self::XWeb => "x_web",
            Self::PinterestWeb => "pinterest_web",
        }
    }

    /// The position in [`HostGroup::ALL`] and in [`LimitsConfig`].
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Instagram => 0,
            Self::X => 1,
            Self::Pinterest => 2,
            Self::InstagramWeb => 3,
            Self::XWeb => 4,
            Self::PinterestWeb => 5,
        }
    }

    /// Whether this is a CDN group.
    #[must_use]
    pub const fn is_cdn(self) -> bool {
        matches!(self, Self::Instagram | Self::X | Self::Pinterest)
    }

    /// The group's hosts, as [`HostSet`] patterns.
    #[must_use]
    pub const fn patterns(self) -> &'static [&'static str] {
        match self {
            Self::Instagram => &["*.cdninstagram.com", "*.fbcdn.net"],
            Self::X => &["pbs.twimg.com", "video.twimg.com"],
            Self::Pinterest => &["*.pinimg.com"],
            Self::InstagramWeb => &["www.instagram.com"],
            Self::XWeb => &["cdn.syndication.twimg.com", "publish.x.com"],
            Self::PinterestWeb => &["www.pinterest.com", "widgets.pinterest.com"],
        }
    }

    /// The group's hosts, for a request's host allowlist.
    #[must_use]
    pub fn hosts(self) -> Arc<HostSet> {
        Arc::clone(&GROUP_HOSTS[self.index()])
    }

    /// The platform's `Referer` for the group's requests.
    #[must_use]
    pub const fn referer(self) -> &'static str {
        match self {
            Self::Instagram | Self::InstagramWeb => "https://www.instagram.com/",
            Self::X | Self::XWeb => "https://x.com/",
            Self::Pinterest | Self::PinterestWeb => "https://www.pinterest.com/",
        }
    }

    /// The group of a host name.
    #[must_use]
    pub fn of_host(host: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|group| GROUP_HOSTS[group.index()].matches(host))
    }

    /// The group of a URL's host (a name, not an address).
    #[must_use]
    pub fn of_url(url: &Url) -> Option<Self> {
        match url.host()? {
            Host::Domain(name) => Self::of_host(name),
            Host::Ipv4(_) | Host::Ipv6(_) => None,
        }
    }

    /// The defaults of the table above.
    #[must_use]
    pub const fn default_limits(self) -> GroupLimits {
        const CDN_JITTER: Option<(Duration, Duration)> =
            Some((Duration::from_millis(120), Duration::from_millis(400)));
        const WEB_SPREAD: Duration = Duration::from_millis(250);
        let (rate, concurrency, jitter, spread) = match self {
            Self::Instagram | Self::Pinterest => (2.0, 4, CDN_JITTER, Duration::ZERO),
            Self::X => (2.0, 8, None, Duration::ZERO),
            Self::InstagramWeb => (1.0 / 3.0, 1, None, WEB_SPREAD),
            Self::XWeb | Self::PinterestWeb => (1.0, 1, None, WEB_SPREAD),
        };
        GroupLimits {
            rate,
            concurrency,
            jitter,
            spread,
        }
    }
}

/// The limits of one host group.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GroupLimits {
    /// Requests per second.
    pub rate: f64,
    /// Fetches in flight.
    pub concurrency: usize,
    /// A random wait at the start of each fetch, between the two bounds.
    pub jitter: Option<(Duration, Duration)>,
    /// At most this much more, at random, between two requests.
    pub spread: Duration,
}

/// The limits of every host group, indexed by [`HostGroup::index`].
#[derive(Clone, Debug, PartialEq)]
pub struct LimitsConfig(pub [GroupLimits; 6]);

impl Default for LimitsConfig {
    fn default() -> Self {
        Self(HostGroup::ALL.map(HostGroup::default_limits))
    }
}

impl LimitsConfig {
    /// The limits of `group`.
    #[must_use]
    pub fn get(&self, group: HostGroup) -> &GroupLimits {
        &self.0[group.index()]
    }

    /// The limits of `group`, to change.
    pub fn get_mut(&mut self, group: HostGroup) -> &mut GroupLimits {
        &mut self.0[group.index()]
    }
}

/// A rate: requests start at least one period apart (GCRA, burst 1), plus
/// a random spread.
#[derive(Debug)]
pub struct Pacer {
    period: Duration,
    spread: Duration,
    /// When the next request may start; `None` before the first one.
    next: Mutex<Option<Instant>>,
}

impl Pacer {
    /// A pacer of `rate` requests per second.
    ///
    /// # Panics
    ///
    /// When `rate` is not a positive finite number.
    #[must_use]
    pub fn per_second(rate: f64) -> Self {
        Self::new(rate, Duration::ZERO)
    }

    /// A pacer of `rate` requests per second whose spacings are up to
    /// `spread` longer, at random.
    ///
    /// # Panics
    ///
    /// When `rate` is not a positive finite number.
    #[must_use]
    pub fn new(rate: f64, spread: Duration) -> Self {
        assert!(rate.is_finite() && rate > 0.0, "a rate is positive");
        Self {
            period: Duration::from_secs_f64(1.0 / rate),
            spread,
            next: Mutex::new(None),
        }
    }

    /// The shortest time between two requests.
    #[must_use]
    pub fn period(&self) -> Duration {
        self.period
    }

    /// Reserves the next slot at or after `now` and returns when it starts.
    pub fn reserve(&self, now: Instant) -> Instant {
        let extra = random_between(Duration::ZERO, self.spread);
        let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = next.map_or(now, |next| next.max(now));
        *next = Some(slot + self.period + extra);
        slot
    }

    /// Waits for the next slot.
    pub async fn wait(&self) {
        let slot = self.reserve(Instant::now());
        tokio::time::sleep_until(slot).await;
    }
}

/// The limits of one host group, live.
#[derive(Debug)]
struct GroupLimit {
    pacer: Pacer,
    slots: Arc<Semaphore>,
    jitter: Option<(Duration, Duration)>,
}

/// A fetch's concurrency slot in its host group, released on drop.
#[derive(Debug)]
pub struct GroupSlot {
    _permit: OwnedSemaphorePermit,
}

/// The limits of every host group.
#[derive(Debug)]
pub struct HostLimits {
    groups: [GroupLimit; 6],
}

impl HostLimits {
    /// The limits of `config`.
    ///
    /// # Panics
    ///
    /// When a rate is not positive or a concurrency is 0 (the configuration
    /// validates both).
    #[must_use]
    pub fn new(config: &LimitsConfig) -> Self {
        Self {
            groups: HostGroup::ALL.map(|group| {
                let limits = config.get(group);
                assert!(limits.concurrency > 0, "a group admits one fetch");
                GroupLimit {
                    pacer: Pacer::new(limits.rate, limits.spread),
                    slots: Arc::new(Semaphore::new(limits.concurrency)),
                    jitter: limits.jitter,
                }
            }),
        }
    }

    fn group(&self, group: HostGroup) -> &GroupLimit {
        &self.groups[group.index()]
    }

    /// Waits for a concurrency slot of `group`, then for its jitter. Hold
    /// the slot until the fetch is over.
    pub async fn acquire(&self, group: HostGroup) -> GroupSlot {
        let limit = self.group(group);
        let permit = Arc::clone(&limit.slots)
            .acquire_owned()
            .await
            .expect("the semaphore is never closed");
        if let Some((min, max)) = limit.jitter {
            tokio::time::sleep(random_between(min, max)).await;
        }
        GroupSlot { _permit: permit }
    }

    /// Waits for the next request slot of `group`'s rate. Call it before
    /// every request to the group.
    pub async fn pace(&self, group: HostGroup) {
        self.group(group).pacer.wait().await;
    }

    /// The shortest time between two requests to `group`.
    #[must_use]
    pub fn period(&self, group: HostGroup) -> Duration {
        self.group(group).pacer.period()
    }

    /// Slots of `group` free now.
    #[must_use]
    pub fn free_slots(&self, group: HostGroup) -> usize {
        self.group(group).slots.available_permits()
    }
}

/// A random duration in `[min, max]`, at millisecond resolution.
fn random_between(min: Duration, max: Duration) -> Duration {
    let span = u64::try_from(max.saturating_sub(min).as_millis()).unwrap_or(u64::MAX);
    if span == 0 {
        return min;
    }
    let roll = getrandom::u64().unwrap_or(0) % span.saturating_add(1);
    min + Duration::from_millis(roll)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    const MS: Duration = Duration::from_millis(1);

    fn without_jitter() -> LimitsConfig {
        let mut config = LimitsConfig::default();
        for limits in &mut config.0 {
            limits.jitter = None;
            limits.spread = Duration::ZERO;
        }
        config
    }

    #[test]
    fn hosts_belong_to_their_group() {
        for (host, group) in [
            (
                "scontent-mxp1-1.cdninstagram.com",
                Some(HostGroup::Instagram),
            ),
            (
                "instagram.fmxp1-1.fna.fbcdn.net",
                Some(HostGroup::Instagram),
            ),
            ("cdninstagram.com", Some(HostGroup::Instagram)),
            ("pbs.twimg.com", Some(HostGroup::X)),
            ("video.twimg.com", Some(HostGroup::X)),
            ("i.pinimg.com", Some(HostGroup::Pinterest)),
            ("v1.pinimg.com", Some(HostGroup::Pinterest)),
            ("www.instagram.com", Some(HostGroup::InstagramWeb)),
            ("cdn.syndication.twimg.com", Some(HostGroup::XWeb)),
            ("publish.x.com", Some(HostGroup::XWeb)),
            ("www.pinterest.com", Some(HostGroup::PinterestWeb)),
            ("widgets.pinterest.com", Some(HostGroup::PinterestWeb)),
            ("abs.twimg.com", None),
            ("instagram.com", None),
            ("x.com", None),
            ("api.x.com", None),
            ("pinimg.com.evil.test", None),
            ("evilpinimg.com", None),
        ] {
            assert_eq!(HostGroup::of_host(host), group, "{host}");
        }
        assert_eq!(
            HostGroup::of_url(&Url::parse("https://127.0.0.1/x.jpg").unwrap()),
            None
        );
        assert_eq!(
            HostGroup::ALL.map(HostGroup::label),
            [
                "instagram",
                "x",
                "pinterest",
                "instagram_web",
                "x_web",
                "pinterest_web"
            ]
        );
        for group in HostGroup::ALL {
            assert_eq!(HostGroup::ALL[group.index()], group);
            assert_eq!(group.is_cdn(), HostGroup::CDN.contains(&group));
        }
    }

    #[test]
    fn the_defaults_follow_the_spikes() {
        let config = LimitsConfig::default();
        let get = |group| *config.get(group);
        assert_eq!(get(HostGroup::Instagram).rate, 2.0);
        assert_eq!(get(HostGroup::X).concurrency, 8);
        assert_eq!(get(HostGroup::Pinterest).concurrency, 4);
        assert_eq!(get(HostGroup::Instagram).jitter, Some((120 * MS, 400 * MS)));
        assert_eq!(get(HostGroup::X).jitter, None);
        let instagram_web = HostLimits::new(&config).period(HostGroup::InstagramWeb);
        assert!(
            instagram_web >= 2999 * MS && instagram_web <= 3001 * MS,
            "{instagram_web:?}"
        );
        for group in [
            HostGroup::InstagramWeb,
            HostGroup::XWeb,
            HostGroup::PinterestWeb,
        ] {
            assert_eq!(get(group).concurrency, 1, "{group:?}");
            assert_eq!(get(group).spread, 250 * MS, "{group:?}");
        }
        assert_eq!(get(HostGroup::XWeb).rate, 1.0);
    }

    #[tokio::test(start_paused = true)]
    async fn the_pacer_spaces_requests_by_the_period() {
        let pacer = Pacer::per_second(2.0);
        assert_eq!(pacer.period(), 500 * MS);
        let t0 = Instant::now();
        let mut starts = Vec::new();
        for _ in 0..5 {
            pacer.wait().await;
            starts.push(Instant::now() - t0);
        }
        assert_eq!(starts, [0, 500, 1000, 1500, 2000].map(|ms| ms * MS));

        // Idle time does not bank a burst: after a pause, the pace is the same.
        tokio::time::advance(Duration::from_secs(10)).await;
        let t1 = Instant::now();
        let mut later = Vec::new();
        for _ in 0..3 {
            pacer.wait().await;
            later.push(Instant::now() - t1);
        }
        assert_eq!(later, [0, 500, 1000].map(|ms| ms * MS));
    }

    #[tokio::test(start_paused = true)]
    async fn a_spread_lengthens_each_spacing() {
        let pacer = Pacer::new(1.0 / 3.0, 250 * MS);
        let mut previous = None;
        for _ in 0..20 {
            pacer.wait().await;
            let now = Instant::now();
            if let Some(previous) = previous {
                let gap: Duration = now - previous;
                assert!(
                    (Duration::from_secs(3)..=Duration::from_millis(3251)).contains(&gap),
                    "{gap:?}"
                );
            }
            previous = Some(now);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn concurrent_waiters_get_successive_slots() {
        let pacer = Arc::new(Pacer::per_second(4.0));
        let t0 = Instant::now();
        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let pacer = Arc::clone(&pacer);
                tokio::spawn(async move {
                    pacer.wait().await;
                    Instant::now() - t0
                })
            })
            .collect();
        let mut starts = Vec::new();
        for task in tasks {
            starts.push(task.await.unwrap());
        }
        starts.sort();
        assert_eq!(starts, [0, 250, 500, 750, 1000, 1250].map(|ms| ms * MS));
    }

    #[test]
    fn fractional_rates() {
        assert_eq!(Pacer::per_second(0.5).period(), Duration::from_secs(2));
        assert_eq!(Pacer::per_second(8.0).period(), 125 * MS);
        let pacer = Pacer::per_second(20.0);
        let now = Instant::now();
        assert_eq!(pacer.reserve(now), now);
        assert_eq!(pacer.reserve(now), now + 50 * MS);
        let later = now + Duration::from_secs(1);
        assert_eq!(pacer.reserve(later), later);
    }

    #[tokio::test(start_paused = true)]
    async fn concurrency_is_capped_per_group() {
        let limits = Arc::new(HostLimits::new(&without_jitter()));
        for group in HostGroup::ALL {
            let cap = LimitsConfig::default().get(group).concurrency;
            let running = Arc::new(AtomicUsize::new(0));
            let peak = Arc::new(AtomicUsize::new(0));
            let tasks: Vec<_> = (0..20)
                .map(|_| {
                    let (limits, running, peak) =
                        (Arc::clone(&limits), Arc::clone(&running), Arc::clone(&peak));
                    tokio::spawn(async move {
                        let _slot = limits.acquire(group).await;
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        running.fetch_sub(1, Ordering::SeqCst);
                    })
                })
                .collect();
            for task in tasks {
                task.await.unwrap();
            }
            assert_eq!(peak.load(Ordering::SeqCst), cap, "{group:?}");
            assert_eq!(limits.free_slots(group), cap, "{group:?}: every slot back");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn cdn_fetches_of_instagram_and_pinterest_start_with_a_jitter() {
        let limits = HostLimits::new(&LimitsConfig::default());
        for group in [HostGroup::Instagram, HostGroup::Pinterest] {
            for _ in 0..20 {
                let t0 = Instant::now();
                let _slot = limits.acquire(group).await;
                let waited = Instant::now() - t0;
                assert!(
                    (120 * MS..=400 * MS).contains(&waited),
                    "{group:?}: {waited:?}"
                );
            }
        }
        let t0 = Instant::now();
        let _slot = limits.acquire(HostGroup::X).await;
        assert_eq!(Instant::now() - t0, Duration::ZERO, "no jitter for X");
    }

    #[tokio::test(start_paused = true)]
    async fn each_group_has_its_own_pace() {
        let mut config = without_jitter();
        config.get_mut(HostGroup::Instagram).rate = 2.0;
        config.get_mut(HostGroup::X).rate = 4.0;
        config.get_mut(HostGroup::Pinterest).rate = 1.0;
        let limits = HostLimits::new(&config);
        assert_eq!(limits.period(HostGroup::Instagram), 500 * MS);
        assert_eq!(limits.period(HostGroup::X), 250 * MS);
        assert_eq!(limits.period(HostGroup::Pinterest), 1000 * MS);
        let t0 = Instant::now();
        for group in HostGroup::ALL {
            limits.pace(group).await;
        }
        assert_eq!(
            Instant::now() - t0,
            Duration::ZERO,
            "groups do not wait on each other"
        );
        limits.pace(HostGroup::X).await;
        assert_eq!(Instant::now() - t0, 250 * MS);
        limits.pace(HostGroup::InstagramWeb).await;
        assert_eq!(
            Instant::now() - t0,
            Duration::from_secs(3),
            "1 request per 3 s"
        );
    }

    #[test]
    fn random_waits_stay_in_range() {
        for _ in 0..200 {
            let wait = random_between(120 * MS, 400 * MS);
            assert!((120 * MS..=400 * MS).contains(&wait), "{wait:?}");
        }
        assert_eq!(random_between(5 * MS, 5 * MS), 5 * MS);
        assert_eq!(
            random_between(Duration::ZERO, Duration::ZERO),
            Duration::ZERO
        );
    }
}
