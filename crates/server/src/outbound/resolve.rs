//! Where outbound requests may go: the address policy, origins, per-use host
//! allowlists and the DNS resolvers of the outbound clients (L11, P2-G4).
//!
//! **Public addresses only.** Without the egress proxy, the public client
//! resolves names itself, through [`PublicResolver`]: it keeps only the
//! addresses [`is_public`] accepts and connects to exactly those, so a name
//! cannot be re-resolved to another address between the check and the
//! connection (DNS rebinding). A name with no public address is refused with
//! [`BlockedAddress`]. Literal addresses never reach a resolver (hyper connects
//! to them directly): the client checks them first ([`super::client`]).
//!
//! [`non_public_reason`] refuses, for IPv4: `0.0.0.0/8`, RFC 1918, loopback,
//! link-local (with the `169.254.169.254` metadata address), CGNAT
//! (`100.64.0.0/10`), the IETF, documentation, benchmarking and relay blocks,
//! multicast and `240.0.0.0/4` (with the broadcast address). For IPv6 it
//! accepts global unicast (`2000::/3`) only, and refuses inside it the
//! documentation blocks, the IETF block (`2001::/23`, Teredo included) and
//! 6to4 (`2002::/16`); everything outside `2000::/3` is refused: unspecified,
//! loopback, IPv4-mapped (`::ffff:0:0/96`) and IPv4-compatible forms, NAT64
//! (`64:ff9b::/96`, `64:ff9b:1::/48`), discard, ULA (`fc00::/7`), link-local,
//! site-local and multicast.
//!
//! **Exceptions**, never user-editable:
//! - the operator allowlist (`SHELFY_EGRESS_ALLOW_ORIGINS`, L15): exact
//!   [`Origin`]s that the operator's own integrations may reach at any
//!   address, through [`PinnedResolver`];
//! - the capture service's origin (`SHELFY_CAPTURE_URL`), for `internal()`;
//! - the dev hosts (`SHELFY_DEV_EGRESS_HOSTS`): names mapped to loopback
//!   ports, for local runs and tests only.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;

use futures_util::future::BoxFuture;
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use url::{Host, Url};

/// The error type of a resolver.
type BoxError = Box<dyn std::error::Error + Send + Sync>;

/// IPv4 blocks outside the public internet, as (network, prefix length,
/// reason).
const V4_BLOCKS: &[([u8; 4], u8, &str)] = &[
    ([0, 0, 0, 0], 8, "this_network"),
    ([10, 0, 0, 0], 8, "private"),
    ([100, 64, 0, 0], 10, "cgnat"),
    ([127, 0, 0, 0], 8, "loopback"),
    ([169, 254, 0, 0], 16, "link_local"),
    ([172, 16, 0, 0], 12, "private"),
    ([192, 0, 0, 0], 24, "protocol"),
    ([192, 0, 2, 0], 24, "documentation"),
    ([192, 88, 99, 0], 24, "relay"),
    ([192, 168, 0, 0], 16, "private"),
    ([198, 18, 0, 0], 15, "benchmarking"),
    ([198, 51, 100, 0], 24, "documentation"),
    ([203, 0, 113, 0], 24, "documentation"),
    ([224, 0, 0, 0], 4, "multicast"),
    ([240, 0, 0, 0], 4, "reserved"),
];

/// IPv6 blocks outside the public internet, besides `::/96` and
/// `::ffff:0:0/96` (checked first) and everything outside `2000::/3`.
const V6_BLOCKS: &[(u128, u8, &str)] = &[
    (0x0064_ff9b_0000_0000_0000_0000_0000_0000, 96, "nat64"),
    (0x0064_ff9b_0001_0000_0000_0000_0000_0000, 48, "nat64"),
    (0x0100_0000_0000_0000_0000_0000_0000_0000, 64, "discard"),
    (
        0x2001_0db8_0000_0000_0000_0000_0000_0000,
        32,
        "documentation",
    ),
    (0x2001_0000_0000_0000_0000_0000_0000_0000, 23, "protocol"),
    (0x2002_0000_0000_0000_0000_0000_0000_0000, 16, "6to4"),
    (
        0x3fff_0000_0000_0000_0000_0000_0000_0000,
        20,
        "documentation",
    ),
    (0x5f00_0000_0000_0000_0000_0000_0000_0000, 16, "srv6"),
    (0xfc00_0000_0000_0000_0000_0000_0000_0000, 7, "unique_local"),
    (0xfe80_0000_0000_0000_0000_0000_0000_0000, 10, "link_local"),
    (0xfec0_0000_0000_0000_0000_0000_0000_0000, 10, "site_local"),
    (0xff00_0000_0000_0000_0000_0000_0000_0000, 8, "multicast"),
];

