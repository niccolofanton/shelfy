//! The image pool (plan §2.3): a dedicated rayon pool of 2 threads for image
//! decode, resize and encode, so CPU-heavy work never runs on tokio's async
//! workers and at most two images are processed at once in the process.
//!
//! Work is queued without bound: callers cap how many jobs they submit (the
//! archive worker runs "2 encodes", §2.12). A panicking job (a decoder bug on
//! hostile input) is caught and reported as an error; it never takes a pool
//! thread or the process down.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

use crate::render::{self, RenderError, RenderSpec, Rendered};

/// A job panicked; the pool caught it.
#[derive(Debug, thiserror::Error)]
#[error("an image job panicked")]
pub struct JobPanicked;

impl From<JobPanicked> for RenderError {
    fn from(_: JobPanicked) -> Self {
        Self::Panicked
    }
}

/// A pool of image threads.
pub struct ImagePool {
    pool: rayon::ThreadPool,
}

impl ImagePool {
    /// Threads of the [shared](ImagePool::shared) pool (plan §2.3).
    pub const THREADS: usize = 2;

    /// A pool of `threads` threads named `shelfy-image-<n>`.
    ///
    /// # Errors
    ///
    /// The operating system refused to start the threads.
    pub fn new(threads: usize) -> std::io::Result<Self> {
        rayon::ThreadPoolBuilder::new()
            .num_threads(threads.max(1))
            .thread_name(|n| format!("shelfy-image-{n}"))
            .build()
            .map(|pool| Self { pool })
            .map_err(std::io::Error::other)
    }

    /// The process-wide pool of [`ImagePool::THREADS`] threads, started on
    /// first use. Every subsystem that processes images (archive worker,
    /// installs, uploads) shares it, which keeps the §2.3 limit of two image
    /// transforms at a time.
    ///
    /// # Panics
    ///
    /// When the threads cannot be started on first use.
    pub fn shared() -> &'static Self {
        static SHARED: OnceLock<ImagePool> = OnceLock::new();
        SHARED.get_or_init(|| Self::new(Self::THREADS).expect("cannot start the image threads"))
    }

    /// Runs `job` on the pool and waits for it without blocking the async
    /// worker. Dropping the future does not cancel a job that has started.
    ///
    /// # Errors
    ///
    /// [`JobPanicked`] when `job` panicked.
    pub async fn run<T, F>(&self, job: F) -> Result<T, JobPanicked>
    where
        F: FnOnce() -> T + Send + 'static,
        T: Send + 'static,
    {
        let (done, result) = tokio::sync::oneshot::channel();
        self.pool.spawn(move || {
            // The receiver may be gone (the caller gave up): nothing to do then.
            let _ = done.send(catch_unwind(AssertUnwindSafe(job)));
        });
        match result.await {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) | Err(_) => Err(JobPanicked),
        }
    }

    /// Runs `job` on the pool and blocks the calling thread until it ends, for
    /// callers outside an async runtime (CLI commands, tests).
    ///
    /// # Errors
    ///
    /// [`JobPanicked`] when `job` panicked.
    pub fn run_blocking<T, F>(&self, job: F) -> Result<T, JobPanicked>
    where
        F: FnOnce() -> T + Send,
        T: Send,
    {
        self.pool
            .install(|| catch_unwind(AssertUnwindSafe(job)))
            .map_err(|_| JobPanicked)
    }

    /// Renders the image file at `path` on the pool.
    ///
    /// # Errors
    ///
    /// [`RenderError`], including [`RenderError::Panicked`].
    pub async fn render_file(
        &self,
        path: PathBuf,
        spec: RenderSpec,
    ) -> Result<Rendered, RenderError> {
        self.run(move || render::render_file(&path, spec)).await?
    }

    /// Renders an image held in memory on the pool.
    ///
    /// # Errors
    ///
    /// [`RenderError`], including [`RenderError::Panicked`].
    pub async fn render_bytes(
        &self,
        bytes: Arc<[u8]>,
        spec: RenderSpec,
    ) -> Result<Rendered, RenderError> {
        self.run(move || render::render_bytes(&bytes, spec)).await?
    }

    /// Transcodes the image file at `path` to a JPEG whose long side is at
    /// most `max_side`, on the pool ([`render::jpeg_file`]).
    ///
    /// # Errors
    ///
    /// [`RenderError`], including [`RenderError::Panicked`].
    pub async fn jpeg_file(
        &self,
        path: PathBuf,
        max_side: u32,
        quality: u8,
    ) -> Result<Vec<u8>, RenderError> {
        self.run(move || render::jpeg_file(&path, max_side, quality))
            .await?
    }

    /// Transcodes an image held in memory to a JPEG, on the pool
    /// ([`render::jpeg_bytes`]).
    ///
    /// # Errors
    ///
    /// [`RenderError`], including [`RenderError::Panicked`].
    pub async fn jpeg_bytes(
        &self,
        bytes: Arc<[u8]>,
        max_side: u32,
        quality: u8,
    ) -> Result<Vec<u8>, RenderError> {
        self.run(move || render::jpeg_bytes(&bytes, max_side, quality))
            .await?
    }

    /// Threads in the pool.
    #[must_use]
    pub fn threads(&self) -> usize {
        self.pool.current_num_threads()
    }
}

impl std::fmt::Debug for ImagePool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImagePool")
            .field("threads", &self.threads())
            .finish()
    }
}
