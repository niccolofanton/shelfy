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

## P3-13 deploy and the first Instagram run (E15)

From the owner's handoff of 2026-10-03 (§17.4–17.5), for when P3-13 has landed and passed its acceptance tests:

1. Tag the next server release on a tip whose CI is green (F20), after re-running the whole suite on that tip.
2. In osn: pin the image; set `SHELFY_OPERATOR_AI_URL` (with `/v1`), `_KEY`, `_MODEL=ornith-1.5-35b-a3b`, `_VISION_MODEL=qwen3.8-27b`, `_CONCURRENCY=1` and an explicit `_TIMEOUT` (the documented default is 60 s; a catalog answer can take about 150 s on qwen), `SHELFY_OPERATOR_STT_URL` and `_KEY`, and the AI and STT origins in `SHELFY_EGRESS_ALLOW_ORIGINS` (the osn env template does not have these names yet). Record Hermes's state before and after; run `admin ai-probe operator` (never print the keys).
3. Run the 40-post gold sample on the node, one request at a time after `health` and `models`; keep a private report (composite, per field, worst cases, done/gated/error, observed pace). The scorer's 1.000 on the gold itself is a format self-check, not the node's score.
4. Check the latest backup and the deployed version; find the owner's user id through the admin CLI.
5. Ask for the estimate on an Instagram selector with mode `all`; note `waitingForMedia` and `alreadyQueued`. At the lane's provisional 180 s per post, 3,997 posts take about 200 hours at concurrency 1: measure the pace on the 40 posts and update the ETA first. The install left 1,641 posts in `client` and 223 pending, and not every video was migrated, so the run will exclude posts without media: list them in aggregate with a recovery path.
6. Confirm with the returned token and a stable Idempotency-Key; watch one post go pending → analyzing → done with its tags saved; then monitor done, error, waiting, provider state and backlog without re-launching `all` while the first run is queued.

Status: todo (needs P3-13).
