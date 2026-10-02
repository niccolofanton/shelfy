//! Shelfy media pipeline.
//!
//! It owns:
//!
//! - the content-addressed store (CAS) for each user's media files;
//! - the image pipeline: decode, resize, WebP encoding and ThumbHash
//!   placeholders;
//! - the wrappers around the `ffmpeg` and `yt-dlp` subprocesses.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.4 and §2.13.

#[cfg(test)]
mod tests {
    /// Smoke test: the crate builds and its test harness runs.
    #[test]
    fn smoke() {
        assert_eq!(env!("CARGO_PKG_NAME"), "shelfy-media");
    }
}
