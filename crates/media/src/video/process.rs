//! One tool process, contained (see the parent module): an empty environment
//! plus an allowlist, `nice +10`, its own process group, a time limit, a size
//! cap on its directory, and a stop that ends the whole group.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{ExitStatus, Stdio};
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt as _};
use tokio::process::{Child, Command};
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

/// The niceness every child adds to its own (§2.3: `nice -n 10`).
pub(crate) const NICENESS: i32 = 10;
/// How often a run's directory is measured against its cap.
const WATCH_EVERY: Duration = Duration::from_millis(200);
/// Deepest directory level the measure walks.
const WATCH_DEPTH: usize = 3;
/// Longest line kept from a child's output; the rest of the line is dropped.
const MAX_LINE: usize = 4 * 1024;
/// How many of the last stderr lines are kept (yt-dlp's errors come last).
const TAIL_LINES: usize = 64;
/// How much of stderr's start is kept (ffmpeg's stream dump comes first).
const HEAD_BYTES: usize = 64 * 1024;
/// How long the pipes are still read once the child has exited.
const DRAIN: Duration = Duration::from_secs(2);

/// What to run.
pub(crate) struct Spec<'a> {
    pub(crate) program: &'a Path,
    pub(crate) args: Vec<OsString>,
    pub(crate) cwd: &'a Path,
    pub(crate) env: Vec<(&'static str, OsString)>,
    /// Whether stdout is read (line by line, through the callback).
    pub(crate) stdout: bool,
    pub(crate) timeout: Duration,
    pub(crate) kill_grace: Duration,
    /// A directory whose size stops the run past a cap.
    pub(crate) watch: Option<Watch>,
}

/// A directory and its cap.
pub(crate) struct Watch {
    pub(crate) dir: PathBuf,
    pub(crate) max_bytes: u64,
}

/// What the stdout callback asks for.
pub(crate) enum Flow {
    Continue,
    Stop(Stop),
}

/// Why a run was stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    TimedOut,
    TooLarge,
}

/// Why a run did not finish.
#[derive(Debug)]
pub(crate) enum RunError {
    /// The process could not start.
    Spawn(io::Error),
    /// It was stopped; its group is gone.
    Stopped(Stop),
    /// Waiting for it failed.
    Io(io::Error),
}

/// A run that exited by itself.
pub(crate) struct Finished {
    pub(crate) status: ExitStatus,
    pub(crate) stderr: Captured,
}

/// What a run printed on stderr: its start and its last lines.
#[derive(Debug, Default)]
pub(crate) struct Captured {
    head: String,
    tail: VecDeque<String>,
}

impl Captured {
    fn push(&mut self, line: &str) {
        if self.head.len() < HEAD_BYTES {
            let room = HEAD_BYTES - self.head.len();
            let mut end = line.len().min(room.saturating_sub(1));
            while !line.is_char_boundary(end) {
                end -= 1;
            }
            self.head.push_str(&line[..end]);
            self.head.push('\n');
        }
        if self.tail.len() == TAIL_LINES {
            self.tail.pop_front();
        }
        self.tail.push_back(line.to_owned());
    }

    /// The first [`HEAD_BYTES`] of stderr, one line per line.
    pub(crate) fn head(&self) -> &str {
        &self.head
    }

    /// The last [`TAIL_LINES`] lines.
    pub(crate) fn tail(&self) -> impl Iterator<Item = &str> {
        self.tail.iter().map(String::as_str)
    }
}

