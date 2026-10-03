//! Outbound HTTP (L11: P2-04 with P4-01). Everything the server fetches over
//! HTTP goes through this module, and [`client`] is the only place that
//! builds a `reqwest` client.
//!
//! | Module | Contents |
//! |---|---|
//! | [`client`] | the clients, requests and responses: redirects, caps, the in-flight limit, the metric |
//! | [`resolve`] | the address policy, origins, host allowlists, the resolvers |
//! | [`cdn`] | the archive's CDN fetcher and its outcomes |
//! | [`limits`] | the host groups (three CDNs, three hydration hosts) and their rate, concurrency and jitter |
//! | [`breaker`] | the circuit breaker of each host group |
//!
//! **Modes.** `SHELFY_EGRESS_PROXY` is the switch (L11):
//!
//! | Mode | When | Names and addresses |
//! |---|---|---|
//! | proxy | `SHELFY_EGRESS_PROXY` is the egress proxy (§3.2: `http://shelfy-egress:4750`, P4-02) | the proxy resolves names and refuses private destinations (D17); the client still refuses non-public literal addresses |
//! | direct | unset: the default until P4-02 ships the proxy (P2-G4) | the client's resolver keeps public addresses only ([`resolve`]) |
//!
//! In both modes the client applies the same URL checks and redirect policy
//! ([`client`]). P4-01's "outbound off without a proxy" is not built: L11
//! keeps P2-04's guarded direct mode until the proxy exists.
//!
//! **Purposes** ([`Purpose`]) label the metric and set each request's
//! defaults. Only [`Purpose::AiOperator`] reaches the operator allowlist
//! (`SHELFY_EGRESS_ALLOW_ORIGINS`, L15), and only [`Outbound::internal`]
//! reaches the capture service (`SHELFY_CAPTURE_URL`); both go direct, never
//! through the proxy.
//!
//! **Host groups** ([`HostGroup`]): the platforms' hosts share a pace, a
//! concurrency cap and a breaker per group, whoever calls them:
//! [`Outbound::limits`] and [`Outbound::breakers`]. The CDN fetcher
//! ([`Outbound::cdn`]) applies them itself; other callers (link hydration,
//! P2-11; direct video fetches, P4-16) take a slot, pace each request and
//! report to the breaker.
//!
//! **Recipes** (`outbound` is `state.outbound()`):
//! - *A CDN image* (P2-10): `outbound.cdn().fetch(FetchRequest { url, media,
//!   expires_at_ms, now_ms })`, then act on the [`FetchOutcome`].
//! - *A hydration call* (P2-11): `outbound.breakers().get(group).admit(now)`;
//!   `outbound.limits().acquire(group)` and hold the slot;
//!   `outbound.limits().pace(group)` before each request;
//!   `outbound.client(Purpose::Link).get(url).hosts(group.hosts()).send()`;
//!   read with a cap; then [`Breaker::record`] (or [`Breaker::trip`] at the
//!   first 429, challenge or login wall, SPIKE-9).
//! - *An AI provider* (P3): [`Purpose::Ai`] for the user's providers,
//!   [`Purpose::AiOperator`] for the node; `post(url)` with a body;
//!   [`EgressResponse::json_capped`] or [`EgressResponse::stream_capped`].
//! - *The capture service* (P4-14): [`Outbound::internal`], and
//!   [`EgressResponse::stream_capped`] for its NDJSON.
//! - *yt-dlp* (P4-06) fetches on its own: pass `--proxy`
//!   [`Outbound::proxy_url`] when there is one.
//!
//! **Settings** (also in `deploy/README.md`):
//!
//! | Variable | Default | Meaning |
//! |---|---|---|
//! | `SHELFY_EGRESS_PROXY` | none | the egress proxy; unset: direct mode |
//! | `SHELFY_EGRESS_ALLOW_ORIGINS` | none | exact origins `scheme://host:port` the operator's AI node is reached at, even at a private address (L15) |
//! | `SHELFY_CAPTURE_URL` | none | the capture service's origin, the only one `internal()` reaches (P4-14) |
//! | `SHELFY_ARCHIVE_RATE_INSTAGRAM`, `…_X`, `…_PINTEREST` | `2` | CDN requests per second per host group (SPIKE-2) |
//! | `SHELFY_DEV_EGRESS_HOSTS` | none | dev and tests: `host=127.0.0.1:port` pairs that bypass DNS and the address check; loopback public URL only, direct mode only |
//! | `SHELFY_DEV_EGRESS_CA` | none | dev and tests: a PEM file of extra trusted roots (a fixture's CA); loopback public URL only |

