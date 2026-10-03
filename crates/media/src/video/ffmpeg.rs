//! ffmpeg (plan §2.13): remux a video for the browser, read its duration and
//! size, and extract a poster or keyframes. ffmpeg never transcodes here.
//!
//! # Every run
//!
//! ```text
//! ffmpeg -hide_banner -nostdin -nostats -loglevel <level> -filter_threads 1
//!        -threads 1 -protocol_whitelist file [-ss <t>] -f <demuxer> -i file:<input>
//!        <operation> -threads 1 … -y file:<output>
//! ```
//!
//! - The input is sniffed first ([`MediaKind::sniff`]): MP4 and QuickTime
//!   open with the `mov` demuxer, WebM with `matroska`, anything else is
//!   [`VideoError::InvalidMedia`] before ffmpeg starts. A forced demuxer and
//!   `-protocol_whitelist file` keep a hostile file (an HLS playlist or a
//!   concat script dressed as a video) from opening other files or the
//!   network. The input must be a regular file (no symlink) of at most
//!   [`VideoToolsConfig::max_video_bytes`].
//! - `-threads 1` for the decoder and the encoder, `-filter_threads 1`
//!   (§2.3: the host's CPUs are shared).
//!
//! # Operations
//!
//! - **Remux** ([`VideoTools::remux`]): `-map 0:v:0 -map 0:a:0? -c copy
//!   -map_metadata -1 -map_chapters -1 -movflags +faststart -f mp4`. The
//!   first video and audio streams are copied into an MP4 whose index comes
//!   first, so the browser can play it while it loads. Metadata and chapters
//!   are dropped: a phone video's location does not travel, and the result's
//!   own report cannot be forged by its tags (`-map_metadata -1` drops the
//!   global, stream and chapter tags alike). The rotation is kept: it is side
//!   data, not a tag.
//! - **Ready** ([`VideoTools::ready`]): a downloaded video becomes the stored
//!   form, an MP4 with its index first, remuxed only when it is not one
//!   already. SPIKE-9 found `moov` before `mdat` in all 178 files it fetched
//!   from the three platforms, so the remux is the exception ([`index_first`]
//!   reads the order without ffmpeg).
//! - **Inspect** ([`VideoTools::inspect`]): `-map 0:v:0 -c copy -t 0 -f null
//!   -`, and the stream dump ffmpeg prints is read: the duration, the first
//!   video stream's size (with its sample aspect ratio and rotation applied)
//!   and codecs. The dump is free text: on a file that was not remuxed here, a
//!   crafted metadata key could fake these values, so they are informational.
//!   A remux inspects its own, metadata-free result.
//! - **Frame** ([`VideoTools::poster`], [`VideoTools::keyframes`]): an input
//!   seek, one frame, scaled down to fit the requested side (never up, square
//!   pixels, rotation applied), written as PNG; the image pool encodes the
//!   WebP. ffmpeg needs no WebP encoder, which Homebrew's build lacks.

use std::ffi::OsString;
use std::fs;
use std::io::{self, Read as _, Seek as _, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use sha2::{Digest as _, Sha256};
use tokio_util::sync::CancellationToken;

use super::process::{self, Finished, Flow, Spec, Watch};
use super::{Tool, VideoError, VideoFile, VideoTools, blocking, child_env, open_regular};
use crate::digest::Digest;
use crate::kind::{MediaKind, SNIFF_LEN};
use crate::pool::ImagePool;
use crate::render::{RenderSpec, Rendered};

/// The poster frame (§2.13 "video poster"): WebP q78, at most 1080 px.
pub const POSTER: RenderSpec = RenderSpec {
    max_side: 1080,
    quality: 78.0,
};

/// Most keyframes one call extracts (P3 asks for 4).
pub const MAX_KEYFRAMES: usize = 16;

/// The poster is taken 10 % into the video, at most this far.
const POSTER_AT_MAX_MS: u64 = 1_000;

/// Largest frame file ffmpeg may write: a PNG of at most 16,384 px a side
/// is far smaller once scaled.
const MAX_FRAME_BYTES: u64 = 64 * 1024 * 1024;

/// What ffmpeg reports about a video.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VideoInfo {
    /// The duration, in milliseconds; `None` when ffmpeg does not know it.
    pub duration_ms: Option<u64>,
    /// The displayed width: after the sample aspect ratio and the rotation.
    pub width: Option<u32>,
    /// The displayed height.
    pub height: Option<u32>,
    /// The first video stream's codec (`h264`, `hevc`, `vp9`…).
    pub video_codec: Option<String>,
    /// The first audio stream's codec, if there is one.
    pub audio_codec: Option<String>,
}

