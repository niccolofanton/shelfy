//! `shelfy-server healthcheck`: the container's health probe (plan §3.2).
//!
//! It asks `GET /health` of the server running next to it and exits 0 only
//! when the answer is 200 with `"status": "ok"`; anything else (no listener,
//! a timeout, another status, a body that is not the health document) exits
//! 1 with the reason on stderr. The image has no curl, and the compose
//! healthcheck runs `["CMD", "/app/shelfy-server", "healthcheck"]`.
//!
//! The probe reads `SHELFY_LISTEN_ADDR`, like `serve`, so it needs no option
//! inside the container. A listener on an unspecified address (`0.0.0.0`,
//! `[::]`) is probed on the loopback of its family. It speaks HTTP/1.0
//! over a plain socket: one request, a body framed by `Content-Length` or by
//! the end of the connection, and no client library.

use std::io::{self, Read as _, Write as _};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use clap::Args;

use crate::config::DEFAULT_LISTEN_ADDR;
use crate::routes::health::{CheckStatus, Health};

/// Default of `--timeout`, in seconds: under compose's 5 s healthcheck
/// timeout, over the 2 s the database check may take.
pub const DEFAULT_TIMEOUT_SECS: u64 = 4;

/// Largest answer read, in bytes. The health document is a few hundred.
const MAX_RESPONSE_BYTES: u64 = 64 * 1024;

/// Arguments of `shelfy-server healthcheck`.
#[derive(Clone, Debug, Args)]
pub struct HealthcheckArgs {
    /// Address of the API listener to probe: the server's own setting. An
    /// unspecified address (`0.0.0.0`, `[::]`) is probed on the loopback.
    #[arg(
        long = "listen",
        env = "SHELFY_LISTEN_ADDR",
        value_name = "ADDR",
        default_value = DEFAULT_LISTEN_ADDR
    )]
    pub listen: SocketAddr,

    /// Seconds the whole probe may take.
    #[arg(
        long = "timeout",
        value_name = "SECONDS",
        default_value_t = DEFAULT_TIMEOUT_SECS,
        value_parser = clap::value_parser!(u64).range(1..=60)
    )]
    pub timeout_secs: u64,
}

/// `shelfy-server healthcheck`: prints the version on success.
///
/// # Errors
///
/// [`ProbeError`]: the server is not healthy or did not answer.
pub fn run(args: &HealthcheckArgs) -> anyhow::Result<()> {
    let target = probe_addr(args.listen);
    let health = probe(target, Duration::from_secs(args.timeout_secs))?;
    println!("ok: shelfy-server {} on {target}", health.version);
    Ok(())
}

/// Where a server listening on `listen` is reached from the same host.
#[must_use]
pub fn probe_addr(listen: SocketAddr) -> SocketAddr {
    let ip = match listen.ip() {
        IpAddr::V4(ip) if ip.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(ip) if ip.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        ip => ip,
    };
    SocketAddr::new(ip, listen.port())
}

/// Why the server is not healthy. The socket error of `Connect` and `Io` is
/// their source: `{err:#}` prints it after the message.
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    /// Nothing accepted the connection.
    #[error("cannot connect to {addr}")]
    Connect {
        /// The probed address.
        addr: SocketAddr,
        /// The socket error.
        source: io::Error,
    },
    /// The exchange failed or timed out.
    #[error("no answer from {addr}")]
    Io {
        /// The probed address.
        addr: SocketAddr,
        /// The socket error.
        source: io::Error,
    },
    /// The answer is not an HTTP response with the health document.
    #[error("malformed answer from {addr}: {reason}")]
    Malformed {
        /// The probed address.
        addr: SocketAddr,
        /// What is wrong with it.
        reason: &'static str,
    },
    /// `/health` answered, but not 200 with `"status": "ok"`.
    #[error("unhealthy: HTTP {status}, status {health}")]
    Unhealthy {
        /// The HTTP status code.
        status: u16,
        /// The `status` member of the document.
        health: &'static str,
    },
}

/// Asks `GET /health` of `addr`, within `timeout` in total; the document
/// when the server is healthy.
///
/// # Errors
///
/// [`ProbeError`] for every other outcome.
pub fn probe(addr: SocketAddr, timeout: Duration) -> Result<Health, ProbeError> {
    let deadline = Instant::now() + timeout;
    let io_error = |source| ProbeError::Io { addr, source };
    let mut stream = TcpStream::connect_timeout(&addr, timeout)
        .map_err(|source| ProbeError::Connect { addr, source })?;
    stream
        .set_write_timeout(Some(remaining(deadline).map_err(io_error)?))
        .map_err(io_error)?;
    let request = format!(
        "GET /health HTTP/1.0\r\nHost: {addr}\r\nUser-Agent: shelfy-healthcheck\r\n\
         Accept: application/json\r\n\r\n"
    );
    stream.write_all(request.as_bytes()).map_err(io_error)?;
    let raw = read_until_closed(&mut stream, deadline).map_err(io_error)?;
    let (status, body) =
        parse_response(&raw).map_err(|reason| ProbeError::Malformed { addr, reason })?;
    let health: Health = serde_json::from_slice(body).map_err(|_| ProbeError::Malformed {
        addr,
        reason: "the body is not the health document",
    })?;
    match (status, health.status) {
        (200, CheckStatus::Ok) => Ok(health),
        (status, CheckStatus::Ok) => Err(ProbeError::Unhealthy {
            status,
            health: "ok",
        }),
        (status, CheckStatus::Fail) => Err(ProbeError::Unhealthy {
            status,
            health: "fail",
        }),
    }
}

