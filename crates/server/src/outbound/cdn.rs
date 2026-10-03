//! The archive's CDN fetcher (§2.13, D4, D14, SPIKE-2): one image from a
//! platform CDN, streamed into the user's store, and what happened.
//!
//! **Hosts.** Three host groups, https only, and every redirect stays in its
//! group:
//!
//! | Group | Hosts | `Referer` | URL variant |
//! |---|---|---|---|
//! | `instagram` | `*.cdninstagram.com`, `*.fbcdn.net` | `https://www.instagram.com/` | as served (≤ 1080 px) |
//! | `x` | `pbs.twimg.com`, `video.twimg.com` | `https://x.com/` | `/media/<id>?format=…&name=large` (≤ 2048 px) |
//! | `pinterest` | `*.pinimg.com` | `https://www.pinterest.com/` | `/1200x/…` |
//!
//! A variant that answers 403 or 404 falls back to the URL as served. The
//! request is the browser's (SPIKE-2, DL-14): a desktop Chrome
//! `User-Agent`, an image `Accept`, `Accept-Language`, the platform
//! `Referer`, `Sec-Fetch-*`, and no cookie or credential. 30 s per request;
//! the body streams into [`UserMedia::ingest_async`] with
//! [`IngestLimits::ARCHIVE_IMAGE`] (15 MB, images only).
//!
//! **Outcomes** ([`FetchOutcome`]):
//!
//! | Outcome | When | Breaker |
//! |---|---|---|
//! | `Stored` | a 2xx image, staged in the store | a sample, not blocked |
//! | `Expired` | an Instagram `oe` (or the caller's expiry) in the past: **no request is sent**; or a 403 whose body is the signature expiry text | never counts |
//! | `Gone` | 404, 410, 451 | never counts |
//! | `Blocked` | any other 403, 401, 407, 429, or a 2xx page instead of an image (a challenge) | a blocked sample |
//! | `Transient` | 5xx, 408, 425, timeouts, connection errors, an empty body; with `Retry-After` when sent | a sample, not blocked |
//! | `Rejected` | not a CDN URL; a URL or redirect our policy refuses; over 15 MB; a file that is not an image of the allowlist; too many redirects; another status | never counts |
//! | `BreakerOpen` | the group's breaker is open: **no request is sent** | — |
//!
//! Limits and breakers are per host group ([`super::limits`],
//! [`super::breaker`]); the fetcher shares them with the rest of outbound
//! HTTP. `shelfy_media_fetch_total{host_group,outcome}` counts every fetch;
//! `host_group` is `none` for a URL outside the three CDN groups.

use std::sync::Arc;
use std::time::Duration;

use axum::http::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use shelfy_core::legacy::convert::cdn_url_expiry_ms;
use shelfy_media::store::{IngestError, IngestLimits, StagedObject, UserMedia};
use tokio::sync::watch;
use tokio::time::Instant;
use url::Url;

use super::breaker::{Admission, Breaker, BreakerState, Breakers, Signal};
use super::client::{Egress, EgressError, EgressResponse, Refusal};
pub use super::limits::HostGroup;
use super::limits::HostLimits;
use crate::telemetry::metrics::{MEDIA_FETCH_TOTAL, fetch_outcome};

/// The desktop Chrome `User-Agent` SPIKE-2 validated from the VPS.
pub const CHROME_USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) \
     AppleWebKit/537.36 (KHTML, like Gecko) Chrome/141.0.0.0 Safari/537.36";
/// The `Accept` of a browser's image request.
pub const IMAGE_ACCEPT: &str = "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8";
/// The `Accept-Language` of the requests.
pub const LANGUAGES: &str = "en-US,en;q=0.9";
/// The time of one request, its body included (§2.13).
pub const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

/// Bytes of a 403 body read to tell an expired signature from a block.
const EXPIRY_PROBE_BYTES: usize = 1024;
/// The bodies of Instagram's 403 for a signature that is no longer valid
/// (SPIKE-2): a fresh URL fixes them, so they are expiries, not blocks.
const EXPIRY_TEXTS: &[&str] = &[
    "url signature expired",
    "bad url timestamp",
    "url signature mismatch",
];