/// A video ready to cache or keep: an MP4 with its index first.
#[derive(Debug)]
pub struct ReadyVideo {
    /// The MP4, in its run's directory.
    pub file: VideoFile,
    /// Its SHA-256: the name of the cached or stored object.
    pub digest: Digest,
    /// Its duration and size.
    pub info: VideoInfo,
    /// Whether ffmpeg remuxed it; `false` when it came ready.
    pub remuxed: bool,
}

/// One extracted frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Keyframe {
    /// Where it was taken, in milliseconds from the start.
    pub at_ms: u64,
    /// The frame, encoded.
    pub image: Rendered,
}

/// A checked input.
struct Input {
    path: PathBuf,
    demuxer: &'static str,
}

impl VideoTools {
    /// Remuxes the video at `input` into an MP4 with its index first
    /// (`-c copy -movflags +faststart`), without metadata. The result lives
    /// in its own run directory until it is persisted or dropped.
    ///
    /// # Errors
    ///
    /// - [`VideoError::InvalidMedia`]: not a video ffmpeg can copy into MP4;
    /// - [`VideoError::TooLarge`]: the input or the output passes the cap;
    /// - [`VideoError::TimedOut`], [`VideoError::Cancelled`],
    ///   [`VideoError::Unavailable`], [`VideoError::Io`].
    pub async fn remux(
        &self,
        input: &Path,
        cancel: &CancellationToken,
    ) -> Result<ReadyVideo, VideoError> {
        let max_bytes = self.config().max_video_bytes;
        let input = self.check(input).await?;
        let _slot = self.slot(cancel).await?;
        let dir = self.run_dir(Tool::Ffmpeg).await?;
        let output = dir.path().join("video.mp4");

        let mut args = head_args("error", &input, None);
        args.extend(
            [
                "-map",
                "0:v:0",
                "-map",
                "0:a:0?",
                "-c",
                "copy",
                "-map_metadata",
                "-1",
                "-map_chapters",
                "-1",
                "-movflags",
                "+faststart",
                "-threads",
                "1",
                "-f",
                "mp4",
                "-y",
            ]
            .map(OsString::from),
        );
        args.push(file_arg(&output));
        let finished = self.ffmpeg(args, dir.path(), max_bytes, cancel).await?;
        if !finished.status.success() {
            return Err(VideoError::InvalidMedia);
        }
        let hashed = output.clone();
        let (digest, bytes) = blocking(move || hash_mp4(&hashed, max_bytes)).await??;
        // The report of the result, which has no metadata left to forge it.
        let clean = Input {
            path: output.clone(),
            demuxer: "mov",
        };
        let info = self.inspect_input(&clean, dir.path(), cancel).await?;
        Ok(ReadyVideo {
            file: VideoFile {
                dir,
                path: output,
                kind: MediaKind::Mp4,
                bytes,
            },
            digest,
            info,
            remuxed: true,
        })
    }

