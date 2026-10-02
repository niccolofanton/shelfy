//! Network addresses: the client address of a request (plan §2.9 rate
//! limits), the proxies trusted to report it, and which hosts are local.
//!
//! In production the API's TCP peer is the edge nginx (osn), behind
//! Cloudflare's tunnel, and the client's address arrives in
//! `CF-Connecting-IP`. Any client can send that header, so it is believed only
//! when the TCP peer is inside `SHELFY_TRUSTED_PROXIES`; otherwise the peer
//! address is the client. With no trusted proxy (the default), the header is
//! ignored.
//!
//! The server reads the TCP peer from `ConnectInfo<SocketAddr>`, which
//! [`crate::serve`] provides. In-process tests have no peer: [`ClientIp`] is
//! then `None`, unless the test inserts a `ConnectInfo`.

use std::fmt;
use std::net::{IpAddr, SocketAddr};

use axum::extract::{ConnectInfo, FromRequestParts};
use axum::http::request::Parts;
use axum::http::{HeaderMap, HeaderName};

use crate::state::AppState;

/// Header with the client address, set by Cloudflare.
pub const CLIENT_IP_HEADER: HeaderName = HeaderName::from_static("cf-connecting-ip");

/// An address block: `203.0.113.0/24`, `2001:db8::/32`, or one address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IpNet {
    network: IpAddr,
    prefix: u8,
}

impl IpNet {
    /// Parses `addr/prefix` or a bare address (a block of one). Host bits
    /// beyond the prefix are cleared.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong.
    pub fn parse(text: &str) -> Result<Self, String> {
        let text = text.trim();
        let (addr, prefix) = match text.split_once('/') {
            Some((addr, prefix)) => (addr, Some(prefix)),
            None => (text, None),
        };
        let addr: IpAddr = addr
            .parse()
            .map_err(|_| format!("{text:?} is not an IP address or CIDR block"))?;
        let max = if addr.is_ipv4() { 32 } else { 128 };
        let prefix = match prefix {
            None => max,
            Some(prefix) => prefix
                .parse::<u8>()
                .ok()
                .filter(|p| *p <= max)
                .ok_or_else(|| format!("{text:?} has a prefix length outside 0..={max}"))?,
        };
        Ok(Self {
            network: mask(addr, prefix),
            prefix,
        })
    }

    /// Whether `ip` is inside the block. IPv4-mapped IPv6 addresses count as
    /// IPv4.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        let ip = ip.to_canonical();
        ip.is_ipv4() == self.network.is_ipv4() && mask(ip, self.prefix) == self.network
    }
}

impl fmt::Display for IpNet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.network, self.prefix)
    }
}

/// `ip` with every bit after the first `prefix` cleared.
fn mask(ip: IpAddr, prefix: u8) -> IpAddr {
    match ip {
        IpAddr::V4(v4) => {
            let bits = u32::from(v4);
            let mask = u32::MAX.checked_shl(32 - u32::from(prefix)).unwrap_or(0);
            IpAddr::from((bits & mask).to_be_bytes())
        }
        IpAddr::V6(v6) => {
            let bits = u128::from(v6);
            let mask = u128::MAX.checked_shl(128 - u32::from(prefix)).unwrap_or(0);
            IpAddr::from((bits & mask).to_be_bytes())
        }
    }
}

/// `SHELFY_TRUSTED_PROXIES`: the proxies whose `CF-Connecting-IP` is
/// believed. Empty by default: no header is believed.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TrustedProxies(Vec<IpNet>);

impl TrustedProxies {
    /// Parses a list of blocks separated by commas or spaces; an empty list
    /// trusts no proxy.
    ///
    /// # Errors
    ///
    /// A message naming the first invalid entry.
    pub fn parse(text: &str) -> Result<Self, String> {
        text.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|entry| !entry.is_empty())
            .map(IpNet::parse)
            .collect::<Result<Vec<_>, _>>()
            .map(Self)
    }

    /// Whether `ip` is a trusted proxy.
    #[must_use]
    pub fn contains(&self, ip: IpAddr) -> bool {
        self.0.iter().any(|net| net.contains(ip))
    }

    /// Whether no proxy is trusted.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl fmt::Display for TrustedProxies {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0.is_empty() {
            return f.write_str("none");
        }
        for (i, net) in self.0.iter().enumerate() {
            if i > 0 {
                f.write_str(",")?;
            }
            write!(f, "{net}")?;
        }
        Ok(())
    }
}

/// The client address of a request: `CF-Connecting-IP` when the TCP `peer`
/// is a trusted proxy and the header holds an address, otherwise the peer.
/// IPv4-mapped IPv6 addresses come back as IPv4.
#[must_use]
pub fn client_ip(
    headers: &HeaderMap,
    peer: Option<IpAddr>,
    trusted: &TrustedProxies,
) -> Option<IpAddr> {
    let peer = peer.map(|ip| ip.to_canonical())?;
    if trusted.contains(peer)
        && let Some(forwarded) = headers
            .get(CLIENT_IP_HEADER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<IpAddr>().ok())
    {
        return Some(forwarded.to_canonical());
    }
    Some(peer)
}

/// Extractor: the client address of the request ([`client_ip`]); `None`
/// when the request has no TCP peer (in-process tests).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

impl FromRequestParts<AppState> for ClientIp {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let peer = parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|ConnectInfo(addr)| addr.ip());
        Ok(Self(client_ip(
            &parts.headers,
            peer,
            &state.config().trusted_proxies,
        )))
    }
}

