//! `shelfy-migrate`: moves a desktop Shelfy library to Shelfy Web.
//!
//! The library half of the CLI, so the same code can later run from the
//! desktop app (plan §4.1, P6). Today it holds the dry run ([`plan`]): it
//! reads the desktop library through the read-only legacy reader of
//! `shelfy-core` and reports how every row maps to the web schema, without
//! writing anything.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §4 and
//! `docs/web-port/spikes/01-legacy-mapping.md`.

pub mod files;
pub mod plan;
pub mod render;
pub mod report;
