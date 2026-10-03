//! yt-dlp (plan D15, §2.13): an anonymous download of one post's video.
//!
//! # The post URL
//!
//! A [`PostUrl`] is https, on one of the platform's post hosts (the desktop's
//! `VIDEO_POST_HOSTS`), without credentials or a port; the fragment is
//! dropped. X's old `https://x.com//status/<id>` (no author) becomes
//! `/i/status/<id>`, as the desktop does. It reaches yt-dlp after `--`, so it
//! can never be read as an option.
//!
//! # The argv
//!
//! ```text
//! yt-dlp --ignore-config --no-cookies --no-cookies-from-browser --no-cache-dir
//!        --no-plugin-dirs --no-update --use-extractors <extractor> --newline
//!        --progress-template <template> --sleep-requests 1
//!        -S "proto:https,vcodec:h264,res:1080" -f "bv*+ba/b" --merge-output-format mp4
//!        --ffmpeg-location <ffmpeg> --proxy <egress>
//!        --no-playlist | --yes-playlist --playlist-items <n>
//!        -o <run dir>/video.%(ext)s -- <post URL>
//! ```
//!
//! - The first five flags are the desktop's anonymous ones: no configuration
//!   file, cookie jar, browser cookies, cache or plugin is ever read, so no
//!   account credential can reach a run.
//! - `--no-update` only silences the "older than 90 days" warning of a pinned
//!   build.
//! - `--use-extractors` allows the platform's extractor alone: never the
//!   generic one, which would fetch any page.
//! - `--sleep-requests 1` spaces the two or three requests of one extraction,
//!   as in SPIKE-9's runs from the VPS (30/30 Instagram, 29/29 X, 30/30
//!   Pinterest).
//! - The format is SPIKE-9's (L17), which replaces §2.13's
//!   `bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b`: prefer https over HLS, H.264,
//!   and at most 1080p. §2.13's string picked VP9 for 21 of 30 Instagram
//!   videos, more than 1080p for 9 of 29 X videos (up to 130 MB) and HLS for
//!   4 of 30 Pinterest videos.
//! - `--proxy` comes from §2.13 and D17.
//!
//! # Progress and output
//!
//! `--progress-template` prints one machine-readable line per update, which
//! [`VideoTools::download`] turns into a [`Progress`]: bytes so far over the
//! parts it has seen (a video and an audio part are merged). Bytes downloaded
//! or announced past the size cap stop the run at once. The run's directory
//! is measured too, as a backstop, against twice the cap: while yt-dlp merges,
//! it holds both parts and the result. The result is the one `video.<ext>`
//! file in the run directory, at most the cap, sniffed: it must be an MP4,
//! QuickTime or WebM video.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::Read as _;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use tokio_util::sync::CancellationToken;
use url::{Host, Url};

use super::process::{self, Flow, Spec, Stop, Watch};
use super::{Egress, Tool, VideoError, VideoFile, VideoTools, blocking, child_env, classify};
use crate::kind::{MediaKind, SNIFF_LEN};

/// The format sort (SPIKE-9, L17): https before HLS and DASH fragments,
/// H.264 before VP9 and AV1, and the best resolution up to 1080p.
pub const FORMAT_SORT: &str = "proto:https,vcodec:h264,res:1080";

/// The format selector (SPIKE-9, L17): the best video with the best audio,
/// merged, else the best single file, in [`FORMAT_SORT`]'s order.
pub const FORMAT: &str = "bv*+ba/b";

/// The output template inside a run's directory.
const OUTPUT: &str = "video.%(ext)s";

/// The stem of the output file.
const OUTPUT_STEM: &str = "video";

/// What each progress line prints: a marker, the status, the bytes so far,
/// the total and the estimated total (`NA` when unknown).
pub const PROGRESS_TEMPLATE: &str = "download:shelfy-progress %(progress.status)s \
    %(progress.downloaded_bytes)s %(progress.total_bytes)s %(progress.total_bytes_estimate)s";

/// The marker of a progress line.
const PROGRESS_MARKER: &str = "shelfy-progress";

/// Shortest gap between two progress reports; a part's end is always
/// reported.
const PROGRESS_EVERY: Duration = Duration::from_millis(250);

/// A platform whose posts yt-dlp can fetch.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VideoPlatform {
    /// Instagram.
    Instagram,
    /// X (stored as `twitter`).
    Twitter,
    /// Pinterest.
    Pinterest,
}

impl VideoPlatform {
    /// Every platform, in a stable order.
    pub const ALL: [Self; 3] = [Self::Instagram, Self::Twitter, Self::Pinterest];

