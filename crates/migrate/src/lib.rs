//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! The library half of the CLI, so the same code can later run from the
//! desktop app (plan §4.1, P6):
//!
//! - [`login`]: the device sign-in that saves a `migrate` token.
//! - [`plan`]: the dry run. It reads the desktop library through the
//!   read-only legacy reader of `shelfy-core` and reports how every row maps
//!   to the web schema, without writing anything.
//! - [`desktop`]: whether the desktop app holds its library open (`plan` and
//!   `run` refuse then), and [`snapshot`]: the consistent copy `run` reads
//!   when it may (OI-8).
//! - [`settings`] (over [`leveldb`]): the desktop's language and asset
//!   preferences, from its localStorage (OI-10).
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
pub mod desktop;
pub mod files;
pub mod leveldb;
pub mod login;
pub mod plan;
pub mod render;
pub mod report;
pub mod run;
pub mod settings;
pub mod snapshot;