/// Whether `host` (a name or an address, IPv6 with or without brackets) is
/// this machine: `localhost`, a `*.localhost` name or a loopback address.
#[must_use]
pub fn is_loopback_host(host: &str) -> bool {
    let host = unbracket(host);
    match host.parse::<IpAddr>() {
        Ok(ip) => ip.to_canonical().is_loopback(),
        Err(_) => {
            let host = host.trim_end_matches('.').to_ascii_lowercase();
            host == "localhost" || host.ends_with(".localhost")
        }
    }
}

/// Whether `host` is on a local network: a loopback host, a private (RFC
/// 1918) or link-local IPv4 address, a unique-local or link-local IPv6
/// address, or a single-label name (a container on the same network, such as
/// `mailpit`).
#[must_use]
pub fn is_local_host(host: &str) -> bool {
    if is_loopback_host(host) {
        return true;
    }
    let host = unbracket(host);
    match host.parse::<IpAddr>() {
        Ok(ip) => match ip.to_canonical() {
            IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
            IpAddr::V6(v6) => v6.is_unique_local() || v6.is_unicast_link_local(),
        },
        Err(_) => !host.trim_end_matches('.').contains('.'),
    }
}

fn unbracket(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host)
}

#[cfg(test)]
mod tests {
    use axum::http::HeaderValue;

    use super::*;

    fn ip(text: &str) -> IpAddr {
        text.parse().unwrap()
    }

    #[test]
    fn blocks_parse_and_match() {
        let net = IpNet::parse("172.18.0.5/16").unwrap();
        assert_eq!(net.to_string(), "172.18.0.0/16", "host bits are cleared");
        assert!(net.contains(ip("172.18.255.1")));
        assert!(net.contains(ip("::ffff:172.18.0.9")), "IPv4-mapped");
        assert!(!net.contains(ip("172.19.0.1")));
        assert!(!net.contains(ip("2001:db8::1")));

        let one = IpNet::parse("10.0.0.7").unwrap();
        assert!(one.contains(ip("10.0.0.7")));
        assert!(!one.contains(ip("10.0.0.8")));

        let v6 = IpNet::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(ip("2001:db8:ffff::1")));
        assert!(!v6.contains(ip("2001:db9::1")));
        assert!(IpNet::parse("0.0.0.0/0").unwrap().contains(ip("8.8.8.8")));

        for bad in [
            "",
            "nope",
            "10.0.0.0/33",
            "::/129",
            "10.0.0.0/x",
            "10.0.0/8",
        ] {
            assert!(IpNet::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn trusted_proxies_parse_lists() {
        assert!(TrustedProxies::parse("").unwrap().is_empty());
        assert!(TrustedProxies::parse(" , ").unwrap().is_empty());
        let list = TrustedProxies::parse("172.16.0.0/12, 127.0.0.1 ::1").unwrap();
        assert_eq!(list.to_string(), "172.16.0.0/12,127.0.0.1/32,::1/128");
        assert!(list.contains(ip("172.20.1.1")));
        assert!(list.contains(ip("::1")));
        assert!(!list.contains(ip("192.0.2.1")));
        assert!(TrustedProxies::parse("172.16.0.0/12,bogus").is_err());
        assert_eq!(TrustedProxies::default().to_string(), "none");
    }

    #[test]
    fn the_header_counts_only_from_a_trusted_peer() {
        let mut headers = HeaderMap::new();
        headers.insert(CLIENT_IP_HEADER, HeaderValue::from_static(" 203.0.113.7 "));
        let trusted = TrustedProxies::parse("172.18.0.0/16").unwrap();

        // From the trusted proxy: the header wins.
        assert_eq!(
            client_ip(&headers, Some(ip("172.18.0.2")), &trusted),
            Some(ip("203.0.113.7"))
        );
        // From anyone else, or with no trusted proxy: the peer.
        assert_eq!(
            client_ip(&headers, Some(ip("198.51.100.9")), &trusted),
            Some(ip("198.51.100.9"))
        );
        assert_eq!(
            client_ip(&headers, Some(ip("172.18.0.2")), &TrustedProxies::default()),
            Some(ip("172.18.0.2"))
        );
        // A trusted peer without a usable header is the client itself.
        headers.insert(CLIENT_IP_HEADER, HeaderValue::from_static("not-an-ip"));
        assert_eq!(
            client_ip(&headers, Some(ip("172.18.0.2")), &trusted),
            Some(ip("172.18.0.2"))
        );
        // IPv4-mapped peers are IPv4; no peer, no client.
        assert_eq!(
            client_ip(&HeaderMap::new(), Some(ip("::ffff:192.0.2.1")), &trusted),
            Some(ip("192.0.2.1"))
        );
        assert_eq!(client_ip(&headers, None, &trusted), None);
    }

    #[test]
    fn local_hosts() {
        for host in [
            "localhost",
            "app.localhost",
            "127.0.0.1",
            "127.9.9.9",
            "::1",
            "[::1]",
        ] {
            assert!(is_loopback_host(host), "{host}");
            assert!(is_local_host(host), "{host}");
        }
        for host in [
            "mailpit",
            "10.1.2.3",
            "192.168.1.10",
            "172.20.0.4",
            "fd00::25",
            "fe80::1",
        ] {
            assert!(!is_loopback_host(host), "{host}");
            assert!(is_local_host(host), "{host}");
        }
        for host in [
            "smtp.resend.com",
            "8.8.8.8",
            "2001:db8::1",
            "localhost.evil.test",
        ] {
            assert!(!is_local_host(host), "{host}");
        }
    }
}
