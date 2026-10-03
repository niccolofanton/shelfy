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
//! - [`queue`]: the item-work state machine of social cataloging in the
//!   `posts.ai_*` columns (P3-13), which the server's `ai.drain` works through.
//! - [`inputs`]: the frames, caption and vocabulary a catalog call needs.
//! - [`estimate`]: the token and time estimate of an analyze request.
//!
//! Providers and network calls are not here: the core is synchronous and
//! offline. The queue chooses inputs and records state; the server reads
//! media bytes, calls the provider and drives the job.

pub mod catalog;
pub mod estimate;
pub mod inputs;
pub mod normalize;
pub mod prompts;
pub mod queue;
pub mod taxonomy_prompt;
pub mod template;

pub mod chat;

pub mod chat_prompt;
