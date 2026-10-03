//! The video tools (plan §2.13, §2.3, D15, L17): yt-dlp fetches a post's
//! video anonymously, and ffmpeg makes it ready for the browser, reads its
//! duration and size, and extracts a poster or keyframes.
//!
//! | Module | Contents |
//! |---|---|
//! | [`ytdlp`] | post URLs, yt-dlp's argv, progress and output |
//! | [`ffmpeg`] | ready and remux (MP4, index first), inspect, poster, keyframes |
//! | [`classify`] | yt-dlp's errors mapped to a failure code |
//!
//! [`VideoTools`] runs both. The server builds one from its configuration and
//! shares it: on-demand videos (P4-16), kept videos and P3's keyframes. A
//! yt-dlp fetch is [`VideoTools::download`] then [`VideoTools::ready`], which
//! remuxes only a file whose index does not come first (SPIKE-9 saw none).
//!
//! # How every run is contained
//!
//! - **Two at a time.** yt-dlp and ffmpeg share
//!   [`VideoToolsConfig::max_processes`] slots (2, §2.3). A run holds its slot
//!   until its process is gone; yt-dlp's own ffmpeg (the merge) runs inside
//!   it.
//! - **A clean environment.** The child starts from an empty environment plus
//!   a fixed allowlist (`PATH`, `LANG`, Python's UTF-8 mode, `HOME` and
//!   `TMPDIR` in its own directory, and the proxy for yt-dlp). No secret of
//!   the server reaches a process that parses untrusted media, and no
//!   inherited proxy or configuration changes what it does. Stdin is closed.
//! - **Low priority.** It runs at `nice -n 10`, as §2.3 asks: `nice(10)`
//!   between fork and exec, so its own children inherit it.
//! - **Its own directory.** Each run writes only in a fresh directory under
//!   [`VideoToolsConfig::scratch_dir`] (`.ytdlp-*`, `.ffmpeg-*`). The directory
//!   goes when the run fails or its result is dropped; [`sweep_scratch`]
//!   removes what a crash left.
//! - **Stopping.** Cancellation, the time limit and the size cap stop the run
//!   the same way: SIGTERM to the run's process group, SIGKILL to the group
//!   after [`VideoToolsConfig::kill_grace`] (5 s), then the directory is
//!   removed. Dropping a run's future kills the group at once.
//! - **A size cap.** Past [`VideoToolsConfig::max_video_bytes`] (300 MiB,
//!   [`IngestLimits::VIDEO`]) the run is stopped: yt-dlp's downloaded and
//!   announced bytes are checked, and every run's directory is measured
//!   while it runs (twice the cap for yt-dlp, whose merge holds both parts
//!   and the result). SPIKE-9's largest video at 1080p was about 60 MB.
//!
//! # Egress
//!
//! yt-dlp makes its own requests, so the server's outbound client (P2-04)
//! cannot carry them: `--proxy` sends every request through the egress proxy
//! (D17), and `HTTP(S)_PROXY` sends any ffmpeg it starts there too. Without an
//! [`Egress`], yt-dlp does not run. ffmpeg reads local files only:
//! `-protocol_whitelist file` and a demuxer chosen from the sniffed type keep
//! a hostile file from opening anything else.
//!
//! # Privacy
//!
//! Errors carry codes, never a URL or the tools' output (plan §3.7):
//! [`VideoError::code`] is what a job records.
//!
//! # Availability
//!
//! The binaries come from `SHELFY_YTDLP_BIN` and `SHELFY_FFMPEG_BIN`
//! ([`ToolPaths`]; the image's paths by default, L5 and L6).
//! [`VideoTools::probe`] reports whether each one runs: a missing tool turns
//! its features off, it never stops the server.
//!
//! [`IngestLimits::VIDEO`]: crate::store::IngestLimits::VIDEO

pub mod classify;
pub mod ffmpeg;
mod process;
pub mod ytdlp;

use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;
use tokio::sync::{Semaphore, SemaphorePermit};
use tokio_util::sync::CancellationToken;
use url::Url;

pub use classify::{YtdlpFailure, classify};
pub use ffmpeg::{Keyframe, POSTER, ReadyVideo, VideoInfo, index_first};
pub use ytdlp::{DownloadRequest, PostUrl, PostUrlError, Progress, VideoPlatform};

