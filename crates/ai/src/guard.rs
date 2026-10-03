//! Which endpoints an AI call may reach (plan §7.1 "Outbound HTTP", G3-2,
//! G3-24, G3-28).
//!
//! Two kinds of endpoint exist:
//!
//! - **Allowlisted origins**: the operator provider's AI and STT endpoints,
//!   which the server defines from its own environment (P3-09). Each is an
//!   exact `scheme://host:port`. They may use plain http and a private address
//!   (the owner's node is a Tailscale peer), and the transport connects to them
//!   directly, never through the egress proxy.
//! - **User URLs**: the base URLs users enter for their own providers (P3-19).
//!   They must use https, must not name a loopback, private or otherwise
//!   non-public address in any spelling (decimal, octal and hex IPv4, IPv4-mapped
//!   IPv6, NAT64), and must not name an operator host on any port. When the
//!   name resolves, every answer must be public ([`check_answers`]); the
//!   transport connects only to the addresses it checked. Once
//!   `SHELFY_EGRESS_PROXY` is set (P4), the proxy carries these calls.
//!
//! A test-only switch, [`EgressPolicy::allow_loopback`] (P3-19's
//! `SHELFY_AI_ALLOW_LOOPBACK`), lets user URLs reach loopback over http, so
//! that tests can use the stub as a user provider.
//!
//! [`EgressPolicy::classify`] judges a URL the server configured itself;
//! [`EgressPolicy::check_user_url`] judges one a user entered. Both return the
//! [`Egress`] class the transport routes by.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use futures_util::future::BoxFuture;
use serde::Serialize;
use url::{Host, Url};

/// How the transport must route a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Egress {
    /// An allowlisted origin (the operator provider): directly, any address,
    /// http allowed, never through the egress proxy.
    Allowlisted,
    /// A loopback address, allowed by the test-only loopback switch: directly.
    Loopback,
    /// A public https endpoint: through `SHELFY_EGRESS_PROXY` when set, else
    /// directly to addresses that pass [`check_answers`].
    Public,
}

/// Why the guard refused a URL or an address.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GuardError {
    /// The text is not an absolute URL with a host.
    #[error("the URL is not valid")]
    InvalidUrl,
    /// Neither http nor https.
    #[error("only http and https URLs are allowed")]
    Scheme,
    /// The URL carries a user name or password.
    #[error("a URL with a user name or password is not allowed")]
    Credentials,
    /// A base URL with a query or a fragment.
    #[error("a base URL with a query or fragment is not allowed")]
    QueryOrFragment,
    /// A user URL over plain http.
    #[error("a provider URL must use https")]
    NotHttps,
    /// A loopback, private, link-local, CGNAT, unique-local, multicast or
    /// reserved address, in any spelling.
    #[error("{0} is not a public address")]
    NonPublicAddress(IpAddr),
    /// `localhost` or a name under it.
    #[error("{0} is a local host name")]
    LocalName(String),
    /// A user URL naming a host of the operator provider.
    #[error("{0} is reserved for the operator provider")]
    OperatorHost(String),
    /// The test-only loopback route reached a non-loopback address.
    #[error("{0} is not a loopback address")]
    NotLoopback(IpAddr),
    /// The name resolved to no address.
    #[error("the host name resolved to no address")]
    NoAddress,
}

/// An exact `scheme://host:port`, with the scheme's default port filled in.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Origin {
    https: bool,
    host: Host<String>,
    port: u16,
}

impl Origin {
    /// The origin of `url`.
    ///
    /// # Errors
    ///
    /// [`GuardError::Scheme`] for other schemes, [`GuardError::InvalidUrl`]
    /// without a host.
    pub fn of(url: &Url) -> Result<Self, GuardError> {
        let https = match url.scheme() {
            "https" => true,
            "http" => false,
            _ => return Err(GuardError::Scheme),
        };
        let host = normalize_host(url.host().ok_or(GuardError::InvalidUrl)?);
        let port = url.port_or_known_default().ok_or(GuardError::InvalidUrl)?;
        Ok(Self { https, host, port })
    }

