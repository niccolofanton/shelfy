//! The SSRF suite (P4-02, plan §6.1, §7.1). It drives the probe list in
//! `deploy/egress/ssrf/probes.json` through the API's real outbound client
//! (`shelfy_server::outbound`, L11) against a live Smokescreen egress proxy,
//! and proves that every probe of §6.1 is refused while the positive controls
//! still pass. P4-13's `capture-isolation-check.sh` reuses the same list inside
//! the capture container.
//!
//! It runs only when `SHELFY_SSRF_PROXY` names a reachable proxy, so a plain
//! `cargo test --workspace` (no proxy) skips it. The CI `ssrf` job brings up
//! `deploy/compose.test.yml`'s egress, resolver and fixtures and sets the
//! variable; see that file and `deploy/egress/README.md` for how to run it
//! locally. A probe reaches the proxy two ways:
//!
//! - `client`: through [`shelfy_server::outbound::Outbound`] in proxy mode, so
//!   the production client's own URL checks run first (a literal private
//!   address, a bad port or scheme, or `localhost` is refused before the proxy
//!   sees it; `by: "client"`), and a name that resolves inward reaches the
//!   proxy and comes back as [`Refusal::Proxy`] (`by: "proxy"`);
//! - `raw-connect` / `raw-http`: the exact bytes written to the proxy (as
//!   `scripts/spikes/ssrf-probe.mjs` does), so encodings and ports reach
//!   Smokescreen unnormalised. A 407, an `X-Smokescreen-Error` header or a DNS
//!   failure at the proxy is a refusal.
//!
//! This file never spells the HTTP client crate's name: `tests/outbound.rs`'s
//! single-construction rule scans it like any other.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::Deserialize;
use shelfy_server::outbound::{EgressError, Lookup, Outbound, OutboundConfig, Purpose, Refusal};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use url::Url;

/// The environment variable that points the suite at a live proxy.
const PROXY_ENV: &str = "SHELFY_SSRF_PROXY";

/// A body cap for the positive controls: the fixture answers a short string.
const READ_CAP: u64 = 64 * 1024;

#[derive(Deserialize)]
struct ProbeFile {
    probes: Vec<Probe>,
}

/// One probe. The optional fields describe where a refusal is expected.
#[derive(Deserialize)]
struct Probe {
    cat: String,
    id: String,
    /// `client`, `raw-connect` or `raw-http`.
    via: String,
    /// A URL (`client`, `raw-http`) or a `host:port` authority (`raw-connect`).
    target: String,
    /// `allow` or `refuse`.
    expect: String,
    /// For `refuse`: `client` (the outbound client refuses it) or `proxy`.
    #[serde(default)]
    by: Option<String>,
    /// For `by: client`: the expected [`Refusal`] code, or `any` when the URL
    /// crate may instead reject the spelling as invalid.
    #[serde(default)]
    refusal: Option<String>,
    /// An extra request header for raw probes (`Name: value`).
    #[serde(default)]
    header: Option<String>,
}

impl Probe {
    fn label(&self) -> String {
        format!("{}/{}", self.cat, self.id)
    }
}

/// How the proxy answered a raw request.
#[derive(Debug, PartialEq, Eq)]
enum RawOutcome {
    /// A 407, an `X-Smokescreen-Error` header, or a DNS failure: refused.
    Refused,
    /// A 200 tunnel, or a 2xx/3xx answer with no refusal header.
    Allowed,
    /// Anything else (reported so a surprise is visible, never passed).
    Other(String),
}

/// The proxy's origin, for the raw probes, as `host:port`.
fn proxy_authority(proxy: &Url) -> String {
    format!(
        "{}:{}",
        proxy.host_str().expect("the proxy URL has a host"),
        proxy.port_or_known_default().unwrap_or(80)
    )
}

