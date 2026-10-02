# SPIKE-1 — Mapping the desktop library

**Question.** Can a desktop library (`<userData>/shelfy.sqlite` plus its `assets/`) be mapped losslessly onto the web schema of plan §2.7? The hard parts are the Instagram ids, the local files and the site versions.

**Pass criteria (§9).**
- every row accounted for;
- duplicate groups listed;
- no unmapped column.

**Answer.** Yes. The dry run passes all three criteria on the reference library.
- **Rows:** 19,976 rows in 14 tables. Every row has exactly one outcome: inserted, rebuilt or dropped on purpose. None is unmappable.
- **Columns:** the 121 columns of the 13 desktop tables are all known. 99 are mapped and 22 are dropped on purpose.
- **Identity:** every row gets a canonical key (§2.8). All 3,997 Instagram shortcodes decode to the pk of their row's id. The one site's legacy `web:<sha1>` id is reproduced by the Rust port of `normalizeWebUrl`.
- **Duplicates:** this library has none. There are 0 Instagram same-pk groups, 0 http/https site twins, 0 other shared keys and 0 collection duplicates. The merge path is covered by synthetic tests only.
- **Files:** 6,302 distinct files are referenced and 6,038 are present (4.1 GiB). The 264 missing files are all `posts.video_path`: the paths are still set, but the videos are no longer on disk. The desktop clears stale paths only before a cover repair (DATA-34).
- **Site versions:** the one site has a single version. All 88 of its capture-file references were found on disk; they use 9 of the 11 roles. The 41 `post_facets` rows are all rebuilt from `ai_web_json`.

| Criterion | Result |
|---|---|
| Every row accounted for | **yes**: 14 / 14 tables, the outcomes add up to `COUNT(*)` |
| Duplicate groups listed | **yes**: the library has 0 groups; the listing itself is covered by synthetic tests |
| No unmapped column | **yes**: 121 / 121 columns in the catalog, 0 unknown tables or columns, no views or triggers |
| Errors | 0 |
| Warnings | 2: a `-wal` file was present (left by earlier readers of the copy), and 264 missing files |

Run on 2026-10-02 on the reference library copy (`../shelfy-web-local/ref/shelfy.sqlite`, Appendix C). The media root was the desktop userData directory, read-only. The debug build took 0.6 s and peaked at 18 MB RSS.

## Method

**Reader: `crates/core::legacy`.**
- **Read-only open.** It opens the file with `SQLITE_OPEN_READ_ONLY` and never creates it, then sets `PRAGMA query_only`.
  - With no `-wal` or `-journal` file next to the library, it opens the file `immutable`. This matters because a plain read-only open of a cleanly closed WAL library leaves new `-wal` and `-shm` files behind (seen in testing).
  - Otherwise it opens a shared read-only connection, which also sees the WAL.
- **One snapshot.** Every query runs inside one read transaction, so the counts agree even while the desktop writes. A test covers this.
- **Typed rows.** There is one struct per desktop table, streamed in rowid order through a callback.
- **Catalog.** `legacy::catalog` lists every table and column that `SCHEMA` and `migrate()` in `electron/db.ts` can create (`SCHEMA_VERSION = 3`). For each column it records two things:
  - whether it is a base column or an `ALTER`-added one;
  - its §2.7 target or its drop reason.
- **Older files.** A missing table reads as empty. A missing `ALTER`-added column reads as that `ALTER`'s default, for example `media_count` = 1 and `tag_alias.status` = `'accepted'`. Tests cover the current desktop DDL and an early one: `user_version` 0, 15 `posts` columns and no later tables.

**Identity: `crates/core::ids`.**
- **Instagram.** A legacy id is read in this order:
  1. the shortcode, when the id equals the row's shortcode;
  2. `<pk>_<owner>`;
  3. a decimal pk;
  4. any other string in the shortcode alphabet, decoded.
- **Instagram pk.** It is kept as canonical decimal text of any length (a small bignum, up to 64 shortcode characters).
- **X and Pinterest.** The id, else the status or pin URL.
- **Web.** An exact port of the desktop `normalizeWebUrl`, then the scheme is dropped.
- **Manual.** A deterministic ULID.

**Dry run: `shelfy-migrate plan --db <path> [--media-root <dir>] [--json] [--redact]`.** It streams every table and maps each row:
- **Identity:** each row gets its key or an unmappable reason.
- **Duplicates:** rows are grouped by key; the kept row follows §4.2.
- **Children:** child rows are remapped through their parent's key and deduplicated.
- **Files:** every stored path is collected, including the paths inside the site JSON. Each path is rebased from the detected desktop root onto `<media root>/assets/`. The tool only reads file metadata and lists `assets/` to find orphans.