/// One fetch.
#[derive(Clone, Copy, Debug)]
pub struct FetchRequest<'a> {
    /// The URL as the post holds it.
    pub url: &'a str,
    /// The user's store, where the bytes go.
    pub media: &'a UserMedia,
    /// When the URL expires, if the caller knows (`*_expires_at`); else an
    /// Instagram URL's `oe` decides.
    pub expires_at_ms: Option<i64>,
    /// The caller's time (unix ms): the job clock.
    pub now_ms: i64,
}

/// What a fetch did.
#[derive(Debug)]
pub enum FetchOutcome {
    /// The image is staged in the user's store: publish it, or drop it to
    /// remove the temporary file.
    Stored(StagedObject),
    /// The URL's signature expired: the extension refreshes it (P2-14).
    Expired,
    /// The media is gone (404, 410, 451).
    Gone,
    /// The CDN refused the server.
    Blocked {
        /// The status.
        status: u16,
        /// `Retry-After`, when sent.
        retry_after: Option<Duration>,
    },
    /// Try again later.
    Transient {
        /// `Retry-After`, when sent.
        retry_after: Option<Duration>,
    },
    /// This URL cannot be archived by the server.
    Rejected(Rejection),
    /// The group's breaker is open: nothing was sent.
    BreakerOpen,
}

impl FetchOutcome {
    /// The `outcome` label of `shelfy_media_fetch_total`.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::Stored(_) => fetch_outcome::STORED,
            Self::Expired => fetch_outcome::EXPIRED,
            Self::Gone => fetch_outcome::GONE,
            Self::Blocked { .. } => fetch_outcome::BLOCKED,
            Self::Transient { .. } => fetch_outcome::TRANSIENT,
            Self::Rejected(_) => fetch_outcome::REJECTED,
            Self::BreakerOpen => fetch_outcome::BREAKER_OPEN,
        }
    }

    /// What the outcome tells the breaker.
    fn signal(&self) -> Signal {
        match self {
            Self::Stored(_) => Signal::Served,
            Self::Blocked { .. } => Signal::Blocked,
            Self::Transient { .. } => Signal::Transient,
            Self::Expired
            | Self::Gone
            | Self::Rejected(Rejection::TooLarge | Rejection::NotImage | Rejection::Status(_)) => {
                Signal::Answered
            }
            Self::Rejected(Rejection::Url | Rejection::Refused | Rejection::Redirects)
            | Self::BreakerOpen => Signal::Unknown,
        }
    }
}

/// Why the server cannot archive a URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Rejection {
    /// Not an https URL on a CDN host group; nothing was sent.
    Url,
    /// The egress policy refused a redirect target or an address.
    Refused,
    /// Over 15 MB.
    TooLarge,
    /// The bytes are not an image of the store's allowlist.
    NotImage,
    /// More redirects than allowed.
    Redirects,
    /// Another status (a 400, a 3xx without a usable `Location`…).
    Status(u16),
}

/// The fetcher: its egress handle, and the limits and breakers it shares
/// with the rest of outbound HTTP.
pub struct Cdn {
    egress: Egress,
    limits: Arc<HostLimits>,
    breakers: Arc<Breakers>,
    timeout: Duration,
}

impl Cdn {
    pub(crate) fn new(
        egress: Egress,
        limits: Arc<HostLimits>,
        breakers: Arc<Breakers>,
        timeout: Duration,
    ) -> Self {
        Self {
            egress,
            limits,
            breakers,
            timeout,
        }
    }

    /// Fetches one image into the user's store.
    pub async fn fetch(&self, request: FetchRequest<'_>) -> FetchOutcome {
        let (group, outcome) = self.fetch_counted(request).await;
        let host_group = group.map_or(fetch_outcome::NO_GROUP, HostGroup::label);
        metrics::counter!(MEDIA_FETCH_TOTAL, "host_group" => host_group, "outcome" => outcome.label())
            .increment(1);
        outcome
    }