pub mod breaker;
pub mod cdn;
pub mod client;
pub mod limits;
pub mod resolve;

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use clap::Args;
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use url::{Host, Url};

pub use self::breaker::{Admission, Breaker, BreakerConfig, BreakerState, Breakers, Signal};
pub use self::cdn::{Cdn, FetchOutcome, FetchRequest, Rejection};
use self::client::Shared;
pub use self::client::{
    Egress, EgressError, EgressRequest, EgressResponse, MAX_REDIRECTS, Refusal,
};
pub use self::limits::{GroupLimits, GroupSlot, HostGroup, HostLimits, LimitsConfig};
pub use self::resolve::{HostSet, Lookup, Origin, OriginAllowlist, is_public, non_public_reason};
use crate::config::PublicUrl;
use crate::net;

/// Requests in flight over every client (§2.3).
pub const MAX_IN_FLIGHT: usize = 64;

/// The highest `SHELFY_ARCHIVE_RATE_<GROUP>` accepted, in requests per
/// second.
const MAX_RATE: f64 = 100.0;

/// What a request is for: the `purpose` label of
/// `shelfy_egress_requests_total` and the request's defaults.
///
/// | Purpose | Label | Who | https only | Timeout | Redirects |
/// |---|---|---|---|---|---|
/// | `Cdn` | `cdn` | the archive's CDN fetcher (P2-04, P2-10) | yes | 30 s | 5 |
/// | `Link` | `link` | short links and public link data (P2-11) | no | 15 s | 5 |
/// | `Ai` | `ai` | user-configured AI providers (P3) | no | 120 s | 0 |
/// | `AiOperator` | `ai_operator` | the operator's AI node (L15, P3): may reach `SHELFY_EGRESS_ALLOW_ORIGINS` | no | 120 s | 0 |
/// | `Video` | `video` | on-demand video files (P4-16) | yes | 10 min | 5 |
/// | `Feedback` | `feedback` | the feedback relay (P4-22) | yes | 20 s | 0 |
/// | `Capture` | `capture` | the capture service, through [`Outbound::internal`] only (P4-14) | no | 11 min | 0 |
///
/// A request can change its timeout, redirects and host allowlist.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Purpose {
    /// Archive fetches from the platform CDNs.
    Cdn,
    /// Short-link resolution and public link hydration.
    Link,
    /// AI providers the user configured: the strict rules always apply.
    Ai,
    /// The operator's own AI node, defined in the environment.
    AiOperator,
    /// On-demand video files.
    Video,
    /// The feedback relay.
    Feedback,
    /// The capture service.
    Capture,
}

impl Purpose {
    /// Every purpose.
    pub const ALL: [Self; 7] = [
        Self::Cdn,
        Self::Link,
        Self::Ai,
        Self::AiOperator,
        Self::Video,
        Self::Feedback,
        Self::Capture,
    ];