    /// Makes a downloaded video ([`VideoTools::download`]) ready to cache or
    /// keep: an MP4 whose index comes first is only hashed and inspected,
    /// anything else (`mdat` first, QuickTime, WebM) is remuxed
    /// ([`VideoTools::remux`]) and the download dropped.
    ///
    /// # Errors
    ///
    /// As [`VideoTools::remux`].
    pub async fn ready(
        &self,
        file: VideoFile,
        cancel: &CancellationToken,
    ) -> Result<ReadyVideo, VideoError> {
        let path = file.path.clone();
        let index_first = file.kind == MediaKind::Mp4
            && blocking(move || index_first(&path)).await?? == Some(true);
        if !index_first {
            return self.remux(&file.path, cancel).await;
        }
        let max_bytes = self.config().max_video_bytes;
        let hashed = file.path.clone();
        let (digest, bytes) = blocking(move || hash_mp4(&hashed, max_bytes)).await??;
        let input = Input {
            path: file.path.clone(),
            demuxer: "mov",
        };
        let _slot = self.slot(cancel).await?;
        let info = self.inspect_input(&input, file.dir.path(), cancel).await?;
        Ok(ReadyVideo {
            file: VideoFile { bytes, ..file },
            digest,
            info,
            remuxed: false,
        })
    }

    /// Reads the duration, displayed size and codecs of the video at
    /// `input` (see the module docs on how far to trust them).
    ///
    /// # Errors
    ///
    /// [`VideoError::InvalidMedia`] when ffmpeg cannot open it or it has no
    /// video stream; [`VideoError::TooLarge`], [`VideoError::TimedOut`],
    /// [`VideoError::Cancelled`], [`VideoError::Unavailable`],
    /// [`VideoError::Io`].
    pub async fn inspect(
        &self,
        input: &Path,
        cancel: &CancellationToken,
    ) -> Result<VideoInfo, VideoError> {
        let input = self.check(input).await?;
        let _slot = self.slot(cancel).await?;
        let dir = self.run_dir(Tool::Ffmpeg).await?;
        self.inspect_input(&input, dir.path(), cancel).await
    }

    /// The poster frame of the video at `input`: [`POSTER`] (WebP q78, at
    /// most 1080 px) with its ThumbHash, taken 10 % in (at most 1 s), or
    /// from the start when that time has no frame.
    ///
    /// # Errors
    ///
    /// As [`VideoTools::inspect`], and [`VideoError::Render`].
    pub async fn poster(
        &self,
        input: &Path,
        cancel: &CancellationToken,
    ) -> Result<Rendered, VideoError> {
        let input = self.check(input).await?;
        let _slot = self.slot(cancel).await?;
        let dir = self.run_dir(Tool::Ffmpeg).await?;
        let info = self.inspect_input(&input, dir.path(), cancel).await?;
        let at = info
            .duration_ms
            .map_or(0, |duration| (duration / 10).min(POSTER_AT_MAX_MS));
        if let Some(image) = self.frame(&input, dir.path(), at, POSTER, cancel).await? {
            return Ok(image);
        }
        if at > 0
            && let Some(image) = self.frame(&input, dir.path(), 0, POSTER, cancel).await?
        {
            return Ok(image);
        }
        Err(VideoError::InvalidMedia)
    }

    /// `count` frames (1 to [`MAX_KEYFRAMES`]) spread evenly over the video
    /// at `input`, at the middle of each of `count` equal spans, as the
    /// desktop analyzer takes them; one frame at the start when the
    /// duration is unknown. Each is encoded with `spec`. A time without a
    /// frame is skipped.
    ///
    /// # Errors
    ///
    /// As [`VideoTools::poster`]; [`VideoError::InvalidMedia`] when no frame
    /// came out.
    pub async fn keyframes(
        &self,
        input: &Path,
        count: usize,
        spec: RenderSpec,
        cancel: &CancellationToken,
    ) -> Result<Vec<Keyframe>, VideoError> {
        let count = count.clamp(1, MAX_KEYFRAMES);
        let input = self.check(input).await?;
        let _slot = self.slot(cancel).await?;
        let dir = self.run_dir(Tool::Ffmpeg).await?;
        let info = self.inspect_input(&input, dir.path(), cancel).await?;
        let mut frames = Vec::with_capacity(count);
        for at in keyframe_times(info.duration_ms, count) {
            if let Some(image) = self.frame(&input, dir.path(), at, spec, cancel).await? {
                frames.push(Keyframe { at_ms: at, image });
            }
        }
        if frames.is_empty() {
            return Err(VideoError::InvalidMedia);
        }
        Ok(frames)
    }