    async fn fetch_counted(&self, request: FetchRequest<'_>) -> (Option<HostGroup>, FetchOutcome) {
        let Ok(url) = Url::parse(request.url) else {
            return (None, FetchOutcome::Rejected(Rejection::Url));
        };
        let Some(group) = HostGroup::of_url(&url).filter(|group| group.is_cdn()) else {
            return (None, FetchOutcome::Rejected(Rejection::Url));
        };
        if url.scheme() != "https" {
            return (Some(group), FetchOutcome::Rejected(Rejection::Url));
        }
        let expires_at = request.expires_at_ms.or_else(|| {
            (group == HostGroup::Instagram)
                .then(|| cdn_url_expiry_ms(request.url))
                .flatten()
        });
        if expires_at.is_some_and(|at| at <= request.now_ms) {
            return (Some(group), FetchOutcome::Expired);
        }
        let breaker = self.breakers.get(group);
        let probe = match breaker.admit(Instant::now()) {
            Admission::Refuse => return (Some(group), FetchOutcome::BreakerOpen),
            Admission::Pass => None,
            Admission::Probe => Some(ProbeGuard {
                breaker,
                done: false,
            }),
        };
        let slot = self.limits.acquire(group).await;
        let outcome = self.fetch_variants(group, &url, request.media).await;
        drop(slot);
        let signal = outcome.signal();
        match probe {
            Some(probe) => probe.finish(signal),
            None => breaker.record(signal, Instant::now()),
        }
        (Some(group), outcome)
    }

    async fn fetch_variants(&self, group: HostGroup, url: &Url, media: &UserMedia) -> FetchOutcome {
        let candidates = variants(group, url);
        let last = candidates.len() - 1;
        for (i, candidate) in candidates.iter().enumerate() {
            self.limits.pace(group).await;
            let sent = self
                .egress
                .get(candidate.as_str())
                .headers(request_headers(group))
                .https_only()
                .hosts(group.hosts())
                .timeout(self.timeout)
                .send()
                .await;
            let response = match sent {
                Ok(response) => response,
                Err(err) => return outcome_of_error(&err),
            };
            if i < last && matches!(response.status().as_u16(), 403 | 404) {
                continue;
            }
            return classify(group, response, media).await;
        }
        FetchOutcome::Rejected(Rejection::Url)
    }

    /// The breaker of `group`.
    #[must_use]
    pub fn breaker(&self, group: HostGroup) -> &Breaker {
        self.breakers.get(group)
    }

    /// The state of `group`'s breaker now.
    #[must_use]
    pub fn breaker_state(&self, group: HostGroup) -> BreakerState {
        self.breakers.get(group).state(Instant::now())
    }

    /// A receiver that changes when a breaker opens or closes.
    #[must_use]
    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.breakers.subscribe()
    }

    /// The per-group limits.
    #[must_use]
    pub fn limits(&self) -> &HostLimits {
        &self.limits
    }
}

/// Frees the probe slot of a fetch dropped before it finished.
struct ProbeGuard<'a> {
    breaker: &'a Breaker,
    done: bool,
}

impl ProbeGuard<'_> {
    fn finish(mut self, signal: Signal) {
        self.done = true;
        self.breaker.record_probe(signal, Instant::now());
    }
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if !self.done {
            self.breaker.release_probe();
        }
    }
}

/// The headers of a group's requests: what Chrome sends for an image.
#[must_use]
pub fn request_headers(group: HostGroup) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(USER_AGENT, HeaderValue::from_static(CHROME_USER_AGENT));
    headers.insert(ACCEPT, HeaderValue::from_static(IMAGE_ACCEPT));
    headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static(LANGUAGES));
    headers.insert(REFERER, HeaderValue::from_static(group.referer()));
    headers.insert("sec-fetch-dest", HeaderValue::from_static("image"));
    headers.insert("sec-fetch-mode", HeaderValue::from_static("no-cors"));
    headers.insert("sec-fetch-site", HeaderValue::from_static("cross-site"));
    headers
}

