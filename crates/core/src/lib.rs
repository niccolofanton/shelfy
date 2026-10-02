//! Shelfy domain core.
//!
//! The synchronous domain library behind the web server and, after the desktop
//! convergence (plan §2.20), the Electron app through napi. It owns:
//!
//! - the schema and its migrations, and the repositories built on it;
//! - ingest validation and merge rules, canonical ids and URL normalization;
//! - search, tags, aliases and clusters;
//! - web captures;
//! - AI prompt assembly and output normalization;
//! - the read-only reader for legacy desktop libraries.
//!
//! It never depends on an async runtime such as tokio: callers run it on
//! blocking threads.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.4.

pub mod db;
pub mod ids;
pub mod ingest;
pub mod legacy;
pub mod repo;
pub mod schema;
pub mod search;

#[cfg(test)]
mod tests {
    /// Smoke test: the crate builds and its test harness runs.
    #[test]
    fn smoke() {
        assert_eq!(env!("CARGO_PKG_NAME"), "shelfy-core");
    }
}
