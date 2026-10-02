//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! The library half of the CLI, so the same code can later run from the
//! desktop app (plan §4.1, P6):
//!
//! - [`plan`]: the dry run. It reads the desktop library through the
//!   read-only legacy reader of `shelfy-core` and reports how every row maps
//!   to the web schema, without writing anything.
//! - [`snapshot`]: the consistent copy `run` reads when the desktop app may
//!   hold the library (OI-8).
//! - [`bundle`]: the bundle `run` uploads, built from the dry run's
//!   decisions: a new-schema `library.sqlite` and the content-addressed
//!   objects it references.
//! - [`client`]: the server's migration API, with resumable tus uploads.
//! - [`run`]: `shelfy-migrate run` end to end, resumable.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4 and
//! `docs/web-port/spikes/01-legacy-mapping.md`.

pub mod bundle;
pub mod client;
pub mod files;
pub mod plan;
pub mod render;
pub mod report;
pub mod run;
pub mod snapshot;