    /// The `purpose` label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Cdn => "cdn",
            Self::Link => "link",
            Self::Ai => "ai",
            Self::AiOperator => "ai_operator",
            Self::Video => "video",
            Self::Feedback => "feedback",
            Self::Capture => "capture",
        }
    }

    /// The default total time of a request.
    #[must_use]
    pub const fn timeout(self) -> Duration {
        Duration::from_secs(match self {
            Self::Cdn => 30,
            Self::Link => 15,
            Self::Ai | Self::AiOperator => 120,
            Self::Video => 600,
            Self::Feedback => 20,
            Self::Capture => 660,
        })
    }

    /// The default number of redirects followed.
    #[must_use]
    pub const fn max_redirects(self) -> u8 {
        match self {
            Self::Cdn | Self::Link | Self::Video => client::MAX_REDIRECTS,
            Self::Ai | Self::AiOperator | Self::Feedback | Self::Capture => 0,
        }
    }

    /// Whether every URL must be https.
    #[must_use]
    pub const fn https_only(self) -> bool {
        matches!(self, Self::Cdn | Self::Video | Self::Feedback)
    }

    /// Whether requests may reach the operator allowlist (L15).
    #[must_use]
    pub const fn reaches_operator_origins(self) -> bool {
        matches!(self, Self::AiOperator)
    }
}

/// The settings of outbound HTTP, flattened into `serve`.
#[derive(Clone, Debug, Args)]
pub struct OutboundArgs {
    /// The egress proxy, for example `http://shelfy-egress:4750` (Smokescreen,
    /// plan §3.2). Every outbound request goes through it, and it refuses
    /// private destinations. Unset: the server connects directly, and its
    /// own resolver refuses non-public addresses.
    #[arg(long = "egress-proxy", env = "SHELFY_EGRESS_PROXY", value_name = "URL")]
    pub egress_proxy: Option<String>,

    /// Exact origins (`scheme://host:port`, comma-separated) that the
    /// operator's own AI node is reached at, even at a private address (a
    /// Tailscale node). Only the operator sets them; user-entered URLs never
    /// use them.
    #[arg(
        long = "egress-allow-origins",
        env = "SHELFY_EGRESS_ALLOW_ORIGINS",
        value_name = "ORIGINS",
        default_value = "",
        hide_default_value = true
    )]
    pub egress_allow_origins: String,

    /// The capture service, for example `http://shelfy-capture:8080`: the
    /// only origin the internal client reaches, without the proxy.
    #[arg(long = "capture-url", env = "SHELFY_CAPTURE_URL", value_name = "URL")]
    pub capture_url: Option<String>,

    /// Requests per second to the Instagram CDN (archive fetches).
    #[arg(
        long = "archive-rate-instagram",
        env = "SHELFY_ARCHIVE_RATE_INSTAGRAM",
        value_name = "PER_SECOND",
        default_value = "2",
        value_parser = parse_rate
    )]
    pub archive_rate_instagram: f64,

    /// Requests per second to the X CDN (archive fetches).
    #[arg(
        long = "archive-rate-x",
        env = "SHELFY_ARCHIVE_RATE_X",
        value_name = "PER_SECOND",
        default_value = "2",
        value_parser = parse_rate
    )]
    pub archive_rate_x: f64,

    /// Requests per second to the Pinterest CDN (archive fetches).
    #[arg(
        long = "archive-rate-pinterest",
        env = "SHELFY_ARCHIVE_RATE_PINTEREST",
        value_name = "PER_SECOND",
        default_value = "2",
        value_parser = parse_rate
    )]
    pub archive_rate_pinterest: f64,

    /// Local runs and tests only: `host=127.0.0.1:port` pairs, comma-separated.
    /// Requests to these names go to that loopback port, without DNS or the
    /// address check (a fixture CDN). Needs a loopback public URL and no
    /// egress proxy.
    #[arg(
        long = "dev-egress-hosts",
        env = "SHELFY_DEV_EGRESS_HOSTS",
        value_name = "HOST=ADDR,…",
        default_value = "",
        hide_default_value = true
    )]
    pub dev_egress_hosts: String,

    /// Local runs and tests only: a PEM file of extra root certificates to
    /// trust (a fixture CDN's CA). Needs a loopback public URL.
    #[arg(
        long = "dev-egress-ca",
        env = "SHELFY_DEV_EGRESS_CA",
        value_name = "FILE"
    )]
    pub dev_egress_ca: Option<String>,
}