/// Runs `spec` to its end, or stops it on `cancel`, its time limit, its size
/// cap, or when `on_stdout` asks.
pub(crate) async fn run(
    spec: Spec<'_>,
    cancel: &CancellationToken,
    on_stdout: &mut (dyn FnMut(&str) -> Flow + Send),
) -> Result<Finished, RunError> {
    if cancel.is_cancelled() {
        return Err(RunError::Stopped(Stop::Cancelled));
    }
    let mut command = Command::new(spec.program);
    command
        .args(&spec.args)
        .current_dir(spec.cwd)
        .env_clear()
        .envs(spec.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(if spec.stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    sys::contain(&mut command).map_err(RunError::Spawn)?;
    let mut child = command.spawn().map_err(RunError::Spawn)?;
    let mut group = Group::new(child.id());
    let mut stdout = child.stdout.take().map(Lines::new);
    let mut stderr = child.stderr.take().map(Lines::new);
    let mut captured = Captured::default();

    let deadline = tokio::time::sleep(spec.timeout);
    tokio::pin!(deadline);
    let cancelled = cancel.cancelled();
    tokio::pin!(cancelled);
    let mut ticks = tokio::time::interval(WATCH_EVERY);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);

    let outcome = loop {
        tokio::select! {
            biased;
            () = &mut cancelled => break Err(Stop::Cancelled),
            () = &mut deadline => break Err(Stop::TimedOut),
            _ = ticks.tick(), if spec.watch.is_some() => {
                if over_cap(spec.watch.as_ref()).await {
                    break Err(Stop::TooLarge);
                }
            }
            line = next_line(&mut stdout), if stdout.is_some() => match line {
                Ok(Some(line)) => {
                    if let Flow::Stop(stop) = on_stdout(&line) {
                        break Err(stop);
                    }
                }
                Ok(None) | Err(_) => stdout = None,
            },
            line = next_line(&mut stderr), if stderr.is_some() => match line {
                Ok(Some(line)) => captured.push(&line),
                Ok(None) | Err(_) => stderr = None,
            },
            status = child.wait() => break Ok(status),
        }
    };

    match outcome {
        Err(stop) => {
            // Closing the pipes first lets a child stuck writing to a full
            // one see the stop instead of waiting for SIGKILL.
            drop(stdout.take());
            drop(stderr.take());
            terminate(&mut child, &group, spec.kill_grace).await;
            group.disarm();
            Err(RunError::Stopped(stop))
        }
        Ok(Err(err)) => {
            // Waiting failed: the child is in an unknown state; end it.
            terminate(&mut child, &group, Duration::ZERO).await;
            group.disarm();
            Err(RunError::Io(err))
        }
        Ok(Ok(status)) => {
            // What the child printed just before it exited. A process of the
            // group that still holds the pipes after `DRAIN` is killed.
            let drained = tokio::time::timeout(DRAIN, async {
                while stdout.is_some() || stderr.is_some() {
                    tokio::select! {
                        line = next_line(&mut stdout), if stdout.is_some() => match line {
                            Ok(Some(line)) => {
                                // The run is over: a late stop request changes nothing.
                                let _ = on_stdout(&line);
                            }
                            Ok(None) | Err(_) => stdout = None,
                        },
                        line = next_line(&mut stderr), if stderr.is_some() => match line {
                            Ok(Some(line)) => captured.push(&line),
                            Ok(None) | Err(_) => stderr = None,
                        },
                    }
                }
            })
            .await;
            if drained.is_err() {
                group.signal(sys::Signal::Kill);
            }
            group.disarm();
            Ok(Finished {
                status,
                stderr: captured,
            })
        }
    }
}

/// Stops the group: SIGTERM, then SIGKILL after `grace` if the child is
/// still there, then SIGKILL again for any process of the group that
/// outlived it (ffmpeg under yt-dlp).
async fn terminate(child: &mut Child, group: &Group, grace: Duration) {
    group.signal(sys::Signal::Term);
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        group.signal(sys::Signal::Kill);
        let _ = child.wait().await;
    }
    group.signal(sys::Signal::Kill);
}

/// The process group of a child. Until disarmed, dropping it kills the whole
/// group: a run whose future is dropped leaves no process behind.
struct Group {
    pgid: Option<i32>,
    armed: bool,
}

impl Group {
    fn new(pid: Option<u32>) -> Self {
        // Never 0 or 1: `killpg(0)` would signal the server's own group.
        let pgid = pid
            .and_then(|pid| i32::try_from(pid).ok())
            .filter(|&pid| pid > 1);
        Self { pgid, armed: true }
    }

