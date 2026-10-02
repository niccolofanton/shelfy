-- library.sqlite schema v2: the infix index of search (P1-05).
-- Spec: docs/web-port/IMPLEMENTATION-PLAN.md §2.14; decision in
-- docs/web-port/spikes/05-fts-relevance.md ("P1-05: the infix index").
--
-- posts_fts matches whole tokens and token prefixes only, so a query term inside a
-- longer token (a compound hashtag: "fluidi" in #simulazionefluidi) never matched.
-- posts_infix indexes the searchable text of each live post as trigrams: a term of 3
-- or more characters matches anywhere in it, as the desktop's LIKE '%term%' did.
--
-- Derived data: the core writes a post's row in the same transaction as its posts_fts
-- row (crates/core/src/search/index.rs), never by triggers. The text is the SQL
-- expression `search::index::INFIX_TEXT_SQL`; the backfill below inlines it as it is at
-- v2, and `search::index::rebuild_infix` is its idempotent re-derivation.
--
-- detail=full is required: FTS5 answers a phrase of trigrams (any term longer than 3
-- characters) only with full position lists.

CREATE VIRTUAL TABLE posts_infix USING fts5(
  text,
  content='', contentless_delete=1,
  tokenize="trigram remove_diacritics 1");

INSERT INTO posts_infix (rowid, text)
SELECT id, text FROM (
  SELECT p.id AS id,
         concat_ws(char(10), p.caption, p.ai_description, p.user_note, p.author_username,
                   p.author_name, p.ai_tags_json, p.ai_keywords_json, p.ai_entities_json,
                   p.user_tags_json,
                   (SELECT group_concat(tag_form, char(10)) FROM (
                      SELECT t.tag_form FROM post_tags t WHERE t.post_id = p.id
                      ORDER BY t.tag_norm, t.source))) AS text
  FROM posts p
  WHERE p.deleted_at IS NULL
)
WHERE text <> '';