/// Time left before `deadline`, or a timeout error.
fn remaining(deadline: Instant) -> io::Result<Duration> {
    let left = deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        Err(io::Error::new(io::ErrorKind::TimedOut, "timed out"))
    } else {
        Ok(left)
    }
}

/// Reads the whole answer: the server closes the connection after an
/// HTTP/1.0 response.
fn read_until_closed(stream: &mut TcpStream, deadline: Instant) -> io::Result<Vec<u8>> {
    let mut raw = Vec::new();
    let mut chunk = [0_u8; 4096];
    loop {
        stream.set_read_timeout(Some(remaining(deadline)?))?;
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(raw),
            Ok(n) => {
                raw.extend_from_slice(&chunk[..n]);
                if raw.len() as u64 > MAX_RESPONSE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "the answer is too large",
                    ));
                }
            }
            Err(err) if err.kind() == io::ErrorKind::Interrupted => {}
            Err(err) => return Err(err),
        }
    }
}

/// The status code and the body of a raw HTTP/1.x response.
fn parse_response(raw: &[u8]) -> Result<(u16, &[u8]), &'static str> {
    let head_end = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or("no end of headers")?;
    let head = std::str::from_utf8(&raw[..head_end]).map_err(|_| "headers are not text")?;
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or_default();
    let mut parts = status_line.split(' ');
    let version = parts.next().unwrap_or_default();
    if !version.starts_with("HTTP/1.") {
        return Err("not an HTTP/1 response");
    }
    let status = parts
        .next()
        .and_then(|code| code.parse::<u16>().ok())
        .filter(|code| (100..600).contains(code))
        .ok_or("no status code")?;
    let mut body = &raw[head_end + 4..];
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("transfer-encoding") {
            return Err("chunked body");
        }
        if name.trim().eq_ignore_ascii_case("content-length") {
            let length: usize = value.trim().parse().map_err(|_| "bad Content-Length")?;
            body = body
                .get(..length)
                .ok_or("the body is shorter than announced")?;
        }
    }
    Ok((status, body))
}

#[cfg(test)]
mod tests {
    use std::net::TcpListener;
    use std::thread;

    use super::*;

    /// A one-shot server on the loopback that answers `response` to the first
    /// connection, or keeps silent with `None` until the client gives up.
    fn fake_server(response: Option<Vec<u8>>) -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = socket.read(&mut request);
            match response {
                Some(response) => {
                    let _ = socket.write_all(&response);
                }
                None => thread::sleep(Duration::from_secs(3)),
            }
        });
        addr
    }

    fn answer(head: &str, body: &str) -> Option<Vec<u8>> {
        Some(format!("{head}\r\n\r\n{body}").into_bytes())
    }

    const OK: &str = r#"{"status":"ok","version":"0.1.0","checks":{"controlDb":"ok"}}"#;
    const FAIL: &str = r#"{"status":"fail","version":"0.1.0","checks":{"controlDb":"fail"}}"#;

    #[test]
    fn unspecified_listeners_are_probed_on_the_loopback() {
        let probe = |listen: &str| probe_addr(listen.parse().unwrap()).to_string();
        assert_eq!(probe("0.0.0.0:8080"), "127.0.0.1:8080");
        assert_eq!(probe("[::]:8080"), "[::1]:8080");
        assert_eq!(probe("10.0.0.5:18189"), "10.0.0.5:18189");
    }

    #[test]
    fn a_healthy_answer_passes() {
        let head = format!(
            "HTTP/1.0 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}",
            OK.len()
        );
        let addr = fake_server(answer(&head, OK));
        let health = probe(addr, Duration::from_secs(2)).unwrap();
        assert_eq!(health.status, CheckStatus::Ok);
        assert_eq!(health.version, "0.1.0");
    }

    #[test]
    fn every_other_answer_fails() {
        let cases = [
            (
                answer("HTTP/1.0 503 Service Unavailable", FAIL),
                "unhealthy: HTTP 503, status fail",
            ),
            (
                answer("HTTP/1.0 200 OK", FAIL),
                "unhealthy: HTTP 200, status fail",
            ),
            (
                answer("HTTP/1.0 500 Internal Server Error", OK),
                "unhealthy: HTTP 500, status ok",
            ),
            (
                answer(
                    "HTTP/1.0 200 OK\r\ncontent-type: text/html",
                    "<!doctype html>",
                ),
                "not the health document",
            ),
            (
                Some(b"SSH-2.0-OpenSSH_9.6\r\n".to_vec()),
                "malformed answer",
            ),
        ];
        for (response, expected) in cases {
            let addr = fake_server(response);
            let err = probe(addr, Duration::from_secs(2)).unwrap_err().to_string();
            assert!(err.contains(expected), "{err:?} lacks {expected:?}");
        }
    }

    #[test]
    fn a_closed_port_and_a_silent_server_fail() {
        let free = TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap();
        let err = probe(free, Duration::from_secs(1)).unwrap_err();
        assert!(matches!(err, ProbeError::Connect { .. }), "{err}");

        let silent = fake_server(None);
        let started = Instant::now();
        let err = probe(silent, Duration::from_millis(500)).unwrap_err();
        assert!(matches!(err, ProbeError::Io { .. }), "{err}");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the timeout holds"
        );
    }

    #[test]
    fn content_length_frames_the_body() {
        let raw = b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\n{}trailing";
        assert_eq!(parse_response(raw).unwrap(), (200, &b"{}"[..]));
        let short = b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\n\r\n{}";
        assert!(parse_response(short).is_err());
        let chunked = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n\r\n";
        assert_eq!(parse_response(chunked).unwrap_err(), "chunked body");
    }
}