/// `SHELFY_ARCHIVE_RATE_<GROUP>`: a positive number of requests per second,
/// at most 100.
fn parse_rate(text: &str) -> Result<f64, String> {
    let rate: f64 = text
        .trim()
        .parse()
        .map_err(|_| format!("{text:?} is not a number"))?;
    if !(rate.is_finite() && rate > 0.0 && rate <= MAX_RATE) {
        return Err(format!("{text:?} must be above 0 and at most {MAX_RATE}"));
    }
    Ok(rate)
}

/// The validated settings of outbound HTTP.
#[derive(Clone, Debug)]
pub struct OutboundConfig {
    /// `SHELFY_EGRESS_PROXY`; `None` is direct mode.
    pub proxy: Option<Url>,
    /// `SHELFY_EGRESS_ALLOW_ORIGINS` (L15).
    pub allow_origins: OriginAllowlist,
    /// `SHELFY_CAPTURE_URL`: the origin of [`Outbound::internal`].
    pub capture: Option<Origin>,
    /// The rate, concurrency and jitter of each host group; the CDN rates
    /// come from `SHELFY_ARCHIVE_RATE_<GROUP>`.
    pub limits: LimitsConfig,
    /// The breaker thresholds, the same for every host group.
    pub breaker: BreakerConfig,
    /// The time of one CDN request, its body included: 30 s (§2.13).
    pub cdn_timeout: Duration,
    /// Requests in flight over every client: [`MAX_IN_FLIGHT`].
    pub max_in_flight: usize,
    /// `SHELFY_DEV_EGRESS_HOSTS`: names sent to loopback ports.
    pub dev_hosts: BTreeMap<String, SocketAddr>,
    /// `SHELFY_DEV_EGRESS_CA`: extra trusted roots.
    pub extra_roots: Vec<CertificateDer<'static>>,
    /// Name lookups: the system resolver. Tests plant answers; no setting
    /// of the environment does.
    pub lookup: Lookup,
}

impl Default for OutboundConfig {
    fn default() -> Self {
        Self {
            proxy: None,
            allow_origins: OriginAllowlist::default(),
            capture: None,
            limits: LimitsConfig::default(),
            breaker: BreakerConfig::default(),
            cdn_timeout: cdn::FETCH_TIMEOUT,
            max_in_flight: MAX_IN_FLIGHT,
            dev_hosts: BTreeMap::new(),
            extra_roots: Vec::new(),
            lookup: Lookup::system(),
        }
    }
}