use crate::kind::MediaKind;
use crate::render::RenderError;
use crate::store::IngestLimits;
use process::{Flow, RunError, Spec, Stop};

/// Default of `SHELFY_YTDLP_BIN`: the image's pinned, unpacked build (L6).
pub const DEFAULT_YTDLP_BIN: &str = "/opt/yt-dlp/yt-dlp";
/// Default of `SHELFY_FFMPEG_BIN`: Debian's ffmpeg in the image (L5).
pub const DEFAULT_FFMPEG_BIN: &str = "/usr/bin/ffmpeg";

/// Tool processes at once, yt-dlp and ffmpeg together (§2.3).
pub const MAX_PROCESSES: usize = 2;
/// Wait between SIGTERM and SIGKILL when a run is stopped.
pub const KILL_GRACE: Duration = Duration::from_secs(5);
/// Longest yt-dlp run. The `media.video` job's lease is 15 minutes (P4-16).
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Longest ffmpeg run: a remux of 300 MB is bound by the disk, a frame by
/// one seek and decode.
pub const FFMPEG_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// Longest `--version` run of [`VideoTools::probe`].
const PROBE_TIMEOUT: Duration = Duration::from_secs(15);

/// Name prefix of yt-dlp run directories.
const YTDLP_RUN_PREFIX: &str = ".ytdlp-";
/// Name prefix of ffmpeg run directories.
const FFMPEG_RUN_PREFIX: &str = ".ffmpeg-";

/// The two binaries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolPaths {
    /// yt-dlp (`SHELFY_YTDLP_BIN`).
    pub ytdlp: PathBuf,
    /// ffmpeg (`SHELFY_FFMPEG_BIN`).
    pub ffmpeg: PathBuf,
}

impl Default for ToolPaths {
    /// The image's paths.
    fn default() -> Self {
        Self {
            ytdlp: DEFAULT_YTDLP_BIN.into(),
            ffmpeg: DEFAULT_FFMPEG_BIN.into(),
        }
    }
}

/// Where yt-dlp's requests go (D17).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Egress {
    /// Through the egress proxy (`SHELFY_EGRESS_PROXY`; on the VPS
    /// `http://shelfy-egress:4750`).
    Proxy(ProxyUrl),
    /// Straight out, with no proxy. The server allows it only with a loopback
    /// public URL (`SHELFY_EGRESS_PROXY=direct`, P4-01): local runs and tests.
    Direct,
}

/// The URL of an HTTP proxy: `http(s)://host[:port]`, without credentials
/// (argv is readable by other processes of the same user), a path, a query
/// or a fragment.
#[derive(Clone, PartialEq, Eq)]
pub struct ProxyUrl(String);

impl ProxyUrl {
    /// Checks and normalizes a proxy URL.
    ///
    /// # Errors
    ///
    /// A message naming what is wrong.
    pub fn parse(input: &str) -> Result<Self, String> {
        let url = Url::parse(input.trim()).map_err(|e| format!("not an absolute URL ({e})"))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("the scheme must be http or https".into());
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err("a host is required".into());
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err("credentials are not allowed".into());
        }
        if url.path() != "/" || url.query().is_some() || url.fragment().is_some() {
            return Err("give the origin only, without a path, query or fragment".into());
        }
        Ok(Self(url.origin().ascii_serialization()))
    }

    /// The proxy, for example `http://shelfy-egress:4750`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ProxyUrl {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ProxyUrl({})", self.0)
    }
}

/// The settings of [`VideoTools`].
#[derive(Clone, Debug)]
pub struct VideoToolsConfig {
    /// yt-dlp.
    pub ytdlp: PathBuf,
    /// ffmpeg; yt-dlp merges with it too.
    pub ffmpeg: PathBuf,
    /// Where runs write, one private directory each. Put it on the file
    /// system of the video cache, so a result can be renamed into place.
    pub scratch_dir: PathBuf,
    /// yt-dlp's way out; `None` turns yt-dlp off.
    pub egress: Option<Egress>,
    /// Largest video a run may produce or read (§2.12: 300 MB).
    pub max_video_bytes: u64,
    /// Longest yt-dlp run.
    pub download_timeout: Duration,
    /// Longest ffmpeg run.
    pub ffmpeg_timeout: Duration,
    /// Wait between SIGTERM and SIGKILL.
    pub kill_grace: Duration,
    /// Tool processes at once.
    pub max_processes: usize,
}

