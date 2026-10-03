-- library.sqlite schema v3: the fetch state of a cover without slides (P2-10).
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.12 (5 tries per item, 30 s × 2^n
-- backoff), §2.13; the archive-state rule in crates/core/src/ingest/archive.rs.
--
-- The archive fetches a post's cover through its slide 0 and keeps that item's tries,
-- next time and error in post_media.fetch_*. A post without slides (an X text tweet,
-- whose cover is the author's avatar; a desktop post that never had slides) has no
-- such row: these columns hold the same state for its cover, so a gone or refused
-- cover fails for good instead of being fetched again at every drain.
--
-- Additive (§3.8 expand/contract): an older build ignores them.

ALTER TABLE posts ADD COLUMN cover_fetch_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE posts ADD COLUMN cover_fetch_next_at INTEGER;
ALTER TABLE posts ADD COLUMN cover_fetch_error TEXT;
