//! The AI providers' transport (P3-01 on P2-04, L11): `shelfy_ai`'s
//! [`Transport`] over the outbound client, so that every AI call is routed,
//! capped and counted like any other outbound request.
//!
//! | `shelfy_ai` route | Purpose | Reaches |
//! |---|---|---|
//! | `Allowlisted` (the operator provider) | [`Purpose::AiOperator`] | the operator's origins (`SHELFY_EGRESS_ALLOW_ORIGINS`), directly, never through the proxy |
//! | `Public` (a user's provider) | [`Purpose::Ai`] | https on ports 80 and 443, public addresses only, through `SHELFY_EGRESS_PROXY` when set |
//! | `Loopback` (P3-19's test switch) | [`Purpose::Ai`] | nothing: the client refuses loopback addresses. Only [`AiTransport::with_loopback_for_tests`] (feature `test-loopback`, tests only) reaches a loopback stub, on any port |
//!
//! User providers stay on ports 80 and 443 (F15): the plan sends custom base
//! URLs through the egress proxy (§2.15), which allows those two ports only
//! (§2.18, L18), and L11 gives the direct mode the same rule, so that both
//! modes refuse the same URLs. A private or odd-port endpoint is the
//! operator's, on `SHELFY_EGRESS_ALLOW_ORIGINS` (L15).
//!
//! **Timeouts.** Connecting takes at most the purpose's connect timeout
//! ([`Purpose::connect_timeout`]: 3 s for the node, so that a sleeping node
//! is offline quickly; 10 s for a user's provider), the same values the AI
//! service puts in its `Timeouts::connect`. Passing it is
//! `TransportError::Connect(ConnectFailure::Timeout)`, which the adapters
//! report as `ErrorKind::Offline` (hold), never as a transient timeout. The
//! adapters keep their own deadlines (first token, exchange, call) and
//! report them; the client's timeout is set a little past them. Redirects
//! are never followed. An answer is capped at [`MAX_AI_BODY`].
//!
//! ```no_run
//! # fn example(state: &shelfy_server::state::AppState) -> Result<(), shelfy_ai::AiError> {
//! use std::sync::Arc;
//! use shelfy_ai::{EgressPolicy, Provider, ProviderConfig};
//! use shelfy_server::outbound::ai::AiTransport;
//!
//! let transport = Arc::new(AiTransport::new(state.outbound()));
//! # let (config, policy): (ProviderConfig, EgressPolicy) = todo!();
//! let provider = Provider::new(config, &policy, transport)?;
//! # let _ = provider;
//! # Ok(())
//! # }
//! ```

use std::error::Error as StdError;
use std::io;
use std::time::Duration;

use futures_util::StreamExt as _;
use futures_util::future::BoxFuture;
use shelfy_ai::transport::{ConnectFailure, HttpRequest, HttpResponse, Transport, TransportError};

use super::{Egress, EgressError, Outbound, Purpose};

/// The largest answer an AI call reads: 64 MiB (the adapters stop at 32).
pub const MAX_AI_BODY: u64 = 64 << 20;

/// How far past the adapter's own deadline the client's timeout is set, so
/// that the adapter reports the timeout with its own error.
const MARGIN: Duration = Duration::from_secs(5);

/// `shelfy_ai`'s transport over the outbound client. Cheap to clone.
#[derive(Clone)]
pub struct AiTransport {
    operator: Egress,
    user: Egress,
    /// Tests only: the route of `Egress::Loopback`.
    #[cfg(any(test, feature = "test-loopback"))]
    loopback: Option<Egress>,
}

impl AiTransport {
    /// The transport of `outbound`'s AI purposes.
    #[must_use]
    pub fn new(outbound: &Outbound) -> Self {
        Self {
            operator: outbound.client(Purpose::AiOperator),
            user: outbound.client(Purpose::Ai),
            #[cfg(any(test, feature = "test-loopback"))]
            loopback: None,
        }
    }