impl VideoToolsConfig {
    /// The plan's defaults for `paths`, writing under `scratch_dir`, with
    /// yt-dlp going out through `egress`.
    #[must_use]
    pub fn new(paths: ToolPaths, scratch_dir: impl Into<PathBuf>, egress: Option<Egress>) -> Self {
        Self {
            ytdlp: paths.ytdlp,
            ffmpeg: paths.ffmpeg,
            scratch_dir: scratch_dir.into(),
            egress,
            max_video_bytes: IngestLimits::VIDEO.max_bytes,
            download_timeout: DOWNLOAD_TIMEOUT,
            ffmpeg_timeout: FFMPEG_TIMEOUT,
            kill_grace: KILL_GRACE,
            max_processes: MAX_PROCESSES,
        }
    }
}

/// One of the two tools.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Tool {
    /// yt-dlp.
    Ytdlp,
    /// ffmpeg.
    Ffmpeg,
}

impl Tool {
    /// `ytdlp` or `ffmpeg`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ytdlp => "ytdlp",
            Self::Ffmpeg => "ffmpeg",
        }
    }

    const fn run_prefix(self) -> &'static str {
        match self {
            Self::Ytdlp => YTDLP_RUN_PREFIX,
            Self::Ffmpeg => FFMPEG_RUN_PREFIX,
        }
    }
}

impl fmt::Display for Tool {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why a run failed. No variant holds a URL or the tools' output.
#[derive(Debug, thiserror::Error)]
pub enum VideoError {
    /// yt-dlp failed; the code says why.
    #[error("yt-dlp failed: {0}")]
    Ytdlp(YtdlpFailure),
    /// The input is not a video ffmpeg can read and remux: not an MP4,
    /// QuickTime or WebM file, damaged, without a video stream, or with a
    /// codec MP4 cannot hold.
    #[error("not a usable video")]
    InvalidMedia,
    /// The tool exited well but left no single, usable video.
    #[error("the tool produced no usable video")]
    InvalidOutput,
    /// The video passed the size cap; the run was stopped.
    #[error("the video is larger than {limit} bytes")]
    TooLarge {
        /// The cap.
        limit: u64,
    },
    /// The run passed its time limit and was stopped.
    #[error("the run took too long")]
    TimedOut,
    /// The caller cancelled; the run was stopped.
    #[error("cancelled")]
    Cancelled,
    /// No egress is configured, so yt-dlp does not run.
    #[error("no egress is configured for yt-dlp")]
    NoEgress,
    /// The binary could not be started.
    #[error("{tool} cannot start: {source}")]
    Unavailable {
        /// Which binary.
        tool: Tool,
        /// Why.
        #[source]
        source: io::Error,
    },
    /// Encoding a frame failed.
    #[error("cannot encode the frame: {0}")]
    Render(#[from] RenderError),
    /// A file-system error in the scratch directory.
    #[error(transparent)]
    Io(#[from] io::Error),
}

impl VideoError {
    /// A stable `snake_case` code: yt-dlp's [`YtdlpFailure::code`], or
    /// `invalid_media`, `invalid_output`, `too_large`, `timed_out`,
    /// `cancelled`, `no_egress`, `tool_unavailable`, `render_failed`, `io`.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Ytdlp(failure) => failure.code(),
            Self::InvalidMedia => "invalid_media",
            Self::InvalidOutput => "invalid_output",
            Self::TooLarge { .. } => "too_large",
            Self::TimedOut => "timed_out",
            Self::Cancelled => "cancelled",
            Self::NoEgress => "no_egress",
            Self::Unavailable { .. } => "tool_unavailable",
            Self::Render(_) => "render_failed",
            Self::Io(_) => "io",
        }
    }

    /// Whether another try may succeed: yt-dlp's transient failures, a time
    /// limit and file-system errors.
    #[must_use]
    pub const fn is_transient(&self) -> bool {
        match self {
            Self::Ytdlp(failure) => failure.is_transient(),
            Self::TimedOut | Self::Io(_) => true,
            _ => false,
        }
    }