    /// The `posts.platform` value.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Instagram => "instagram",
            Self::Twitter => "twitter",
            Self::Pinterest => "pinterest",
        }
    }

    /// The yt-dlp extractor allowed for it (`--use-extractors`), as on the
    /// desktop.
    #[must_use]
    pub const fn extractor(self) -> &'static str {
        match self {
            Self::Instagram => "Instagram",
            Self::Twitter => "twitter",
            Self::Pinterest => "Pinterest",
        }
    }

    /// The hosts a post URL may name (the desktop's `VIDEO_POST_HOSTS`).
    #[must_use]
    pub const fn post_hosts(self) -> &'static [&'static str] {
        match self {
            Self::Instagram => &["instagram.com", "www.instagram.com", "m.instagram.com"],
            Self::Twitter => &[
                "x.com",
                "www.x.com",
                "mobile.x.com",
                "twitter.com",
                "www.twitter.com",
                "mobile.twitter.com",
            ],
            Self::Pinterest => &["pinterest.com", "www.pinterest.com"],
        }
    }
}

impl fmt::Display for VideoPlatform {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A platform name that is not one of [`VideoPlatform::ALL`].
#[derive(Debug, thiserror::Error)]
#[error("not a video platform")]
pub struct UnknownPlatform;

impl FromStr for VideoPlatform {
    type Err = UnknownPlatform;

    /// Parses a `posts.platform` value (`instagram`, `twitter`, `pinterest`).
    fn from_str(name: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|platform| platform.as_str() == name)
            .ok_or(UnknownPlatform)
    }
}

/// Why a post URL was refused. No variant holds the URL.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PostUrlError {
    /// Longer than [`PostUrl::MAX_LEN`].
    #[error("the URL is too long")]
    TooLong,
    /// Not an absolute URL.
    #[error("not a valid URL")]
    Invalid,
    /// Not https.
    #[error("the URL is not https")]
    NotHttps,
    /// It carries a user name or a password.
    #[error("the URL carries credentials")]
    Credentials,
    /// It names a port other than 443.
    #[error("the URL names a port")]
    Port,
    /// Its host is not one of the platform's post hosts.
    #[error("the host is not a post host of the platform")]
    Host,
}

/// A post URL yt-dlp may fetch: https, on one of the platform's post hosts
/// (see the module docs). Its `Debug` hides the URL, which the logs never
/// carry (plan §3.7).
#[derive(Clone, PartialEq, Eq)]
pub struct PostUrl {
    platform: VideoPlatform,
    url: String,
}

impl PostUrl {
    /// Longest URL accepted, in bytes.
    pub const MAX_LEN: usize = 2048;

    /// Checks and normalizes `raw` for `platform`.
    ///
    /// # Errors
    ///
    /// [`PostUrlError`] naming the rule it breaks.
    pub fn parse(platform: VideoPlatform, raw: &str) -> Result<Self, PostUrlError> {
        let raw = raw.trim();
        if raw.len() > Self::MAX_LEN {
            return Err(PostUrlError::TooLong);
        }
        let mut url = Url::parse(raw).map_err(|_| PostUrlError::Invalid)?;
        if url.scheme() != "https" {
            return Err(PostUrlError::NotHttps);
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(PostUrlError::Credentials);
        }
        // `Url` drops the scheme's default port, so only another one is left.
        if url.port().is_some() {
            return Err(PostUrlError::Port);
        }
        match url.host() {
            Some(Host::Domain(host)) if platform.post_hosts().contains(&host) => {}
            _ => return Err(PostUrlError::Host),
        }
        url.set_fragment(None);
        if platform == VideoPlatform::Twitter
            && let Some(rest) = url.path().strip_prefix("//status/")
        {
            let path = format!("/i/status/{rest}");
            url.set_path(&path);
        }
        let url = String::from(url);
        if url.len() > Self::MAX_LEN {
            return Err(PostUrlError::TooLong);
        }
        Ok(Self { platform, url })
    }

    /// The platform.
    #[must_use]
    pub fn platform(&self) -> VideoPlatform {
        self.platform
    }

    /// The normalized URL.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.url
    }
}

impl fmt::Debug for PostUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PostUrl")
            .field("platform", &self.platform)
            .field("url", &format_args!("[url]"))
            .finish()
    }
}

/// One video for yt-dlp to fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DownloadRequest {
    url: PostUrl,
    playlist_item: Option<NonZeroU32>,
}

impl DownloadRequest {
    /// The post's video (`--no-playlist`): a post with one video.
    #[must_use]
    pub fn new(url: PostUrl) -> Self {
        Self {
            url,
            playlist_item: None,
        }
    }