It never writes: the tests check that the library's bytes and its directory listing are unchanged after a CLI run. `shelfy-migrate mapping` prints the catalog as a table, one row per column; the [column mapping](#column-mapping) below condenses it.

**Checks against the desktop.**
- **Web ids.** 63 URL vectors (`normalizeWebUrl` + `webPostId`) were produced by running the real functions exported from `electron/db.ts` under Node (tsx). The Rust port matches all 63, quirks included:
  - query re-serialization;
  - the trailing-slash regex;
  - the BOM-trimming fallback.
- **Instagram codec.** pk ↔ shortcode pairs were checked against JavaScript BigInt, and `igDateFromShortcode` outputs against the desktop function.
- **Property tests.** Proptest round-trips cover:
  - u64 and u128 pks, 90-digit pks and random shortcodes;
  - all three IG id forms mapping to one key;
  - the scheme never changing a web key;
  - normalization idempotence;
  - ISO timestamps.
- **On real data.** All 3,997 shortcodes decode to their row's pk. The site's legacy id is reproduced from `web_url`. The 264 missing videos and 240 present ones were recounted with an independent script.

**Privacy.** The report reads aggregates; `--redact` hides keys, legacy ids and the media root. This note holds counts only.

## Results

### Rows per table

| Desktop table | Rows | Outcome | Web target |
|---|---|---|---|
| `posts` | 6,138 | insert 6,138 | `posts` (+ `web_captures`, `media_objects`) |
| `post_media` | 11,482 | insert 11,482 | `post_media` (+ `media_objects`) |
| `collections` | 1 | insert 1 | `collections` |
| `post_collections` | 2,264 | insert 2,264 | `post_collections` |
| `post_tags` | 33 | insert 33 | `post_tags` |
| `post_entities` | 10 | insert 10 | `post_entities` |
| `post_facets` | 41 | rebuilt 41 | dropped: rebuilt from `posts.ai_web_json` |
| `tag_alias`, `tag_cluster`, `tag_cluster_membership` | 0 | — | same tables |
| `web_snapshots` | 0 | — | `web_captures` + `web_capture_assets` |
| `jobs` | 5 | dropped 5 (all finished) | dropped |
| `downloads` | 0 | — | dropped (dead table) |
| `sqlite_sequence` | 2 | dropped 2 | dropped (SQLite bookkeeping) |

The other outcomes exist but do not occur in this library: `merge` (a duplicate folded into the kept row), `orphan` (the parent row is missing, so the row is dropped with a warning), `not_derivable` (an error), and `unmappable` / `unmappable_parent` / `unmapped` (errors).

### Columns and values

- **Coverage.** 121 columns, all present: 99 mapped, 22 dropped. The drops are `thumb_blur`, `post_facets` ×3, `jobs` ×10 and `downloads` ×8. No column is absent: the file is at `user_version` 3, so no desktop repair is pending. No column holds an unexpected storage class.
- **Posts.**
  - By platform: Instagram 3,997, X 2,140, web 1.
  - By media type: video 4,631, carousel 828, image 391, text 173, images 114, website 1.
