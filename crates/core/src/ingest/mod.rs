//! Ingest: how posts that come from outside the library join it.
//!
//! - [`sanitize`] ports the desktop's `sanitizeInterceptedBatch` with the
//!   stricter rules of §2.16: it checks a capture batch from the browser
//!   extension item by item, stamps the batch platform, canonicalizes keys
//!   and maps each item to an [`merge::IncomingPost`]. [`hosts`] holds the
//!   platforms' URL allowlists it checks against.
//! - [`merge`] ports the desktop's `bulkUpsert` (DATA-04): an incoming post
//!   is inserted, or merged into the stored post with the same key without
//!   clobbering what the library already has. Capture batches (P2), imports
//!   (P4) and a migrated library's new posts go through it.
//! - [`duplicates`] is the plan's §4.2 policy for two full rows of one post:
//!   the row with archived files wins, then the one with an AI analysis, then
//!   the one with a user layer; folders are united and notes joined. The
//!   migration uses it for duplicate desktop ids and for `--merge` into a
//!   library that already holds the post.
//! - [`archive`] derives a post's `archive_state` (P2 contract C10): what is
//!   left to store of its media, and whether the server or the extension
//!   acts on it. Ingest, the archive drain and the migration share it.
//!
//! The functions that write take the transaction of [`UserDb::write`] and the
//! current time, like the repositories. A capture batch goes through
//! [`sanitize::sanitize_batch`], then [`merge::upsert_batch`], then
//! [`archive::refresh_states`] for the posts it inserted or changed, in one
//! transaction.
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.12–2.13 (archive), §2.16
//! (ingest), §4.1–4.2 and §6.1 (golden parity, `scripts/golden/`).
//!
//! [`UserDb::write`]: crate::db::UserDb::write

pub mod archive;
pub mod duplicates;
pub mod hosts;
pub mod merge;
pub mod sanitize;

pub use hosts::cdn_url_expiry_ms;

#[cfg(test)]
mod tests;