/// The URLs to try, in order: the group's bounded variant when it differs,
/// then the URL as served.
#[must_use]
pub fn variants(group: HostGroup, url: &Url) -> Vec<Url> {
    let variant = match group {
        HostGroup::X => x_large(url),
        HostGroup::Pinterest => pinterest_1200(url),
        HostGroup::Instagram
        | HostGroup::InstagramWeb
        | HostGroup::XWeb
        | HostGroup::PinterestWeb => None,
    };
    match variant {
        Some(variant) if variant != *url => vec![variant, url.clone()],
        _ => vec![url.clone()],
    }
}

/// `pbs.twimg.com/media/<id>[.<ext>][:<size>]` → `/media/<id>?format=<f>&name=large`,
/// keeping a png or webp format and using jpg otherwise.
fn x_large(url: &Url) -> Option<Url> {
    if url.host_str() != Some("pbs.twimg.com") {
        return None;
    }
    let file = url.path().strip_prefix("/media/")?;
    let file = file.split(':').next()?;
    let (id, ext) = match file.rsplit_once('.') {
        Some((id, ext)) => (id, Some(ext)),
        None => (file, None),
    };
    let valid_id = !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    if !valid_id {
        return None;
    }
    let declared = url
        .query_pairs()
        .find(|(name, _)| name == "format")
        .map(|(_, value)| value.into_owned());
    let format = declared
        .as_deref()
        .into_iter()
        .chain(ext)
        .find_map(|format| match format.to_ascii_lowercase().as_str() {
            "jpg" | "jpeg" => Some("jpg"),
            "png" => Some("png"),
            "webp" => Some("webp"),
            _ => None,
        })
        .unwrap_or("jpg");
    let mut variant = url.clone();
    variant.set_path(&format!("/media/{id}"));
    variant.set_query(Some(&format!("format={format}&name=large")));
    variant.set_fragment(None);
    Some(variant)
}

/// `*.pinimg.com/<size>/…` (`236x`, `736x`, `originals`…) → `/1200x/…`.
fn pinterest_1200(url: &Url) -> Option<Url> {
    let mut segments = url.path_segments()?;
    let first = segments.next()?;
    let rest: Vec<&str> = segments.collect();
    let sized = first == "originals"
        || first.split_once('x').is_some_and(|(width, height)| {
            !width.is_empty()
                && width.bytes().all(|b| b.is_ascii_digit())
                && height.bytes().all(|b| b.is_ascii_digit())
        });
    if !sized || first == "1200x" || rest.is_empty() {
        return None;
    }
    let mut variant = url.clone();
    variant.set_path(&format!("/1200x/{}", rest.join("/")));
    Some(variant)
}

/// The outcome of an answer.
async fn classify(group: HostGroup, response: EgressResponse, media: &UserMedia) -> FetchOutcome {
    let status = response.status().as_u16();
    let retry_after = response.retry_after();
    match status {
        200..=299 => store(response, media).await,
        403 => {
            if group == HostGroup::Instagram
                && let Ok(head) = response.read_prefix(EXPIRY_PROBE_BYTES).await
                && is_expiry_text(&head)
            {
                return FetchOutcome::Expired;
            }
            FetchOutcome::Blocked {
                status,
                retry_after,
            }
        }
        401 | 407 | 429 => FetchOutcome::Blocked {
            status,
            retry_after,
        },
        404 | 410 | 451 => FetchOutcome::Gone,
        408 | 425 | 500..=599 => FetchOutcome::Transient { retry_after },
        other => FetchOutcome::Rejected(Rejection::Status(other)),
    }
}