    /// Checks `input` (see the module docs) off the async workers.
    async fn check(&self, input: &Path) -> Result<Input, VideoError> {
        let path = input.to_owned();
        let max_bytes = self.config().max_video_bytes;
        blocking(move || check_input(path, max_bytes)).await?
    }

    /// Inspects a checked input; the caller holds a slot.
    async fn inspect_input(
        &self,
        input: &Input,
        dir: &Path,
        cancel: &CancellationToken,
    ) -> Result<VideoInfo, VideoError> {
        let mut args = head_args("info", input, None);
        args.extend(
            ["-map", "0:v:0", "-c", "copy", "-t", "0", "-f", "null", "-"].map(OsString::from),
        );
        let finished = self.ffmpeg(args, dir, 0, cancel).await?;
        if !finished.status.success() {
            return Err(VideoError::InvalidMedia);
        }
        Ok(parse_banner(finished.stderr.head()))
    }

    /// One frame at `at_ms`, encoded with `spec`; `None` when that time has
    /// no frame. The caller holds a slot.
    async fn frame(
        &self,
        input: &Input,
        dir: &Path,
        at_ms: u64,
        spec: RenderSpec,
        cancel: &CancellationToken,
    ) -> Result<Option<Rendered>, VideoError> {
        let output = dir.join(format!("frame-{at_ms}.png"));
        let mut args = head_args("error", input, Some(at_ms));
        args.extend(
            [
                "-map",
                "0:v:0",
                "-frames:v",
                "1",
                "-an",
                "-sn",
                "-dn",
                "-vf",
            ]
            .map(OsString::from),
        );
        args.push(fit_filter(spec.max_side).into());
        args.extend(
            [
                "-threads", "1", "-c:v", "png", "-f", "image2", "-update", "1", "-y",
            ]
            .map(OsString::from),
        );
        args.push(file_arg(&output));
        let finished = self.ffmpeg(args, dir, MAX_FRAME_BYTES, cancel).await?;
        if !finished.status.success() {
            return Err(VideoError::InvalidMedia);
        }
        let written = output.clone();
        let exists = blocking(move || {
            fs::symlink_metadata(&written).is_ok_and(|meta| meta.is_file() && meta.len() > 0)
        })
        .await?;
        if !exists {
            return Ok(None);
        }
        let image = ImagePool::shared().render_file(output.clone(), spec).await;
        // The directory goes at the end anyway; keep it small meanwhile.
        let _ = blocking(move || fs::remove_file(&output)).await;
        Ok(Some(image?))
    }

    /// Runs ffmpeg in `dir`, stopped past `max_bytes` in `dir` (0: no
    /// output, not watched).
    async fn ffmpeg(
        &self,
        args: Vec<OsString>,
        dir: &Path,
        max_bytes: u64,
        cancel: &CancellationToken,
    ) -> Result<Finished, VideoError> {
        let config = self.config();
        let spec = Spec {
            program: &config.ffmpeg,
            args,
            cwd: dir,
            env: child_env(Some(dir), None),
            stdout: false,
            timeout: config.ffmpeg_timeout,
            kill_grace: config.kill_grace,
            watch: (max_bytes > 0).then(|| Watch {
                dir: dir.to_owned(),
                max_bytes,
            }),
        };
        process::run(spec, cancel, &mut |_| Flow::Continue)
            .await
            .map_err(|err| VideoError::from_run(Tool::Ffmpeg, err, max_bytes))
    }
}

/// The arguments up to and including the input.
fn head_args(loglevel: &str, input: &Input, seek_ms: Option<u64>) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "-hide_banner",
        "-nostdin",
        "-nostats",
        "-loglevel",
        loglevel,
        "-filter_threads",
        "1",
        "-threads",
        "1",
        "-protocol_whitelist",
        "file",
    ]
    .map(OsString::from)
    .into();
    if let Some(ms) = seek_ms {
        args.push("-ss".into());
        args.push(format!("{}.{:03}", ms / 1000, ms % 1000).into());
    }
    args.push("-f".into());
    args.push(input.demuxer.into());
    args.push("-i".into());
    args.push(file_arg(&input.path));
    args
}

