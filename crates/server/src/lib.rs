//! `shelfy-server`: the Shelfy Web API process.
//!
//! It owns:
//!
//! - the axum application: REST API, SSE and SPA hosting;
//! - authentication and sessions;
//! - the job scheduler and its workers;
//! - the `shelfy-server admin …` operator CLI.
//!
//! The binary (`main.rs`) stays a thin entry point: the logic lives in this
//! library so tests can drive it in-process.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.2 and §2.4.

/// Version of this build (the workspace version).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