/// Why `ip` is not a public unicast address, as a stable code (`loopback`,
/// `private`, `cgnat`, `ipv4_mapped`…), or `None` when it is public.
#[must_use]
pub fn non_public_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => v4_reason(v4),
        IpAddr::V6(v6) => v6_reason(v6),
    }
}

/// Whether `ip` is a public unicast address that outbound requests may reach.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    non_public_reason(ip).is_none()
}

fn v4_reason(ip: Ipv4Addr) -> Option<&'static str> {
    let bits = u32::from(ip);
    V4_BLOCKS
        .iter()
        .find(|(net, len, _)| {
            let mask = u32::MAX.checked_shl(32 - u32::from(*len)).unwrap_or(0);
            bits & mask == u32::from_be_bytes(*net)
        })
        .map(|&(_, _, reason)| reason)
}

fn v6_reason(ip: Ipv6Addr) -> Option<&'static str> {
    let bits = u128::from(ip);
    match bits >> 32 {
        0xffff => return Some("ipv4_mapped"),
        0 => {
            return Some(match bits {
                0 => "unspecified",
                1 => "loopback",
                _ => "ipv4_compatible",
            });
        }
        _ => {}
    }
    let block = V6_BLOCKS.iter().find(|(net, len, _)| {
        let mask = u128::MAX.checked_shl(128 - u32::from(*len)).unwrap_or(0);
        bits & mask == *net
    });
    if let Some(&(_, _, reason)) = block {
        return Some(reason);
    }
    // Only global unicast is public.
    (bits >> 125 != 0b001).then_some("reserved")
}

/// Whether `host` (a domain of a parsed URL) is a name that means this
/// machine (RFC 6761): `localhost` or a name below it.
#[must_use]
pub fn is_localhost_name(host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host.ends_with(".localhost")
}

/// A web origin: scheme (http or https), host and port. The unit of the
/// operator allowlist and of the capture service.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Origin {
    https: bool,
    host: Host<String>,
    port: u16,
}

impl Origin {
    /// Parses an exact origin, `http(s)://host[:port]`, with an optional
    /// trailing `/`. The port defaults to the scheme's.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong: not a URL, another scheme, a path,
    /// query, fragment or credentials.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let url = Url::parse(text).map_err(|e| format!("{text:?} is not an absolute URL ({e})"))?;
        if !url.username().is_empty() || url.password().is_some() {
            return Err(format!("{text:?}: credentials are not allowed"));
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err(format!(
                "{text:?}: give the origin only (scheme://host:port), without a path, query or fragment"
            ));
        }
        Self::of(&url).ok_or_else(|| format!("{text:?}: the scheme must be http or https"))
    }

    /// The origin of `url`; `None` unless its scheme is http or https.
    #[must_use]
    pub fn of(url: &Url) -> Option<Self> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return None,
        };
        Some(Self {
            https,
            host: url.host()?.to_owned(),
            port: url.port_or_known_default()?,
        })
    }

    /// The host when it is a name (not an address literal).
    #[must_use]
    pub fn domain(&self) -> Option<&str> {
        match &self.host {
            Host::Domain(domain) => Some(domain),
            Host::Ipv4(_) | Host::Ipv6(_) => None,
        }
    }

    /// The port, explicit or the scheme's default.
    #[must_use]
    pub fn port(&self) -> u16 {
        self.port
    }
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let scheme = if self.https { "https" } else { "http" };
        write!(f, "{scheme}://{}:{}", self.host, self.port)
    }
}

/// `SHELFY_EGRESS_ALLOW_ORIGINS` (L15): exact origins the operator's own
/// integrations may reach even at a private address. Empty by default; only
/// the operator sets it, from the environment.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OriginAllowlist(Vec<Origin>);