    /// The `ordinal`-th video of a post with several slides (1-based,
    /// `--yes-playlist --playlist-items <ordinal>`).
    ///
    /// **Count the video slides only.** Anonymously, yt-dlp lists only a
    /// post's videos as playlist entries: Instagram's logged-out extractor
    /// skips image slides, and X's skips photos. So the video at 0-based
    /// position 2 of `[image, video, video]` is ordinal 2, not 3. The
    /// desktop's `position + 1` holds only for its old logged-in path, which
    /// listed every slide.
    #[must_use]
    pub fn video_slide(url: PostUrl, ordinal: NonZeroU32) -> Self {
        Self {
            url,
            playlist_item: Some(ordinal),
        }
    }

    /// The post URL.
    #[must_use]
    pub fn url(&self) -> &PostUrl {
        &self.url
    }

    /// The 1-based playlist item, for a post with several videos.
    #[must_use]
    pub fn playlist_item(&self) -> Option<NonZeroU32> {
        self.playlist_item
    }

    /// yt-dlp's arguments (see the module docs): `ffmpeg` merges the parts,
    /// `egress` carries every request, and the output goes to `output`, a
    /// template such as `<dir>/video.%(ext)s`. [`VideoTools::download`] runs
    /// exactly these.
    #[must_use]
    pub fn argv(&self, ffmpeg: &Path, egress: &Egress, output: &OsStr) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "--ignore-config",
            "--no-cookies",
            "--no-cookies-from-browser",
            "--no-cache-dir",
            "--no-plugin-dirs",
            "--no-update",
            "--use-extractors",
            self.url.platform.extractor(),
            "--newline",
            "--progress-template",
            PROGRESS_TEMPLATE,
            "--sleep-requests",
            "1",
            "-S",
            FORMAT_SORT,
            "-f",
            FORMAT,
            "--merge-output-format",
            "mp4",
            "--ffmpeg-location",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        args.push(ffmpeg.as_os_str().to_owned());
        args.push("--proxy".into());
        args.push(match egress {
            Egress::Proxy(proxy) => proxy.as_str().into(),
            // yt-dlp: "Pass in an empty string (--proxy "") for direct connection".
            Egress::Direct => OsString::new(),
        });
        match self.playlist_item {
            None => args.push("--no-playlist".into()),
            Some(item) => {
                args.push("--yes-playlist".into());
                args.push("--playlist-items".into());
                args.push(item.to_string().into());
            }
        }
        args.push("-o".into());
        args.push(output.to_owned());
        args.push("--".into());
        args.push(self.url.url.as_str().into());
        args
    }
}

/// How far a download got.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Progress {
    /// Bytes received so far, over every part.
    pub downloaded_bytes: u64,
    /// The expected total of the parts seen so far: a part that starts later
    /// (the audio after the video) raises it. `None` until known.
    pub total_bytes: Option<u64>,
}

impl Progress {
    /// `downloaded / total`, from 0 to 1, once the total is known.
    #[must_use]
    #[allow(clippy::cast_precision_loss)] // a ratio: a few ulps do not matter
    pub fn fraction(&self) -> Option<f64> {
        self.total_bytes
            .filter(|&total| total > 0)
            .map(|total| (self.downloaded_bytes as f64 / total as f64).clamp(0.0, 1.0))
    }
}

/// One parsed progress line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProgressLine {
    finished: bool,
    downloaded: u64,
    total: Option<u64>,
}

/// Parses a line printed with [`PROGRESS_TEMPLATE`]; `None` for any other
/// line.
pub(crate) fn parse_progress(line: &str) -> Option<ProgressLine> {
    let mut fields = line.split_ascii_whitespace();
    if fields.next()? != PROGRESS_MARKER {
        return None;
    }
    let status = fields.next()?;
    let downloaded = number(fields.next()?);
    let total = number(fields.next()?);
    let estimate = number(fields.next()?);
    if fields.next().is_some() {
        return None;
    }
    Some(ProgressLine {
        finished: status == "finished",
        downloaded: downloaded.unwrap_or(0),
        total: total.or(estimate),
    })
}

/// A byte count printed by yt-dlp: an integer, or a float for an estimate;
/// `None` for `NA` or anything else.
// A float checked to be in range: truncating it to whole bytes is the point.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn number(field: &str) -> Option<u64> {
    if let Ok(value) = field.parse::<u64>() {
        return Some(value);
    }
    let value = field.parse::<f64>().ok()?;
    (0.0..1e18).contains(&value).then_some(value as u64)
}

