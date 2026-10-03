//! The requests of a hydration (P2-04's recipe for P2-11, SPIKE-9's
//! pacing and stop rule).
//!
//! A [`Gate`] is one hydration's turn on a host group: the breaker admits
//! it ([`Breaker::admit`]), it holds the group's slot (concurrency 1) for
//! its whole chain of requests, and each request waits for the group's pace
//! (1 per 3 s on `www.instagram.com`, 1 per second on X's and Pinterest's
//! hosts, plus 0–250 ms). [`Gate::finish`] reports the chain to the breaker
//! once. Every request:
//!
//! - goes through [`Purpose::Link`] with the group's hosts as its
//!   allowlist, without cookies or credentials;
//! - follows no redirect by itself: a redirect to a login or challenge page
//!   is a block signal (SPIKE-9 never follows them), any other one is
//!   followed by hand within the group's hosts, at most [`MAX_HOPS`] times;
//! - reads its body with a cap ([`PAGE_CAP`], [`JSON_CAP`]).
//!
//! **Block signals** ([`Block`]): a 429, a redirect to a login or challenge
//! page, Cloudflare's `cf-mitigated`, or, in a body the caller could not
//! read, the rate-limit, challenge or login-wall markers SPIKE-9 matched.
//! The first one trips the group's breaker at once ([`Breaker::trip`]),
//! which hands Instagram's links to the extension for 30 minutes.

use std::sync::LazyLock;

use axum::http::header::{ACCEPT, ACCEPT_LANGUAGE, LOCATION, USER_AGENT};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method};
use regex::Regex;
use tokio::time::Instant;
use url::Url;

use crate::outbound::cdn::{CHROME_USER_AGENT, LANGUAGES};
use crate::outbound::{Admission, EgressError, GroupSlot, HostGroup, Outbound, Purpose, Signal};

/// Redirects a hydration request follows by hand.
pub const MAX_HOPS: u8 = 3;
/// The largest HTML page read (an Instagram post page is about 1 MB).
pub const PAGE_CAP: u64 = 4 * 1024 * 1024;
/// The largest JSON answer read.
pub const JSON_CAP: u64 = 2 * 1024 * 1024;

/// What a browser sends for a document load (SPIKE-9: Instagram serves its
/// logged-out data to these).
pub const HTML_ACCEPT: &str =
    "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8";

/// A block signal: the platform refuses this server.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Block {
    /// 429, or a rate-limit message.
    RateLimited,
    /// A challenge page, or Cloudflare's `cf-mitigated`.
    Challenge,
    /// A redirect to, or a page of, the login wall.
    LoginWall,
}

impl Block {
    /// A stable code for logs.
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::RateLimited => "rate_limited",
            Self::Challenge => "challenge",
            Self::LoginWall => "login_wall",
        }
    }
}

/// Why a chain stopped without a verdict.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stop {
    /// The group's breaker is open until then, or another probe is out:
    /// nothing was sent.
    BreakerOpen(Instant),
    /// A block signal: the breaker tripped.
    Blocked(Block),
}

/// A request's answer.
#[derive(Debug)]
pub struct Reply {
    /// The status.
    pub status: u16,
    /// The body, read with the cap; empty when it was over the cap or not
    /// text.
    pub body: String,
    /// The headers.
    pub headers: HeaderMap,
}

/// What a request gave.
#[derive(Debug)]
pub enum Sent {
    /// An answer (any status but a followed redirect).
    Reply(Reply),
    /// A block signal: the gate tripped the breaker.
    Blocked(Block),
    /// No answer: a timeout, a network failure, a refused redirect, a body
    /// over the cap.
    Failed(&'static str),
}

/// One hydration's turn on a host group (module docs).
pub struct Gate<'a> {
    outbound: &'a Outbound,
    group: HostGroup,
    probe: bool,
    finished: bool,
    _slot: GroupSlot,
}

impl<'a> Gate<'a> {
    /// Waits for `group`'s slot once its breaker admits a fetch.
    ///
    /// # Errors
    ///
    /// [`Stop::BreakerOpen`] when the breaker refuses: nothing was sent.
    pub async fn open(outbound: &'a Outbound, group: HostGroup) -> Result<Self, Stop> {
        let breaker = outbound.breakers().get(group);
        let probe = match breaker.admit(Instant::now()) {
            Admission::Pass => false,
            Admission::Probe => true,
            Admission::Refuse => return Err(Stop::BreakerOpen(reopens(outbound, group))),
        };
        let slot = outbound.limits().acquire(group).await;
        Ok(Self {
            outbound,
            group,
            probe,
            finished: false,
            _slot: slot,
        })
    }