impl OutboundConfig {
    /// Validates the arguments. The dev settings need a loopback
    /// `public_url`, like `SHELFY_DEV_MAILBOX`.
    ///
    /// # Errors
    ///
    /// A message naming the variable and what is wrong.
    pub fn from_args(args: OutboundArgs, public_url: &PublicUrl) -> Result<Self, String> {
        let proxy = non_empty(args.egress_proxy)
            .map(|text| parse_proxy(&text))
            .transpose()
            .map_err(|e| format!("SHELFY_EGRESS_PROXY: {e}"))?;
        let allow_origins = OriginAllowlist::parse(&args.egress_allow_origins)
            .map_err(|e| format!("SHELFY_EGRESS_ALLOW_ORIGINS: {e}"))?;
        let capture = non_empty(args.capture_url)
            .map(|text| Origin::parse(&text))
            .transpose()
            .map_err(|e| format!("SHELFY_CAPTURE_URL: {e}"))?;
        let dev_hosts = parse_dev_hosts(&args.dev_egress_hosts)
            .map_err(|e| format!("SHELFY_DEV_EGRESS_HOSTS: {e}"))?;
        let extra_roots = match non_empty(args.dev_egress_ca) {
            Some(path) => {
                read_roots(Path::new(&path)).map_err(|e| format!("SHELFY_DEV_EGRESS_CA: {e}"))?
            }
            None => Vec::new(),
        };
        let local = public_url_is_loopback(public_url);
        if !dev_hosts.is_empty() && !local {
            return Err(
                "SHELFY_DEV_EGRESS_HOSTS sends outbound requests to this machine: it needs \
                 a loopback SHELFY_PUBLIC_URL (localhost)"
                    .to_owned(),
            );
        }
        if !extra_roots.is_empty() && !local {
            return Err(
                "SHELFY_DEV_EGRESS_CA trusts a test certificate authority: it needs a \
                 loopback SHELFY_PUBLIC_URL (localhost)"
                    .to_owned(),
            );
        }
        if !dev_hosts.is_empty() && proxy.is_some() {
            return Err(
                "SHELFY_DEV_EGRESS_HOSTS works without SHELFY_EGRESS_PROXY only: the \
                 proxy resolves names itself"
                    .to_owned(),
            );
        }
        let mut limits = LimitsConfig::default();
        for (group, rate) in [
            (HostGroup::Instagram, args.archive_rate_instagram),
            (HostGroup::X, args.archive_rate_x),
            (HostGroup::Pinterest, args.archive_rate_pinterest),
        ] {
            limits.get_mut(group).rate = rate;
        }
        Ok(Self {
            proxy,
            allow_origins,
            capture,
            limits,
            dev_hosts,
            extra_roots,
            ..Self::default()
        })
    }
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// The proxy: `http(s)://host[:port]`, no credentials, no path.
fn parse_proxy(text: &str) -> Result<Url, String> {
    let url = Url::parse(text).map_err(|e| format!("{text:?} is not an absolute URL ({e})"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(format!("{text:?}: the scheme must be http or https"));
    }
    if url.host().is_none() {
        return Err(format!("{text:?}: a host is required"));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(format!("{text:?}: credentials are not allowed"));
    }
    if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
        return Err(format!("{text:?}: give the proxy's origin only"));
    }
    Ok(url)
}

/// `host=addr` pairs separated by commas or spaces; every address loopback.
fn parse_dev_hosts(text: &str) -> Result<BTreeMap<String, SocketAddr>, String> {
    let mut hosts = BTreeMap::new();
    for entry in text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|entry| !entry.is_empty())
    {
        let (name, addr) = entry
            .split_once('=')
            .ok_or_else(|| format!("{entry:?} is not host=address:port"))?;
        let name = name.trim().trim_end_matches('.').to_ascii_lowercase();
        let named = matches!(Host::parse(&name), Ok(Host::Domain(_))) && !name.is_empty();
        if !named {
            return Err(format!("{entry:?}: {name:?} is not a host name"));
        }
        let addr: SocketAddr = addr
            .trim()
            .parse()
            .map_err(|_| format!("{entry:?}: {addr:?} is not an address:port"))?;
        if !addr.ip().to_canonical().is_loopback() {
            return Err(format!("{entry:?}: only loopback addresses are allowed"));
        }
        hosts.insert(name, addr);
    }
    Ok(hosts)
}

/// The certificates of a PEM file; at least one.
fn read_roots(path: &Path) -> Result<Vec<CertificateDer<'static>>, String> {
    let certs = CertificateDer::pem_file_iter(path)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if certs.is_empty() {
        return Err(format!("{} holds no certificate", path.display()));
    }
    Ok(certs)
}

fn public_url_is_loopback(public_url: &PublicUrl) -> bool {
    Url::parse(public_url.as_str())
        .ok()
        .and_then(|url| url.host_str().map(net::is_loopback_host))
        .unwrap_or(false)
}

/// How public requests leave the server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Through `SHELFY_EGRESS_PROXY`.
    Proxy,
    /// Directly, with the resolver's address check.
    Direct,
}

impl Mode {
    /// `proxy` or `direct`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Proxy => "proxy",
            Self::Direct => "direct",
        }
    }
}

/// Outbound HTTP for the whole server. Cheap to clone.
#[derive(Clone)]
pub struct Outbound {
    shared: Arc<Shared>,
    limits: Arc<HostLimits>,
    breakers: Arc<Breakers>,
    cdn: Arc<Cdn>,
    proxy: Option<Url>,
    capture: bool,
}