    fn signal(&self, signal: sys::Signal) {
        if let Some(pgid) = self.pgid {
            sys::signal_group(pgid, signal);
        }
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        if self.armed {
            self.signal(sys::Signal::Kill);
        }
    }
}

/// Whether the watched directory passed its cap.
async fn over_cap(watch: Option<&Watch>) -> bool {
    let Some(watch) = watch else { return false };
    let dir = watch.dir.clone();
    let max_bytes = watch.max_bytes;
    tokio::task::spawn_blocking(move || dir_size(&dir, WATCH_DEPTH) > max_bytes)
        .await
        .unwrap_or(false)
}

/// The bytes of the regular files under `dir`, `depth` levels deep at most;
/// symlinks are not followed.
fn dir_size(dir: &Path, depth: usize) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_file() {
            let size = entry.metadata().map_or(0, |meta| meta.len());
            total = total.saturating_add(size);
        } else if kind.is_dir() && depth > 1 {
            total = total.saturating_add(dir_size(&entry.path(), depth - 1));
        }
    }
    total
}

async fn next_line<R: AsyncRead + Unpin>(
    lines: &mut Option<Lines<R>>,
) -> io::Result<Option<String>> {
    match lines {
        Some(lines) => lines.next_line().await,
        None => std::future::pending().await,
    }
}

/// Lines of a child's output: split at `\n` or `\r`, empty ones skipped,
/// decoded lossily, each at most [`MAX_LINE`] bytes.
struct Lines<R> {
    reader: R,
    buf: Vec<u8>,
    /// Dropping the rest of an over-long line.
    discarding: bool,
    eof: bool,
}

impl<R: AsyncRead + Unpin> Lines<R> {
    fn new(reader: R) -> Self {
        Self {
            reader,
            buf: Vec::new(),
            discarding: false,
            eof: false,
        }
    }

    /// The next line; `None` at the end. Cancel-safe: the only await is a
    /// read, and what it read is kept in `buf` before the next one.
    async fn next_line(&mut self) -> io::Result<Option<String>> {
        loop {
            if let Some(end) = self.buf.iter().position(|&b| b == b'\n' || b == b'\r') {
                let line: Vec<u8> = self.buf.drain(..=end).collect();
                if std::mem::take(&mut self.discarding) {
                    continue;
                }
                let line = &line[..line.len() - 1];
                if line.is_empty() {
                    continue;
                }
                return Ok(Some(String::from_utf8_lossy(line).into_owned()));
            }
            if self.buf.len() >= MAX_LINE {
                if self.discarding {
                    self.buf.clear();
                } else {
                    let line: Vec<u8> = self.buf.drain(..MAX_LINE).collect();
                    self.buf.clear();
                    self.discarding = true;
                    return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
                }
            }
            if self.eof {
                let line = std::mem::take(&mut self.buf);
                if line.is_empty() || std::mem::take(&mut self.discarding) {
                    return Ok(None);
                }
                return Ok(Some(String::from_utf8_lossy(&line).into_owned()));
            }
            let mut chunk = [0u8; 8 * 1024];
            let read = self.reader.read(&mut chunk).await?;
            if read == 0 {
                self.eof = true;
            } else {
                self.buf.extend_from_slice(&chunk[..read]);
            }
        }
    }
}

#[cfg(unix)]
mod sys {
    use super::NICENESS;

    /// The signals a run sends.
    #[derive(Clone, Copy, Debug)]
    pub(super) enum Signal {
        Term,
        Kill,
    }

    /// Puts the child in its own process group (so a stop reaches what it
    /// starts) and lowers its priority by [`NICENESS`].
    #[allow(unsafe_code)]
    pub(super) fn contain(command: &mut tokio::process::Command) -> std::io::Result<()> {
        command.process_group(0);
        // SAFETY: the closure runs in the child between fork and exec. It
        // only calls nice(2), a system call that allocates nothing and takes
        // no lock, and returns a value that owns no memory. A failure (which
        // raising one's own niceness does not have) leaves the priority as
        // it was: the run goes on.
        unsafe {
            command.pre_exec(|| {
                let _ = libc::nice(NICENESS);
                Ok(())
            });
        }
        Ok(())
    }