    /// Tests only (F15): like [`AiTransport::new`], but a request the guard
    /// classified `Egress::Loopback` (`EgressPolicy::allow_loopback`) reaches
    /// a loopback literal address on any port through [`Purpose::Ai`], so
    /// that a test can use a loopback `shelfy-ai-stub` as a user's provider.
    /// `Public` requests keep the strict rules. Built only with the
    /// `test-loopback` feature, which no release build turns on.
    #[cfg(any(test, feature = "test-loopback"))]
    #[must_use]
    pub fn with_loopback_for_tests(outbound: &Outbound) -> Self {
        Self {
            loopback: Some(outbound.client(Purpose::Ai).loopback_for_tests()),
            ..Self::new(outbound)
        }
    }

    /// The handle of the `Loopback` route: the strict user handle, which
    /// refuses loopback addresses, outside tests.
    fn loopback(&self) -> &Egress {
        #[cfg(any(test, feature = "test-loopback"))]
        if let Some(loopback) = &self.loopback {
            return loopback;
        }
        &self.user
    }

    async fn exchange(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        let (egress, https_only) = match request.egress {
            shelfy_ai::Egress::Allowlisted => (&self.operator, false),
            shelfy_ai::Egress::Public => (&self.user, true),
            shelfy_ai::Egress::Loopback => (self.loopback(), false),
        };
        let mut outgoing = egress
            .request(request.method, request.url.as_str())
            .headers(request.headers)
            .max_redirects(0)
            .timeout(request.timeout.saturating_add(MARGIN));
        if !request.body.is_empty() {
            outgoing = outgoing.body(request.body);
        }
        if https_only {
            outgoing = outgoing.https_only();
        }
        let response = outgoing.send().await.map_err(transport_error)?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response
            .stream_capped(MAX_AI_BODY)
            .map(|chunk| chunk.map_err(transport_error));
        Ok(HttpResponse {
            status,
            headers,
            body: Box::pin(body),
        })
    }
}

impl Transport for AiTransport {
    fn send(&self, request: HttpRequest) -> BoxFuture<'_, Result<HttpResponse, TransportError>> {
        Box::pin(self.exchange(request))
    }
}

/// An outbound error in `shelfy_ai`'s terms. No URL, no header, no body.
/// Only a failure before any byte was sent is `Connect` (offline, held); a
/// request that connected and then ran out of time is `Io` (transient).
fn transport_error(error: EgressError) -> TransportError {
    if error.is_connect_timeout() {
        return TransportError::Connect(ConnectFailure::Timeout);
    }
    match error {
        EgressError::InvalidUrl => TransportError::Blocked("the URL is not valid".to_owned()),
        EgressError::Refused(refusal) => TransportError::Blocked(refusal.to_string()),
        EgressError::TooManyRedirects(_) => {
            TransportError::Blocked("redirects are not followed".to_owned())
        }
        EgressError::Connect(source) => TransportError::Connect(connect_failure(source.as_ref())),
        EgressError::Timeout => TransportError::Io("the request timed out".to_owned()),
        EgressError::TooLarge { limit } => {
            TransportError::Io(format!("the answer is larger than {limit} bytes"))
        }
        EgressError::Network(_) => TransportError::Io("the exchange failed".to_owned()),
        EgressError::Decode(_) => TransportError::Io("the answer is not JSON".to_owned()),
    }
}

/// Why a connection failed, from the I/O error under it.
fn connect_failure(error: &(dyn StdError + Send + Sync + 'static)) -> ConnectFailure {
    let mut current: Option<&(dyn StdError + 'static)> = Some(error);
    while let Some(error) = current {
        if let Some(io) = error.downcast_ref::<io::Error>() {
            match io.kind() {
                io::ErrorKind::ConnectionRefused => return ConnectFailure::Refused,
                io::ErrorKind::HostUnreachable | io::ErrorKind::NetworkUnreachable => {
                    return ConnectFailure::Unreachable;
                }
                io::ErrorKind::TimedOut => return ConnectFailure::Timeout,
                _ => {}
            }
        }
        current = error.source();
    }
    ConnectFailure::Other("the connection failed".to_owned())
}
