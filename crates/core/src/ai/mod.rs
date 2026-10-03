//! AI prompt assembly and output normalization (plan §2.15, P3-03).
//!
//! The prompts and response schemas are the files of `shared/ai/`, shared with
//! the desktop app and embedded here with `include_str!`; golden fixtures keep
//! this port byte for byte equal to the desktop's (`shared/golden/ai/`).
//!
//! - [`prompts`]: the manifest's tasks: rendered prompts, response schemas,
//!   sampling, and [`prompts::SCHEMA_VERSION`].
//! - [`template`]: the template language of the prompt files.
//! - [`catalog`]: catalog requests for social posts and websites.
//! - [`normalize`]: a catalog answer, checked against its schema, as an
//!   [`crate::repo::posts::AiPatch`].
//!
//! Providers, network calls and the queue are not here: the core is
//! synchronous and offline.

pub mod catalog;
pub mod normalize;
pub mod prompts;
pub mod template;