    /// Parses `scheme://host[:port]` (a path is ignored).
    ///
    /// # Errors
    ///
    /// As [`Origin::of`], and [`GuardError::InvalidUrl`] for text that is not
    /// a URL.
    pub fn parse(text: &str) -> Result<Self, GuardError> {
        Self::of(&Url::parse(text.trim()).map_err(|_| GuardError::InvalidUrl)?)
    }

    /// Whether the origin uses https.
    #[must_use]
    pub fn is_https(&self) -> bool {
        self.https
    }

    /// The host, as the URL parser normalized it.
    #[must_use]
    pub fn host(&self) -> &Host<String> {
        &self.host
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

/// A domain without its trailing dot; addresses unchanged.
fn normalize_host(host: Host<&str>) -> Host<String> {
    match host {
        Host::Domain(name) => Host::Domain(name.trim_end_matches('.').to_ascii_lowercase()),
        Host::Ipv4(ip) => Host::Ipv4(ip),
        Host::Ipv6(ip) => Host::Ipv6(ip),
    }
}

/// The endpoints AI calls may reach: the server's allowlist of exact origins,
/// and the test-only loopback switch.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EgressPolicy {
    allowlist: Vec<Origin>,
    allow_loopback: bool,
}

impl EgressPolicy {
    /// No allowlisted origin, no loopback: only public https URLs pass.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Allowlists one exact origin (an operator endpoint).
    #[must_use]
    pub fn allow(mut self, origin: Origin) -> Self {
        if !self.allowlist.contains(&origin) {
            self.allowlist.push(origin);
        }
        self
    }

    /// The test-only switch: user URLs may use loopback addresses, over http.
    /// The server refuses it unless its public URL is loopback (P3-19).
    #[must_use]
    pub fn allow_loopback(mut self, allow: bool) -> Self {
        self.allow_loopback = allow;
        self
    }

    /// The allowlisted origins.
    #[must_use]
    pub fn allowlist(&self) -> &[Origin] {
        &self.allowlist
    }

    /// Whether the loopback switch is on.
    #[must_use]
    pub fn loopback_allowed(&self) -> bool {
        self.allow_loopback
    }

    /// The route of a URL the server configured itself: [`Egress::Allowlisted`]
    /// when its origin is allowlisted, else the rules of user URLs.
    ///
    /// # Errors
    ///
    /// The first rule the URL breaks.
    pub fn classify(&self, url: &Url) -> Result<Egress, GuardError> {
        let origin = check_shape(url)?;
        if self.allowlist.contains(&origin) {
            return Ok(Egress::Allowlisted);
        }
        self.user_route(url, &origin)
    }

    /// The route of a base URL a user entered: https, a public host, and never
    /// a host of the operator provider, on any port or scheme.
    ///
    /// # Errors
    ///
    /// The first rule the URL breaks.
    pub fn check_user_url(&self, url: &Url) -> Result<Egress, GuardError> {
        let origin = check_shape(url)?;
        self.user_route(url, &origin)
    }

