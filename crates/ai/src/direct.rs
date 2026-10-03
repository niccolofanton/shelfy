//! A plain-HTTP transport for tests, the stub and dev harnesses such as
//! SPIKE-6's (feature `direct`).
//!
//! It reaches only what needs no egress control: loopback addresses (the
//! test-only loopback route) and allowlisted origins (the operator provider,
//! which the server always connects to directly). For a public destination it
//! resolves the name and applies the guard's answer check, then refuses: public
//! providers go through the server's outbound client and the egress proxy
//! (L11), never through this transport. It speaks HTTP/1.1 without TLS, opens
//! one connection per request, and never follows a redirect.

use std::io;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use http::header::HOST;
use http_body_util::{BodyStream, Full};
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use url::{Host, Position};

use crate::guard::{self, Egress, GuardError, Resolve, SystemResolver};
use crate::transport::{ConnectFailure, HttpRequest, HttpResponse, Transport, TransportError};

/// A plain-HTTP transport to loopback and allowlisted endpoints.
#[derive(Clone)]
pub struct DirectTransport {
    resolver: Arc<dyn Resolve>,
}

impl Default for DirectTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for DirectTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DirectTransport").finish_non_exhaustive()
    }
}

impl DirectTransport {
    /// Resolving through the system resolver.
    #[must_use]
    pub fn new() -> Self {
        Self::with_resolver(Arc::new(SystemResolver))
    }

    /// Resolving through `resolver` (tests answer with any address).
    #[must_use]
    pub fn with_resolver(resolver: Arc<dyn Resolve>) -> Self {
        Self { resolver }
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let url = &request.url;
        let unsupported = |what: &str| TransportError::Unsupported(what.to_owned());
        let host = url
            .host()
            .ok_or_else(|| unsupported("the URL has no host"))?;
        let port = url
            .port_or_known_default()
            .ok_or_else(|| unsupported("the URL has no port"))?;
        let addresses = match host {
            Host::Ipv4(ip) => vec![IpAddr::V4(ip)],
            Host::Ipv6(ip) => vec![IpAddr::V6(ip)],
            Host::Domain(name) => {
                let resolved = tokio::time::timeout(
                    request.connect_timeout,
                    self.resolver.resolve(name, port),
                )
                .await
                .map_err(|_| TransportError::Connect(ConnectFailure::Timeout))?
                .map_err(|_| TransportError::Connect(ConnectFailure::Dns))?;
                if resolved.is_empty() {
                    return Err(TransportError::Connect(ConnectFailure::Dns));
                }
                resolved
            }
        };
        match request.egress {
            Egress::Allowlisted => {}
            Egress::Loopback => {
                if let Some(ip) = addresses.iter().find(|ip| !ip.to_canonical().is_loopback()) {
                    return Err(GuardError::NotLoopback(*ip).into());
                }
            }
            Egress::Public => {
                guard::check_answers(&addresses)?;
                return Err(unsupported(
                    "public providers go through the server's outbound client, not the direct transport",
                ));
            }
        }
        if url.scheme() != "http" {
            return Err(unsupported("the direct transport speaks plain http only"));
        }
        let stream = connect(&addresses, port, request.connect_timeout).await?;
        let io_error = |error: hyper::Error| TransportError::Io(error.to_string());
        let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(io_error)?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        let authority = match url.port() {
            Some(port) => format!("{}:{port}", url.host_str().unwrap_or_default()),
            None => url.host_str().unwrap_or_default().to_owned(),
        };
        let mut outgoing = http::Request::builder()
            .method(request.method)
            .uri(&url[Position::BeforePath..Position::AfterQuery])
            .header(HOST, authority)
            .body(Full::new(request.body))
            .map_err(|error| TransportError::Io(error.to_string()))?;
        for (name, value) in &request.headers {
            if name != HOST {
                outgoing.headers_mut().append(name, value.clone());
            }
        }
        let response = sender.send_request(outgoing).await.map_err(io_error)?;
        let (parts, body) = response.into_parts();
        let body = BodyStream::new(body).filter_map(|frame| async move {
            match frame {
                Ok(frame) => frame.into_data().ok().map(Ok),
                Err(error) => Some(Err(TransportError::Io(error.to_string()))),
            }
        });
        Ok(HttpResponse {
            status: parts.status,
            headers: parts.headers,
            body: Box::pin(body),
        })
    }
}

impl Transport for DirectTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(self.exchange(request))
    }
}

/// Connects to the first address that accepts, within `timeout`.
async fn connect(
    addresses: &[IpAddr],
    port: u16,
    timeout: Duration,
) -> Result<TcpStream, TransportError> {
    let attempt = async {
        let mut last = io::Error::new(io::ErrorKind::NotFound, "no address");
        for ip in addresses {
            match TcpStream::connect((*ip, port)).await {
                Ok(stream) => return Ok(stream),
                Err(error) => last = error,
            }
        }
        Err(last)
    };
    let stream = match tokio::time::timeout(timeout, attempt).await {
        Err(_) => return Err(TransportError::Connect(ConnectFailure::Timeout)),
        Ok(Err(error)) => {
            return Err(TransportError::Connect(match error.kind() {
                io::ErrorKind::ConnectionRefused => ConnectFailure::Refused,
                io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
                    ConnectFailure::Unreachable
                }
                _ => ConnectFailure::Other(error.to_string()),
            }));
        }
        Ok(Ok(stream)) => stream,
    };
    let _ = stream.set_nodelay(true);
    Ok(stream)
}