/// `file:<path>`: the file protocol, named, so no part of a path can select
/// another protocol.
fn file_arg(path: &Path) -> OsString {
    let mut arg = OsString::from("file:");
    arg.push(path);
    arg
}

/// Square pixels, then fit inside `max_side` × `max_side`, never up.
fn fit_filter(max_side: u32) -> String {
    format!(
        "scale=w='trunc(iw*sar)':h=ih,setsar=1,\
         scale=w='min(iw,{max_side})':h='min(ih,{max_side})':force_original_aspect_ratio=decrease"
    )
}

/// The times of `count` keyframes: the middle of each of `count` equal
/// spans; the start alone when the duration is unknown.
fn keyframe_times(duration_ms: Option<u64>, count: usize) -> Vec<u64> {
    match duration_ms {
        Some(duration) if duration > 0 => {
            let count = count as u64;
            (0..count)
                .map(|i| duration.saturating_mul(2 * i + 1) / (2 * count))
                .collect()
        }
        _ => vec![0],
    }
}

/// Checks an input: an absolute path to a regular file (no symlink), not
/// empty, at most `max_bytes`, sniffed as MP4, QuickTime or WebM.
fn check_input(path: PathBuf, max_bytes: u64) -> Result<Input, VideoError> {
    if !path.is_absolute() {
        return Err(VideoError::InvalidMedia);
    }
    let meta = fs::symlink_metadata(&path)?;
    if !meta.is_file() {
        return Err(VideoError::InvalidMedia);
    }
    let file = open_regular(&path)?;
    let bytes = file.metadata()?.len();
    if bytes == 0 {
        return Err(VideoError::InvalidMedia);
    }
    if bytes > max_bytes {
        return Err(VideoError::TooLarge { limit: max_bytes });
    }
    let mut head = Vec::with_capacity(SNIFF_LEN);
    file.take(SNIFF_LEN as u64).read_to_end(&mut head)?;
    let demuxer = match MediaKind::sniff(&head) {
        Some(MediaKind::Mp4 | MediaKind::Mov) => "mov",
        Some(MediaKind::Webm) => "matroska",
        _ => return Err(VideoError::InvalidMedia),
    };
    Ok(Input { path, demuxer })
}

/// Hashes a remux's output, checking it is a non-empty MP4 within the cap.
fn hash_mp4(path: &Path, max_bytes: u64) -> Result<(Digest, u64), VideoError> {
    let mut file = open_regular(path)?;
    let bytes = file.metadata()?.len();
    if bytes == 0 {
        return Err(VideoError::InvalidOutput);
    }
    if bytes > max_bytes {
        return Err(VideoError::TooLarge { limit: max_bytes });
    }
    let mut hasher = Sha256::new();
    let mut head = Vec::with_capacity(SNIFF_LEN);
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        if head.len() < SNIFF_LEN {
            let take = read.min(SNIFF_LEN - head.len());
            head.extend_from_slice(&buf[..take]);
        }
        hasher.update(&buf[..read]);
    }
    if MediaKind::sniff(&head) != Some(MediaKind::Mp4) {
        return Err(VideoError::InvalidOutput);
    }
    Ok((Digest::from_hasher(hasher), bytes))
}