    /// Sends one request of the chain: paced, on the group's hosts, with
    /// `headers` (a Chrome user agent and `Accept-Language` unless given).
    pub async fn send(
        &mut self,
        method: Method,
        url: &str,
        headers: HeaderMap,
        body: Option<String>,
        cap: u64,
    ) -> Sent {
        let mut headers = headers;
        headers
            .entry(USER_AGENT)
            .or_insert(HeaderValue::from_static(CHROME_USER_AGENT));
        headers
            .entry(ACCEPT_LANGUAGE)
            .or_insert(HeaderValue::from_static(LANGUAGES));
        headers
            .entry(ACCEPT)
            .or_insert(HeaderValue::from_static("application/json"));
        let mut target = url.to_owned();
        let mut method = method;
        let mut body = body;
        for _ in 0..=MAX_HOPS {
            self.outbound.limits().pace(self.group).await;
            let mut request = self
                .outbound
                .client(Purpose::Link)
                .request(method.clone(), &target)
                .headers(headers.clone())
                .max_redirects(0)
                .hosts(self.group.hosts());
            if let Some(body) = &body {
                request = request.body(body.clone());
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(err) => return Sent::Failed(failure(&err)),
            };
            let status = response.status().as_u16();
            if status == 429 {
                return self.blocked(Block::RateLimited);
            }
            if response.headers().contains_key("cf-mitigated") {
                return self.blocked(Block::Challenge);
            }
            if matches!(status, 301 | 302 | 303 | 307 | 308) {
                let next = response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .and_then(|location| response.url().join(location.trim()).ok());
                let Some(next) = next else {
                    return Sent::Failed("bad_redirect");
                };
                if let Some(block) = path_block(&next) {
                    return self.blocked(block);
                }
                if !self
                    .group
                    .hosts()
                    .matches(next.host_str().unwrap_or_default())
                {
                    return Sent::Failed("redirect_off_hosts");
                }
                if status == 303 || (matches!(status, 301 | 302) && method == Method::POST) {
                    method = Method::GET;
                    body = None;
                }
                target = next.into();
                continue;
            }
            let headers = response.headers().clone();
            let body = match response.text_capped(cap).await {
                Ok(text) => text,
                Err(EgressError::TooLarge { .. }) => return Sent::Failed("too_large"),
                Err(err) => return Sent::Failed(failure(&err)),
            };
            return Sent::Reply(Reply {
                status,
                body,
                headers,
            });
        }
        Sent::Failed("too_many_redirects")
    }

    /// A block signal: trips the breaker (SPIKE-9's stop rule).
    fn blocked(&mut self, block: Block) -> Sent {
        tracing::warn!(
            host_group = self.group.label(),
            signal = block.code(),
            "hydration blocked: the breaker trips"
        );
        self.outbound
            .breakers()
            .get(self.group)
            .trip(Instant::now());
        self.finished = true;
        Sent::Blocked(block)
    }

    /// Checks the body of an answer the caller could not read for the
    /// block markers; a match trips the breaker.
    pub fn check_body(&mut self, reply: &Reply) -> Option<Block> {
        let block = body_block(&reply.body)?;
        let _ = self.blocked(block);
        Some(block)
    }

    /// Reports the chain to the breaker: [`Signal::Served`] for data,
    /// [`Signal::Answered`] for a verdict about the post (gone, gated),
    /// [`Signal::Blocked`] for a refusal that is not a stop signal (a 403),
    /// [`Signal::Transient`] for a 5xx or a network failure,
    /// [`Signal::Unknown`] for our own refusals.
    pub fn finish(mut self, signal: Signal) {
        self.report(signal);
    }

    fn report(&mut self, signal: Signal) {
        if std::mem::replace(&mut self.finished, true) {
            return;
        }
        let breaker = self.outbound.breakers().get(self.group);
        if self.probe {
            breaker.record_probe(signal, Instant::now());
        } else {
            breaker.record(signal, Instant::now());
        }
    }
}

impl Drop for Gate<'_> {
    fn drop(&mut self) {
        if !self.finished && self.probe {
            // Dropped without an answer (the job stopped): the next one probes.
            self.outbound.breakers().get(self.group).release_probe();
        }
    }
}