/// Sends one raw request to the proxy and classifies the answer. Reads only the
/// response head (up to `\r\n\r\n`), so a successful CONNECT does not block on
/// the open tunnel.
async fn raw_proxy(
    authority: &str,
    request_line: &str,
    host: &str,
    header: Option<&str>,
) -> RawOutcome {
    let attempt = async {
        let mut sock = TcpStream::connect(authority).await?;
        let mut request = format!("{request_line} HTTP/1.1\r\nHost: {host}\r\n");
        if let Some(header) = header {
            request.push_str(header);
            request.push_str("\r\n");
        }
        request.push_str("Connection: close\r\n\r\n");
        sock.write_all(request.as_bytes()).await?;
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 1024];
        loop {
            if find_head_end(&buf).is_some() || buf.len() > 64 * 1024 {
                break;
            }
            let read = sock.read(&mut chunk).await?;
            if read == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..read]);
        }
        Ok::<Vec<u8>, std::io::Error>(buf)
    };
    match tokio::time::timeout(Duration::from_secs(15), attempt).await {
        Ok(Ok(buf)) => classify_raw(&buf, request_line.starts_with("CONNECT")),
        Ok(Err(err)) => RawOutcome::Other(format!("io: {err}")),
        Err(_) => RawOutcome::Other("timeout".to_owned()),
    }
}

fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n")
}

/// Classifies a proxy answer the way `ssrf-probe.mjs` does: a 407, an
/// `X-Smokescreen-Error` header or a DNS failure (502 with that header) is a
/// refusal; a 200 CONNECT tunnel or any other 2xx/3xx is allowed.
fn classify_raw(buf: &[u8], is_connect: bool) -> RawOutcome {
    let end = find_head_end(buf).unwrap_or(buf.len());
    let head = String::from_utf8_lossy(&buf[..end]);
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let status: u16 = status_line
        .split(' ')
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or(0);
    let has_error_header = lines.any(|line| {
        line.split_once(':')
            .is_some_and(|(name, _)| name.trim().eq_ignore_ascii_case("x-smokescreen-error"))
    });
    if status == 407 || has_error_header {
        return RawOutcome::Refused;
    }
    if is_connect && status == 200 {
        return RawOutcome::Allowed;
    }
    if (200..400).contains(&status) {
        return RawOutcome::Allowed;
    }
    RawOutcome::Other(format!("HTTP {status}"))
}

/// The repository's `deploy/egress/ssrf/probes.json`.
fn probes_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../deploy/egress/ssrf/probes.json")
}

/// Checks one `client` probe and returns an error message when it does not meet
/// its expectation.
async fn run_client_probe(outbound: &Outbound, probe: &Probe) -> Result<(), String> {
    let client = outbound.client(Purpose::Link);
    let result = client.get(&probe.target).send().await;
    if probe.expect == "allow" {
        return match result {
            Ok(response) if response.status().is_success() => {
                // Drain the body so the in-flight slot is released.
                let _ = response.read_capped(READ_CAP).await;
                Ok(())
            }
            Ok(response) => Err(format!("expected a 2xx, got HTTP {}", response.status())),
            Err(err) => Err(format!("expected a 2xx, got {err:?}")),
        };
    }
    // expect == "refuse"
    match probe.by.as_deref() {
        Some("proxy") => match result {
            Err(EgressError::Refused(Refusal::Proxy)) => Ok(()),
            other => Err(format!("expected Refusal::Proxy, got {}", describe(&other))),
        },
        Some("client") => match (probe.refusal.as_deref(), result) {
            (Some("any"), Err(EgressError::Refused(_) | EgressError::InvalidUrl)) => Ok(()),
            (Some(code), Err(EgressError::Refused(refusal))) if refusal.code() == code => Ok(()),
            (expected, other) => Err(format!(
                "expected a client refusal ({}), got {}",
                expected.unwrap_or("?"),
                describe(&other)
            )),
        },
        other => Err(format!(
            "a refuse probe needs by=client|proxy, not {other:?}"
        )),
    }
}