    fn from_run(tool: Tool, err: RunError, max_bytes: u64) -> Self {
        match err {
            RunError::Spawn(source) => Self::Unavailable { tool, source },
            RunError::Stopped(Stop::Cancelled) => Self::Cancelled,
            RunError::Stopped(Stop::TimedOut) => Self::TimedOut,
            RunError::Stopped(Stop::TooLarge) => Self::TooLarge { limit: max_bytes },
            RunError::Io(err) => Self::Io(err),
        }
    }
}

/// A video a run produced, in the run's own directory. Dropping it removes
/// the file and the directory.
#[derive(Debug)]
pub struct VideoFile {
    dir: TempDir,
    path: PathBuf,
    kind: MediaKind,
    bytes: u64,
}

impl VideoFile {
    /// The file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Its type, sniffed: MP4, QuickTime or WebM.
    #[must_use]
    pub fn kind(&self) -> MediaKind {
        self.kind
    }

    /// Its size in bytes.
    #[must_use]
    pub fn bytes(&self) -> u64 {
        self.bytes
    }

    /// Moves the file to `dest`, on the same file system: fsyncs it, renames
    /// it, fsyncs `dest`'s directory, then removes the run's directory.
    /// Blocking: call it from blocking code.
    ///
    /// # Errors
    ///
    /// The file system refused; the file then stays where it was and goes
    /// with the directory.
    pub fn persist(self, dest: &Path) -> io::Result<()> {
        File::open(&self.path)?.sync_all()?;
        fs::rename(&self.path, dest)?;
        #[cfg(unix)]
        if let Some(parent) = dest.parent() {
            File::open(parent)?.sync_all()?;
        }
        drop(self.dir);
        Ok(())
    }
}

/// Whether a tool runs, from [`VideoTools::probe`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ToolStatus {
    /// It runs; `version` is what it reports (`2026.08.19`, `5.1.6-0+deb12u1`).
    Ready {
        /// The version, at most 64 printable ASCII characters.
        version: String,
    },
    /// No executable file at the configured path.
    Missing,
    /// The file is there but does not run as the tool.
    Broken,
}

impl ToolStatus {
    /// Whether the tool runs.
    #[must_use]
    pub fn is_ready(&self) -> bool {
        matches!(self, Self::Ready { .. })
    }
}

/// What [`VideoTools::probe`] found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Availability {
    /// yt-dlp.
    pub ytdlp: ToolStatus,
    /// ffmpeg.
    pub ffmpeg: ToolStatus,
}

impl Availability {
    /// Whether yt-dlp downloads can run: yt-dlp, and ffmpeg for its merge
    /// and the remux. An egress must be configured too.
    #[must_use]
    pub fn can_download(&self) -> bool {
        self.ytdlp.is_ready() && self.ffmpeg.is_ready()
    }
}

/// The video tools: yt-dlp and ffmpeg behind one pair of slots. Cheap to
/// clone; clones share the slots.
#[derive(Clone)]
pub struct VideoTools {
    inner: Arc<Inner>,
}

struct Inner {
    config: VideoToolsConfig,
    slots: Semaphore,
}

impl VideoTools {
    /// Tools with `config`. Nothing runs or is created until the first call.
    #[must_use]
    pub fn new(config: VideoToolsConfig) -> Self {
        let slots = Semaphore::new(config.max_processes.max(1));
        Self {
            inner: Arc::new(Inner { config, slots }),
        }
    }

    /// The settings.
    #[must_use]
    pub fn config(&self) -> &VideoToolsConfig {
        &self.inner.config
    }

    /// Checks each binary: an executable file at its path that answers
    /// `--version` (yt-dlp) or `-version` (ffmpeg). It takes a tool slot, so
    /// call it at start and keep the answer rather than per request.
    pub async fn probe(&self) -> Availability {
        let cancel = CancellationToken::new();
        let Ok(_slot) = self.slot(&cancel).await else {
            return Availability {
                ytdlp: ToolStatus::Broken,
                ffmpeg: ToolStatus::Broken,
            };
        };
        let config = self.config();
        Availability {
            ytdlp: self
                .probe_one(&config.ytdlp, &["--version"], ytdlp_version)
                .await,
            ffmpeg: self
                .probe_one(
                    &config.ffmpeg,
                    &["-hide_banner", "-version"],
                    ffmpeg_version,
                )
                .await,
        }
    }