/// Streams a 2xx body into the store.
async fn store(response: EgressResponse, media: &UserMedia) -> FetchOutcome {
    let limits = IngestLimits::ARCHIVE_IMAGE;
    if response
        .content_length()
        .is_some_and(|len| len > limits.max_bytes)
    {
        return FetchOutcome::Rejected(Rejection::TooLarge);
    }
    let status = response.status().as_u16();
    let declared_image = response.content_type().is_some_and(|value| {
        value
            .trim_start()
            .to_ascii_lowercase()
            .starts_with("image/")
    });
    let body = response.reader_capped(limits.max_bytes);
    match media.ingest_async(body, limits).await {
        Ok(staged) => FetchOutcome::Stored(staged),
        Err(IngestError::TooLarge { .. }) => FetchOutcome::Rejected(Rejection::TooLarge),
        Err(IngestError::NotAccepted(_)) => FetchOutcome::Rejected(Rejection::NotImage),
        // An image that is not one of ours, or a page (a challenge) instead of
        // an image: the second is the CDN refusing us.
        Err(IngestError::UnknownType) if declared_image => {
            FetchOutcome::Rejected(Rejection::NotImage)
        }
        Err(IngestError::UnknownType) => FetchOutcome::Blocked {
            status,
            retry_after: None,
        },
        Err(IngestError::Empty) => FetchOutcome::Transient { retry_after: None },
        Err(IngestError::Io(err)) => match EgressError::in_io(&err) {
            Some(EgressError::TooLarge { .. }) => FetchOutcome::Rejected(Rejection::TooLarge),
            _ => FetchOutcome::Transient { retry_after: None },
        },
    }
}

fn is_expiry_text(body: &[u8]) -> bool {
    let text = String::from_utf8_lossy(body).to_ascii_lowercase();
    EXPIRY_TEXTS.iter().any(|expiry| text.contains(expiry))
}