    fn user_route(&self, url: &Url, origin: &Origin) -> Result<Egress, GuardError> {
        if self
            .allowlist
            .iter()
            .any(|allowed| allowed.host == origin.host)
        {
            return Err(GuardError::OperatorHost(origin.host.to_string()));
        }
        if self.allow_loopback && is_loopback_host(&origin.host) {
            return Ok(Egress::Loopback);
        }
        if url.scheme() != "https" {
            return Err(GuardError::NotHttps);
        }
        match &origin.host {
            Host::Ipv4(ip) => check_ip(IpAddr::V4(*ip))?,
            Host::Ipv6(ip) => check_ip(IpAddr::V6(*ip))?,
            Host::Domain(name) => {
                if name == "localhost" || name.ends_with(".localhost") {
                    return Err(GuardError::LocalName(name.clone()));
                }
            }
        }
        Ok(Egress::Public)
    }
}

/// The checks every endpoint URL passes: http(s), a host, no credentials, no
/// query or fragment.
fn check_shape(url: &Url) -> Result<Origin, GuardError> {
    let origin = Origin::of(url)?;
    if !url.username().is_empty() || url.password().is_some() {
        return Err(GuardError::Credentials);
    }
    if url.query().is_some() || url.fragment().is_some() {
        return Err(GuardError::QueryOrFragment);
    }
    Ok(origin)
}

/// `localhost`, a name under it, or a loopback address.
fn is_loopback_host(host: &Host<String>) -> bool {
    match host {
        Host::Domain(name) => name == "localhost" || name.ends_with(".localhost"),
        Host::Ipv4(ip) => ip.is_loopback(),
        Host::Ipv6(ip) => {
            ip.is_loopback() || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Whether `ip` is a public unicast address.
///
/// Refused: "this network" 0/8, private 10/8, 172.16/12 and 192.168/16,
/// CGNAT 100.64/10, loopback 127/8, link-local 169.254/16 (cloud metadata
/// included), the IETF, documentation, 6to4-relay and benchmarking blocks,
/// multicast, reserved 240/4 and broadcast. In IPv6, everything outside
/// global unicast 2000::/3 (unspecified, loopback, unique-local, link-local,
/// multicast…), the special-purpose 2001::/23, documentation 2001:db8::/32 and
/// 3fff::/20, and 6to4 2002::/16. IPv4-mapped (`::ffff:a.b.c.d`) and NAT64
/// (`64:ff9b::/96`) addresses are judged by the IPv4 address they embed.
#[must_use]
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => is_public_v4(v4),
        IpAddr::V6(v6) => is_public_v6(v6),
    }
}

fn is_public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !(a == 0
        || a == 10
        || (a == 100 && (64..=127).contains(&b))
        || a == 127
        || (a == 169 && b == 254)
        || (a == 172 && (16..=31).contains(&b))
        || (a == 192 && b == 0 && (c == 0 || c == 2))
        || (a == 192 && b == 88 && c == 99)
        || (a == 192 && b == 168)
        || (a == 198 && (b == 18 || b == 19))
        || (a == 198 && b == 51 && c == 100)
        || (a == 203 && b == 0 && c == 113)
        || a >= 224)
}

fn is_public_v6(ip: Ipv6Addr) -> bool {
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_public_v4(v4);
    }
    let seg = ip.segments();
    if seg[..6] == [0x64, 0xff9b, 0, 0, 0, 0] {
        let [.., a, b, c, d] = ip.octets();
        return is_public_v4(Ipv4Addr::new(a, b, c, d));
    }
    if seg[0] & 0xe000 != 0x2000 {
        return false;
    }
    !((seg[0] == 0x2001 && seg[1] < 0x0200)
        || (seg[0] == 0x2001 && seg[1] == 0x0db8)
        || seg[0] == 0x2002
        || (seg[0] == 0x3fff && seg[1] < 0x1000))
}

/// [`is_public`] as a check.
///
/// # Errors
///
/// [`GuardError::NonPublicAddress`].
pub fn check_ip(ip: IpAddr) -> Result<(), GuardError> {
    if is_public(ip) {
        Ok(())
    } else {
        Err(GuardError::NonPublicAddress(ip))
    }
}

/// Checks a name's answer set: refused when it is empty or holds any
/// non-public address (an attacker's name may answer with a public address
/// and a private one).
///
/// # Errors
///
/// [`GuardError::NoAddress`] or the first non-public address.
pub fn check_answers(addresses: &[IpAddr]) -> Result<(), GuardError> {
    if addresses.is_empty() {
        return Err(GuardError::NoAddress);
    }
    addresses.iter().try_for_each(|ip| check_ip(*ip))
}

/// Resolves host names. The transport resolves through it, so tests can
/// answer with any address.
pub trait Resolve: Send + Sync {
    /// The addresses of `host`.
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>>;
}

/// The system resolver (`getaddrinfo` on tokio's blocking pool).
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemResolver;

