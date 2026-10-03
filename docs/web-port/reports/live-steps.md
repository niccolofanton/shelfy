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
2. In osn: pin the image; set `SHELFY_OPERATOR_AI_URL` (with `/v1`), `_KEY`, `_MODEL=ornith-1.5-35b-a3b`, `_VISION_MODEL=qwen3.8-27b`, `_CONCURRENCY=1` and `_TIMEOUT=240`, `SHELFY_OPERATOR_STT_URL` and `_KEY`, and the AI and STT origins in `SHELFY_EGRESS_ALLOW_ORIGINS` (osn f2d209d now wires these names; encrypted credential and exact origins are prepared, not applied). Record Hermes's state before and after; run `admin ai-probe operator` (never print the keys).
3. Run the 40-post gold sample on the node, one request at a time after `health` and `models`; keep a private report (composite, per field, worst cases, done/gated/error, observed pace). The scorer's 1.000 on the gold itself is a format self-check, not the node's score.
4. Check the latest backup and the deployed version; find the owner's user id through the admin CLI.
5. Ask for the estimate on an Instagram selector with mode `all`; note `waitingForMedia` and `alreadyQueued`. The latest read-only census has 3,997 active Instagram posts: 2,136 analyzable, 1,861 waiting and zero queued. All waiting covers have expired URLs. Keep the full Instagram scope visible while recovering media; do not call the eligible subset the completed full library.
6. Confirm with the returned token and a stable Idempotency-Key; watch one post go pending → analyzing → done with its tags saved; then monitor done, error, waiting, provider state and backlog without re-launching `all` while the first run is queued.

Status: engine integrated; production deploy and owner run not started. X1 full40 validation completed serially on Ornyth: 38 valid, 2 gated, zero provider/schema errors; report `x1-node-benchmark-2026-10-03.md`, prompt digest `ac46d33ae3fc`.

Additional gates and recovery, recorded on 2026-10-03:

- Match the evaluated input profile: `deep=true` gives still images at 1024; the owner Instagram library currently has no stored video objects, so this does not imply frame extraction. Use poster/slide plus weak caption evidence; do not download all videos or run STT implicitly.
- Admin helper `24a3368` can recover 81 existing poster files from the verified private bundle. After backup, perform its live default dry-run first. Stop only Shelfy API, apply with `--server-stopped --apply`, restart and recount readiness. Do not substitute the local mirror dry-run for a live check.
- Remaining expired URLs need P2-17 refresh through the already authenticated Instagram tab. Its synthetic tests pass and independent review closed legacy queue account binding in integrated `3e44bef`; deployment and live verification remain. No automated live social action has run yet.
- P3-18 settings and P3-16 taxonomy jobs are integrated; P3-20 queue UI and BYOK remain separate tracked work. The owner first run must still verify persisted tags and progress on production.
- Record the exact release commit, CI run, image digest, backup evidence and unchanged Hermes health/restart count. CI `37144780896` on `85c37a8` has all blocking jobs green. The newer `37146987723` on `970959a` failed live-browser setup because its AI stub was not built; fix `56d82ee` is integrated, awaiting publication and rerun. Lighthouse F18 remains open. No new release tag exists after rc.5 yet.

## P2-13: the first real syncs (owner, P2-23)

The sync controller only ran against synthetic pages. On the owner's real accounts, check that the replay gate works on live Instagram, that a signed-out replay really answers 401/403 (it maps to `login_required`), and how folder names taken from the page `h1` come out. On Instagram the scroll after a full replay adds nothing today (SPIKE-3) and can take up to 30 minutes: `shelfy-server admin flags extension.instagram.scroll=false` turns it off (it is on by default, as the card asks); decide after the first real sync.

Status: todo (owner).
