//! Pinterest's short links (`pin.it/<code>`), resolved before `POST /links`
//! keys them (P2-11).
//!
//! `pin.it` answers with a redirect to `api.pinterest.com`'s shortener,
//! which redirects to the pin. The resolver follows the chain by hand, at
//! most [`MAX_REDIRECTS`] hops, each through P2-04's client with
//! [`Purpose::Link`] and the allowlist below, and stops at the first
//! `Location` that names a pin ([`link::classify`]): the pin page itself is
//! never fetched (SPIKE-9: it is 1 MB and rate-limits fast). A hop off the
//! allowlist is refused before anything is sent to it; a chain that ends
//! anywhere but on a pin is not a pin link. Requests take the
//! `pinterest_web` pace (1 per second) and respect its breaker; a 429 or a
//! challenge trips it.

use std::sync::{Arc, LazyLock};

use axum::http::Method;
use axum::http::header::LOCATION;
use shelfy_core::ids::link::{self, Link, PINTEREST_SHORT_HOST, PostLink};
use shelfy_core::ingest::hosts::PINTEREST_HOSTS;
use tokio::time::Instant;
use url::Url;

use super::fetch::{self, Block};
use crate::outbound::{Admission, HostGroup, HostSet, MAX_REDIRECTS, Outbound, Purpose};

/// The hosts a short link's chain may visit: `pin.it`, the shortener on
/// `api.pinterest.com`, and Pinterest's sites.
static CHAIN_HOSTS: LazyLock<Arc<HostSet>> = LazyLock::new(|| {
    let mut patterns = vec![PINTEREST_SHORT_HOST.to_owned()];
    patterns.extend(PINTEREST_HOSTS.iter().map(|host| format!("*.{host}")));
    Arc::new(HostSet::new(patterns))
});

/// Why a short link gave no pin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShortLinkError {
    /// The chain left the allowlist, ended without a pin, or the link does
    /// not exist (404): not a link Shelfy saves.
    NotAPin,
    /// No answer now (a timeout, a 5xx, the breaker open, a block signal):
    /// try again later.
    Unavailable,
}

/// Resolves a `pin.it` link to the pin it names.
///
/// # Errors
///
/// [`ShortLinkError`].
pub async fn resolve(outbound: &Outbound, short: &Url) -> Result<PostLink, ShortLinkError> {
    let group = HostGroup::PinterestWeb;
    let breaker = outbound.breakers().get(group);
    if breaker.admit(Instant::now()) == Admission::Refuse {
        return Err(ShortLinkError::Unavailable);
    }
    // A probe's answer about a short link says little about the hydration
    // hosts: free the probe slot for the next hydration.
    breaker.release_probe();
    let mut url = short.clone();
    for _ in 0..MAX_REDIRECTS {
        outbound.limits().pace(group).await;
        let response = outbound
            .client(Purpose::Link)
            .request(Method::GET, url.as_str())
            .headers(fetch::document_headers())
            .max_redirects(0)
            .hosts(Arc::clone(&CHAIN_HOSTS))
            .send()
            .await
            .map_err(|_| ShortLinkError::Unavailable)?;
        let status = response.status().as_u16();
        if status == 429 || response.headers().contains_key("cf-mitigated") {
            breaker.trip(Instant::now());
            return Err(ShortLinkError::Unavailable);
        }
        if !matches!(status, 301 | 302 | 303 | 307 | 308) {
            return Err(match status {
                500..=599 => ShortLinkError::Unavailable,
                _ => ShortLinkError::NotAPin,
            });
        }
        let next = response
            .headers()
            .get(LOCATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|location| url.join(location.trim()).ok())
            .ok_or(ShortLinkError::NotAPin)?;
        if matches!(fetch::path_block(&next), Some(Block::Challenge)) {
            breaker.trip(Instant::now());
            return Err(ShortLinkError::Unavailable);
        }
        let on_chain = matches!(next.scheme(), "http" | "https")
            && next
                .host_str()
                .is_some_and(|host| CHAIN_HOSTS.matches(host));
        if !on_chain {
            return Err(ShortLinkError::NotAPin);
        }
        if let Ok(Link::Post(post)) = link::classify(next.as_str())
            && post.id.platform() == shelfy_core::ids::Platform::Pinterest
        {
            return Ok(post);
        }
        url = next;
    }
    Err(ShortLinkError::NotAPin)
}