/// Adds up the parts of a download (a video and an audio part, merged).
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// Bytes of the parts that finished.
    finished: u64,
    /// Bytes of the current part.
    current: u64,
    /// Expected size of the current part.
    current_total: Option<u64>,
    /// Whether a part has finished and no other has started.
    idle: bool,
}

impl Tracker {
    pub(crate) fn update(&mut self, line: ProgressLine) -> Progress {
        if line.finished {
            self.finished = self
                .finished
                .saturating_add(line.downloaded.max(self.current));
            self.current = 0;
            self.current_total = None;
            self.idle = true;
        } else {
            if line.downloaded < self.current {
                // A new part started without a "finished" line.
                self.finished = self.finished.saturating_add(self.current);
            }
            self.current = line.downloaded;
            self.current_total = line.total;
            self.idle = false;
        }
        let downloaded = self.finished.saturating_add(self.current);
        let total = if self.idle {
            Some(self.finished)
        } else {
            self.current_total
                .map(|total| self.finished.saturating_add(total.max(self.current)))
        };
        Progress {
            downloaded_bytes: downloaded,
            total_bytes: total,
        }
    }
}

/// The `-o` template for a run's directory. `%` starts a field in yt-dlp's
/// templates, so the directory's own `%` are doubled.
fn output_template(dir: &Path) -> OsString {
    let dir = dir.to_string_lossy().replace('%', "%%");
    let mut template = OsString::from(dir);
    template.push(std::path::MAIN_SEPARATOR_STR);
    template.push(OUTPUT);
    template
}

/// The video yt-dlp left in `dir`: the one regular file named
/// `video.<ext>`, sniffed as a video, at most `max_bytes` long. Symlinks are
/// never followed.
fn find_output(dir: &Path, max_bytes: u64) -> Result<(PathBuf, MediaKind, u64), VideoError> {
    let mut found = None;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Some(ext) = name
            .strip_prefix(OUTPUT_STEM)
            .and_then(|rest| rest.strip_prefix('.'))
        else {
            continue;
        };
        let plain_ext = (1..=5).contains(&ext.len())
            && ext
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit());
        if !plain_ext || !entry.file_type()?.is_file() {
            continue;
        }
        if found.is_some() {
            return Err(VideoError::InvalidOutput);
        }
        found = Some(entry.path());
    }
    let path = found.ok_or(VideoError::InvalidOutput)?;
    let file = super::open_regular(&path)?;
    let bytes = file.metadata()?.len();
    if bytes == 0 {
        return Err(VideoError::InvalidOutput);
    }
    if bytes > max_bytes {
        return Err(VideoError::TooLarge { limit: max_bytes });
    }
    let mut head = Vec::with_capacity(SNIFF_LEN);
    file.take(SNIFF_LEN as u64).read_to_end(&mut head)?;
    match MediaKind::sniff(&head) {
        Some(kind) if kind.is_video() => Ok((path, kind, bytes)),
        _ => Err(VideoError::InvalidOutput),
    }
}

