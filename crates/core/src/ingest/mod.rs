//! Ingest: how posts that come from outside the library join it.
//!
//! - [`merge`] ports the desktop's `bulkUpsert` (DATA-04): an incoming post
//!   is inserted, or merged into the stored post with the same key without
//!   clobbering what the library already has. Capture batches (P2), imports
//!   (P4) and a migrated library's new posts go through it.
//! - [`duplicates`] is the plan's §4.2 policy for two full rows of one post:
//!   the row with archived files wins, then the one with an AI analysis, then
//!   the one with a user layer; folders are united and notes joined. The
//!   migration uses it for duplicate desktop ids and for `--merge` into a
//!   library that already holds the post.
//!
//! Both take the transaction of [`UserDb::write`] and the current time, like
//! the repositories. The batch sanitizer (the port of
//! `sanitizeInterceptedBatch`) joins this module in P2: it validates raw items,
//! stamps the batch platform, canonicalizes keys and hands the result to
//! [`merge::upsert_batch`].
//!
//! See `docs/web-port/IMPLEMENTATION-PLAN.md` §2.16 (ingest), §4.1–4.2 and
//! §6.1 (golden parity, `scripts/golden/merge.ts`).
//!
//! [`UserDb::write`]: crate::db::UserDb::write

pub mod duplicates;
pub mod merge;

#[cfg(test)]
mod tests;