impl Outbound {
    /// Builds the clients of `config`.
    ///
    /// # Errors
    ///
    /// The TLS configuration or a client cannot be built (a bad extra root,
    /// a bad proxy URL), or a host group's limits are unusable.
    pub fn new(config: &OutboundConfig) -> anyhow::Result<Self> {
        for group in HostGroup::ALL {
            let limits = config.limits.get(group);
            anyhow::ensure!(
                limits.rate.is_finite() && limits.rate > 0.0 && limits.concurrency > 0,
                "the limits of the {} host group are unusable",
                group.label()
            );
        }
        let shared = Arc::new(Shared::new(config)?);
        let limits = Arc::new(HostLimits::new(&config.limits));
        let breakers = Arc::new(Breakers::new(config.breaker));
        let cdn = Cdn::new(
            Egress::new(Arc::clone(&shared), Purpose::Cdn),
            Arc::clone(&limits),
            Arc::clone(&breakers),
            config.cdn_timeout,
        );
        Ok(Self {
            shared,
            limits,
            breakers,
            cdn: Arc::new(cdn),
            proxy: config.proxy.clone(),
            capture: config.capture.is_some(),
        })
    }

    /// The pace and concurrency of every host group, shared by every caller.
    #[must_use]
    pub fn limits(&self) -> &HostLimits {
        &self.limits
    }

    /// The breaker of every host group, shared by every caller.
    #[must_use]
    pub fn breakers(&self) -> &Breakers {
        &self.breakers
    }

    /// A handle for requests of `purpose`. [`Purpose::Capture`] requests
    /// reach the capture service only; prefer [`Outbound::internal`].
    #[must_use]
    pub fn client(&self, purpose: Purpose) -> Egress {
        Egress::new(Arc::clone(&self.shared), purpose)
    }

    /// The capture service's client (P4-14): its origin only, without the
    /// proxy. `None` without `SHELFY_CAPTURE_URL`.
    #[must_use]
    pub fn internal(&self) -> Option<Egress> {
        self.capture.then(|| self.client(Purpose::Capture))
    }

    /// The CDN fetcher of the archive.
    #[must_use]
    pub fn cdn(&self) -> &Cdn {
        &self.cdn
    }

    /// Whether public requests go through the proxy.
    #[must_use]
    pub fn mode(&self) -> Mode {
        if self.proxy.is_some() {
            Mode::Proxy
        } else {
            Mode::Direct
        }
    }