impl VideoTools {
    /// Downloads a post's video with yt-dlp, anonymously, through the
    /// configured egress.
    ///
    /// The run waits for a tool slot, writes only in its own directory under
    /// the scratch directory, and reports [`Progress`] to `on_progress` at
    /// most every 250 ms (and at the end of each part). `on_progress` runs
    /// inline and must not block: hand the value on (a `watch` channel)
    /// rather than awaiting a write. The result is the file as yt-dlp left
    /// it, usually an MP4: [`VideoTools::ready`] makes it the stored form.
    /// Dropping the result removes its directory.
    ///
    /// # Errors
    ///
    /// - [`VideoError::NoEgress`]: no egress is configured;
    /// - [`VideoError::Ytdlp`]: yt-dlp failed, classified from its stderr;
    /// - [`VideoError::TooLarge`], [`VideoError::TimedOut`],
    ///   [`VideoError::Cancelled`]: the run was stopped and its directory
    ///   removed;
    /// - [`VideoError::InvalidOutput`]: no single video came out;
    /// - [`VideoError::Unavailable`], [`VideoError::Io`].
    pub async fn download(
        &self,
        request: &DownloadRequest,
        cancel: &CancellationToken,
        on_progress: &mut (dyn FnMut(Progress) + Send),
    ) -> Result<VideoFile, VideoError> {
        let config = self.config();
        let egress = config.egress.as_ref().ok_or(VideoError::NoEgress)?;
        let _slot = self.slot(cancel).await?;
        let dir = self.run_dir(Tool::Ytdlp).await?;
        let max_bytes = config.max_video_bytes;

        let mut tracker = Tracker::default();
        let mut reported: Option<Instant> = None;
        let mut on_line = |line: &str| {
            let Some(parsed) = parse_progress(line) else {
                return Flow::Continue;
            };
            let progress = tracker.update(parsed);
            if progress.downloaded_bytes > max_bytes
                || progress.total_bytes.is_some_and(|total| total > max_bytes)
            {
                return Flow::Stop(Stop::TooLarge);
            }
            let now = Instant::now();
            if parsed.finished || reported.is_none_or(|at| now - at >= PROGRESS_EVERY) {
                reported = Some(now);
                on_progress(progress);
            }
            Flow::Continue
        };
        let spec = Spec {
            program: &config.ytdlp,
            args: request.argv(&config.ffmpeg, egress, &output_template(dir.path())),
            cwd: dir.path(),
            env: child_env(Some(dir.path()), Some(egress)),
            stdout: true,
            timeout: config.download_timeout,
            kill_grace: config.kill_grace,
            // The progress lines cap what is downloaded at `max_bytes`. The
            // directory is a backstop for a run that prints none: while
            // yt-dlp merges, it holds both parts and the result, so it may
            // reach twice the video.
            watch: Some(Watch {
                dir: dir.path().to_owned(),
                max_bytes: max_bytes.saturating_mul(2),
            }),
        };
        let finished = process::run(spec, cancel, &mut on_line)
            .await
            .map_err(|err| VideoError::from_run(Tool::Ytdlp, err, max_bytes))?;
        if !finished.status.success() {
            return Err(VideoError::Ytdlp(classify::classify(
                finished.stderr.tail(),
            )));
        }
        let root = dir.path().to_owned();
        let (path, kind, bytes) = blocking(move || find_output(&root, max_bytes)).await??;
        Ok(VideoFile {
            dir,
            path,
            kind,
            bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(text: &str) -> ProgressLine {
        parse_progress(text).expect("a progress line")
    }

    #[test]
    fn progress_lines_parse_and_others_do_not() {
        assert_eq!(
            line("shelfy-progress downloading 1024 47611 NA"),
            ProgressLine {
                finished: false,
                downloaded: 1024,
                total: Some(47_611)
            }
        );
        // HLS: no total, a float estimate.
        assert_eq!(
            line("shelfy-progress downloading 2048 NA 100000.5"),
            ProgressLine {
                finished: false,
                downloaded: 2048,
                total: Some(100_000)
            }
        );
        assert_eq!(
            line("shelfy-progress finished 47611 47611 NA"),
            ProgressLine {
                finished: true,
                downloaded: 47_611,
                total: Some(47_611)
            }
        );
        for other in [
            "[download] Destination: /tmp/x/video.mp4",
            "[download]  45.3% of    1.23MiB at    2.34MiB/s ETA 00:00",
            "shelfy-progress",
            "shelfy-progress downloading 1 2",
            "shelfy-progress downloading 1 2 3 4",
            "xshelfy-progress downloading 1 2 3",
        ] {
            assert_eq!(parse_progress(other), None, "{other}");
        }
        assert_eq!(number("-5"), None);
        assert_eq!(number("NaN"), None);
        assert_eq!(number("1e30"), None);
    }

    #[test]
    fn the_tracker_adds_up_the_parts() {
        let mut tracker = Tracker::default();
        let video = |downloaded| ProgressLine {
            finished: false,
            downloaded,
            total: Some(1000),
        };
        assert_eq!(
            tracker.update(video(500)),
            Progress {
                downloaded_bytes: 500,
                total_bytes: Some(1000)
            }
        );
        let done = tracker.update(ProgressLine {
            finished: true,
            downloaded: 1000,
            total: Some(1000),
        });
        assert_eq!(done.fraction(), Some(1.0));
        // The audio part starts: the total grows by its size.
        let audio = tracker.update(ProgressLine {
            finished: false,
            downloaded: 50,
            total: Some(100),
        });
        assert_eq!(
            audio,
            Progress {
                downloaded_bytes: 1050,
                total_bytes: Some(1100)
            }
        );
        // A part that restarts without a "finished" line still counts once.
        let mut tracker = Tracker::default();
        tracker.update(video(800));
        let next = tracker.update(ProgressLine {
            finished: false,
            downloaded: 10,
            total: None,
        });
        assert_eq!(
            next,
            Progress {
                downloaded_bytes: 810,
                total_bytes: None
            }
        );
    }

    #[test]
    fn the_output_template_escapes_percent_signs() {
        let template = output_template(Path::new("/data/50%/run"));
        assert_eq!(
            template,
            OsString::from(format!(
                "/data/50%%/run{}video.%(ext)s",
                std::path::MAIN_SEPARATOR
            ))
        );
    }
}