impl OriginAllowlist {
    /// Parses a list of origins separated by commas or spaces.
    ///
    /// # Errors
    ///
    /// A message naming the first invalid entry.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut origins = text
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|entry| !entry.is_empty())
            .map(Origin::parse)
            .collect::<Result<Vec<_>, _>>()?;
        origins.sort();
        origins.dedup();
        Ok(Self(origins))
    }

    /// An allowlist of `origins`.
    #[must_use]
    pub fn new(origins: impl IntoIterator<Item = Origin>) -> Self {
        let mut origins: Vec<Origin> = origins.into_iter().collect();
        origins.sort();
        origins.dedup();
        Self(origins)
    }

    /// Whether `origin` is on the list, exactly: same scheme, host and port.
    #[must_use]
    pub fn contains(&self, origin: &Origin) -> bool {
        self.0.contains(origin)
    }

    /// Whether the list is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The origins.
    pub fn iter(&self) -> impl Iterator<Item = &Origin> {
        self.0.iter()
    }
}

impl fmt::Display for OriginAllowlist {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        for (i, origin) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{origin}")?;
        }
        Ok(())
    }
}

/// A per-use host allowlist (P2-G4): exact names and `*.suffix` patterns.
/// `*.example.com` matches `example.com` and every name below it. Names
/// compare in lowercase; an address literal never matches.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct HostSet {
    exact: BTreeSet<String>,
    /// `.example.com` for `*.example.com`.
    suffixes: Vec<String>,
}

impl HostSet {
    /// A set of `patterns`.
    #[must_use]
    pub fn new<I, S>(patterns: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let mut set = Self::default();
        for pattern in patterns {
            let pattern = pattern.as_ref().trim().to_ascii_lowercase();
            match pattern.strip_prefix("*.") {
                Some(apex) => {
                    set.exact.insert(apex.to_owned());
                    set.suffixes.push(format!(".{apex}"));
                }
                None => {
                    set.exact.insert(pattern);
                }
            }
        }
        set
    }

    /// Whether the host `name` is in the set.
    #[must_use]
    pub fn matches(&self, name: &str) -> bool {
        let name = name.to_ascii_lowercase();
        self.exact.contains(&name) || self.suffixes.iter().any(|suffix| name.ends_with(suffix))
    }
}

/// The error of a lookup whose every address is outside the public internet
/// (or a name a pinned resolver does not serve). The client reports it as a
/// refusal, never as a network failure. Its message holds no host name, as
/// logs never carry post data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("refused: the name has no public address ({reason})")]
pub struct BlockedAddress {
    /// The [`non_public_reason`] of the first address refused, or
    /// `not_allowed` for a name the resolver does not serve.
    pub reason: &'static str,
}

type LookupFn = dyn Fn(&str) -> BoxFuture<'static, io::Result<Vec<IpAddr>>> + Send + Sync;

/// Answers the name lookups of the outbound clients: the system resolver in
/// production. Tests plant their own answers ([`Lookup::fixed`]); no setting
/// of the environment does.
#[derive(Clone)]
pub struct Lookup(Arc<LookupFn>);

impl Lookup {
    /// The system resolver (`getaddrinfo` on tokio's blocking pool).
    #[must_use]
    pub fn system() -> Self {
        Self(Arc::new(|host: &str| {
            let host = host.to_owned();
            Box::pin(async move {
                let found = tokio::net::lookup_host((host.as_str(), 0)).await?;
                Ok(found.map(|addr| addr.ip()).collect())
            })
        }))
    }

    /// Fixed answers, for tests: any other name is not found.
    #[must_use]
    pub fn fixed<I, S>(answers: I) -> Self
    where
        I: IntoIterator<Item = (S, Vec<IpAddr>)>,
        S: AsRef<str>,
    {
        let answers: Arc<BTreeMap<String, Vec<IpAddr>>> = Arc::new(
            answers
                .into_iter()
                .map(|(name, ips)| (name.as_ref().to_ascii_lowercase(), ips))
                .collect(),
        );
        Self(Arc::new(move |host: &str| {
            let found = answers.get(&host.to_ascii_lowercase()).cloned();
            Box::pin(async move {
                found.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such name"))
            })
        }))
    }

    /// The addresses of `host`.
    ///
    /// # Errors
    ///
    /// The lookup failed.
    pub async fn lookup(&self, host: &str) -> io::Result<Vec<IpAddr>> {
        (self.0)(host).await
    }
}

impl Default for Lookup {
    fn default() -> Self {
        Self::system()
    }
}

impl fmt::Debug for Lookup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Lookup")
    }
}

