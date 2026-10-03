//! Post search (plan D10, §2.14): content-term extraction shared with the
//! desktop, FTS5 query building, and the maintenance of the `posts_fts` index.
//!
//! - [`terms`]: the port of the desktop's `extractContentTerms`, pinned by a
//!   golden fixture.
//! - [`query`]: FTS5 expressions and the ranking constants.
//! - [`index`]: the explicit, in-transaction maintenance of `posts_fts`.
//!
//! The list query that combines search with the other filters is in
//! [`crate::repo::posts`].

pub mod index;
pub mod query;
pub mod terms;

pub mod vocab;