    async fn probe_one(
        &self,
        program: &Path,
        args: &[&str],
        version: fn(&str) -> Option<String>,
    ) -> ToolStatus {
        if !is_executable_file(program).await {
            return ToolStatus::Missing;
        }
        let mut first = None;
        let mut on_line = |line: &str| {
            if first.is_none() {
                first = Some(line.to_owned());
            }
            Flow::Continue
        };
        let spec = Spec {
            program,
            args: args.iter().map(OsString::from).collect(),
            cwd: Path::new("/"),
            env: child_env(None, None),
            stdout: true,
            timeout: PROBE_TIMEOUT,
            kill_grace: self.config().kill_grace,
            watch: None,
        };
        match process::run(spec, &CancellationToken::new(), &mut on_line).await {
            Ok(finished) if finished.status.success() => first
                .as_deref()
                .and_then(version)
                .map_or(ToolStatus::Broken, |version| ToolStatus::Ready { version }),
            Err(RunError::Spawn(err)) if err.kind() == io::ErrorKind::NotFound => {
                ToolStatus::Missing
            }
            _ => ToolStatus::Broken,
        }
    }

    /// Waits for a tool slot, or for `cancel`.
    async fn slot(&self, cancel: &CancellationToken) -> Result<SemaphorePermit<'_>, VideoError> {
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(VideoError::Cancelled),
            permit = self.inner.slots.acquire() => permit.map_err(|_| VideoError::Cancelled),
        }
    }

    /// A fresh directory for one run of `tool`, under the scratch directory
    /// (created, mode 0750, if missing).
    async fn run_dir(&self, tool: Tool) -> Result<TempDir, VideoError> {
        let scratch = self.config().scratch_dir.clone();
        blocking(move || {
            create_private_dir(&scratch)?;
            tempfile::Builder::new()
                .prefix(tool.run_prefix())
                .tempdir_in(&scratch)
        })
        .await?
        .map_err(VideoError::Io)
    }
}

impl fmt::Debug for VideoTools {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VideoTools")
            .field("config", &self.inner.config)
            .field("free_slots", &self.inner.slots.available_permits())
            .finish()
    }
}

/// Removes the run directories under `scratch_dir` that a crash or a kill
/// left, once older than `older_than`; returns how many went. Run it at
/// start and hourly (plan PG20). Blocking.
///
/// # Errors
///
/// The directory cannot be listed. A missing directory is empty.
pub fn sweep_scratch(scratch_dir: &Path, older_than: Duration) -> io::Result<usize> {
    let entries = match fs::read_dir(scratch_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err),
    };
    let now = SystemTime::now();
    let mut removed = 0;
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let ours = name.to_str().is_some_and(|name| {
            name.starts_with(YTDLP_RUN_PREFIX) || name.starts_with(FFMPEG_RUN_PREFIX)
        });
        if !ours {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= older_than);
        if !stale {
            continue;
        }
        let gone = if meta.is_dir() {
            fs::remove_dir_all(entry.path())
        } else {
            fs::remove_file(entry.path())
        };
        if gone.is_ok() {
            removed += 1;
        }
    }
    Ok(removed)
}

/// The environment of a child: nothing of the server's, only this
/// allowlist. `run_dir` becomes `HOME` and `TMPDIR` (else `HOME` names no
/// directory); `egress` sets the proxy variables.
fn child_env(run_dir: Option<&Path>, egress: Option<&Egress>) -> Vec<(&'static str, OsString)> {
    let mut env: Vec<(&'static str, OsString)> = vec![
        ("PATH", "/usr/local/bin:/usr/bin:/bin".into()),
        ("LANG", "C.UTF-8".into()),
        // yt-dlp is Python: UTF-8 I/O whatever the locale, no bytecode
        // written next to the read-only build, no user site-packages.
        ("PYTHONUTF8", "1".into()),
        ("PYTHONDONTWRITEBYTECODE", "1".into()),
        ("PYTHONNOUSERSITE", "1".into()),
    ];
    match run_dir {
        Some(dir) => {
            env.push(("HOME", dir.as_os_str().to_owned()));
            env.push(("TMPDIR", dir.as_os_str().to_owned()));
        }
        None => env.push(("HOME", "/nonexistent".into())),
    }
    if let Some(Egress::Proxy(proxy)) = egress {
        for name in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            env.push((name, proxy.as_str().into()));
        }
    }
    env
}