/// When `group`'s breaker lets a probe through, as tokio time.
#[must_use]
pub fn reopens(outbound: &Outbound, group: HostGroup) -> Instant {
    let now = Instant::now();
    match outbound.breakers().get(group).state(now) {
        crate::outbound::BreakerState::Open { until } => until,
        // Another probe is out: look again in a minute.
        _ => now + std::time::Duration::from_secs(60),
    }
}

/// A stable code of an outbound failure.
fn failure(err: &EgressError) -> &'static str {
    match err {
        // A connect timeout keeps the code it had before F15 made it `Connect`.
        EgressError::Timeout => "timeout",
        EgressError::Connect(_) if err.is_connect_timeout() => "timeout",
        EgressError::Refused(_) | EgressError::InvalidUrl | EgressError::TooManyRedirects(_) => {
            "refused"
        }
        EgressError::TooLarge { .. } => "too_large",
        EgressError::Connect(_) | EgressError::Network(_) | EgressError::Decode(_) => "network",
    }
}

/// The block a redirect target's path names: a login or a challenge page.
#[must_use]
pub fn path_block(url: &Url) -> Option<Block> {
    let path = url.path().to_ascii_lowercase();
    if ["/accounts/login", "/login", "/i/flow/login", "/signup"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return Some(Block::LoginWall);
    }
    if ["/challenge", "/checkpoint", "/captcha"]
        .iter()
        .any(|p| path.starts_with(p))
    {
        return Some(Block::Challenge);
    }
    None
}

static RATE_LIMIT_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)please wait a few minutes before you try again|rate limit exceeded")
        .expect("valid pattern")
});
static CHALLENGE_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)<title>\s*just a moment\.\.\.|cf-chl-|challenge-platform/h/|are you a robot|unusual traffic from your",
    )
    .expect("valid pattern")
});
static LOGIN_TEXT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?i)"require_login":\s*true|"pageID":"[^"]*login[^"]*""#).expect("valid pattern")
});

/// The block markers of a body the caller could not read (SPIKE-9's
/// `bodyBlockOf`). Only checked on failures: healthy pages mention login
/// and captcha modules in their scripts.
#[must_use]
pub fn body_block(body: &str) -> Option<Block> {
    if RATE_LIMIT_TEXT.is_match(body) {
        Some(Block::RateLimited)
    } else if CHALLENGE_TEXT.is_match(body) {
        Some(Block::Challenge)
    } else if LOGIN_TEXT.is_match(body) {
        Some(Block::LoginWall)
    } else {
        None
    }
}

/// A header map of `pairs` (static names and values).
#[must_use]
pub fn headers(pairs: &[(&'static str, &'static str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.insert(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        );
    }
    map
}

/// The headers of a document load (SPIKE-9's `DOC_HEADERS`), with the
/// desktop Chrome identity.
#[must_use]
pub fn document_headers() -> HeaderMap {
    headers(&[
        ("user-agent", CHROME_USER_AGENT),
        ("accept-language", LANGUAGES),
        ("accept", HTML_ACCEPT),
        ("upgrade-insecure-requests", "1"),
        ("sec-fetch-dest", "document"),
        ("sec-fetch-mode", "navigate"),
        ("sec-fetch-site", "none"),
        ("sec-fetch-user", "?1"),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_and_challenge_redirects_are_blocks() {
        let block = |url: &str| path_block(&Url::parse(url).unwrap());
        assert_eq!(
            block("https://www.instagram.com/accounts/login/?next=/p/x/"),
            Some(Block::LoginWall)
        );
        assert_eq!(
            block("https://www.instagram.com/challenge/?next=/p/x/"),
            Some(Block::Challenge)
        );
        assert_eq!(block("https://x.com/i/flow/login"), Some(Block::LoginWall));
        assert_eq!(block("https://www.instagram.com/reel/abc/"), None);
    }

    #[test]
    fn body_markers() {
        assert_eq!(
            body_block("<p>Please wait a few minutes before you try again.</p>"),
            Some(Block::RateLimited)
        );
        assert_eq!(
            body_block("<title>Just a moment...</title>"),
            Some(Block::Challenge)
        );
        assert_eq!(
            body_block(r#"{"pageID":"httpLoginPage"}"#),
            Some(Block::LoginWall)
        );
        assert_eq!(
            body_block(r#"{"require_login": true}"#),
            Some(Block::LoginWall)
        );
        assert_eq!(body_block("<html>a post</html>"), None);
    }
}