impl Resolve for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let mut addresses: Vec<IpAddr> = tokio::net::lookup_host((host, port))
                .await?
                .map(|address| address.ip())
                .collect();
            addresses.dedup();
            Ok(addresses)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn operator() -> EgressPolicy {
        EgressPolicy::new().allow(Origin::parse("http://100.94.10.20:8080").unwrap())
    }

    #[test]
    fn public_and_non_public_ipv4() {
        for ip in [
            "1.1.1.1",
            "8.8.8.8",
            "93.184.216.34",
            "100.63.255.255",
            "100.128.0.1",
        ] {
            assert!(is_public(ip.parse().unwrap()), "{ip} should be public");
        }
        for ip in [
            "0.0.0.0",
            "10.1.2.3",
            "100.64.0.1",
            "100.127.255.254",
            "127.0.0.1",
            "127.255.255.255",
            "169.254.169.254",
            "172.16.0.1",
            "172.31.255.255",
            "192.0.0.8",
            "192.0.2.1",
            "192.88.99.1",
            "192.168.1.1",
            "198.18.0.1",
            "198.51.100.7",
            "203.0.113.9",
            "224.0.0.1",
            "240.0.0.1",
            "255.255.255.255",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} should not be public");
        }
    }

    #[test]
    fn public_and_non_public_ipv6() {
        for ip in [
            "2606:4700:4700::1111",
            "2a00:1450:4001:80b::200e",
            "64:ff9b::808:808",
        ] {
            assert!(is_public(ip.parse().unwrap()), "{ip} should be public");
        }
        for ip in [
            "::",
            "::1",
            "::ffff:127.0.0.1",
            "::ffff:10.0.0.1",
            "::ffff:169.254.169.254",
            "::127.0.0.1",
            "64:ff9b::7f00:1",
            "64:ff9b::a9fe:a9fe",
            "64:ff9b:1::1",
            "100::1",
            "fc00::1",
            "fd7a:115c:a1e0::1",
            "fe80::1",
            "fec0::1",
            "ff02::1",
            "2001::1",
            "2001:2::1",
            "2001:db8::1",
            "2002:7f00:1::1",
            "3fff::1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip} should not be public");
        }
    }

    #[test]
    fn ipv4_literals_in_every_spelling_are_refused() {
        let policy = EgressPolicy::new();
        for text in [
            "https://127.0.0.1/v1",
            "https://2130706433/v1",
            "https://017700000001/v1",
            "https://0x7f000001/v1",
            "https://0x7f.0.0.1/v1",
            "https://0177.0.0.1/v1",
            "https://127.1/v1",
            "https://0xa9fea9fe/v1",
            "https://[::ffff:127.0.0.1]/v1",
            "https://[::ffff:a9fe:a9fe]/v1",
            "https://[64:ff9b::7f00:1]/v1",
            "https://[::1]/v1",
            "https://100.100.100.100/v1",
        ] {
            let result = policy.check_user_url(&url(text));
            assert!(
                matches!(result, Err(GuardError::NonPublicAddress(_))),
                "{text}: {result:?}"
            );
        }
    }

    #[test]
    fn user_urls_need_https_and_a_public_host() {
        let policy = EgressPolicy::new();
        assert_eq!(
            policy.check_user_url(&url("https://api.openai.com/v1")),
            Ok(Egress::Public)
        );
        assert_eq!(
            policy.check_user_url(&url(
                "https://generativelanguage.googleapis.com/v1beta/openai"
            )),
            Ok(Egress::Public)
        );
        assert_eq!(
            policy.check_user_url(&url("http://api.openai.com/v1")),
            Err(GuardError::NotHttps)
        );
        assert_eq!(
            policy.check_user_url(&url("https://localhost/v1")),
            Err(GuardError::LocalName("localhost".into()))
        );
        assert_eq!(
            policy.check_user_url(&url("https://ai.localhost./v1")),
            Err(GuardError::LocalName("ai.localhost".into()))
        );
        assert_eq!(
            policy.check_user_url(&url("https://user:pass@api.openai.com/v1")),
            Err(GuardError::Credentials)
        );
        assert_eq!(
            policy.check_user_url(&url("https://api.openai.com/v1?key=x")),
            Err(GuardError::QueryOrFragment)
        );
        assert_eq!(
            policy.check_user_url(&url("ftp://api.openai.com/")),
            Err(GuardError::Scheme)
        );
        assert_eq!(
            policy.check_user_url(&url("file:///etc/passwd")),
            Err(GuardError::Scheme)
        );
    }

    #[test]
    fn the_operator_origin_is_allowlisted_exactly() {
        let policy = operator();
        assert_eq!(
            policy.classify(&url("http://100.94.10.20:8080/v1")),
            Ok(Egress::Allowlisted)
        );
        // Another port or another scheme is not the operator's origin, and a
        // user may not name the operator's host at all.
        for text in [
            "http://100.94.10.20:8081/v1",
            "https://100.94.10.20:8080/v1",
            "https://100.94.10.20/v1",
        ] {
            assert_eq!(
                policy.classify(&url(text)),
                Err(GuardError::OperatorHost("100.94.10.20".into())),
                "{text}"
            );
        }
        // Even the exact origin is refused when a user enters it.
        assert_eq!(
            policy.check_user_url(&url("http://100.94.10.20:8080/v1")),
            Err(GuardError::OperatorHost("100.94.10.20".into()))
        );
    }

    #[test]
    fn allowlisted_names_compare_without_case_or_trailing_dot() {
        let policy =
            EgressPolicy::new().allow(Origin::parse("http://ornith.tail.ts.net:8080").unwrap());
        assert_eq!(
            policy.classify(&url("http://Ornith.Tail.TS.net.:8080/v1")),
            Ok(Egress::Allowlisted)
        );
        assert_eq!(
            policy.check_user_url(&url("https://ornith.tail.ts.net/v1")),
            Err(GuardError::OperatorHost("ornith.tail.ts.net".into()))
        );
    }

    #[test]
    fn the_loopback_switch_admits_loopback_only() {
        let policy = EgressPolicy::new().allow_loopback(true);
        for text in [
            "http://127.0.0.1:18381/v1",
            "http://localhost:9/v1",
            "http://[::1]:9/v1",
        ] {
            assert_eq!(
                policy.check_user_url(&url(text)),
                Ok(Egress::Loopback),
                "{text}"
            );
        }
        assert_eq!(
            policy.check_user_url(&url("http://10.0.0.1/v1")),
            Err(GuardError::NotHttps)
        );
        assert_eq!(
            policy.check_user_url(&url("https://10.0.0.1/v1")),
            Err(GuardError::NonPublicAddress("10.0.0.1".parse().unwrap()))
        );
        // Without the switch, loopback is refused like any private address.
        assert_eq!(
            EgressPolicy::new().check_user_url(&url("http://127.0.0.1:18381/v1")),
            Err(GuardError::NotHttps)
        );
    }

    #[test]
    fn an_answer_set_with_one_private_address_is_refused() {
        let public: IpAddr = "93.184.216.34".parse().unwrap();
        let private: IpAddr = "10.0.0.7".parse().unwrap();
        assert_eq!(check_answers(&[public]), Ok(()));
        assert_eq!(
            check_answers(&[public, private]),
            Err(GuardError::NonPublicAddress(private))
        );
        assert_eq!(check_answers(&[]), Err(GuardError::NoAddress));
    }

    #[test]
    fn origins_fill_in_default_ports() {
        let origin = Origin::parse("https://api.anthropic.com").unwrap();
        assert_eq!(origin.port(), 443);
        assert!(origin.is_https());
        assert_eq!(origin.to_string(), "https://api.anthropic.com:443");
        assert_eq!(
            Origin::parse("http://[::1]:8178/inference")
                .unwrap()
                .to_string(),
            "http://[::1]:8178"
        );
    }
}