/// Whether the MP4 or QuickTime file at `path` has its index (`moov`)
/// before its samples (`mdat`), from the top-level box headers alone:
/// `Some(true)` streams as is, `Some(false)` needs a remux, `None` is not an
/// ISO media file or has neither box. Blocking.
///
/// # Errors
///
/// The file cannot be read.
pub fn index_first(path: &Path) -> io::Result<Option<bool>> {
    let mut file = open_regular(path)?;
    let len = file.metadata()?.len();
    let mut at = 0u64;
    // A real file has a handful of top-level boxes.
    for _ in 0..1024 {
        if at.saturating_add(8) > len {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(at))?;
        let mut header = [0u8; 16];
        file.read_exact(&mut header[..8])?;
        let size = u64::from(u32::from_be_bytes([
            header[0], header[1], header[2], header[3],
        ]));
        let size = match size {
            // The box runs to the end of the file.
            0 => len - at,
            // A 64-bit size follows the type.
            1 => {
                file.read_exact(&mut header[8..])?;
                u64::from_be_bytes([
                    header[8], header[9], header[10], header[11], header[12], header[13],
                    header[14], header[15],
                ])
            }
            size => size,
        };
        match &header[4..8] {
            b"moov" => return Ok(Some(true)),
            b"mdat" => return Ok(Some(false)),
            _ => {}
        }
        if size < 8 {
            return Ok(None);
        }
        at = at.saturating_add(size);
    }
    Ok(None)
}

/// `  Duration: HH:MM:SS.ss`.
static DURATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+Duration: (\d+):(\d{2}):(\d{2})(?:\.(\d+))?").expect("valid pattern")
});

/// `  Stream #0:N…: Video: codec …` (the `…` after `#0:N` is `[0x1](und)` and
/// the like).
static STREAM: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s+Stream #0:\d+\S*: (\w+): (\w+)").expect("valid pattern"));

/// `, 1080x1920` followed by a space, a comma, `[` or the end.
static SIZE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r", (\d{1,5})x(\d{1,5})(?:[ ,\[]|$)").expect("valid pattern"));

/// `SAR 4:3 DAR 16:9`; the last one on the line is the container's.
static SAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"SAR (\d{1,6}):(\d{1,6}) DAR").expect("valid pattern"));

/// `displaymatrix: rotation of -90.00 degrees` (side data), or an older
/// `rotate : 90` tag.
static ROTATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\s+(?:displaymatrix: rotation of (-?\d+(?:\.\d+)?) degrees|rotate\s*: (-?\d+))")
        .expect("valid pattern")
});

/// Reads what ffmpeg prints about its first input (`Input #0, … Stream
/// mapping:`): the duration, the first video stream's displayed size and
/// codec, the first audio stream's codec. Missing values stay `None`.
#[must_use]
pub fn parse_banner(stderr: &str) -> VideoInfo {
    let mut info = VideoInfo::default();
    let mut in_input = false;
    let mut in_video = false;
    let mut stored: Option<(u32, u32)> = None;
    let mut sar: Option<(u32, u32)> = None;
    let mut rotation: Option<f64> = None;
    for line in stderr.lines() {
        if !in_input {
            in_input = line.starts_with("Input #0");
            continue;
        }
        if line.starts_with("Output #")
            || line.starts_with("Stream mapping:")
            || line.starts_with("Input #")
        {
            break;
        }
        if info.duration_ms.is_none()
            && let Some(caps) = DURATION.captures(line)
        {
            info.duration_ms = duration_ms(&caps);
            continue;
        }
        if let Some(caps) = STREAM.captures(line) {
            in_video = false;
            let codec = codec_name(&caps[2]);
            match &caps[1] {
                "Video" if info.video_codec.is_none() => {
                    in_video = true;
                    info.video_codec = codec;
                    stored = SIZE
                        .captures(line)
                        .and_then(|size| Some((size[1].parse().ok()?, size[2].parse().ok()?)));
                    sar = SAR
                        .captures_iter(line)
                        .last()
                        .and_then(|ratio| Some((ratio[1].parse().ok()?, ratio[2].parse().ok()?)));
                }
                "Audio" if info.audio_codec.is_none() => info.audio_codec = codec,
                _ => {}
            }
            continue;
        }
        if in_video
            && rotation.is_none()
            && let Some(caps) = ROTATION.captures(line)
        {
            rotation = caps
                .get(1)
                .or_else(|| caps.get(2))
                .and_then(|value| value.as_str().parse().ok());
        }
    }
    if let Some((width, height)) = stored.filter(|&(w, h)| w > 0 && h > 0) {
        let width = match sar {
            Some((num, den)) if num > 0 && den > 0 && num != den => {
                let scaled = u64::from(width) * u64::from(num) / u64::from(den);
                u32::try_from(scaled).unwrap_or(width)
            }
            _ => width,
        };
        let quarter_turn = rotation.is_some_and(|degrees| {
            let turns = (degrees / 90.0).round().rem_euclid(4.0);
            turns == 1.0 || turns == 3.0
        });
        let (width, height) = if quarter_turn {
            (height, width)
        } else {
            (width, height)
        };
        info.width = Some(width);
        info.height = Some(height);
    }
    info
}