/// The first line of `yt-dlp --version`.
fn ytdlp_version(line: &str) -> Option<String> {
    clean_version(line.trim())
}

/// The version in `ffmpeg version <version> Copyright …`.
fn ffmpeg_version(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("ffmpeg version ")?;
    clean_version(rest.split_whitespace().next()?)
}

/// A version string fit to show: non-empty, at most 64 printable ASCII
/// characters without spaces.
fn clean_version(version: &str) -> Option<String> {
    let ok =
        !version.is_empty() && version.len() <= 64 && version.bytes().all(|b| b.is_ascii_graphic());
    ok.then(|| version.to_owned())
}

/// Whether `path` is an absolute path to an executable regular file.
async fn is_executable_file(path: &Path) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let path = path.to_owned();
    blocking(move || {
        let Ok(meta) = fs::metadata(&path) else {
            return false;
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            meta.is_file() && meta.permissions().mode() & 0o111 != 0
        }
        #[cfg(not(unix))]
        {
            meta.is_file()
        }
    })
    .await
    .unwrap_or(false)
}

/// Opens `path` for reading: a regular file, reached without following a
/// symlink at its last component.
fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        // O_NONBLOCK: opening a FIFO must not wait for a writer.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

/// Creates `path` and its missing parents; new directories get mode 0750
/// (§2.5).
fn create_private_dir(path: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o750);
    }
    builder.create(path)
}

/// Runs blocking file work off the async workers.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, VideoError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| VideoError::Io(io::Error::other(err)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_are_read_and_cleaned() {
        assert_eq!(ytdlp_version("2026.08.19\n").as_deref(), Some("2026.08.19"));
        assert_eq!(
            ffmpeg_version(
                "ffmpeg version 5.1.6-0+deb12u1 Copyright (c) 2000-2024 the FFmpeg developers"
            )
            .as_deref(),
            Some("5.1.6-0+deb12u1")
        );
        assert_eq!(
            ffmpeg_version("ffmpeg version 8.0.1 Copyright (c) 2000-2025 the FFmpeg developers")
                .as_deref(),
            Some("8.0.1")
        );
        assert_eq!(ffmpeg_version("not ffmpeg"), None);
        assert_eq!(ytdlp_version(""), None);
        assert_eq!(ytdlp_version("two words"), None);
        assert_eq!(ytdlp_version(&"9".repeat(65)), None);
        assert_eq!(ytdlp_version("bell\u{7}"), None);
    }

    #[test]
    fn proxy_urls_are_origins_without_credentials() {
        let proxy = ProxyUrl::parse("http://shelfy-egress:4750/").unwrap();
        assert_eq!(proxy.as_str(), "http://shelfy-egress:4750");
        assert_eq!(
            ProxyUrl::parse("https://Proxy.Example:443")
                .unwrap()
                .as_str(),
            "https://proxy.example"
        );
        for bad in [
            "",
            "shelfy-egress:4750",
            "socks5://proxy:1080",
            "http://user:pw@proxy:4750",
            "http://proxy:4750/path",
            "http://proxy:4750/?q=1",
            "file:///tmp/sock",
        ] {
            assert!(ProxyUrl::parse(bad).is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn the_child_environment_is_an_allowlist() {
        let proxy = Egress::Proxy(ProxyUrl::parse("http://shelfy-egress:4750").unwrap());
        let env = child_env(Some(Path::new("/scratch/.ytdlp-1")), Some(&proxy));
        let names: Vec<&str> = env.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            [
                "PATH",
                "LANG",
                "PYTHONUTF8",
                "PYTHONDONTWRITEBYTECODE",
                "PYTHONNOUSERSITE",
                "HOME",
                "TMPDIR",
                "HTTP_PROXY",
                "HTTPS_PROXY",
                "http_proxy",
                "https_proxy"
            ]
        );
        let direct = child_env(None, Some(&Egress::Direct));
        assert!(
            direct
                .iter()
                .all(|(name, _)| !name.ends_with("_PROXY") && !name.ends_with("_proxy"))
        );
        assert!(direct.contains(&("HOME", OsString::from("/nonexistent"))));
    }
}