/// A short description of a client result, for failure messages.
fn describe(result: &Result<shelfy_server::outbound::EgressResponse, EgressError>) -> String {
    match result {
        Ok(response) => format!("Ok(HTTP {})", response.status()),
        Err(err) => format!("{err:?}"),
    }
}

/// Checks one raw probe.
async fn run_raw_probe(authority: &str, probe: &Probe) -> Result<(), String> {
    let outcome = match probe.via.as_str() {
        "raw-connect" => {
            raw_proxy(
                authority,
                &format!("CONNECT {}", probe.target),
                &probe.target,
                probe.header.as_deref(),
            )
            .await
        }
        "raw-http" => {
            let host = probe
                .target
                .strip_prefix("http://")
                .unwrap_or(&probe.target)
                .split('/')
                .next()
                .unwrap_or("");
            raw_proxy(
                authority,
                &format!("GET {}", probe.target),
                host,
                probe.header.as_deref(),
            )
            .await
        }
        other => return Err(format!("unknown raw via {other:?}")),
    };
    let want_allowed = probe.expect == "allow";
    match (&outcome, want_allowed) {
        (RawOutcome::Allowed, true) | (RawOutcome::Refused, false) => Ok(()),
        _ => Err(format!(
            "expected {}, got {outcome:?}",
            if want_allowed { "allowed" } else { "refused" }
        )),
    }
}

#[tokio::test]
async fn every_ssrf_probe_is_refused() {
    let Ok(proxy_url) = std::env::var(PROXY_ENV) else {
        eprintln!("{PROXY_ENV} is unset: the SSRF suite is skipped (see deploy/egress/README.md)");
        return;
    };
    let proxy = Url::parse(&proxy_url)
        .unwrap_or_else(|e| panic!("{PROXY_ENV}={proxy_url:?} is not a URL: {e}"));
    let authority = proxy_authority(&proxy);

    // The real outbound client in proxy mode. In this mode destination names go
    // to the proxy verbatim, so the client resolves nothing itself.
    let config = OutboundConfig {
        proxy: Some(proxy.clone()),
        lookup: Lookup::fixed::<_, &str>([]),
        ..OutboundConfig::default()
    };
    let outbound = Outbound::new(&config).expect("the outbound client builds");

    let raw = std::fs::read(probes_path()).expect("deploy/egress/ssrf/probes.json is readable");
    let file: ProbeFile = serde_json::from_slice(&raw).expect("probes.json is valid JSON");
    assert!(!file.probes.is_empty(), "the probe list is empty");

    let started = Instant::now();
    let mut failures = Vec::new();
    let mut allowed = 0_usize;
    let mut refused = 0_usize;
    let mut per_cat: std::collections::BTreeMap<String, (usize, usize)> = Default::default();

    for probe in &file.probes {
        let result = if probe.via == "client" {
            run_client_probe(&outbound, probe).await
        } else {
            run_raw_probe(&authority, probe).await
        };
        let entry = per_cat.entry(probe.cat.clone()).or_default();
        if probe.expect == "allow" {
            allowed += 1;
        } else {
            refused += 1;
        }
        match result {
            Ok(()) => entry.0 += 1,
            Err(reason) => {
                entry.1 += 1;
                failures.push(format!("{} [{}]: {reason}", probe.label(), probe.via));
            }
        }
    }

    let elapsed = started.elapsed();
    eprintln!(
        "SSRF suite: {} probes ({refused} refuse, {allowed} allow) in {:.1}s against {proxy}",
        file.probes.len(),
        elapsed.as_secs_f64(),
    );
    for (cat, (ok, bad)) in &per_cat {
        eprintln!("  category {cat}: {ok} ok, {bad} failed");
    }

    assert!(
        failures.is_empty(),
        "{} SSRF probe(s) did not meet their expectation:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