    /// Sends `signal` to every process of the group `pgid` (> 1). A group
    /// that is already gone is not an error.
    #[allow(unsafe_code)]
    pub(super) fn signal_group(pgid: i32, signal: Signal) {
        let signal = match signal {
            Signal::Term => libc::SIGTERM,
            Signal::Kill => libc::SIGKILL,
        };
        // SAFETY: killpg(2) takes two integers and touches no memory of this
        // process; `pgid` > 1 is a group this process created (`Group::new`).
        let _ = unsafe { libc::killpg(pgid, signal) };
    }
}

#[cfg(not(unix))]
mod sys {
    /// The signals a run sends.
    #[derive(Clone, Copy, Debug)]
    pub(super) enum Signal {
        Term,
        Kill,
    }

    /// The video tools run on unix hosts only: elsewhere nothing starts.
    pub(super) fn contain(_command: &mut tokio::process::Command) -> std::io::Result<()> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the video tools need a unix host",
        ))
    }

    pub(super) fn signal_group(_pgid: i32, _signal: Signal) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn lines_of(input: &[u8]) -> Vec<String> {
        let mut lines = Lines::new(input);
        let mut out = Vec::new();
        while let Some(line) = lines.next_line().await.unwrap() {
            out.push(line);
        }
        out
    }

    #[tokio::test]
    async fn lines_split_on_newlines_and_carriage_returns() {
        assert_eq!(
            lines_of(b"one\ntwo\r\nthree\rfour").await,
            ["one", "two", "three", "four"]
        );
        assert_eq!(lines_of(b"\n\n\r\n").await, Vec::<String>::new());
        assert_eq!(lines_of(b"caf\xc3\xa9 \xff\n").await, ["café \u{fffd}"]);
    }

    #[tokio::test]
    async fn an_over_long_line_is_cut_and_its_rest_dropped() {
        let mut input = vec![b'a'; MAX_LINE * 3];
        input.extend_from_slice(b"\nnext\n");
        let lines = lines_of(&input).await;
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].len(), MAX_LINE);
        assert_eq!(lines[1], "next");
        // An over-long last line without a newline.
        let lines = lines_of(&vec![b'b'; MAX_LINE + 10]).await;
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), MAX_LINE);
    }

    #[test]
    fn stderr_keeps_its_head_and_tail() {
        let mut captured = Captured::default();
        for n in 0..TAIL_LINES + 10 {
            captured.push(&format!("line {n}"));
        }
        assert!(captured.head().starts_with("line 0\nline 1\n"));
        let tail: Vec<&str> = captured.tail().collect();
        assert_eq!(tail.len(), TAIL_LINES);
        assert_eq!(tail[0], "line 10");
        let mut big = Captured::default();
        big.push(&"é".repeat(HEAD_BYTES));
        big.push("after");
        assert!(big.head().len() <= HEAD_BYTES);
        assert!(!big.head().contains("after"));
    }

    #[test]
    fn group_ids_never_name_this_process_group() {
        assert_eq!(Group::new(None).pgid, None);
        assert_eq!(Group::new(Some(0)).pgid, None);
        assert_eq!(Group::new(Some(1)).pgid, None);
        assert_eq!(Group::new(Some(u32::MAX)).pgid, None);
        let mut group = Group::new(Some(4242));
        assert_eq!(group.pgid, Some(4242));
        group.disarm(); // dropping it must not signal anything
    }

    #[test]
    fn the_size_of_a_directory_skips_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a"), [0u8; 100]).unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub").join("b"), [0u8; 50]).unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        fs::write(outside.path(), vec![0u8; 10_000]).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        assert_eq!(dir_size(dir.path(), WATCH_DEPTH), 150);
        assert_eq!(dir_size(dir.path(), 1), 100);
        assert_eq!(dir_size(&dir.path().join("missing"), WATCH_DEPTH), 0);
    }
}