/// The outcome of a request that got no answer.
fn outcome_of_error(err: &EgressError) -> FetchOutcome {
    match err {
        // The proxy refused or could not reach the CDN: try again later.
        EgressError::Refused(Refusal::Proxy)
        | EgressError::Timeout
        | EgressError::Connect(_)
        | EgressError::Network(_)
        | EgressError::Decode(_) => FetchOutcome::Transient { retry_after: None },
        EgressError::Refused(_) => FetchOutcome::Rejected(Rejection::Refused),
        EgressError::InvalidUrl => FetchOutcome::Rejected(Rejection::Url),
        EgressError::TooManyRedirects(_) => FetchOutcome::Rejected(Rejection::Redirects),
        EgressError::TooLarge { .. } => FetchOutcome::Rejected(Rejection::TooLarge),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(text: &str) -> Url {
        Url::parse(text).unwrap()
    }

    fn variant_strings(text: &str) -> Vec<String> {
        let url = url(text);
        let group = HostGroup::of_url(&url).unwrap();
        variants(group, &url).iter().map(Url::to_string).collect()
    }

    #[test]
    fn hydration_hosts_get_no_variant() {
        for served in [
            "https://www.instagram.com/p/C0ffee/",
            "https://cdn.syndication.twimg.com/tweet-result?id=1&token=a",
            "https://widgets.pinterest.com/v3/pidgets/pins/info/?pin_ids=1",
        ] {
            assert_eq!(variant_strings(served), [served], "{served}");
        }
    }

    #[test]
    fn x_media_asks_for_the_large_size() {
        assert_eq!(
            variant_strings("https://pbs.twimg.com/media/GxYz12_-a.jpg"),
            [
                "https://pbs.twimg.com/media/GxYz12_-a?format=jpg&name=large",
                "https://pbs.twimg.com/media/GxYz12_-a.jpg"
            ]
        );
        assert_eq!(
            variant_strings("https://pbs.twimg.com/media/GxYz?format=png&name=small")[0],
            "https://pbs.twimg.com/media/GxYz?format=png&name=large"
        );
        assert_eq!(
            variant_strings("https://pbs.twimg.com/media/GxYz.webp:orig")[0],
            "https://pbs.twimg.com/media/GxYz?format=webp&name=large"
        );
        assert_eq!(
            variant_strings("https://pbs.twimg.com/media/GxYz?name=orig")[0],
            "https://pbs.twimg.com/media/GxYz?format=jpg&name=large"
        );
        // Already large: one URL.
        assert_eq!(
            variant_strings("https://pbs.twimg.com/media/GxYz?format=jpg&name=large"),
            ["https://pbs.twimg.com/media/GxYz?format=jpg&name=large"]
        );
        // Video thumbnails and odd paths stay as served.
        for served in [
            "https://pbs.twimg.com/ext_tw_video_thumb/1/pu/img/a.jpg",
            "https://pbs.twimg.com/media/",
            "https://pbs.twimg.com/media/a/b.jpg",
            "https://video.twimg.com/ext_tw_video/1/pu/vid/a.mp4",
        ] {
            assert_eq!(variant_strings(served), [served], "{served}");
        }
    }

    #[test]
    fn pinterest_asks_for_1200_px() {
        for size in ["236x", "474x", "736x", "136x136", "originals"] {
            let served = format!("https://i.pinimg.com/{size}/ab/cd/ef/abcdef0123.jpg");
            assert_eq!(
                variant_strings(&served),
                [
                    "https://i.pinimg.com/1200x/ab/cd/ef/abcdef0123.jpg".to_owned(),
                    served.clone()
                ],
                "{size}"
            );
        }
        for served in [
            "https://i.pinimg.com/1200x/ab/cd/ef/abcdef0123.jpg",
            "https://i.pinimg.com/75x75_RS/ab/cd.jpg",
            "https://i.pinimg.com/736x",
            "https://i.pinimg.com/videos/thumbnails/a.jpg",
        ] {
            assert_eq!(variant_strings(served), [served], "{served}");
        }
    }

    #[test]
    fn instagram_urls_stay_as_served() {
        let served = "https://scontent-mxp1-1.cdninstagram.com/v/t51.2885-15/1_n.jpg?stp=dst-jpg_e35&oe=66F1A2B3&oh=00_x";
        assert_eq!(variant_strings(served), [served]);
    }

    #[test]
    fn the_request_looks_like_a_browser() {
        let headers = request_headers(HostGroup::Pinterest);
        assert_eq!(headers[USER_AGENT], CHROME_USER_AGENT);
        assert!(CHROME_USER_AGENT.contains("Chrome/141.0.0.0"));
        assert_eq!(headers[ACCEPT], IMAGE_ACCEPT);
        assert_eq!(headers[ACCEPT_LANGUAGE], LANGUAGES);
        assert_eq!(headers[REFERER], "https://www.pinterest.com/");
        assert_eq!(headers["sec-fetch-dest"], "image");
        assert!(!headers.contains_key("cookie"));
        assert!(!headers.contains_key("authorization"));
    }

    #[test]
    fn expiry_texts() {
        assert!(is_expiry_text(b"URL signature expired"));
        assert!(is_expiry_text(b"Bad URL timestamp"));
        assert!(is_expiry_text(b"URL signature mismatch"));
        assert!(!is_expiry_text(b"Forbidden"));
        assert!(!is_expiry_text(b""));
    }

    #[test]
    fn outcomes_signal_the_breaker() {
        assert_eq!(FetchOutcome::Gone.signal(), Signal::Answered);
        assert_eq!(FetchOutcome::Expired.signal(), Signal::Answered);
        assert_eq!(
            FetchOutcome::Blocked {
                status: 429,
                retry_after: None
            }
            .signal(),
            Signal::Blocked
        );
        assert_eq!(
            FetchOutcome::Transient { retry_after: None }.signal(),
            Signal::Transient
        );
        assert_eq!(
            FetchOutcome::Rejected(Rejection::Refused).signal(),
            Signal::Unknown
        );
        assert_eq!(
            FetchOutcome::Rejected(Rejection::TooLarge).signal(),
            Signal::Answered
        );
        assert_eq!(
            FetchOutcome::Rejected(Rejection::Url).label(),
            fetch_outcome::REJECTED
        );
        assert_eq!(
            FetchOutcome::BreakerOpen.label(),
            fetch_outcome::BREAKER_OPEN
        );
    }

    #[test]
    fn errors_map_to_outcomes() {
        assert!(matches!(
            outcome_of_error(&EgressError::Refused(Refusal::Address)),
            FetchOutcome::Rejected(Rejection::Refused)
        ));
        assert!(matches!(
            outcome_of_error(&EgressError::Refused(Refusal::Proxy)),
            FetchOutcome::Transient { .. }
        ));
        assert!(matches!(
            outcome_of_error(&EgressError::Timeout),
            FetchOutcome::Transient { .. }
        ));
        assert!(matches!(
            outcome_of_error(&EgressError::TooManyRedirects(5)),
            FetchOutcome::Rejected(Rejection::Redirects)
        ));
    }
}