    /// The egress proxy, for tools that fetch on their own (yt-dlp's
    /// `--proxy`, P4-06). `None` in direct mode.
    #[must_use]
    pub fn proxy_url(&self) -> Option<&Url> {
        self.proxy.as_ref()
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Parser)]
    struct Cli {
        #[command(flatten)]
        outbound: OutboundArgs,
    }

    fn args(argv: &[&str]) -> OutboundArgs {
        let mut full = vec!["shelfy"];
        full.extend_from_slice(argv);
        Cli::try_parse_from(full).expect("valid arguments").outbound
    }

    fn local() -> PublicUrl {
        PublicUrl::parse("http://localhost:18284").unwrap()
    }

    fn public() -> PublicUrl {
        PublicUrl::parse("https://refs.niccolofanton.dev").unwrap()
    }

    #[test]
    fn the_defaults_are_direct_and_strict() {
        let config = OutboundConfig::from_args(args(&[]), &public()).unwrap();
        assert!(config.proxy.is_none());
        assert!(config.allow_origins.is_empty());
        assert!(config.capture.is_none());
        assert!(config.dev_hosts.is_empty());
        assert!(config.extra_roots.is_empty());
        assert_eq!(config.max_in_flight, 64);
        assert_eq!(config.cdn_timeout, Duration::from_secs(30));
        assert_eq!(config.limits, LimitsConfig::default());
        let concurrency = HostGroup::CDN.map(|group| config.limits.get(group).concurrency);
        assert_eq!(concurrency, [4, 8, 4]);
    }

    #[test]
    fn settings_parse() {
        let config = OutboundConfig::from_args(
            args(&[
                "--egress-proxy",
                "http://shelfy-egress:4750",
                "--egress-allow-origins",
                "http://100.101.102.103:8080,http://ornith.tailnet.example:8081",
                "--capture-url",
                "http://shelfy-capture:8080",
                "--archive-rate-instagram",
                "0.5",
                "--archive-rate-x",
                "8",
            ]),
            &public(),
        )
        .unwrap();
        assert_eq!(
            config.proxy.as_ref().map(Url::as_str),
            Some("http://shelfy-egress:4750/")
        );
        assert_eq!(
            config.allow_origins.to_string(),
            "http://ornith.tailnet.example:8081,http://100.101.102.103:8080",
            "sorted, duplicates dropped"
        );
        assert_eq!(
            config.capture.as_ref().map(ToString::to_string).as_deref(),
            Some("http://shelfy-capture:8080")
        );
        let rates = HostGroup::ALL.map(|group| config.limits.get(group).rate);
        assert_eq!(
            rates,
            [0.5, 8.0, 2.0, 1.0 / 3.0, 1.0, 1.0],
            "hydration rates are fixed"
        );
        let outbound = Outbound::new(&config).unwrap();
        assert_eq!(
            outbound.limits().period(HostGroup::Instagram),
            Duration::from_secs(2)
        );
        assert_eq!(
            outbound
                .breakers()
                .get(HostGroup::XWeb)
                .state(tokio::time::Instant::now()),
            BreakerState::Closed
        );
        assert_eq!(outbound.mode(), Mode::Proxy);
        assert!(outbound.internal().is_some());
        assert_eq!(
            outbound.proxy_url().map(Url::as_str),
            Some("http://shelfy-egress:4750/")
        );
    }

    #[test]
    fn empty_values_count_as_unset() {
        let config = OutboundConfig::from_args(
            args(&[
                "--egress-proxy",
                " ",
                "--capture-url",
                "",
                "--dev-egress-ca",
                "",
            ]),
            &public(),
        )
        .unwrap();
        assert!(config.proxy.is_none() && config.capture.is_none());
        let outbound = Outbound::new(&config).unwrap();
        assert_eq!(outbound.mode(), Mode::Direct);
        assert!(outbound.internal().is_none());
        assert!(outbound.proxy_url().is_none());
    }

    #[test]
    fn bad_values_are_refused() {
        for argv in [
            &["--egress-proxy", "shelfy-egress:4750"][..],
            &["--egress-proxy", "socks5://shelfy-egress:1080"],
            &["--egress-proxy", "http://user:pw@shelfy-egress:4750"],
            &["--egress-proxy", "http://shelfy-egress:4750/path"],
            &["--egress-allow-origins", "http://100.101.102.103:8080/v1"],
            &["--egress-allow-origins", "100.101.102.103:8080"],
            &["--capture-url", "http://shelfy-capture:8080/v1/captures"],
        ] {
            let parsed = OutboundConfig::from_args(args(argv), &public());
            assert!(parsed.is_err(), "{argv:?}");
        }
        for rate in ["0", "-1", "NaN", "inf", "1000", "two"] {
            let argv = ["shelfy", "--archive-rate-x", rate];
            assert!(Cli::try_parse_from(argv).is_err(), "{rate}");
        }
    }

    #[test]
    fn dev_hosts_need_a_loopback_public_url_and_no_proxy() {
        let dev = [
            "--dev-egress-hosts",
            "scontent.cdninstagram.com=127.0.0.1:28480, pbs.twimg.com=[::1]:28481",
        ];
        let config = OutboundConfig::from_args(args(&dev), &local()).unwrap();
        assert_eq!(
            config.dev_hosts,
            BTreeMap::from([
                (
                    "pbs.twimg.com".to_owned(),
                    "[::1]:28481".parse::<SocketAddr>().unwrap()
                ),
                (
                    "scontent.cdninstagram.com".to_owned(),
                    "127.0.0.1:28480".parse().unwrap()
                ),
            ])
        );
        let err = OutboundConfig::from_args(args(&dev), &public()).unwrap_err();
        assert!(err.contains("loopback SHELFY_PUBLIC_URL"), "{err}");
        let mut proxied = dev.to_vec();
        proxied.extend(["--egress-proxy", "http://127.0.0.1:4750"]);
        let err = OutboundConfig::from_args(args(&proxied), &local()).unwrap_err();
        assert!(err.contains("without SHELFY_EGRESS_PROXY"), "{err}");
        for bad in [
            "cdn.example=10.0.0.1:443",
            "cdn.example=93.184.215.14:443",
            "cdn.example",
            "127.0.0.1=127.0.0.1:443",
            "cdn.example=127.0.0.1",
        ] {
            let argv = ["--dev-egress-hosts", bad];
            assert!(
                OutboundConfig::from_args(args(&argv), &local()).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn the_dev_ca_needs_a_loopback_public_url() {
        let dir = tempfile::tempdir().unwrap();
        let pem = dir.path().join("ca.pem");
        std::fs::write(&pem, test_ca_pem()).unwrap();
        let path = pem.to_str().unwrap();
        let config = OutboundConfig::from_args(args(&["--dev-egress-ca", path]), &local()).unwrap();
        assert_eq!(config.extra_roots.len(), 1);
        Outbound::new(&config).expect("the root is usable");
        let err =
            OutboundConfig::from_args(args(&["--dev-egress-ca", path]), &public()).unwrap_err();
        assert!(err.contains("loopback SHELFY_PUBLIC_URL"), "{err}");

        let empty = dir.path().join("empty.pem");
        std::fs::write(&empty, "no certificate here").unwrap();
        let empty = empty.to_str().unwrap();
        assert!(OutboundConfig::from_args(args(&["--dev-egress-ca", empty]), &local()).is_err());
        let missing = dir.path().join("missing.pem");
        let missing = missing.to_str().unwrap();
        assert!(OutboundConfig::from_args(args(&["--dev-egress-ca", missing]), &local()).is_err());
    }

    /// A self-signed CA certificate, made with the vendored OpenSSL.
    fn test_ca_pem() -> Vec<u8> {
        use openssl::asn1::Asn1Time;
        use openssl::bn::BigNum;
        use openssl::ec::{EcGroup, EcKey};
        use openssl::hash::MessageDigest;
        use openssl::nid::Nid;
        use openssl::pkey::PKey;
        use openssl::x509::extension::BasicConstraints;
        use openssl::x509::{X509Builder, X509NameBuilder};

        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = PKey::from_ec_key(EcKey::generate(&group).unwrap()).unwrap();
        let mut name = X509NameBuilder::new().unwrap();
        name.append_entry_by_text("CN", "Shelfy test CA").unwrap();
        let name = name.build();
        let mut cert = X509Builder::new().unwrap();
        cert.set_version(2).unwrap();
        cert.set_serial_number(&BigNum::from_u32(1).unwrap().to_asn1_integer().unwrap())
            .unwrap();
        cert.set_subject_name(&name).unwrap();
        cert.set_issuer_name(&name).unwrap();
        cert.set_pubkey(&key).unwrap();
        cert.set_not_before(&Asn1Time::days_from_now(0).unwrap())
            .unwrap();
        cert.set_not_after(&Asn1Time::days_from_now(1).unwrap())
            .unwrap();
        cert.append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        cert.sign(&key, MessageDigest::sha256()).unwrap();
        cert.build().to_pem().unwrap()
    }

    #[test]
    fn purposes_have_stable_labels_and_defaults() {
        assert_eq!(
            Purpose::ALL.map(Purpose::label),
            [
                "cdn",
                "link",
                "ai",
                "ai_operator",
                "video",
                "feedback",
                "capture"
            ]
        );
        assert_eq!(Purpose::Cdn.timeout(), Duration::from_secs(30));
        assert!(Purpose::Cdn.https_only());
        assert_eq!(Purpose::Cdn.max_redirects(), 5);
        assert_eq!(Purpose::Ai.max_redirects(), 0);
        let operator: Vec<Purpose> = Purpose::ALL
            .into_iter()
            .filter(|p| p.reaches_operator_origins())
            .collect();
        assert_eq!(
            operator,
            [Purpose::AiOperator],
            "user AI providers stay strict"
        );
        assert_eq!(Mode::Direct.as_str(), "direct");
    }
}