- **Slides.** By target kind: image 5,849, video 5,627, `page` 6 (the site's pages).
  - The 173 posts without slides are all `text` posts (text-only tweets).
  - No `media_count` disagrees with the slide count.
- **Dates.**
  - `timestamp`: 6,138 valid ISO 8601 values, so 0 empty, NULL or invalid.
  - `imported_at`: 6,138 plausible unix seconds.
- **AI.**
  - `ai_status`: 3 `done`, 1 `error` and 6,134 NULL; 4 posts have AI fields and 1 has `ai_web_json`. None is stuck in `analyzing`.
  - `post_tags`: 9 general and 24 specific, all source `ai`. There are no manual tags, so no manual/AI name collisions.
- **User layer.** No notes and no manual tags. `thumb_blur`: 2,213 data URIs, all dropped and recomputed as ThumbHash.

### Identity and duplicates

| Platform | Rows | Keys | Source of the key | Check |
|---|---|---|---|---|
| Instagram | 3,997 | 3,997 | `<pk>_<owner>` ×3,997 | shortcode → pk equals the id's pk: 3,997 / 3,997 |
| X | 2,140 | 2,140 | tweet id ×2,140 | — |
| web | 1 | 1 | `web_url` ×1 | legacy id reproduced from `web_url`: 1 / 1 |

There are 0 unmappable posts. Duplicate groups: 0 Instagram (same pk), 0 web (http/https twins), 0 other keys, 0 collections (same `(platform, external_id)`).

### Local files

| Class (desktop source) | Refs | Files present | Files missing | Bytes present |
|---|---|---|---|---|
| `cover` (`posts.thumbnail_path`) | 2,214 | 2,214 | 0 | 427.6 MiB |
| `preview` (`posts.preview_path`) | 2,078 | 2,078 | 0 | 80.5 MiB |
| `image` (`posts.image_path`) | 304 | 304 | 0 | 125.1 MiB |
| `video` (`posts.video_path`) | 504 | 240 | **264** | 2.9 GiB |
| `slide_image` (`post_media.local_path`) | 1,480 | 1,480 | 0 | 581.1 MiB |
| `slide_video` (`post_media.local_path`) | 56 | 56 | 0 | 103.1 MiB |
| site files (9 roles, see below) | 88 | 82 | 0 | 18.4 MiB |
| **all classes, distinct files** | **6,724** | **6,038** | **264** | **4.1 GiB** |

- **Paths.** The 6,724 references point to 6,302 distinct files. A file can belong to several classes: `image_path` is also slide 0's `local_path`, and a site's hero is also its cover and its first page slide. All paths sit under one detected desktop root; none falls outside it.
- **Upload estimate.**
  - By default: 5,760 files, 1.0 GiB.
  - With `--with-videos`: another 278 video-only files, 3.0 GiB.
  - This agrees with §4.4.
- **Covers.**
  - 4,101 posts have a local cover. The other 2,037 have none (Instagram 1,861, X 176), the same as Appendix C.
  - Of those 1,861 Instagram cover URLs, 1,641 carry an expired `oe` signature and **220 were still valid** at run time.
- **Orphans.** 19 files under `assets/` (987 MiB) are referenced by no row: 7 videos (987 MiB) and 12 site files (0.4 MiB). One `.DS_Store` was ignored.

### Site versions

- **Versions.** 1 site: captured, 6 pages, 0 placeholders, 0 older versions. It becomes 1 `web_captures` row.
- **Capture files by role.** 88 references in all:

  | Role | Refs |
  |---|---|
  | band | 45 |
  | section | 22 |
  | hero | 6 |
  | screenshot | 6 (the same files as hero) |
  | footer | 5 |
  | og | 1 |
  | favicon | 1 |
  | video | 1 |
  | video_preview | 1 |

  The two roles that do not occur here are `filmstrip` (the chunks of a scroll-jacked page) and `video_poster`.
- **Facets.** The rebuild reproduces all 41 `post_facets` rows exactly, with no extra rows. The rule is the desktop's `applyAiAnalysis`: stringify, trim, cut at 120 UTF-16 units, dedupe.

## Column mapping

Condensed from `shelfy-migrate mapping`, which prints the same catalog (`crates/core/src/legacy/catalog.rs`) one row per column. Here a row may list several columns that share a rule; `<same>` is the column of the same name in the web table. "Since" is `base` for a column of the original `CREATE TABLE` and `added` for one a later desktop migration added (an older file may lack it). Every column of the desktop schema appears exactly once.

| Desktop column | Since | Web target | Rule / reason |
|---|---|---|---|
| **`posts`** (table) | | **posts, web_captures, media_objects** | one post per canonical key; duplicates merge (§4.2); a captured site also yields its current web_captures row |
| `posts.id` | base | `posts.key, posts.native_id` | canonical identity (§2.8): IG `<pk>_<owner>`, pk or shortcode → `ig_<pk>`; tweet id → `x_<id>`; pin id → `pin_<id>`; `web:<sha1>` → `web_<sha1:20>` of the scheme-less URL; `manual:<uuid>` → `m_<ulid>` (legacy id kept in meta) |
| `posts.platform` | base | `posts.platform` | verbatim (instagram, twitter, pinterest, web, manual) |
| `posts.shortcode` | base | `posts.shortcode` | verbatim; '' → NULL |
| `posts.post_url` | base | `posts.post_url` | verbatim; '' → NULL; X `x.com//status/` repaired as in desktop migrate v1 |
| `posts.profile_url` | base | `posts.profile_url` | verbatim; '' → NULL |
| `posts.author_username` | base | `posts.author_username` | verbatim; '' → NULL |
| `posts.author_name` | base | `posts.author_name` | verbatim; '' → NULL |
| `posts.text` | base | `posts.caption` | verbatim (≤ 20 000 chars) |
| `posts.thumbnail_url` | base | `posts.cover_url, posts.cover_url_expires_at` | verbatim; expiry from the IG `oe` parameter |
| `posts.media_type` | base | `posts.media_type` | verbatim; NULL → derived from the slides |
| `posts.timestamp` | base | `posts.posted_at, posts.sort_ts` | ISO 8601 → ms; '', NULL or invalid → NULL; sort_ts = COALESCE(posted_at, imported_at) |
| `posts.thumbnail_path` | base | `media_objects, posts.cover_object` | file hashed into CAS; missing file → archive_state pending |
| `posts.preview_path` | added | `media_objects, posts.cover_object` | 640 px auto cover; CAS; the cover when no downloaded cover exists |
| `posts.image_path` | base | `media_objects, post_media.object_id` | the slide-0 image (same file as post_media position 0); CAS |
| `posts.video_path` | base | `media_objects, post_media.video_object_id` | kept video of slide 0; CAS only with --with-videos |
| `posts.media_count` | added | `posts.media_count` | recomputed from the slides |
| `posts.imported_at` | base | `posts.imported_at` | unix seconds → ms |
| `posts.ai_description` | added | `posts.ai_description` | verbatim |
| `posts.ai_tags` | added | `posts.ai_tags_json` | verbatim JSON array |
| `posts.ai_status` | added | `posts.ai_status` | verbatim; 'analyzing' (stuck) → NULL, as desktop DATA-47 |
| `posts.ai_model` | added | `posts.ai_model` | verbatim; analyzed rows get ai_provider 'desktop-local' and ai_schema_version 1 |
| `posts.ai_analyzed_at` | added | `posts.ai_analyzed_at` | unix seconds → ms |
| `posts.ai_category` | added | `posts.ai_category` | verbatim |
| `posts.ai_content_type` | added | `posts.ai_content_type` | verbatim |
| `posts.ai_entities` | added | `posts.ai_entities_json` | verbatim JSON array |
| `posts.ai_keywords` | added | `posts.ai_keywords_json` | verbatim JSON array |
| `posts.ai_language` | added | `posts.ai_language` | verbatim |
| `posts.ai_save_reason` | added | `posts.ai_save_reason` | verbatim |
| `posts.ai_web_json` | added | `posts.ai_web_json` | verbatim JSON object (source of the rebuilt facets) |
| `posts.user_note` | added | `posts.user_note` | verbatim; merged duplicates concatenate notes |
| `posts.user_tags` | added | `posts.user_tags_json` | verbatim JSON array |
| `posts.web_url` | added | `posts.web_url, web_captures.requested_url` | verbatim; the URL the web identity is computed from |
| `posts.web_domain` | added | `posts.web_domain` | verbatim |
| `posts.web_final_url` | added | `posts.web_final_url, web_captures.final_url` | verbatim |
| `posts.web_palette_json` | added | `web_captures.palette_json` | current capture, verbatim |
| `posts.web_fonts_json` | added | `web_captures.fonts_json` | current capture, verbatim |
| `posts.web_tech_json` | added | `web_captures.tech_json` | current capture, verbatim |
| `posts.web_awards_json` | added | `web_captures.awards_json` | current capture, verbatim |
| `posts.web_pages_json` | added | `web_captures.pages_json, web_capture_assets` | current capture; file paths (screenshot, hero, chunks, sections, footer) → CAS assets, the JSON keeps text and probes |
| `posts.web_meta_json` | added | `web_captures.meta_json, traits_json, engine, viewport, favicon_object, web_capture_assets` | current capture; og image, favicon and scroll video → CAS; traits and capture settings split out |
| `posts.web_captured_at` | added | `web_captures.captured_at` | unix seconds → ms; a site without pages is a placeholder: no capture row |
| `posts.thumb_blur` | added | dropped | ~24 px JPEG data URI; replaced by posts.thumbhash, recomputed from the cover |
| **`post_media`** (table) | | **post_media, media_objects** | one slide per (post, position) |
| `post_media.post_id` | base | `post_media.post_id` | the parent post's new id (merged duplicates point to the kept post) |
| `post_media.position` | base | `post_media.position` | verbatim |
| `post_media.media_type` | base | `post_media.kind` | image, video, file; slides of web posts → page |
| `post_media.source_url` | base | `post_media.source_url, post_media.object_id` | remote URL verbatim (+ IG `oe` expiry); manual posts hold the original file's local path → CAS object |
| `post_media.local_path` | base | `media_objects, post_media.object_id, post_media.video_object_id` | file hashed into CAS; videos only with --with-videos |
| **`collections`** (table) | | **collections** | duplicates on (platform, external_id) merge |
| `collections.id` | base | `collections.id` | renumbered; old → new id map for memberships |
| `collections.name`, `color` | base | `collections.name` / `collections.color` | verbatim |
| `collections.created_at` | base | `collections.created_at` | unix seconds → ms |
| `collections.platform` | added | `collections.platform` | verbatim |
| `collections.external_id` | added | `collections.external_id` | verbatim |
| `collections.ig_name` | added | `collections.source_name` | renamed column, verbatim |
| **`post_collections`** (table) | | **post_collections** | memberships of merged posts and collections are unioned |
| `post_collections.post_id` | base | `post_collections.post_id` | the parent post's new id (merged duplicates point to the kept post) |
| `post_collections.collection_id` | base | `post_collections.collection_id` | the collection's new id |
| `post_collections.added_at` | base | `post_collections.added_at` | unix seconds → ms |
| **`post_tags`** (table) | | **post_tags** | one row per (post, tag, source) |
| `post_tags.post_id` | base | `post_tags.post_id` | the parent post's new id (merged duplicates point to the kept post) |
| `post_tags.tag_norm`, `tag_form` | base | `post_tags.tag_norm` / `post_tags.tag_form` | verbatim |
| `post_tags.tier` | added | `post_tags.source, post_tags.tier` | manual → source 'manual'; general/specific → source 'ai' with the tier; NULL → source 'ai', tier NULL |
| **`post_entities`** (table) | | **post_entities** | verbatim |
| `post_entities.post_id` | base | `post_entities.post_id` | the parent post's new id (merged duplicates point to the kept post) |
| `post_entities.ent_norm`, `ent_form` | base | `post_entities.ent_norm` / `post_entities.ent_form` | verbatim |
| **`post_facets`** (table) | | **dropped** | derived index of posts.ai_web_json.facets (desktop applyAiAnalysis); rebuilt from ai_web_json, which is carried verbatim |
| `post_facets.post_id`, `facet`, `value` | base | dropped | as the table |
| **`tag_alias`** (table) | | **tag_alias** | verbatim; created_at = install time |
| `tag_alias.alias_norm`, `canonical_norm`, `canonical_form` | base | `tag_alias.<same>` | verbatim |
| `tag_alias.status` | added | `tag_alias.status` | verbatim |
| **`tag_cluster`** (table) | | **tag_cluster** | verbatim |
| `tag_cluster.id`, `label`, `label_norm`, `status` | base | `tag_cluster.<same>` | verbatim |
| `tag_cluster.run_id` | base | `tag_cluster.run_id` | verbatim (already ms) |
| `tag_cluster.created_at`, `updated_at` | base | `tag_cluster.<same>` | unix seconds → ms |
| **`tag_cluster_membership`** (table) | | **tag_cluster_membership** | verbatim |
| `tag_cluster_membership.tag_norm`, `cluster_id` | base | `tag_cluster_membership.<same>` | verbatim |
| **`web_snapshots`** (table) | | **web_captures, web_capture_assets** | one older version of a site per row |
| `web_snapshots.id` | base | `web_captures.id` | renumbered |
| `web_snapshots.post_id` | base | `web_captures.post_id` | the parent post's new id (merged duplicates point to the kept post) |
| `web_snapshots.captured_at` | base | `web_captures.captured_at` | unix seconds → ms |
| `web_snapshots.title` | base | `web_captures.title` | verbatim |
| `web_snapshots.web_pages_json` | base | `web_captures.pages_json, web_capture_assets` | file paths → CAS assets; the JSON keeps text and probes |
| `web_snapshots.web_palette_json`, `web_fonts_json`, `web_tech_json`, `web_awards_json` | base | `web_captures.palette_json` / `fonts_json` / `tech_json` / `awards_json` | verbatim |
| `web_snapshots.web_meta_json` | base | `web_captures.meta_json, traits_json, engine, viewport, favicon_object, web_capture_assets` | as posts.web_meta_json |
| `web_snapshots.ai_description`, `ai_tags_json`, `ai_model`, `ai_status`, `ai_analyzed_at`, `ai_category`, `ai_content_type`, `ai_entities_json`, `ai_keywords_json`, `ai_language`, `ai_save_reason` | base | `web_captures.ai_snapshot_json` | frozen AI layer of the version, verbatim inside the JSON |
| `web_snapshots.created_at` | base | `web_captures.created_at` | unix seconds → ms |
| `web_snapshots.ai_web_json` | added | `web_captures.ai_snapshot_json` | frozen AI layer of the version, verbatim inside the JSON |
| **`jobs`** (table) | | **dropped** | desktop queue mirror; the web derives pending work from per-item state and keeps jobs in the control DB (§2.12) |
| `jobs.kind`, `key`, `post_id`, `payload`, `status`, `progress`, `error`, `attempts`, `created_at`, `updated_at` | base | dropped | as the table |
| **`downloads`** (table) | | **dropped** | dead table, never written or read (DATA-57) |
| `downloads.id`, `post_id`, `asset_type`, `status`, `progress`, `error`, `started_at`, `completed_at` | base | dropped | as the table |
| `sqlite_*` | | dropped | SQLite bookkeeping (AUTOINCREMENT counters, statistics); the web schema renumbers rows |

## Decisions

Taken while building the reader. They follow the plan, and the points the plan leaves open were settled plan-consistently.

| # | Decision | Why |
|---|---|---|
| S1-1 | **Web identity.** `native_id` is the full SHA-1 (40 hex) of the desktop normalization with `http://` / `https://` removed. `key` is `web_` + the first 20 hex characters (80 bits). The hash is computed from `web_url`, else `web_final_url`, else `post_url`. Other schemes keep their scheme. | §2.8 asks for a scheme-less hash and `web_<sha1:20>`. `web_url` is the pasted URL, which the desktop also hashes (`db.webPostId(url)`), so re-adding the same paste finds the same site. Keeping other schemes means `ftp://x` never collides with a website. |
| S1-2 | **Instagram decode order.** First `id == shortcode` (shortcode); then `<pk>_<owner>`; then a decimal pk; then any other alphabet string, decoded. The pk is canonical decimal: no leading zeros, never 0, any length up to a 64-character shortcode. | Every desktop fallback stores the shortcode as both `id` and `shortcode`. That disambiguates an all-digit shortcode. |
| S1-3 | **Duplicate kept row.** Rank by archived files present, then AI, then user layer, then more files, then id form (composite, then pk, then shortcode), then the oldest import. Collections keep the lowest id. | §4.2 order, made total so the result is deterministic. |
| S1-4 | **Manual keys.** A deterministic ULID: its time part is `imported_at`, its random part the first 80 bits of `SHA-1("shelfy-manual:" + legacy id)`. | A re-run or a resumed migration yields the same keys. |
| S1-5 | **`post_facets` dropped.** The rows are rebuilt from `posts.ai_web_json` (carried verbatim). The plan fails if any row cannot be rebuilt. | It is a derived index on the desktop. The rebuild reproduces 41 / 41 rows on the reference library. Where the index lives in the web schema is open item OI-1. |
| S1-6 | **Dates.** Strict ISO 8601 with a zone, where a bare date means UTC midnight; anything else becomes NULL. Epoch columns are converted seconds → ms. A value already in ms is kept; anything outside 2000–2100 becomes NULL. | JavaScript reads a zone-less date-time in the desktop's local zone, which is unknown here. The reference has none. |
| S1-7 | **Site files.** All 11 kinds of path inside `web_pages_json` / `web_meta_json` become CAS references: screenshot, hero, chunk (band, or filmstrip on a scroll-jacked page), section, footer, og image, favicon, scroll video, video preview, video poster. Non-path values (URLs) are ignored. | Capture v2 writes all of them (`electron/weborchestrator.ts`). The desktop's own delete paths know only `screenshotPath` and `chunks`. |
| S1-8 | **What counts as video.** `posts.video_path`, video slides and the site scroll video are uploaded only with `--with-videos`. Manual originals are always uploaded. | §4.2 for platform videos. A manual upload cannot be fetched again. |
| S1-9 | **Opening the library.** Open `immutable` when there is no `-wal` or `-journal` file; otherwise open shared read-only (SQLite then updates the shared `-shm` index, as any reader does). `plan` warns when a `-wal` is present; it does not refuse. | Zero side files for a closed desktop app. A consistent snapshot either way. |

## Open items

[P1-19](../phases/P1.md) closes every item below. OI-9 and OI-10 do not show in the reference library's numbers; they come from reading the code.

| # | Owner | Item |
|---|---|---|
| OI-1 | T3 | **Facets have no table in §2.7.** The design-facet filters and "similar sites" (WEB) need an index. Option 1: add `post_facets(post_id, facet, value)` and fill it from `ai_web_json` on install and on every AI write. Option 2: derive the facets at query time. The rebuild rule is ported in `legacy::web::derived_facets`. |
| OI-2 | T3 | **`media_objects.role` lacks three capture roles: hero, video preview and video poster.** Proposal: hero → `screenshot` plus `web_captures.hero_object`; video preview → `preview`; poster → `poster`. Alternatively, extend the role list. |
| OI-3 | T3 | **`web_captures` has columns with no desktop source:** `status` (NOT NULL), `partial`, `engine` and `viewport`. Proposal: `status = 'done'`; `partial` = `meta.capture.skipped` is non-empty; `engine` and `viewport` from `meta.capture`. T3 fixes the `status` vocabulary. |
| OI-4 | T3 | **rusqlite `bundled` compiles SQLite with `SQLITE_DEFAULT_FOREIGN_KEYS=1`,** so every connection enforces foreign keys unless told otherwise. Set the pragma explicitly in `UserDb`, and mind the cascades during a bulk install. |
| OI-5 | T3 | **Text posts and `media_count`.** 173 text posts have no slides but `media_count` 1 (the desktop default). Choose 0 or 1, since `media_count` is NOT NULL DEFAULT 1. `tag_alias.created_at` (NOT NULL) has no desktop source: use the install time. |
| OI-6 | T9 | **264 of 504 `posts.video_path` files are missing.** The desktop reconciles paths (DATA-34) only before a cover repair. Map them as "video not kept" (`video_object_id` NULL), not as an error. |
| OI-7 | T9 | **Covers to archive right after install.** 220 Instagram cover URLs of posts with no local cover were still valid at run time. Archive them right after install, before they expire (SPIKE-2: `server` mode). The 1,641 expired ones become extension `refresh_media` tasks; the 176 X covers are archived server-side. |
| OI-8 | T9 | **`run` must not race the desktop.** A `-wal` next to the library means a connection is, or was, open. Require Shelfy closed, or snapshot first with the SQLite online backup API. The reader's single read transaction already keeps the plan consistent. |
| OI-9 | T9 | **Long private Instagram shortcodes.** §2.8 says to decode the whole shortcode. The extension (T5, `extension/src/identity.ts`) instead keeps an `igsc_` alias, because long private shortcodes may not decode to the pk. A row keyed only by such a shortcode would then miss a merge with its pk-keyed twin. It would not merge wrongly. All 3,997 reference shortcodes are 11 characters, so the question needs a private-account sample (O1). |
| OI-10 | T9 | **Settings live outside `shelfy.sqlite`.** They are in renderer localStorage (LevelDB) and userData JSON files, and §4.2 keeps only the language and the asset preferences. The plan does not read them yet. |
| OI-11 | T9 | **19 orphan files (987 MiB, mostly 7 videos) are referenced by no row.** They are not migrated. `plan` counts them per directory; listing them, so the owner can delete them on the desktop, is left to `run`. |
| OI-12 | desktop | **Site deletion leaks capture-v2 files.** `getWebSiteFilePaths` and `snapshotPagePaths` (`electron/db.ts`) remove only `screenshotPath`, `chunks` and the `post_media` paths, so bands, sections, footers, og images, favicons and scroll videos stay on disk (12 orphan site files here). `posts.video_path` is not cleared when its file disappears. Both are outside the port; the migration handles both cases. |

**Status after T9 (migration v0).** What `shelfy-migrate run` and the install do today, and what P1-19 still owns:

| # | T9 | Left for P1-19 |
|---|---|---|
| OI-6 | A missing kept video means "video not kept": `video_object_id` stays NULL and the run counts it (`repairs.videosMissing`). A still stored as a video slide's file becomes that slide's poster, never a kept video. | — |
| OI-7 | Nothing is fetched. Each post records what it needs: `archive_state` (`client` for an expired Instagram cover, `pending` or `partial` otherwise) and `cover_url_expires_at` from `oe`. The install report counts valid and expired Instagram covers, X and Pinterest covers, and pending image slides. | The archive drain (valid IG covers first) and the extension `refresh_media` tasks (P2). |
| OI-8 | A library with a `-wal` or `-journal` file is copied with the online backup API into the work directory, and `run` reads the copy. | Refusing while the desktop app writes (the plan's "close Shelfy"), if wanted. |
| OI-9 | Unchanged: keys come from the legacy reader. | The private-account sample (O1). |
| OI-10 | Settings are not read. The install keeps the web library's own settings. | Language and asset preferences from the desktop. |
| OI-11 | `run --list-orphans` prints the orphan files relative to `assets/`, for the owner only. | — |

**Status after P1-19 (migration complete).** Every open item is closed: done in code, or decided with the phase that acts on it.

| # | Closed by | How |
|---|---|---|
| OI-1 | decision (P1-19) | No facet table in P1. `posts.ai_web_json` is carried verbatim, and the rebuild rule is ported (`legacy::web::derived_facets`); `plan` fails if a desktop facet row cannot be rebuilt. The design-facet filters arrive with the site captures (P4), which add the index, filled by that rule on install and on every AI write. |
| OI-2 | T9 | A hero is a `screenshot` object plus `web_captures.hero_object`; a video preview is `preview`; a poster is `poster`. The role list is unchanged. |
| OI-3 | T9 | `status = 'done'` (the desktop keeps finished captures only); `partial` when `meta.capture.skipped` is non-empty; `engine` and `viewport` from `meta.capture`. |
| OI-4 | T3, P1-19 | `UserDb` connections and the bundle builder set `foreign_keys = ON` explicitly. A bundle with a broken foreign key is refused, by the CLI and by the server, and a merge writes through `UserDb` with the keys enforced. |
| OI-5 | T9 | A text post keeps `media_count` 1, the desktop default (the insert stores at least 1). `tag_alias.created_at` is the bundle's build time. |
| OI-6 | T9 | A missing kept video is "video not kept": `video_object_id` stays NULL and the run counts it (`repairs.videosMissing`). |
| OI-7 | T9, P2 | Each post records its archive state and its cover URL's expiry; the install report counts valid and expired Instagram covers. The archive drain (valid Instagram covers first, by soonest expiry) and the extension's `refresh_media` tasks are P2's. Covers whose signature expires before P2 lands become extension tasks. |
| OI-8 | P1-19 | `plan` and `run` refuse while another process holds the library open (exit status 4): on Unix the kernel is asked, with `F_GETLK`, for the read locks every open WAL connection holds on the database file and on its `-shm`; elsewhere a `-wal` file stands for one. `--allow-open` reads a snapshot taken with the online backup API. |
| OI-9 | decision (P1-19) | The migration keeps the full-shortcode decode of §2.8: a long private shortcode never merges wrongly, at worst it misses a merge. `plan` counts such rows (`identity.igLongShortcodes`); the reference library has 0. Matching the extension's `igsc_` aliases is P2's sanitizer's job, and needs O1's private-account sample. |
| OI-10 | P1-19 | The desktop's language (`app:language`) and asset types (`download:assetTypes`) are read from its localStorage, with a read-only LevelDB reader that never opens the database the LevelDB way. They become the library's `language` and `archiveAssetTypes` settings; settings the web library already has win. |
| OI-11 | T9 | `run --list-orphans` prints the orphan files relative to `assets/`, for the owner only. |
| OI-12 | P1-19 (no port change) | A desktop issue. The migration handles both cases: orphan site files are counted and not migrated; a stale `video_path` is OI-6. |

## Deviations and assumptions

- **CLI.** The dry run follows the task:
  - `plan --db <path>` replaces §4.1's `--user-data <dir>`.
  - `--media-root` defaults to the library's directory when that holds `assets/`.
  - Added: `--json`, `--redact` and the `mapping` command.
  - Exit codes: 0 pass, 1 criteria failed, 2 usage, 3 unreadable library.
  - The "result against the server quota" of §4.1 needs `login`, so it moves to T9.
- **Dependencies (workspace).** `rusqlite` 0.40 (`bundled`, no default features: the same line as T3), `url`, `sha1`, `serde`, `serde_json`, `thiserror`, `clap` and `anyhow`; `proptest` and `tempfile` for tests. `cargo deny check` passes with no warnings. `clap` is not in §2.21: it was chosen for a CLI that will grow `login` and `run`. The ULID and the shortcode bignum are small hand-rolled codecs (no `ulid` or `num-bigint`).
- **Consistency with the extension.** The Rust keys match `extension/src/identity.ts` on every realistic input. They differ only on unrealistic ones:
  - pks with leading zeros: Rust strips them;
  - an all-digit shortcode used as an id: Rust trusts `id == shortcode`;
  - non-numeric pin ids: Rust accepts URL-safe ids and reports them.
- **Test fixtures.** `legacy::fixture` exposes the desktop DDL (current and early) as doc-hidden constants, so other crates can build synthetic legacy files.