/// The resolver of the direct public client: a dev host resolves to its
/// loopback address; any other name to its public addresses only.
pub(crate) struct PublicResolver {
    lookup: Lookup,
    dev_hosts: Arc<BTreeMap<String, SocketAddr>>,
}

impl PublicResolver {
    pub(crate) fn new(lookup: Lookup, dev_hosts: BTreeMap<String, SocketAddr>) -> Self {
        Self {
            lookup,
            dev_hosts: Arc::new(dev_hosts),
        }
    }
}

impl Resolve for PublicResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        if let Some(&addr) = self.dev_hosts.get(&host) {
            let addrs: Addrs = Box::new(std::iter::once(addr));
            return Box::pin(std::future::ready(Ok::<Addrs, BoxError>(addrs)));
        }
        let lookup = self.lookup.clone();
        Box::pin(async move {
            let found = lookup.lookup(&host).await?;
            if found.is_empty() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no address").into());
            }
            let mut refused: Option<&'static str> = None;
            let public: Vec<SocketAddr> = found
                .into_iter()
                .filter(|&ip| match non_public_reason(ip) {
                    None => true,
                    Some(reason) => {
                        refused.get_or_insert(reason);
                        false
                    }
                })
                .map(|ip| SocketAddr::new(ip, 0))
                .collect();
            if public.is_empty() {
                let reason = refused.unwrap_or("reserved");
                return Err(BlockedAddress { reason }.into());
            }
            Ok::<Addrs, BoxError>(Box::new(public.into_iter()))
        })
    }
}

/// The resolver of a client pinned to known names (the operator allowlist,
/// the capture service, the egress proxy): it resolves those names to any
/// address and refuses every other name.
pub(crate) struct PinnedResolver {
    lookup: Lookup,
    names: Arc<BTreeSet<String>>,
}

impl PinnedResolver {
    pub(crate) fn new<I, S>(lookup: Lookup, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        Self {
            lookup,
            names: Arc::new(
                names
                    .into_iter()
                    .map(|name| name.as_ref().to_ascii_lowercase())
                    .collect(),
            ),
        }
    }
}