/// Milliseconds from the captures of [`DURATION`]; the fraction is
/// hundredths in ffmpeg's dump, but any number of digits works.
fn duration_ms(caps: &regex::Captures<'_>) -> Option<u64> {
    let hours: u64 = caps[1].parse().ok()?;
    let minutes: u64 = caps[2].parse().ok()?;
    let seconds: u64 = caps[3].parse().ok()?;
    let millis = caps.get(4).map_or(Some(0), |fraction| {
        let digits: String = fraction
            .as_str()
            .chars()
            .chain("000".chars())
            .take(3)
            .collect();
        digits.parse::<u64>().ok()
    })?;
    hours
        .checked_mul(3_600_000)?
        .checked_add(minutes * 60_000)?
        .checked_add(seconds * 1000)?
        .checked_add(millis)
}

/// A codec name fit to store: lowercase ASCII word characters, at most 32.
fn codec_name(name: &str) -> Option<String> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
    ok.then(|| name.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ffmpeg 5.1.9 in the image (`v0.1.0-rc.3`), verbatim but the path, on a
    /// synthetic file with a title tag.
    const IMAGE_519: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:/work/out/plain.mp4':
  Metadata:
    major_brand     : isom
    minor_version   : 512
    compatible_brands: isomiso2mp41
    title           : private title
    encoder         : Lavf59.27.100
  Duration: 00:00:02.00, start: 0.000000, bitrate: 187 kb/s
  Stream #0:0[0x1](und): Video: mpeg4 (Simple Profile) (mp4v / 0x7634706D), yuv420p, 320x240 [SAR 1:1 DAR 4:3], 150 kb/s, 10 fps, 10 tbr, 10240 tbn (default)
    Metadata:
      handler_name    : VideoHandler
      vendor_id       : [0][0][0][0]
      encoder         : Lavc59.37.100 mpeg4
  Stream #0:1[0x2](und): Audio: aac (LC) (mp4a / 0x6134706D), 8000 Hz, mono, fltp, 26 kb/s (default)
    Metadata:
      handler_name    : SoundHandler
      vendor_id       : [0][0][0][0]
Output #0, null, to 'pipe:':
  Metadata:
    major_brand     : isom
  Stream #0:0(und): Video: mpeg4 (Simple Profile) (mp4v / 0x7634706D), yuv420p, 320x240 [SAR 1:1 DAR 4:3], q=2-31, 150 kb/s, 10 fps, 10 tbr, 10240 tbn (default)
Stream mapping:
  Stream #0:0 -> #0:0 (copy)
";

    /// The same ffmpeg's line for a typical X video (H.264 with colour tags).
    const DEBIAN_51: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:/data/shelfy/work/video/.ffmpeg-x/video.mp4':
  Metadata:
    major_brand     : isom
    minor_version   : 512
    compatible_brands: isomiso2avc1mp41
    encoder         : Lavf59.27.100
  Duration: 00:00:31.53, start: 0.000000, bitrate: 2178 kb/s
  Stream #0:0(und): Video: h264 (High) (avc1 / 0x31637661), yuv420p(tv, bt709, progressive), 720x1280 [SAR 1:1 DAR 9:16], 2043 kb/s, 30 fps, 30 tbr, 15360 tbn (default)
    Metadata:
      handler_name    : VideoHandler
      vendor_id       : [0][0][0][0]
  Stream #0:1(und): Audio: aac (LC) (mp4a / 0x6134706D), 44100 Hz, stereo, fltp, 128 kb/s (default)
    Metadata:
      handler_name    : SoundHandler
      vendor_id       : [0][0][0][0]
Stream mapping:
  Stream #0:0 -> #0:0 (copy)
Output #0, null, to 'pipe:':
  Stream #0:0(und): Video: h264 (High) (avc1 / 0x31637661), yuv420p, 9999x9999
";

    /// ffmpeg 8.0 (Homebrew), a rotated anamorphic file without audio.
    const HOMEBREW_80: &str = "\
Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'file:/tmp/x/video.mp4':
  Metadata:
    major_brand     : isom
    comment         : line1
                    :   Duration: 99:00:00.00
  Duration: 00:00:02.00, start: 0.000000, bitrate: 190 kb/s
  Stream #0:0[0x1](und): Video: mpeg4 (Simple Profile) (mp4v / 0x7634706D), yuv420p, 320x240 [SAR 1:1 DAR 4:3], 154 kb/s, SAR 4:3 DAR 16:9, 10 fps, 10 tbr, 10240 tbn (default)
    Metadata:
      handler_name    : VideoHandler
    Side data:
      displaymatrix: rotation of -90.00 degrees
Stream mapping:
";

    #[test]
    fn the_banner_of_debian_ffmpeg_reads() {
        assert_eq!(
            parse_banner(IMAGE_519),
            VideoInfo {
                duration_ms: Some(2_000),
                width: Some(320),
                height: Some(240),
                video_codec: Some("mpeg4".into()),
                audio_codec: Some("aac".into()),
            }
        );
        assert_eq!(
            parse_banner(DEBIAN_51),
            VideoInfo {
                duration_ms: Some(31_530),
                width: Some(720),
                height: Some(1280),
                video_codec: Some("h264".into()),
                audio_codec: Some("aac".into()),
            }
        );
    }

    #[test]
    fn rotation_and_sample_aspect_ratio_give_the_displayed_size() {
        // 320x240 with SAR 4:3 is 426x240 on screen; turned a quarter, 240x426.
        assert_eq!(
            parse_banner(HOMEBREW_80),
            VideoInfo {
                duration_ms: Some(2_000),
                width: Some(240),
                height: Some(426),
                video_codec: Some("mpeg4".into()),
                audio_codec: None,
            }
        );
    }

    #[test]
    fn a_banner_without_values_gives_none() {
        let unknown =
            "Input #0, mov,mp4,m4a,3gp,3g2,mj2, from 'x':\n  Duration: N/A, bitrate: N/A\n";
        assert_eq!(parse_banner(unknown), VideoInfo::default());
        assert_eq!(parse_banner(""), VideoInfo::default());
        // Lines before `Input #0` (warnings) do not count.
        let early = "  Duration: 00:01:00.00\nInput #0, mov, from 'x':\n";
        assert_eq!(parse_banner(early).duration_ms, None);
    }

    #[test]
    fn durations_read_any_fraction() {
        let banner = |duration: &str| {
            parse_banner(&format!(
                "Input #0, mov, from 'x':\n  Duration: {duration}, start: 0\n"
            ))
            .duration_ms
        };
        assert_eq!(banner("01:02:03.45"), Some(3_723_450));
        assert_eq!(banner("00:00:01.5"), Some(1_500));
        assert_eq!(banner("00:00:01.123456"), Some(1_123));
        assert_eq!(banner("00:00:07"), Some(7_000));
    }

    #[test]
    fn keyframes_sit_in_the_middle_of_equal_spans() {
        assert_eq!(keyframe_times(Some(8_000), 4), [1_000, 3_000, 5_000, 7_000]);
        assert_eq!(keyframe_times(Some(1_000), 1), [500]);
        assert_eq!(keyframe_times(None, 4), [0]);
        assert_eq!(keyframe_times(Some(0), 4), [0]);
    }

    #[test]
    fn codec_names_are_plain_words() {
        assert_eq!(codec_name("H264").as_deref(), Some("h264"));
        assert_eq!(codec_name(&"x".repeat(33)), None);
        assert_eq!(codec_name(""), None);
    }
}
