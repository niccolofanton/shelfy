# Live steps left by the cloud lanes

The cloud lanes (E16) cannot reach the osn VPS, refs, the owner's AI node or the owner's local data. This file collects, in the order they were found, the steps that need a session that reaches them. The lead runs them under E2; owner steps say so. Mark each one done with the date and the result.

## F12: backfill the migrated site version on refs

**Why.** Before F12, `shelfy-migrate` wrote `NULL` into a web capture's `palette_json`, `fonts_json`, `tech_json` and `awards_json`. The owner's library has one migrated web capture; the mock account (E6) has one too if it was seeded before F12. Re-running `shelfy-migrate run --merge` does not fix it: the merge skips a version it already has with the same capture time (`copy_captures` in `crates/server/src/migrations/merge.rs`).

**Steps** (from the F12 lane; the statements only fill columns that are still `NULL`, so running them twice changes nothing):

1. On the owner's Mac, generate the statements from the desktop library, opened read-only (the query writes nothing):

   ```sql
   -- sqlite3 -readonly "<desktop library.sqlite>" < gen.sql > f12-backfill.sql
   SELECT 'UPDATE web_captures SET '
     || 'palette_json = coalesce(palette_json, CASE WHEN json_valid(' || quote(v.pal) || ') THEN ' || quote(v.pal) || ' END), '
     || 'fonts_json = coalesce(fonts_json, CASE WHEN json_valid(' || quote(v.fon) || ') THEN ' || quote(v.fon) || ' END), '
     || 'tech_json = coalesce(tech_json, CASE WHEN json_valid(' || quote(v.tec) || ') THEN ' || quote(v.tec) || ' END), '
     || 'awards_json = coalesce(awards_json, CASE WHEN json_valid(' || quote(v.awa) || ') THEN ' || quote(v.awa) || ' END) '
     || 'WHERE requested_url = ' || quote(v.url) || ' AND captured_at = ' || (v.at * 1000) || ';'
   FROM (SELECT web_url url, web_captured_at at, web_palette_json pal, web_fonts_json fon, web_tech_json tec, web_awards_json awa
           FROM posts WHERE platform = 'web' AND web_captured_at IS NOT NULL
         UNION ALL
         SELECT p.web_url, s.captured_at, s.web_palette_json, s.web_fonts_json, s.web_tech_json, s.web_awards_json
           FROM web_snapshots s JOIN posts p ON p.id = s.post_id) v;
   ```

   A migrated capture's `requested_url` is the desktop's `web_url`, and its `captured_at` is the desktop seconds × 1000.
2. On the VPS: `shelfy-server admin snapshot` first; stop `shelfy-api` (a running server's ETags and caches do not see another process's writes); `sqlite3 users/<owner id>/library.sqlite`, then `BEGIN;`, `.read f12-backfill.sql`, `SELECT changes();` (expect 1), `COMMIT;`; start the service and check the site's `webCapture.palette` through the API. No search index depends on these columns.
3. Repeat for the mock account's library if its site capture was migrated before F12.

Status: todo.