impl Resolve for PinnedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_ascii_lowercase();
        if !self.names.contains(&host) {
            let refused: BoxError = BlockedAddress {
                reason: "not_allowed",
            }
            .into();
            return Box::pin(std::future::ready(Err(refused)));
        }
        let lookup = self.lookup.clone();
        Box::pin(async move {
            let found = lookup.lookup(&host).await?;
            if found.is_empty() {
                return Err(io::Error::new(io::ErrorKind::NotFound, "no address").into());
            }
            Ok::<Addrs, BoxError>(Box::new(found.into_iter().map(|ip| SocketAddr::new(ip, 0))))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn non_public_ipv4_blocks_are_refused() {
        for (text, reason) in [
            ("0.0.0.0", "this_network"),
            ("0.255.255.255", "this_network"),
            ("10.0.0.1", "private"),
            ("10.255.255.255", "private"),
            ("100.64.0.1", "cgnat"),
            ("100.101.102.103", "cgnat"),
            ("100.127.255.255", "cgnat"),
            ("127.0.0.1", "loopback"),
            ("127.255.255.254", "loopback"),
            ("169.254.169.254", "link_local"),
            ("169.254.0.1", "link_local"),
            ("172.16.0.1", "private"),
            ("172.31.255.255", "private"),
            ("192.0.0.8", "protocol"),
            ("192.0.2.1", "documentation"),
            ("192.88.99.1", "relay"),
            ("192.168.1.1", "private"),
            ("198.18.0.1", "benchmarking"),
            ("198.19.255.255", "benchmarking"),
            ("198.51.100.7", "documentation"),
            ("203.0.113.9", "documentation"),
            ("224.0.0.1", "multicast"),
            ("239.255.255.250", "multicast"),
            ("240.0.0.1", "reserved"),
            ("255.255.255.255", "reserved"),
        ] {
            assert_eq!(non_public_reason(ip(text)), Some(reason), "{text}");
            assert!(!is_public(ip(text)), "{text}");
        }
    }

    #[test]
    fn public_ipv4_neighbours_pass() {
        for text in [
            "1.1.1.1",
            "8.8.8.8",
            "9.255.255.255",
            "11.0.0.1",
            "100.63.255.255",
            "100.128.0.1",
            "126.255.255.255",
            "128.0.0.1",
            "169.253.255.255",
            "169.255.0.1",
            "172.15.255.255",
            "172.32.0.1",
            "192.0.1.1",
            "192.88.98.1",
            "192.167.255.255",
            "192.169.0.1",
            "198.17.255.255",
            "198.20.0.1",
            "223.255.255.255",
            "157.240.1.35",
        ] {
            assert!(is_public(ip(text)), "{text}");
        }
    }

    #[test]
    fn non_public_ipv6_blocks_are_refused() {
        for (text, reason) in [
            ("::", "unspecified"),
            ("::1", "loopback"),
            ("::127.0.0.1", "ipv4_compatible"),
            ("::8.8.8.8", "ipv4_compatible"),
            ("::ffff:127.0.0.1", "ipv4_mapped"),
            ("::ffff:10.0.0.1", "ipv4_mapped"),
            ("::ffff:8.8.8.8", "ipv4_mapped"),
            ("::ffff:7f00:1", "ipv4_mapped"),
            ("64:ff9b::a00:1", "nat64"),
            ("64:ff9b::808:808", "nat64"),
            ("64:ff9b:1::1", "nat64"),
            ("100::1", "discard"),
            ("2001::1", "protocol"),
            ("2001:0:4136:e378::1", "protocol"),
            ("2001:db8::1", "documentation"),
            ("2002:7f00:1::1", "6to4"),
            ("2002:c0a8:101::1", "6to4"),
            ("3fff::1", "documentation"),
            ("5f00::1", "srv6"),
            ("fc00::1", "unique_local"),
            ("fd12:3456:789a::1", "unique_local"),
            ("fe80::1", "link_local"),
            ("febf::1", "link_local"),
            ("fec0::1", "site_local"),
            ("ff02::1", "multicast"),
            ("ff0e::1", "multicast"),
            ("4000::1", "reserved"),
            ("1::1", "reserved"),
            ("e000::1", "reserved"),
        ] {
            assert_eq!(non_public_reason(ip(text)), Some(reason), "{text}");
        }
    }

    #[test]
    fn global_ipv6_passes() {
        for text in [
            "2a03:2880:f12f:83:face:b00c:0:25de",
            "2606:4700:4700::1111",
            "2001:4860:4860::8888",
            "2001:200::1",
            "2003::1",
            "3ffe::1",
        ] {
            assert!(is_public(ip(text)), "{text}");
        }
    }

    #[test]
    fn localhost_names() {
        for host in ["localhost", "LOCALHOST", "localhost.", "app.localhost"] {
            assert!(is_localhost_name(host), "{host}");
        }
        for host in ["localhost.evil.test", "notlocalhost", "cdninstagram.com"] {
            assert!(!is_localhost_name(host), "{host}");
        }
    }

    #[test]
    fn origins_are_exact() {
        let origin = Origin::parse("http://100.101.102.103:8080").unwrap();
        assert_eq!(origin.to_string(), "http://100.101.102.103:8080");
        assert_eq!(origin.port(), 8080);
        assert_eq!(origin.domain(), None);
        let named = Origin::parse("HTTPS://Ornith.Tailnet.Example/").unwrap();
        assert_eq!(named.to_string(), "https://ornith.tailnet.example:443");
        assert_eq!(named.domain(), Some("ornith.tailnet.example"));
        let v6 = Origin::parse("http://[fd7a:115c:a1e0::1]:11434").unwrap();
        assert_eq!(v6.to_string(), "http://[fd7a:115c:a1e0::1]:11434");

        let url = Url::parse("http://100.101.102.103:8080/v1/chat?x=1").unwrap();
        assert_eq!(Origin::of(&url), Some(origin.clone()));
        let other_port = Url::parse("http://100.101.102.103:8081/").unwrap();
        assert_ne!(Origin::of(&other_port), Some(origin.clone()));
        let other_scheme = Url::parse("https://100.101.102.103:8080/").unwrap();
        assert_ne!(Origin::of(&other_scheme), Some(origin));
        assert_eq!(Origin::of(&Url::parse("file:///etc/passwd").unwrap()), None);

        for bad in [
            "",
            "100.101.102.103:8080",
            "ftp://example.com",
            "http://example.com/v1",
            "http://example.com/?a=1",
            "http://example.com/#x",
            "http://user:pw@example.com",
        ] {
            assert!(Origin::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_allowlist_parses_lists() {
        assert!(OriginAllowlist::parse("").unwrap().is_empty());
        assert!(OriginAllowlist::parse(" , ").unwrap().is_empty());
        let list = OriginAllowlist::parse(
            "http://100.101.102.103:8080, http://100.101.102.103:8080 https://a.example",
        )
        .unwrap();
        assert_eq!(
            list.to_string(),
            "http://100.101.102.103:8080,https://a.example:443"
        );
        assert!(list.contains(&Origin::parse("http://100.101.102.103:8080/").unwrap()));
        assert!(!list.contains(&Origin::parse("http://100.101.102.103:8081").unwrap()));
        assert!(OriginAllowlist::parse("http://a.example,http://b.example/x").is_err());
        assert_eq!(OriginAllowlist::default().to_string(), "none");
    }

    #[test]
    fn host_sets_match_names_and_suffixes() {
        let set = HostSet::new(["pbs.twimg.com", "*.cdninstagram.com"]);
        for host in [
            "pbs.twimg.com",
            "PBS.twimg.com",
            "cdninstagram.com",
            "scontent-mxp1-1.cdninstagram.com",
            "a.b.cdninstagram.com",
        ] {
            assert!(set.matches(host), "{host}");
        }
        for host in [
            "video.twimg.com",
            "twimg.com",
            "evilcdninstagram.com",
            "cdninstagram.com.evil.test",
            "127.0.0.1",
            "",
        ] {
            assert!(!set.matches(host), "{host}");
        }
        assert!(!HostSet::default().matches("example.com"));
    }

    #[tokio::test]
    async fn the_public_resolver_keeps_public_addresses_only() {
        let lookup = Lookup::fixed([
            ("mixed.example", vec![ip("10.0.0.1"), ip("93.184.215.14")]),
            ("private.example", vec![ip("127.0.0.1"), ip("::1")]),
            ("metadata.example", vec![ip("169.254.169.254")]),
            ("mapped.example", vec![ip("::ffff:10.0.0.1")]),
        ]);
        let dev = BTreeMap::from([(
            "cdn.dev.example".to_owned(),
            SocketAddr::from(([127, 0, 0, 1], 4443)),
        )]);
        let resolver = PublicResolver::new(lookup, dev);
        let resolve = |host: &str| resolver.resolve(host.parse().unwrap());

        let mixed: Vec<SocketAddr> = resolve("mixed.example").await.unwrap().collect();
        assert_eq!(mixed, vec![SocketAddr::new(ip("93.184.215.14"), 0)]);
        for (host, reason) in [
            ("private.example", "loopback"),
            ("metadata.example", "link_local"),
            ("mapped.example", "ipv4_mapped"),
        ] {
            let Err(err) = resolve(host).await else {
                panic!("{host} resolved");
            };
            let blocked = err.downcast_ref::<BlockedAddress>().expect("a refusal");
            assert_eq!(blocked.reason, reason, "{host}");
        }
        let Err(missing) = resolve("missing.example").await else {
            panic!("resolved");
        };
        assert!(missing.downcast_ref::<BlockedAddress>().is_none());
        // A dev host goes to its loopback port, unchecked.
        let dev: Vec<SocketAddr> = resolve("CDN.dev.example").await.unwrap().collect();
        assert_eq!(dev, vec![SocketAddr::from(([127, 0, 0, 1], 4443))]);
    }

    #[tokio::test]
    async fn a_pinned_resolver_serves_its_names_only() {
        let lookup = Lookup::fixed([
            ("node.tailnet.example", vec![ip("100.101.102.103")]),
            ("other.example", vec![ip("93.184.215.14")]),
        ]);
        let resolver = PinnedResolver::new(lookup, ["node.tailnet.example"]);
        let resolve = |host: &str| resolver.resolve(host.parse().unwrap());
        let node: Vec<SocketAddr> = resolve("node.tailnet.example").await.unwrap().collect();
        assert_eq!(node, vec![SocketAddr::new(ip("100.101.102.103"), 0)]);
        let Err(err) = resolve("other.example").await else {
            panic!("resolved a name it does not serve");
        };
        assert_eq!(
            err.downcast_ref::<BlockedAddress>().map(|b| b.reason),
            Some("not_allowed")
        );
    }
}
