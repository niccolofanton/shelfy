# Library data, persistence & IPC surface — feature index
> Scope: `electron/db.ts`, `electron/ipc.ts`, `electron/preload.ts`, `types/domain.d.ts`, `types/electron-api.d.ts`, `types/globals.d.ts`, `electron/jobstore.ts`, `electron/archive-utils.ts`, `electron/thumbs.ts`, `electron/preview-cache.ts`, `electron/preview-repair.ts`, `electron/serverUtils.ts`, `electron/logger.ts`, `src/hooks/usePosts.ts`, `src/hooks/useCollections.ts`, `src/lib/postFilters.ts`, `src/lib/asset.ts`, the `asset://` protocol in `electron/main.ts`. Snapshot: working tree of `dev`, 2026-10-02.

## Overview
- One SQLite file (`<userData>/shelfy.sqlite`) is opened by the Electron **main process** through `better-sqlite3`: a single synchronous connection in WAL mode. `electron/db.ts` (6.2k lines, no ORM) holds the schema, the migrations, every query, the keyword-search ranking and the tag analytics. 12 tables, 12 explicit indexes, no FTS, no triggers, no views.
- The renderer never runs SQL. React hooks call `window.electronAPI.*` (`electron/preload.ts`, `contextBridge`) → `ipcRenderer.invoke` → `ipcMain.handle` in `electron/ipc.ts` → db or queue managers. The main process pushes state back with `webContents.send` (15 event channels). The bridge exposes 155 invoke channels plus the 15 events.
- Ingest paths:
  - social sync and selection (area 2) → `db:bulkUpsert`
  - website capture (area 4) → `createWebPlaceholder` / `upsertWebReference`
  - manual bookmarks (area 4) → `addManualBookmark`
  - JSON import
- Read path: the Gallery `usePosts` hook → `db:getPosts`. `db:getPosts` takes filters, LIKE-based keyword search with IDF relevance, and sort order. The grid stays live through the `interceptor:newPosts` refresh signal and single-row patches.
- Media handling:
  - The DB stores **absolute local paths**.
  - Files are served by the privileged `asset://` scheme, which also does on-the-fly cached thumbnails (`?w=`).
  - Each row carries a ~24px blur data URI.
  - Small preview covers are fetched automatically into `assets/previews/`.
- Durability: the download, analyze and web queues mirror their state into the `jobs` table and are recovered at boot.
- Not present:
  - no favorite / pinned / hidden / archived / read flags (no columns, no IPC)
  - no soft delete or undo
  - no DB backup or restore other than the JSON export
  - no per-user scoping (single-user desktop)

## Schema
Defined in `electron/db.ts:305` (`SCHEMA`). Columns added later come from `migrate()` (`electron/db.ts:547`). SQLite types; "JSON" means a TEXT column holding a JSON string that is parsed in JS. JSON1 functions are not used.

### `posts` — one row per saved item (social post, website, manual bookmark)
| column | type | notes |
|---|---|---|
| id | TEXT PK | Platform-native id: IG `item.id`, `pk` or shortcode (`electron/webview-injected.ts:187`); tweet id; Pinterest pin id; `web:`+sha1(normalized URL) (`db.ts:2311`); `manual:`+UUID (`electron/bookmarks.ts:99`). No platform prefix for social ids. |
| platform | TEXT NOT NULL | `instagram` / `twitter` / `pinterest` / `web` / `manual` |
| shortcode | TEXT | IG shortcode (no index) |
| post_url, profile_url, author_username, author_name | TEXT | for web posts: final URL, `https://<domain>`, domain, title |
| text | TEXT | caption; for web posts: title + meta description + hero text (≤20k chars) |
| thumbnail_url | TEXT | remote cover URL (IG CDN URLs are signed and expire) |
| media_type | TEXT | `image` / `video` / `carousel` / `images` / `text` / `website` / `file` |
| timestamp | TEXT | ISO-8601, sorted lexicographically. Falls back to import time on insert; legacy `''`/NULL possible (`idx_posts_timestamp` DESC) |
| thumbnail_path, image_path, video_path | TEXT | absolute local paths (downloads, web screenshots, manual originals) |
| preview_path | TEXT | auto-cached 640px cover (also added by ALTER); not counted as "downloaded" |
| media_count | INTEGER DEFAULT 1 | number of slides |
| imported_at | INTEGER DEFAULT unixepoch() | epoch seconds |
| ai_description, ai_status, ai_model, ai_category, ai_content_type, ai_language, ai_save_reason | TEXT (ALTER) | `ai_status` is NULL / `pending` / `analyzing` / `done` / `error` (`idx_posts_ai_status`); `ai_model` is `manuale` for manual edits |
| ai_tags, ai_entities, ai_keywords | TEXT JSON `string[]` (ALTER) | **source of truth** for AI tags and entities |
| ai_analyzed_at | INTEGER (ALTER) | epoch seconds |
| user_note | TEXT (ALTER) | personal note |
| user_tags | TEXT JSON `string[]` (ALTER) | manual tags (display source of truth) |
| web_url, web_domain, web_final_url | TEXT (ALTER) | `idx_posts_web_domain` |
| web_palette_json, web_fonts_json, web_tech_json, web_awards_json, web_pages_json | TEXT JSON arrays (ALTER) | `web_pages_json` = per-page `{url,pageType,title,meta,jsonld,contentText,screenshotPath,chunks[]}` |
| web_meta_json | TEXT JSON object (ALTER) | includes `singlePage` |
| web_captured_at | INTEGER (ALTER) | epoch seconds |
| thumb_blur | TEXT (ALTER) | ~24px JPEG data URI; `''` = tried, ineligible; NULL = not generated |

### Other tables
| table | columns (type) | keys / indexes | purpose |
|---|---|---|---|
| `post_media` | post_id TEXT NOT NULL FK→posts CASCADE; position INTEGER NOT NULL; media_type TEXT NOT NULL (`image`/`video`/`file`); source_url TEXT (remote URL; **local original path** for manual; page URL for web); local_path TEXT | PK(post_id, position); `idx_post_media_post`(post_id), redundant with the PK | ordered slides (carousels, multi-image tweets, web pages) |
| `collections` | id INTEGER PK AUTOINCREMENT; name TEXT NOT NULL; color TEXT NOT NULL DEFAULT `#3d5afe`; platform TEXT (NULL = manual, `instagram`/`pinterest` = native folder or board); external_id TEXT; ig_name TEXT; created_at INTEGER DEFAULT unixepoch() | no UNIQUE constraints, no ordering column | user "sources", folder tags |
| `post_collections` | post_id TEXT FK CASCADE; collection_id INTEGER FK CASCADE; added_at INTEGER DEFAULT unixepoch() | PK(post_id, collection_id); `idx_post_collections_collection` | many-to-many membership |
| `post_tags` | post_id TEXT FK CASCADE; tag_norm TEXT NOT NULL (trim + lowercase, alias-canonicalized); tag_form TEXT NOT NULL (display casing); tier TEXT (ALTER: `general`/`specific`/`manual`/NULL legacy) | PK(post_id, tag_norm); `idx_post_tags_norm` | derived index over `ai_tags` + `user_tags` |
| `post_entities` | post_id TEXT FK CASCADE; ent_norm TEXT NOT NULL; ent_form TEXT NOT NULL | PK(post_id, ent_norm); `idx_post_entities_norm` | derived index over `ai_entities` |
| `tag_alias` | alias_norm TEXT PK; canonical_norm TEXT NOT NULL; canonical_form TEXT NOT NULL; status TEXT NOT NULL DEFAULT `accepted` (ALTER; `proposed`/`accepted`) | no FK | synonym canonicalization (area 3) |
| `tag_cluster` | id INTEGER PK AUTOINCREMENT; label TEXT NOT NULL; label_norm TEXT; status TEXT NOT NULL DEFAULT `proposed`; run_id INTEGER (Date.now() ms); created_at, updated_at INTEGER | — | LLM-named tag clusters (area 3) |
| `tag_cluster_membership` | tag_norm TEXT PK; cluster_id INTEGER NOT NULL FK→tag_cluster CASCADE | `idx_tag_cluster_membership_cluster` | a tag belongs to at most one cluster |
| `web_snapshots` (created in `migrate`, `db.ts:645`) | id INTEGER PK AUTOINCREMENT; post_id TEXT NOT NULL FK CASCADE; captured_at INTEGER NOT NULL; title; web_pages/palette/fonts/tech/awards/meta `_json` TEXT; ai_description, ai_tags_json, ai_model, ai_status, ai_analyzed_at INTEGER, ai_category, ai_content_type, ai_entities_json, ai_keywords_json, ai_language, ai_save_reason; created_at INTEGER NOT NULL | `idx_web_snapshots_post`(post_id, captured_at DESC) | older versions of a captured site; the `posts` row holds the current one |
| `jobs` | kind TEXT NOT NULL (`download`/`analyze`/`web`); key TEXT NOT NULL; post_id TEXT (no FK); payload TEXT JSON; status TEXT NOT NULL; progress REAL DEFAULT 0; error TEXT; attempts INTEGER DEFAULT 0; created_at, updated_at INTEGER DEFAULT unixepoch() | PK(kind, key); `idx_jobs_kind_status` | durable mirror of the in-memory queues |
| `downloads` | id INTEGER PK AUTOINCREMENT; post_id FK CASCADE; asset_type TEXT NOT NULL; status TEXT NOT NULL DEFAULT `pending`; progress REAL; error; started_at; completed_at | — | **dead**: never written or read |

### Other schema notes
- **Foreign keys:** all post children cascade on delete (`post_media`, `downloads`, `post_collections`, `post_tags`, `post_entities`, `web_snapshots`). `tag_cluster_membership` cascades from `tag_cluster`. Enforcement: `PRAGMA foreign_keys = ON` (`db.ts:478`).
- **FTS5 / triggers / views / JSON1:** none. Search is `LIKE '%…%' ESCAPE '\'` plus correlated `EXISTS` on `post_tags`.
- **User-defined function:** `word_match(haystack, needle)` is a deterministic JS function (Unicode-aware whole-word regex, case-insensitive), registered at `db.ts:533`. It is used in relevance scoring.
- **Pragmas set:**
  - `journal_mode = WAL` (`db.ts:477`)
  - `foreign_keys = ON`
  - `user_version` (read `db.ts:548`, written `db.ts:775`)
  - `table_info(...)` for the column guards
  - `wal_checkpoint(TRUNCATE)` on close (`db.ts:500`)
  - Not set: `busy_timeout`, `synchronous`, `cache_size`. One connection, main process only. `unixepoch()` requires SQLite ≥ 3.38.
- **Versioning:** `SCHEMA_VERSION = 3` (`db.ts:545`).
  - Every boot: `CREATE … IF NOT EXISTS`, then `ALTER TABLE ADD COLUMN` guarded by `PRAGMA table_info`, then the late indexes and `web_snapshots`.
  - One-shot repairs gated by `user_version` < 1 / 2 / 3 (DATA-03).
  - No down-migrations and no migrations table.
- **Vectors / embeddings:** none are stored in the DB. During tag-cluster regeneration (area 3), 384-dim L2-normalized multilingual-e5-small vectors are computed in memory for tag display forms (`db.ts:4280-4316`, dims at `electron/binaries.ts:197`) and passed to a `worker_threads` worker via `workerData` (`db.ts:4148`). They are never persisted.
- **Row shapes:** `rowToPost` (`db.ts:891`) maps snake_case to the camelCase `Shelfy.Post` (`types/domain.d.ts:56`). JSON columns are parsed defensively (bad JSON becomes `[]` / null).

## Features

### DATA-01 · SQLite database lifecycle
- **What:**
  - Opens `shelfy.sqlite` with a single synchronous better-sqlite3 handle and sets WAL and foreign keys.
  - Applies the schema, migrations and the UDF.
  - If the DB fails to open, shows a fatal dialog and quits.
  - On quit, runs a WAL checkpoint (TRUNCATE) and closes the handle.
- **Entry points:** `electron/main.ts:587` → `initialize` `electron/db.ts:469`; error dialog `electron/main.ts:592`; `before-quit` `electron/main.ts:815` → `close` `electron/db.ts:497`
- **Data:** `shelfy.sqlite`, `-wal`, `-shm`
- **Local deps:** SQLite (better-sqlite3 native module), filesystem, Electron `app` / `dialog`
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — managed Postgres with a pooled async driver; no process-wide singleton.

### DATA-02 · Schema creation & additive migrations
- **What:**
  - Runs idempotent `CREATE IF NOT EXISTS` on every boot.
  - Adds missing columns via `ALTER ADD COLUMN`, guarded by `PRAGMA table_info`:
    - posts: `media_count`, 11 `ai_*`, 2 `user_*`, 10 `web_*`, `thumb_blur`, `preview_path`
    - collections: `platform`, `external_id`, `ig_name`
    - `post_tags.tier`, `tag_alias.status`
  - Creates `web_snapshots`; the version lives in `user_version`.
- **Entry points:** `migrate` `electron/db.ts:547`; `SCHEMA` `electron/db.ts:305`
- **Data:** all tables
- **Local deps:** SQLite PRAGMAs
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — versioned SQL migrations with a single baseline schema.

### DATA-03 · One-shot boot data repairs
- **What:** One-time repairs gated by `user_version`:
  - v1: rewrite `x.com//status/` URLs; backfill `post_media` from legacy columns; backfill `post_tags` / `post_entities` from JSON.
  - v2: derive IG dates from the shortcode.
  - v3: same again, plus undated rows fall back to `imported_at`.
- **Entry points:** `electron/db.ts:712-772`; `backfillInstagramTimestamps` `db.ts:781`; `backfillDerivedTags` `db.ts:801`
- **Data:** `posts.post_url` / `timestamp`, `post_media`, `post_tags`, `post_entities`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only) — replicate only inside a one-time desktop→cloud importer.

### DATA-04 · Social post bulk upsert (merge, never clobber)
- **What:**
  - Batch `INSERT OR IGNORE` keyed by `posts.id`. An empty date becomes "now" on insert.
  - For existing rows:
    - Refreshes metadata only while no local path exists.
    - Never overwrites a known date.
    - Merges slides without touching downloaded `local_path`.
    - Applies imported AI fields only if the row is unanalyzed (or `overwriteAi` is set).
  - Afterwards queues preview covers and emits `interceptor:newPosts`.
- **Entry points:** `src/hooks/useBrowserSync.ts:247`, `src/hooks/useBrowserSelection.ts:138` → `db:bulkUpsert` (`electron/ipc.ts:366`) → `bulkUpsert` `electron/db.ts:2144` (`upsertPost` `db.ts:2135` is only used by tests)
- **Data:** posts (all columns except paths), `post_media`, `post_tags` / `post_entities` (through AI fields)
- **Local deps:** SQLite (synchronous transaction)
- **External calls:** none here; the cover fetch is DATA-44
- **Status:** shipped. Capture is owned by area 2. The field whitelist lives only in the renderer (`src/lib/browserSanitize.ts:68`).
- **Web port:** api+db — `POST /posts:batchUpsert` with a server-side field whitelist and `ON CONFLICT` upserts.

### DATA-05 · Already-in-library lookup
- **What:**
  - Maps scraper DOM keys (IG shortcode or tweet id) to `[{key,id}]` by matching `posts.id` OR `posts.shortcode`, in chunks of 500.
  - The selection overlay uses it to mark posts that are already saved.
  - The id-only `existingIds` variant has no caller (DATA-57).
- **Entry points:** `src/hooks/useBrowserIntercept.ts:161` → `db:savedByKeys` (`electron/ipc.ts:169`) → `savedByKeys` `electron/db.ts:1996`
- **Data:** `posts.id`, `posts.shortcode` (shortcode is unindexed, so this is a scan)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped (consumer is area 2)
- **Web port:** api+db — `POST /posts:lookup` with an index on (user_id, platform, shortcode).

### DATA-06 · Per-slide media model
- **What:**
  - Each post has ordered slides of type image, video or file.
  - If the parser provides no slides, one slide is synthesized from the thumbnail.
  - New posts replace their slides; existing posts merge them (add missing, refresh the URL only where nothing is downloaded).
  - `media_count` = number of slides. Slides are attached to every post read.
- **Entry points:** `deriveMedia` `electron/db.ts:994`; `replacePostMedia` `db.ts:2100`; `mergePostMedia` `db.ts:2113`; `attachMedia` `db.ts:1039`
- **Data:** `post_media`, `posts.media_count`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage — `local_path` becomes an object key.

### DATA-07 · Website rows (placeholder-first, URL-derived id)
- **What:**
  - A website is a posts row with platform `web` and media_type `website`.
  - Id = `web:` + sha1 of the normalized URL. Normalization: lowercase host, drop `www.`, hash, `utm_*`, `gclid`, `fbclid`, `ref` and trailing slash.
  - A placeholder row is created instantly.
  - `upsertWebReference` later archives the previous version, replaces the slides and force-writes `web_*` plus the hero screenshot paths.
- **Entry points:** `src/components/AddSiteModal.tsx:78` → `web:add` (`electron/ipc.ts:674`) → `electron/weborchestrator.ts:1199` / `:986` → `createWebPlaceholder` `electron/db.ts:2483`, `upsertWebReference` `db.ts:2422`, `webRefToPost` `db.ts:2338`
- **Data:** `posts.web_*`, `text`, AI mapping (industry → `ai_category`, purpose → `ai_content_type`), `post_media`
- **Local deps:** SQLite, Node `crypto`
- **External calls:** none here (capture pipeline = area 4)
- **Status:** shipped
- **Web port:** api+db — same deterministic id, scoped per user. The pipeline is area 4.

### DATA-08 · Website version (snapshot) storage
- **What:**
  - A re-capture archives the prior web and AI state into `web_snapshots`.
  - Supports: list versions (newest first) and per-post counts.
  - Delete one version, together with its screenshots.
  - "Delete latest report": promotes the newest snapshot, or resets the row to a placeholder that keeps manual tags.
  - Full site delete unions the screenshot paths of every version.
- **Entry points:** `src/views/AiWebsites.tsx:1907,1813,2021-2037` → `web:getSnapshots` / `web:snapshotCounts` / `web:deleteSnapshot` / `web:deleteLatestReport` / `web:deleteSites` (`electron/ipc.ts:752-821`) → `electron/db.ts:5790-6058`
- **Data:** `web_snapshots`, `posts.web_*` / `ai_*`, `post_media`, `post_tags` (AI tiers), `assets/web/*.webp`
- **Local deps:** SQLite, filesystem (`unlinkSync`)
- **External calls:** none
- **Status:** shipped (UI is area 4)
- **Web port:** api+db + object-storage — version rows with a per-version object prefix; deletes run as a background purge.

### DATA-09 · Manual bookmark row insert
- **What:**
  - Inserts a `manual` post (id `manual:<UUID>`) from files already written by `bookmarks.ts`.
  - One slide per file (image / video / file): `local_path` = the renderable file or preview; `source_url` = path of the copied original.
  - Then writes the note and manual tags (DATA-28/29).
- **Entry points:** `src/components/AddBookmarkModal.tsx:233` → `bookmark:add` (`electron/ipc.ts:702`) → `electron/bookmarks.ts:91` → `addManualBookmark` `electron/db.ts:2741`
- **Data:** `posts` (platform `manual`), `post_media`, `user_note` / `user_tags`; files at `assets/{images,videos,files,thumbnails}/manual<hex>-<i>.*`
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped (file handling is area 4)
- **Web port:** object-storage + api+db — presigned direct uploads first, then row creation.

### DATA-10 · Paged gallery listing
- **What:**
  - Returns a window of posts (default 50, `limit`/`offset`) plus the unpaged `total` from a separate COUNT(*). Each post comes with slides and collection ids.
  - The hook appends only the missing page on scroll, deduping by id.
  - It replaces the list on filter change and reconciles by id on live refresh.
  - Canvas mode loads a pool of 500.
- **Entry points:** `src/views/Gallery.tsx:258` → `src/hooks/usePosts.ts:224` → `db:getPosts` (`electron/ipc.ts:157`) → `getPosts` `electron/db.ts:1818`. Also called from `src/hooks/useAiTags.ts:450` (area 3) and `src/views/AiWebsites.tsx:1802` (area 4, `limit` 500).
- **Data:** `posts` (`SELECT *`, including `web_pages_json`), `post_media`, `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — `GET /posts` with a keyset cursor on (timestamp, id) and list-projected columns.

### DATA-11 · Source filters (platform / web-vs-social / collection)
- **What:**
  - `platform` equality.
  - `source` bucket: `web` (platform = 'web') or `social` (platform != 'web').
  - `collectionId` via a `post_collections` subquery.
  - Platform and collection are picked from the sidebar or the filter drawer.
- **Entry points:** `src/App.tsx:403` → `src/views/Gallery.tsx:239` → `toApiFilters` `src/lib/postFilters.ts:45` → `db:getPosts` → `buildPostFilter` `electron/db.ts:1423-1441`
- **Data:** `posts.platform` (indexed), `post_collections` (indexed)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped. `source` is only sent by AI search (area 3); the Gallery always sends `all`.
- **Web port:** api+db.

### DATA-12 · Attribute filters (media type, download status, AI-tagged, analyzed)
- **What:**
  - `mediaType` equality.
  - `downloadStatus`:
    - `downloaded`: any of the thumbnail / image / video paths is set
    - `missing`: the UI's `linkonly` option
  - `aiTagged` (`tagged` / `untagged`): EXISTS on AI-tier `post_tags` rows; manual tags don't count.
  - `analyzedStatus` and `missingOnly` exist but no UI uses them.
- **Entry points:** `src/components/FilterDrawer.tsx:353,363,373` → `src/lib/postFilters.ts:50` → `db:getPosts` → `electron/db.ts:1443-1535,1693-1701`
- **Data:** `posts.media_type`, `*_path`, `ai_status`, `post_tags.tier`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped (`analyzedStatus` and `missingOnly` are unused parameters)
- **Web port:** api+db — keep "preview ≠ downloaded" semantics.

### DATA-13 · Tag / entity / category filters
- **What:**
  - Single `tag`: case-insensitive EXISTS on `post_tags`. Set by clicking a tag chip in the post modal; cleared from the chip in the filter bar.
  - `tags[]` with `tagMode` (and/or) and `entity`: used by the AI Tags explorer.
  - `category` / `contentType` equality: supported by the backend, but no UI sets them.
- **Entry points:** `src/components/postmodal/AiPanel.tsx:179` → `src/views/Gallery.tsx:1703`; chip `src/components/FilterBar.tsx:124`; `src/views/AiTags.tsx:446` → `db:getPosts` → `electron/db.ts:1448-1514`
- **Data:** `post_tags`, `post_entities` (both indexed by norm); `posts.ai_category` / `ai_content_type` (unindexed)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-14 · Keyword search with relevance ranking
- **What:**
  - Tokenizes the query into content terms: lowercase, Italian and English stopword list, minimum 3 characters except short terms like `3d` / `ai` / `ux`; falls back to the raw query.
  - A post matches through either:
    - a full-phrase LIKE across 8 columns, or
    - a per-term LIKE across 6 columns plus `post_tags`.
  - Score = sum over terms of a CASE weight, multiplied by the term's IDF (clamped to [1,3]), then recency:

    | match type | weight |
    |---|---|
    | exact tag | 6 |
    | whole-word tag / keyword | 5 |
    | whole-word description / note | 4 |
    | whole-word caption | 3.5 |
    | substring matches | 3 / 2 / 1 |
- **Entry points:** debounce (300 ms) `src/components/FilterBar.tsx:63` → `db:getPosts` → `electron/db.ts:1619-1691`; `termIdfWeights` `db.ts:1719`; `word_match` `db.ts:515`. The same query also backs `search:byText` (area 3).
- **Data:** `posts.text`, `author_username`, `shortcode`, `ai_description`, `ai_tags`, `ai_keywords`, `user_note`, `user_tags`; `post_tags`
- **Local deps:** SQLite (JS UDF, LIKE…ESCAPE)
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — rethink with Postgres FTS or pg_trgm plus unaccent (LIKE case semantics differ).

### DATA-15 · Suggested-concept filters (AND/OR)
- **What:**
  - Concept chips (suggested by `search:suggest`, area 3) are added as extra match blocks, scored like query terms.
  - OR (default) broadens the results; AND requires every block.
  - Concepts reset when the query changes.
- **Entry points:** `src/views/Gallery.tsx:333` (toggle), `:345` (mode) → `db:getPosts` (`concepts`, `conceptMode`) → `electron/db.ts:1654-1690`
- **Data:** same columns as DATA-14
- **Local deps:** SQLite
- **External calls:** none here (the suggestion model is area 3)
- **Status:** shipped (opt-in through the `aiSearchSuggestions` localStorage key)
- **Web port:** api+db.

### DATA-16 · Sort order
- **What:**
  - `newest` (default) or `oldest` by ISO `timestamp`.
  - Undated rows (NULL or '') always sort last; `id` is the tiebreaker so paging stays stable.
  - Relevance is sorted first whenever a search is active.
- **Entry points:** `src/views/Gallery.tsx:1120` → `db:getPosts` / `db:getPostIds` → `recencyOrder` `electron/db.ts:1811`
- **Data:** `posts.timestamp` (index DESC), `posts.id`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — timestamptz plus an index on (user_id, ts, id).

### DATA-17 · Select-all-matching id resolution
- **What:**
  - "Select all" checks the loaded cards instantly, then fetches **every** id matching the current filters (no cap, same WHERE and order).
  - The ids feed bulk assign, delete, analyze, download and clear-AI.
  - Also used by AI Tags "collect" (entity filter) and by collection delete-with-posts.
- **Entry points:** `src/views/Gallery.tsx:867,880`, `src/views/AiTags.tsx:773`, `electron/ipc.ts:433` → `db:getPostIds` (`electron/ipc.ts:160`) → `getPostIds` `electron/db.ts:1857`
- **Data:** post ids
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db (rethink) — server-side bulk operations "by filter" instead of round-tripping every id.

### DATA-18 · Fetch posts by id
- **What:**
  - Hydrates full posts for explicit ids, in input order (chunks of 500).
  - Used to patch one grid row after a download or analysis finishes, to open the post modal from activity / AI views, and to refresh after per-post actions.
- **Entry points:** `src/hooks/usePosts.ts:322`, `src/App.tsx:325`, `src/components/postmodal/ActionsMenu.tsx:109` → `db:getPostsByIds` (`electron/ipc.ts:163`) → `getPostsByIds` `electron/db.ts:1868`
- **Data:** `posts`, `post_media`, `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped (no cap on the number of ids)
- **Web port:** api+db — `GET /posts?ids=` with a cap.

### DATA-19 · Live library refresh push
- **What:**
  - The main process emits `interceptor:newPosts` for: new rows, website placeholders, a preview or blur becoming ready, a bookmark being added.
  - The renderer coalesces reloads: 400 ms quiet window, at least one reload every 2 s.
  - A completed download or analysis patches one row, unless the active filters could change which rows match.
  - Reloads are deferred while the view is hidden.
  - App also refreshes stats and the per-platform "new posts" badge.
- **Entry points:** emitters `electron/ipc.ts:148,175,386,390,739`, `electron/main.ts:649,673` → `src/hooks/usePosts.ts:295-390`, `src/App.tsx:575`
- **Data:** none (signals only)
- **Local deps:** Electron `webContents.send`
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push — per-user SSE or WebSocket with typed events (`posts.changed`, `job.updated`).

### DATA-20 · Library stats & counters
- **What:**
  - Reports: total posts; per platform (instagram / twitter / pinterest / web — `manual` is not broken out); per media type; downloaded count and per asset type.
  - Memoized for 5 s and invalidated on writes.
  - Feeds the sidebar counts, filter drawer, export modal and the Downloads view, which polls every 5 s while its queue is active.
- **Entry points:** `src/App.tsx:381`, `src/views/Settings.tsx:357`, `src/hooks/useDownloads.ts:117` → `db:getStats` (`electron/ipc.ts:172`) → `getStats` `electron/db.ts:2504`
- **Data:** `posts` (full-scan aggregates)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — per-user counters, cached or incrementally maintained.

### DATA-21 · List collections with counts
- **What:**
  - Returns every collection ordered by creation time (no reordering support).
  - Each comes with its live post count, its platform link and the original source name.
- **Entry points:** `src/hooks/useCollections.ts:26`, `src/components/PostModal.tsx:304` → `collections:list` (`electron/ipc.ts:398`) → `getCollections` `electron/db.ts:5329`
- **Data:** `collections`, `post_collections` (count via correlated subquery)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-22 · Create collection (manual or native-folder-linked)
- **What:**
  - Creates a named, colored bucket (default `#3d5afe`; palette or custom color).
  - The linked variant stores `platform` + `externalId` + `igName` for an IG saved folder or Pinterest board, so re-imports reuse it even after a rename.
  - Can also be created inline from the post modal or from a bulk assign.
- **Entry points:** `src/App.tsx:486`, `src/components/PostModal.tsx:300`, `src/views/Gallery.tsx:687`, `src/views/Browser.tsx:353` (area 2) → `collections:create` (`electron/ipc.ts:399`) → `createCollection` `electron/db.ts:5362`
- **Data:** `collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — add UNIQUE(user_id, platform, external_id).

### DATA-23 · Rename / recolor collection
- **What:** Updates the name and/or color via COALESCE, so a blank value keeps the old one. The native-folder link and original name are preserved.
- **Entry points:** `src/components/Sidebar.tsx:348` → `src/App.tsx:515` → `src/hooks/useCollections.ts:63` → `collections:update` (`electron/ipc.ts:418`) → `updateCollection` `electron/db.ts:5395`
- **Data:** `collections.name` / `color`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-24 · Delete collection (keep posts / delete posts too)
- **What:**
  - Default: removes only the collection; memberships cascade and the posts stay.
  - With "delete posts too": first permanently deletes every member post and its files (DATA-31), then the collection.
  - Returns `{deletedPosts, errors}`.
- **Entry points:** `src/components/CollectionModal.tsx:143` → `src/App.tsx:524` → `src/hooks/useCollections.ts:54` → `collections:delete` (`electron/ipc.ts:427`) → `deleteCollection` `electron/db.ts:5410`
- **Data:** `collections`, `post_collections`; optionally posts and files
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + background-job (purge posts and objects).

### DATA-25 · Assign posts to collections (single / bulk)
- **What:**
  - Inserts every posts × collections pair with `INSERT OR IGNORE` in one transaction and returns the number of links added.
  - The UI updates optimistically and rolls back on error.
  - Used from the post modal, gallery multi-select, and sync/selection imports that target a folder.
- **Entry points:** `src/components/PostModal.tsx:275`, `src/views/Gallery.tsx:650`, `src/hooks/useBrowserSync.ts:263`, `src/hooks/useBrowserSelection.ts:146` → `collections:addPosts` (`electron/ipc.ts:441`, cap 100k) → `addPostsToCollections` `electron/db.ts:5417`
- **Data:** `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-26 · Promote a result set to a new collection
- **What:**
  - Works from AI Search results, or from every post matching the AI Tags selection (tags or entity).
  - Asks for a name, creates a collection and bulk-assigns the posts.
- **Entry points:** `src/views/AiSearch.tsx:890`, `src/views/AiTags.tsx:760` → `aitags:postIdsByTags` / `db:getPostIds` → `collections:create` + `collections:addPosts`
- **Data:** `collections`, `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped (views are area 3)
- **Web port:** api+db — a single "create collection from query" endpoint.

### DATA-27 · Remove a post from a collection
- **What:**
  - Deletes one membership.
  - The backend and bridge method exist, but no UI calls them: today a post can only be un-filed by deleting the whole collection.
- **Entry points:** no caller → `collections:removePost` (`electron/ipc.ts:457`) → `removePostFromCollection` `electron/db.ts:5437`
- **Data:** `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** dead (UI missing)
- **Web port:** api+db — implement and expose it in the web UI.

### DATA-28 · Personal note on a post
- **What:**
  - Free-text `user_note`, kept separate from the AI fields so it survives re-analysis.
  - Searchable through DATA-14.
  - Written per field.
- **Entry points:** `src/components/postmodal/AiPanel.tsx:338` → `src/hooks/useAnalysis.tsx:417` → `post:updateUserContent` (`electron/ipc.ts:1053`) → `updateUserContent` `electron/db.ts:2695`
- **Data:** `posts.user_note`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-29 · Manual tags on a post
- **What:**
  - The JSON `user_tags` column is the display source of truth.
  - Tags are mirrored into `post_tags` with tier `manual`, after alias canonicalization and dedupe.
  - Manual tags work with the tag filter and search, do not count as "AI-tagged", and survive AI clear or regeneration.
  - They are renamed and merged together with AI tags (area 3).
- **Entry points:** `src/components/postmodal/AiPanel.tsx:346` → `post:updateUserContent` (`electron/ipc.ts:1053`) → `electron/db.ts:2695-2732`; also written at bookmark creation (`db.ts:2782`)
- **Data:** `posts.user_tags`, `post_tags` (tier `manual`), `tag_alias` (read)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db.

### DATA-30 · Derived tag/entity index maintenance
- **What:**
  - The `ai_tags` / `ai_entities` JSON columns stay the source of truth.
  - Every AI write rebuilds the AI-tier `post_tags` rows:
    - norm = trim + lowercase, then alias resolution
    - tier taken from the general/specific lists
  - It also rebuilds `post_entities`; manual-tier rows are left untouched.
  - Afterwards the global memo caches are dropped.
- **Entry points:** `applyAiAnalysis` `electron/db.ts:2829`, called from `updateAiAnalysis` `db.ts:2680`, `bulkUpsert` and snapshot promote; `normalizeTagRows` `db.ts:975`; `resolveAlias` `db.ts:3786`. The writers that produce AI data are area 3.
- **Data:** `post_tags`, `post_entities`, `tag_alias`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — keep the normalized tables (or JSONB + GIN), maintained in the same transaction.

### DATA-31 · Permanently delete posts + files (single / bulk)
- **What:** Steps, then returns `{deleted, errors}`:
  1. Suspends and cancels downloads for the given ids.
  2. Unlinks every file the post owns: current paths, slides, manual originals inside `assets/`, and every web snapshot screenshot. Runs in batches of 200 with event-loop yields.
  3. Deletes the rows; cascades remove media, memberships, tags, entities and snapshots.
- **Entry points:** `src/views/Gallery.tsx:830`, `src/components/postmodal/ActionsMenu.tsx:231` → `db:deletePosts` (`electron/ipc.ts:358`, cap 100k; `removePostsAndFiles` `ipc.ts:330`) → `getLocalFilePaths` `electron/db.ts:5726`, `deletePosts` `db.ts:5450`
- **Data:** all post-linked tables; files under `assets/`. `thumb-cache/` tiles are **not** purged.
- **Local deps:** SQLite, filesystem, downloader suspension (`electron/downloader.ts:1267`)
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + background-job (object purge); consider soft delete with undo.

### DATA-32 · Free a post's local files (keep the record)
- **What:**
  - Unlinks the current capture's files (thumbnail, preview, image, video, slides; web snapshot files are kept).
  - NULLs the path columns, so the post becomes "link only" again.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:206` → `db:deleteLocalFiles` (`electron/ipc.ts:299`) → `getCurrentCaptureFilePaths` `electron/db.ts:5685`, `clearPostLocalFiles` `db.ts:6061`
- **Data:** `posts.*_path`, `post_media.local_path`; files
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink — in the cloud this means "delete stored objects" (storage quota management).

### DATA-33 · Local asset path bookkeeping
- **What:**
  - When a download completes, writes absolute paths with COALESCE, so existing values are never erased.
  - Writes the per-slide `local_path`, inserting the slide row if it is missing.
  - Writes the blur placeholder and invalidates the stats cache.
- **Entry points:** `electron/downloader.ts:949-959` (area 2) → `updatePaths` `electron/db.ts:2561`, `updateMediaPath` `db.ts:2612`
- **Data:** `posts.thumbnail_path` / `image_path` / `video_path` / `thumb_blur`, `post_media.local_path`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — store object keys and rendition metadata, not paths.

### DATA-34 · Missing-file path reconciliation
- **What:** Before a cover repair, NULLs any path column or slide `local_path` whose file no longer exists. The UPDATE is guarded on the old value.
- **Entry points:** `electron/preview-repair.ts:121` → `clearMissingLocalPaths` `electron/db.ts:5636`
- **Data:** `posts.*_path`, `post_media.local_path`
- **Local deps:** SQLite, filesystem (`existsSync`)
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only) — object storage is authoritative.

### DATA-35 · JSON export (library backup)
- **What:**
  - The user picks platforms, then a save dialog opens.
  - Writes `{"posts":[…],"collections":[…]}` in chunks of 500 posts, yielding between chunks.
  - Each post is a full `rowToPost`, including absolute paths, `thumbBlur`, notes, manual tags and web fields, plus slides.
  - Adds `aiGeneralTags` / `aiSpecificTags` and portable collection keys (`x:<externalId>` / `n:<name>`).
  - Not exported: snapshots, aliases, clusters.
- **Entry points:** `src/views/Settings.tsx:372` → `db:exportJSON` (`electron/ipc.ts:198`) → `exportAllPosts` `electron/db.ts:5235`, `getCollectionsForExport` `db.ts:5199`
- **Data:** posts, `post_media`, `post_tags.tier`, `collections`; output file `saved-posts.json` (user-chosen location)
- **Local deps:** SQLite, filesystem, Electron `dialog`
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job + object-storage — async export to a signed download link; strip local paths.

### DATA-36 · JSON import (restore / merge)
- **What:**
  1. The native picker records a single allowed path.
  2. The import reads the file asynchronously and parses it synchronously. It accepts an array or `{posts,collections}`.
  3. Each record goes through the IG parser if it is `instagram` or has a shortcode, otherwise through the **Twitter parser**.
  4. Records are upserted with `overwriteAi`.
  5. Collections are matched by externalId or name (or created) and posts are re-linked.
  - Returns `{imported, updated, collections, links}`.
- **Entry points:** `src/components/ImportModal.tsx:21,31` → `dialog:openFile` (`electron/ipc.ts:1283`) + `db:importJSON` (`ipc.ts:178`) → `importFromJSON` `electron/db.ts:5106`, `normalizeImportedPost` `db.ts:5096`, `importCollections` `db.ts:5272`
- **Data:** posts, `post_media`, `post_tags` / `post_entities`, `collections`, `post_collections`
- **Local deps:** SQLite, filesystem, Electron `dialog`
- **External calls:** none
- **Status:** shipped, with a bug:
  - Pinterest, web and manual records are re-imported as platform `twitter`.
  - Notes, manual tags and web fields are dropped.
- **Web port:** background-job — upload, then a server import job; needs a fixed, versioned format.

### DATA-37 · Reset: delete all library data
- **What:**
  - Cancels the download, analyze and web queues and any in-flight cluster, alias or chat LLM jobs.
  - Deletes all posts (cascade), tag clusters, aliases and job rows.
  - Keeps the (now empty) **collections** and every file on disk; the UI says files are kept.
- **Entry points:** `src/views/Settings.tsx:2315` → `db:clearAll` (`electron/ipc.ts:237`) → `clearAllData` `electron/db.ts:5556`, `jobDeleteAll` `db.ts:5534`
- **Data:** every table except `collections`; files untouched
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + background-job — per-user wipe including the object purge; decide what happens to collections.

### DATA-38 · Reset: delete all downloaded files
- **What:**
  - Cancels downloads and waits for them to stop.
  - Deletes files in `assets/{thumbnails,images,videos,previews}`, except protected manual/web paths and files named `manual…`.
  - Recreates the directories and NULLs the path columns for instagram / twitter / pinterest posts. Posts are kept.
- **Entry points:** `src/views/Settings.tsx:2291` → `db:clearAssets` (`electron/ipc.ts:293`) → `clearAllAssets` `electron/downloader.ts:324`, `getProtectedAssetPaths` `electron/db.ts:5626`, `clearAllAssetPaths` `db.ts:5612`
- **Data:** `posts.*_path`, `post_media.local_path`, `assets/`; `thumb-cache/` untouched
- **Local deps:** filesystem, SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — purge the social renditions from object storage.

### DATA-39 · Reset: clear all AI analysis
- **What:**
  - First aborts any in-flight cluster, alias and chat jobs.
  - NULLs every `ai_*` column.
  - Deletes the AI-tier `post_tags` rows (manual tags are kept), `post_entities`, clusters, memberships and aliases.
- **Entry points:** `src/views/Settings.tsx:2303` → `db:clearAiAnalysis` (`electron/ipc.ts:277`) → `clearAllAiAnalysis` `electron/db.ts:5573`
- **Data:** `posts.ai_*`, `post_tags`, `post_entities`, `tag_cluster*`, `tag_alias`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped (AI semantics owned by area 3)
- **Web port:** api+db.

### DATA-40 · `asset://` local media protocol
- **What:**
  - Privileged scheme `asset://media/<encodeURIComponent(absPath)>`.
  - Reads are confined via realpath to **all of userData**; anything else returns 403.
  - MIME type chosen from the extension (images, mp4 / webm / mov / m4v / mkv / avi, pdf).
  - ETag = size + mtime, answered with 304; `Cache-Control: public, no-cache`.
  - Range requests return 206 streams; whole files stream through `net.fetch(file://)`.
  - Allowed by the app's CSP.
- **Entry points:** `src/lib/asset.ts:4` → `registerAssetProtocol` `electron/main.ts:164` (scheme registration `main.ts:119`, CSP `main.ts:299`)
- **Data:** any file under userData
- **Local deps:** Electron `protocol` / `net`, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage — CDN with short-lived signed URLs (Range is supported natively).

### DATA-41 · Downscaled grid thumbnails (`?w=`) + tile cache
- **What:**
  - Applies to `?w=` between 64 and 1024, for jpg / jpeg / png / webp only.
  - Generator: the OS thumbnailer (QuickLook / Windows shell); Linux falls back to a synchronous decode. Output: JPEG q80, or PNG to keep alpha.
  - Cache: tiles stored in `thumb-cache/` under sha1(path | mtime | size | w).
  - Concurrency: 8 slots (4 on Linux) served LIFO; in-flight requests are deduped; writes go to a temp file and are renamed.
- **Entry points:** `src/lib/asset.ts:12` (`src/components/PostCard.tsx:356`, width 640) → `electron/main.ts:182-201` → `thumbnailFor` `electron/thumbs.ts:157`, `thumbETag` `thumbs.ts:224`
- **Data:** `thumb-cache/<sha1>.jpg|png` (never garbage-collected)
- **Local deps:** Electron `nativeImage` / `screen`, filesystem
- **External calls:** none
- **Status:** shipped (`SHELFY_THUMB_NO_CACHE` perf flag)
- **Web port:** object-storage — resizing CDN, or renditions generated at ingest.

### DATA-42 · Tile cache pre-warm
- **What:** 15 s after launch, sequentially generates the missing 640px tiles for every file in `assets/{thumbnails,images,web}`.
- **Entry points:** `electron/main.ts:664` → `prewarmThumbCache` `electron/thumbs.ts:241`
- **Data:** `thumb-cache/`
- **Local deps:** filesystem, `nativeImage`
- **External calls:** none
- **Status:** shipped (skipped when `PERF_NO_PREWARM` is set)
- **Web port:** drop (desktop-only) — renditions are produced at ingest.

### DATA-43 · Blur-up placeholders (`thumb_blur`)
- **What:**
  - A ~24px JPEG (q55) data URI stored in `posts.thumb_blur` and shipped in every `getPosts` row.
  - Generated when a download completes, and by a startup backfill after pre-warm. Posts whose cover can't be decoded get the `''` sentinel.
  - Emits a list refresh if any placeholders were written.
  - Posts that only have a preview cover never get one.
- **Entry points:** `electron/downloader.ts:1023`, `electron/main.ts:668` → `microThumbDataUri` `electron/thumbs.ts:280`, `backfillThumbBlurs` `thumbs.ts:319` → `listPostsMissingThumbBlur` `electron/db.ts:2592`, `setThumbBlur` `db.ts:2605`
- **Data:** `posts.thumb_blur`
- **Local deps:** `nativeImage`, filesystem, SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — compute at ingest (sharp, or BlurHash).

### DATA-44 · Automatic preview-cover cache
- **What:**
  - Triggered after every upsert, and once at startup (+5 s, Twitter only).
  - Targets posts with an https CDN cover but no local cover. Fetches a 640px JPEG (q82) anonymously into `assets/previews/`:
    - no cookies, platform Referer, host allowlist
    - ≤8 MB, 15 s timeout, ≤4 redirects, concurrency 3
  - The DB write is guarded, so a newer URL or a real download wins.
- **Entry points:** `electron/ipc.ts:385`, `electron/main.ts:642` → `enqueuePreviews` `electron/preview-cache.ts:142` → `getPreviewCandidates` `electron/db.ts:1899`, `setPreviewPath` `db.ts:1936`
- **Data:** `posts.preview_path`; `assets/previews/<sha256(id,url)>.jpg`
- **Local deps:** Electron `session.fetch`, `nativeImage`, filesystem
- **External calls:** `*.cdninstagram.com`, `*.fbcdn.net`, `pbs.twimg.com`, `*.pinimg.com`
- **Status:** shipped
- **Web port:** background-job — ingest worker; IG URLs expire, and datacenter IPs may be blocked.

### DATA-45 · On-demand expired-cover repair
- **What:** Triggered when a social card's image fails to load. The repair:
  1. Drops missing local paths.
  2. Fetches the public post page anonymously and verifies `og:url`.
  3. Takes `og:image` and swaps `thumbnail_url` (and slide 0) if it is unchanged.
  4. Hands the cover to the preview cache.
  - Limits: concurrency 2, ≤80 pending, 15-minute per-post retry window, 30-minute global pause on HTTP 429.
- **Entry points:** `src/components/PostCard.tsx:659` → `preview:repair` (`electron/ipc.ts:173`) → `requestPreviewRepair` `electron/preview-repair.ts:154` → `refreshPreviewUrl` `electron/db.ts:1950`
- **Data:** `posts.thumbnail_url`, `post_media.source_url`, `preview_path`
- **Local deps:** Electron `session.fetch`
- **External calls:** `www.instagram.com/{p,reel,tv}/…`, `x.com` / `twitter.com/…/status/…`, `www.pinterest.com/pin/…` (HTML)
- **Status:** shipped
- **Web port:** rethink — server-side scraping of IG/X HTML is unreliable; prefer a browser-extension refresh.

### DATA-46 · Durable background job store + boot recovery
- **What:**
  - The download, analyze and web managers mirror every job state change into `jobs`:
    - the payload is compact JSON (minus events / pages / logs / streamText)
    - progress-only changes are skipped
    - bulk changes are batched in one transaction
  - At boot, `recover()` re-enqueues non-terminal rows **first**, then drops the stale ones (`forgetExcept`).
  - Terminal states: done, cancelled, error.
- **Entry points:** `electron/main.ts:683` → `electron/downloader.ts:1368`, `electron/analyzer.ts:4099`, `electron/weborchestrator.ts:1341` → `electron/jobstore.ts:80-202` → `electron/db.ts:5469-5544`
- **Data:** `jobs`. Keys: `<postId>:<asset>[:pos]`, `<postId>:analyze`, `web:<postId>`.
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — Postgres-backed or managed queue with per-user keys and leases.

### DATA-47 · Stuck-analysis reset at boot
- **What:** Resets `ai_status='analyzing'` rows left by a crash back to NULL, before analyzer recovery, so those posts count as unanalyzed again.
- **Entry points:** `electron/analyzer.ts:4101` → `clearStuckAnalyzing` `electron/db.ts:5549`
- **Data:** `posts.ai_status`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — replace with lease or visibility timeouts.

### DATA-48 · In-process query caches & invalidation
- **What:**
  - Caches: stats (5 s TTL), per-term IDF (≤1000 entries), global tag counts, keyword-token counts, accepted-alias map, `word_match` regex cache.
  - `invalidateGlobalCaches` drops all content-derived caches on writes.
- **Entry points:** `electron/db.ts:2498,1717,3373,3719,3765,514`; invalidation `db.ts:3751`
- **Data:** process memory only
- **Local deps:** single-process memory
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink — per-user cache keys (e.g. Redis) or SQL aggregates, with invalidation across instances.

### DATA-49 · Settings & preferences persistence
- **What:** There is no settings table. Settings live in three places:
  - **Main-process JSON files in userData:**
    - `ai-model.json`: model, concurrency, tuning, paused
    - `stt-model.json`
    - `ai-providers.json` (mode 0600)
    - `update-channel.json`, `llama-variant.json`, `binaries.json`
  - **macOS Keychain or environment variables:** API keys.
  - **Renderer localStorage:** `app:language`, `app:disclaimerAcceptance`, `galleryViewMode`, `gridSizeStep`, `download:assetTypes`, `aiSearchSuggestions`, `postModal:videoMuted`, `shelfy.sidebar.expanded{Groups,Platforms}`, `ig-saved-url`, `pin-board-url`.
- **Entry points:** `electron/analyzer.ts:420`, `electron/stt.ts:158`, `electron/ai-providers.ts:96`, `electron/updater.ts:140`, `electron/binaries.ts:65`; `src/i18n/index.tsx:43`, `src/hooks/useDownloadPrefs.ts:3`, `src/hooks/useViewMode.ts:16`
- **Data:** the files and keys listed above
- **Local deps:** filesystem, localStorage, OS Keychain (`security` CLI)
- **External calls:** none
- **Status:** shipped (owners: areas 3 and 5)
- **Web port:** api+db (a `user_settings` JSONB row) plus client-only storage for pure UI preferences; secrets go to an encrypted server-side store.

### DATA-50 · On-disk userData layout & asset naming
- **What:** File layout under userData:
  - Database: `shelfy.sqlite`.
  - Social downloads:
    - `assets/thumbnails/<platform>-<shortcode|id>.<ext>`
    - `assets/images/<platform>-<id>-<pos>.<ext>` (legacy position 0 has no suffix)
    - `assets/videos/<platform>-<id>[-<pos>].mp4`
  - Covers and web: `assets/previews/<sha256>.jpg`; `assets/web/[<epoch>-]<host>-<sha256:16>.webp`.
  - Manual bookmarks: `assets/{images,videos,files}/manual<18hex>-<i>.<ext>` plus `thumbnails/*.webp`.
  - Other directories: `thumb-cache/`, `logs/`, `models/`, `runtime-bin/`, `ms-playwright/`, `capture-mvp/`, `rebuild/`, `updates/`, `tmp-cookies/`.
  - `Partitions/social/` holds the social login cookies.
- **Entry points:** `electron/downloader.ts:313,799,822,899`, `electron/preview-cache.ts:101`, `electron/webcapture.ts:756`, `electron/bookmarks.ts:124`, `electron/thumbs.ts:45`, `electron/logger.ts:114`
- **Data:** filesystem under `app.getPath('userData')`. Dev override: `SHELFY_TEST_USER_DATA` (`electron/main.ts:23`).
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage — keys like `users/<uid>/<kind>/<postId>/<n>`; the DB stores keys, not paths.

### DATA-51 · Main-process file logging
- **What:**
  - Writes to `logs/main.log`: `console.*`, uncaught exceptions and rejections, the renderer console, `did-fail-load` (query string stripped), `render-process-gone`, preload errors.
  - Never logs webview or social-session consoles.
  - Rotates once past 5 MB to `main.log.1`, but only at launch.
  - Not exposed via IPC (`getLogPath` is unused).
- **Entry points:** `electron/main.ts:581,534` → `init` `electron/logger.ts:112`, `attachWindow` `logger.ts:157`
- **Data:** `userData/logs/main.log(.1)`
- **Local deps:** filesystem, Electron `webContents` events
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink — structured server logs and observability, plus client error reporting.

### DATA-52 · IPC boundary guards
- **What:**
  - Bulk arrays are capped at 100k: `bulkUpsert`, `deletePosts`, `addPosts`, `download:posts`, `postIdsByTags`. Non-object posts are dropped.
  - JSON import only accepts the last native-dialog pick, which is consumed on success.
  - `shell:*` paths are confined to userData.
  - Bookmark payloads are capped at 200 MB per file and 500 MB total (checked in preload and in main).
- **Entry points:** `electron/ipc.ts:49,178,118,702`; `electron/preload.ts:222`
- **Data:** none
- **Local deps:** Electron IPC
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — auth plus per-endpoint schema validation, small batch limits, presigned uploads.

### DATA-53 · Shared infra: model / file download helper
- **What:**
  - Resumable streamed download (`.part` file plus Range) with progress.
  - Requires https and an allowlisted final host; checks size and an optional SHA-256.
  - Also provides free-port and health-poll helpers for the local model servers.
- **Entry points:** `downloadFile` `electron/serverUtils.ts:87` (used by `analyzer.ts`, `stt.ts`, `embeddings.ts` — area 3)
- **Data:** `userData/models/`
- **Local deps:** filesystem, loopback sockets
- **External calls:** `huggingface.co`, `*.hf.co`
- **Status:** shipped
- **Web port:** drop (desktop-only) — third-party AI APIs replace local models.

### DATA-54 · Shared infra: archive extraction & update feed URL
- **What:**
  - Spawns `tar` to list and extract `.zip` / `.tar.gz` archives, rejecting zip-slip entries.
  - Reads the update feed URL from `app-update.yml` (https only).
- **Entry points:** `electron/archive-utils.ts:17,94` (used by `electron/updater.ts` and `electron/binaries.ts` — areas 5 and 3)
- **Data:** `resources/app-update.yml`, `userData/runtime-bin`, `rebuild/`
- **Local deps:** OS `tar`, filesystem
- **External calls:** update feed host (GitHub releases)
- **Status:** shipped
- **Web port:** drop (desktop-only).

### DATA-55 · Maintenance CLI scripts on the live DB
- **What:** Dev scripts run under Electron's Node against the real `shelfy.sqlite`:
  - force-import AI fields from an export
  - bulk tag delete or merge
  - diff an IG collection against the DB
  - search and cluster evaluation harnesses (on scratch copies)
- **Entry points:** `scripts/import-ai.ts:1`, `scripts/tag-cleanup.ts:1`, `scripts/verify-collection.ts:162`, `scripts/search-eval/run.ts:134`; scripts in `package.json:36-41`
- **Data:** `posts.ai_*`, `post_tags`, `tag_cluster_membership`
- **Local deps:** better-sqlite3, Electron, a hard-coded macOS userData path
- **External calls:** instagram.com (only `verify-collection`)
- **Status:** experimental/spike (dev-only)
- **Web port:** rethink — admin jobs or data migrations.

### DATA-56 · Capture-on-view DB hooks
- **What:**
  - Feed image bytes captured via CDP are matched to a post and slide by file basename: LIKE on `post_media.source_url`, falling back to `thumbnail_url`.
  - They are persisted with the COALESCE path writers.
  - A single image counts as complete; carousels and videos only get their cover.
- **Entry points:** `electron/main.ts:370` → `electron/capture-mvp.ts:130-160` → `captureFindSlot` `electron/db.ts:2646`, `updatePaths`, `updateMediaPath`
- **Data:** `posts.*_path`, `post_media.local_path`, `assets/`, `capture-mvp/`
- **Local deps:** Electron CDP (`webContents.debugger`), SQLite
- **External calls:** none (zero extra requests)
- **Status:** flag (`SHELFY_CAPTURE_MVP=1` and `SHELFY_CAPTURE_WRITE=1`) — spike
- **Web port:** browser-extension — capture bytes client-side and upload them.

### DATA-57 · Dead data-layer code
- **What:**
  - The `downloads` table is created but never written or read.
  - `existingIds` is bridged but has no caller.
  - `getTopTagsForTextQuery` and `upsertWebReferences` have no callers.
  - `upsertPost` is only used by tests.
- **Entry points:** `electron/db.ts:344`, `db.ts:1975` / `electron/ipc.ts:166`, `db.ts:3313`, `db.ts:2470`, `db.ts:2135`
- **Data:** `downloads` (always empty)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** dead
- **Web port:** drop (desktop-only) — do not port.

### DATA-58 · Dev/perf environment overrides
- **What:**
  - `SHELFY_TEST_USER_DATA`: points userData at a cloned profile (unpackaged builds only).
  - `SHELFY_THUMB_NO_CACHE=1`: disables the tile cache and HTTP caching.
  - `PERF_NO_PREWARM`: skips pre-warm and the blur backfill.
  - `ELECTRON_DEV`: echoes logs to the terminal.
- **Entry points:** `electron/main.ts:23,154,664`, `electron/thumbs.ts:65`, `electron/logger.ts:46`
- **Data:** none
- **Local deps:** environment variables
- **External calls:** none
- **Status:** flag
- **Web port:** drop (desktop-only).

## IPC surface
Every channel goes through `electron/preload.ts`. The preload bridge was cross-checked against `types/electron-api.d.ts` and `electron/ipc.ts`; every invoke channel matches exactly one handler.

| | Count |
|---|---|
| Invoke channels (`ipcRenderer.invoke` ↔ `ipcMain.handle`, all in `electron/ipc.ts`) | **155** |
| Main→renderer push channels (`webContents.send` ↔ `ipcRenderer.on`) | **15** |
| **Total rows** | **170** (169 distinct names: `ai:remoteStatus` is both an invoke and an event) |
| One-way `ipcRenderer.send` / `ipcMain.on` channels | 0 |
| Handlers registered outside `ipc.ts` | 0 |
| Invoke channels with no renderer caller (dead) | 11 |

Per area:

| Area | Invoke | Events |
|---|---|---|
| 1 | 20 | 1 |
| 2 | 16 | 1 |
| 3 | 82 | 9 |
| 4 | 16 | 1 |
| 5 | 21 | 3 |

Not channels, but also on the bridge:
- Synchronous preload values: `platform` (`process.platform`) and `webviewPreloadPath`.
- Webview guest→host messages, delivered through `<webview>` `ipc-message` and never reaching main: `intercepted` (`electron/webview-preload.ts:33`) and `ss-select` (`:42`). Both are handled in `src/hooks/useBrowserIntercept.ts:98,147` (area 2).

Handler paths are relative to `electron/`. **dead** = no renderer caller.

| channel | kind | purpose | handler `path:line` | owning area (1-5) |
|---|---|---|---|---|
| `window:minimize` | invoke | `windowMinimize()` minimize frameless window | `ipc.ts:827` | 5 |
| `window:maximizeToggle` | invoke | `windowMaximizeToggle()` → maximized bool | `ipc.ts:830` | 5 |
| `window:close` | invoke | `windowClose()` | `ipc.ts:836` | 5 |
| `window:isMaximized` | invoke | `windowIsMaximized()` | `ipc.ts:839` | 5 |
| `db:getPosts` | invoke | `getPosts(filters)` → `{posts,total}` filtered/searched/sorted page (DATA-10…16) | `ipc.ts:157` → `db.ts:1818` | 1 |
| `db:getPostIds` | invoke | `getPostIds(filters)` → every matching id (DATA-17) | `ipc.ts:160` → `db.ts:1857` | 1 |
| `db:getPostsByIds` | invoke | `getPostsByIds(ids)` → posts in input order (DATA-18) | `ipc.ts:163` → `db.ts:1868` | 1 |
| `db:existingIds` | invoke | `existingIds(ids)` subset already stored — **dead** | `ipc.ts:166` → `db.ts:1975` | 2 |
| `db:savedByKeys` | invoke | `savedByKeys(keys)` → `[{key,id}]` id/shortcode match (DATA-05) | `ipc.ts:169` → `db.ts:1996` | 2 |
| `db:getStats` | invoke | `getStats()` library counters (DATA-20) | `ipc.ts:172` → `db.ts:2504` | 1 |
| `preview:repair` | invoke | `repairPreview(id)` → bool queued (DATA-45) | `ipc.ts:173` → `preview-repair.ts:154` | 1 |
| `db:importJSON` | invoke | `importJSON(filePath)`; path must equal last dialog pick (DATA-36) | `ipc.ts:178` → `db.ts:5106` | 1 |
| `db:exportJSON` | invoke | `exportJSON(platforms?)` save dialog + chunked write (DATA-35) | `ipc.ts:198` → `db.ts:5235` | 1 |
| `db:clearAll` | invoke | `clearAllData()` stop queues, wipe library (DATA-37) | `ipc.ts:237` → `db.ts:5556` | 1 |
| `db:clearAiAnalysis` | invoke | `clearAllAiAnalysis()` wipe AI layer (DATA-39) | `ipc.ts:277` → `db.ts:5573` | 1 |
| `db:clearAssets` | invoke | `clearAllAssets()` delete social downloads + paths (DATA-38) | `ipc.ts:293` → `downloader.ts:324`, `db.ts:5612` | 1 |
| `collections:list` | invoke | `getCollections()` with counts (DATA-21) | `ipc.ts:398` → `db.ts:5329` | 1 |
| `collections:create` | invoke | `createCollection(name,color,{platform,externalId,igName})` (DATA-22) | `ipc.ts:399` → `db.ts:5362` | 1 |
| `collections:update` | invoke | `updateCollection(id,{name,color})` (DATA-23) | `ipc.ts:418` → `db.ts:5395` | 1 |
| `collections:delete` | invoke | `deleteCollection(id,{deletePosts})` (DATA-24) | `ipc.ts:427` → `db.ts:5410` | 1 |
| `collections:addPosts` | invoke | `addPostsToCollections(postIds,collectionIds)` → `{added}` (DATA-25) | `ipc.ts:441` → `db.ts:5417` | 1 |
| `collections:removePost` | invoke | `removePostFromCollection(postId,collectionId)` — **dead** (DATA-27) | `ipc.ts:457` → `db.ts:5437` | 1 |
| `download:post` | invoke | `downloadPost(postId,assetTypes?)` → `{queued}` | `ipc.ts:465` | 2 |
| `download:posts` | invoke | `downloadPosts(ids,assetTypes?,missingOnly=true)` bulk enqueue | `ipc.ts:508` | 2 |
| `download:all` | invoke | `downloadAll(assetTypes?,missingOnly)` whole library | `ipc.ts:477` | 2 |
| `download:status` | invoke | `getDownloadStatus()` job snapshot | `ipc.ts:534` | 2 |
| `download:isPaused` | invoke | `getDownloadIsPaused()` | `ipc.ts:535` | 2 |
| `download:pauseAll` | invoke | `pauseDownloads()` | `ipc.ts:537` | 2 |
| `download:resumeAll` | invoke | `resumeDownloads()` | `ipc.ts:538` | 2 |
| `download:cancelAll` | invoke | `cancelAllDownloads()` | `ipc.ts:539` | 2 |
| `download:clearCompleted` | invoke | `clearCompletedDownloads()` | `ipc.ts:540` | 2 |
| `download:cancelJob` | invoke | `cancelDownloadJob(key)` | `ipc.ts:542` | 2 |
| `download:retryJob` | invoke | `retryDownloadJob(key)` | `ipc.ts:545` | 2 |
| `analyze:post` | invoke | `analyzePost(postId)` enqueue one | `ipc.ts:551` | 3 |
| `analyze:all` | invoke | `analyzeAll()` enqueue all (batches of 200) | `ipc.ts:561` | 3 |
| `analyze:posts` | invoke | `analyzePosts(postIds)` | `ipc.ts:572` | 3 |
| `analyze:split` | invoke | `splitForAnalysis(postIds)` → `{analyzable,needsDownload}` | `ipc.ts:602` | 3 |
| `analyze:taxonomy` | invoke | `getTaxonomy()` — **dead** | `ipc.ts:621` | 3 |
| `analyze:status` | invoke | `getAnalyzeStatus()` | `ipc.ts:623` | 3 |
| `analyze:cancelJob` | invoke | `cancelAnalyzeJob(key)` | `ipc.ts:624` | 3 |
| `analyze:cancelAll` | invoke | `cancelAllAnalyze()` | `ipc.ts:627` | 3 |
| `analyze:clearAll` | invoke | `clearAllAnalyze()` | `ipc.ts:628` | 3 |
| `analyze:clearCompleted` | invoke | `clearCompletedAnalyze()` | `ipc.ts:629` | 3 |
| `analyze:pauseAll` | invoke | `pauseAnalyze()` | `ipc.ts:630` | 3 |
| `analyze:resumeAll` | invoke | `resumeAnalyze()` | `ipc.ts:631` | 3 |
| `analyze:isPaused` | invoke | `getAnalyzeIsPaused()` | `ipc.ts:632` | 3 |
| `analyze:retryJob` | invoke | `retryAnalyzeJob(key)` | `ipc.ts:633` | 3 |
| `analyze:modelStatus` | invoke | `getModelStatus()` local VLM readiness | `ipc.ts:637` | 3 |
| `analyze:listModels` | invoke | `listModels()` | `ipc.ts:638` | 3 |
| `analyze:setModel` | invoke | `setModel(id)` | `ipc.ts:639` | 3 |
| `analyze:getConcurrency` | invoke | `getAnalyzeConcurrency()` → `{value,max}` | `ipc.ts:642` | 3 |
| `analyze:setConcurrency` | invoke | `setAnalyzeConcurrency(n)` | `ipc.ts:646` | 3 |
| `analyze:getHardware` | invoke | `getHardwareInfo()` | `ipc.ts:649` | 3 |
| `analyze:getTuning` | invoke | `getAnalyzeTuning()` — **dead** | `ipc.ts:650` | 3 |
| `analyze:setTuning` | invoke | `setAnalyzeTuning(patch)` | `ipc.ts:651` | 3 |
| `analyze:downloadModel` | invoke | `downloadModel(id)` | `ipc.ts:656` | 3 |
| `analyze:pauseDownload` | invoke | `pauseModelDownload()` | `ipc.ts:661` | 3 |
| `analyze:cancelDownload` | invoke | `cancelModelDownload(id)` | `ipc.ts:662` | 3 |
| `analyze:deleteModel` | invoke | `deleteModel(id)` | `ipc.ts:665` | 3 |
| `app:getVersion` | invoke | `getAppVersion()` | `ipc.ts:846` | 5 |
| `app:getUpdateChannel` | invoke | `getUpdateChannel()` | `ipc.ts:847` | 5 |
| `app:setUpdateChannel` | invoke | `setUpdateChannel(channel)` + re-check | `ipc.ts:848` | 5 |
| `updater:getState` | invoke | `getUpdateState()` | `ipc.ts:856` | 5 |
| `updater:check` | invoke | `checkForUpdates()` | `ipc.ts:857` | 5 |
| `updater:quitAndInstall` | invoke | `quitAndInstallUpdate()` | `ipc.ts:861` | 5 |
| `updater:openDownload` | invoke | `openUpdateDownload()` | `ipc.ts:862` | 5 |
| `updater:rebuild` | invoke | `rebuildUpdate()` (Windows self-rebuild) | `ipc.ts:863` | 5 |
| `binaries:status` | invoke | `getBinariesStatus()` sidecar binaries | `ipc.ts:866` | 5 |
| `binaries:getVariant` | invoke | `getLlamaVariant()` — **dead** | `ipc.ts:867` | 5 |
| `binaries:setVariant` | invoke | `setLlamaVariant(variant)` | `ipc.ts:868` | 5 |
| `binaries:variantState` | invoke | `getVariantState()` | `ipc.ts:873` | 5 |
| `binaries:ensure` | invoke | `ensureBinaries(force)` provision (+progress events) | `ipc.ts:874` | 5 |
| `aitags:overview` | invoke | `getAiOverview()` | `ipc.ts:897` | 3 |
| `aitags:tagStats` | invoke | `getTagStats({limit,tier})` | `ipc.ts:898` | 3 |
| `aitags:entityStats` | invoke | `getEntityStats({limit})` | `ipc.ts:901` | 3 |
| `aitags:cooccurrence` | invoke | `getTagCooccurrence(tag,limit)` | `ipc.ts:904` | 3 |
| `aitags:clusters` | invoke | `getTagClusters({maxClusters≤200})` | `ipc.ts:909` | 3 |
| `aitags:cluster:regenerate` | invoke | `regenerateClusters()` long LLM run | `ipc.ts:923` | 3 |
| `aitags:cluster:cancel` | invoke | `cancelClusters()` | `ipc.ts:938` | 3 |
| `aitags:aliases:propose` | invoke | `proposeAliases()` LLM → `tag_alias` proposed | `ipc.ts:948` | 3 |
| `aitags:aliases:cancel` | invoke | `cancelAliases()` | `ipc.ts:965` | 3 |
| `aitags:aliases:list` | invoke | `getTagAliases({status})` | `ipc.ts:970` | 3 |
| `aitags:alias:accept` | invoke | `acceptAlias(aliasNorm)` re-canonicalize `post_tags` | `ipc.ts:975` | 3 |
| `aitags:alias:dismiss` | invoke | `dismissAlias(aliasNorm)` | `ipc.ts:980` | 3 |
| `aitags:cluster:setStatus` | invoke | `acceptCluster(id)` / `dismissCluster(id)` | `ipc.ts:985` | 3 |
| `aitags:cluster:rename` | invoke | `renameCluster(id,label)` | `ipc.ts:990` | 3 |
| `aitags:cluster:removeTag` | invoke | `removeTagFromCluster(tag,clusterId)` | `ipc.ts:995` | 3 |
| `aitags:mergeSuggestions` | invoke | `getTagMergeSuggestions({limit})` | `ipc.ts:1000` | 3 |
| `aitags:health` | invoke | `getTagHealth()` | `ipc.ts:1003` | 3 |
| `aitags:renameTag` | invoke | `renameTag(from,to)` rewrites `ai_tags` + `user_tags` | `ipc.ts:1004` | 3 |
| `aitags:mergeTags` | invoke | `mergeTags(sources,target)` | `ipc.ts:1008` | 3 |
| `aitags:postIdsByTags` | invoke | `getPostIdsByTags(tags,mode)` | `ipc.ts:1013` | 3 |
| `aitags:tagGraph` | invoke | `getTagGraph({maxNodes,minEdgeWeight})` — **dead** | `ipc.ts:1025` | 3 |
| `analyze:missing` | invoke | `analyzeMissing()` enqueue unanalyzed | `ipc.ts:1029` | 3 |
| `analyze:updateManual` | invoke | `updatePostAiAnalysis(id,fields)` manual AI edit (`model='manuale'`) | `ipc.ts:1044` | 3 |
| `post:updateUserContent` | invoke | `updatePostUserContent(id,{note,manualTags})` (DATA-28/29) | `ipc.ts:1053` → `db.ts:2695` | 1 |
| `analyze:clearDescriptions` | invoke | `clearPostDescriptions(ids)` → count | `ipc.ts:1060` | 3 |
| `analyze:clearTags` | invoke | `clearPostAiTags(ids)` → count | `ipc.ts:1063` | 3 |
| `search:suggest` | invoke | `suggestSearch(query)` → `{tags}` concept chips | `ipc.ts:1073` | 3 |
| `search:providers` | invoke | `getSearchProviders()` | `ipc.ts:1086` | 3 |
| `search:providerSettings` | invoke | `getAiProviderSettings()` | `ipc.ts:1087` | 3 |
| `search:saveProviderSettings` | invoke | `saveAiProviderSettings(settings)` (+secrets) | `ipc.ts:1088` | 3 |
| `ai:remoteStatus` | invoke | `getAiRemoteStatus()` | `ipc.ts:1101` | 3 |
| `ai:retryRemote` | invoke | `retryAiRemote()` re-probe remote node | `ipc.ts:1102` | 3 |
| `ai:useLocalModels` | invoke | `useLocalAiModels()` session override | `ipc.ts:1106` | 3 |
| `search:selectProvider` | invoke | `selectSearchProvider(id)` | `ipc.ts:1113` | 3 |
| `search:chat` | invoke | `chatSearch(messages,activeTags)` (streams `search:chatToken`) | `ipc.ts:1122` | 3 |
| `search:chatCancel` | invoke | `cancelChatSearch()` | `ipc.ts:1146` | 3 |
| `search:byTags` | invoke | `searchByTags(tags,mode,limit,offset,source)` IDF-ranked | `ipc.ts:1153` | 3 |
| `search:hybrid` | invoke | `searchHybrid(tags,text,mode,limit,offset,source)` | `ipc.ts:1168` | 3 |
| `search:byText` | invoke | `searchByText(query,limit,offset,source)` = DATA-14 query | `ipc.ts:1191` | 3 |
| `web:add` | invoke | `addWebReference(url,maxPages,overwrite,singlePage)` placeholder + capture | `ipc.ts:674` | 4 |
| `web:status` | invoke | `getWebStatus()` | `ipc.ts:686` | 4 |
| `web:isPaused` | invoke | `getWebIsPaused()` — **dead** | `ipc.ts:687` | 4 |
| `web:cancel` | invoke | `cancelWebJob(key)` | `ipc.ts:688` | 4 |
| `web:cancelAll` | invoke | `cancelAllWeb()` | `ipc.ts:691` | 4 |
| `web:pauseAll` | invoke | `pauseWeb()` — **dead** | `ipc.ts:692` | 4 |
| `web:resumeAll` | invoke | `resumeWeb()` — **dead** | `ipc.ts:693` | 4 |
| `web:retryJob` | invoke | `retryWebJob(key)` | `ipc.ts:694` | 4 |
| `web:clearCompleted` | invoke | `clearCompletedWeb()` | `ipc.ts:697` | 4 |
| `bookmark:add` | invoke | `addManualBookmark({note,tags,files[]})` raw bytes (≤200 MB/file, 500 MB total) | `ipc.ts:702` | 4 |
| `web:discover` | invoke | `discoverWebPages(url,maxPages)` — **dead** | `ipc.ts:744` | 4 |
| `web:getSnapshots` | invoke | `getWebSnapshots(postId)` (DATA-08) | `ipc.ts:752` → `db.ts:5869` | 4 |
| `web:snapshotCounts` | invoke | `getWebSnapshotCounts()` (DATA-08) | `ipc.ts:757` → `db.ts:5900` | 4 |
| `web:deleteSites` | invoke | `deleteWebSites(ids)` cancel + unlink all versions + delete | `ipc.ts:761` | 4 |
| `web:deleteSnapshot` | invoke | `deleteWebSnapshot(id)` + files | `ipc.ts:782` | 4 |
| `web:deleteLatestReport` | invoke | `deleteWebLatestReport(ids)` promote previous version | `ipc.ts:799` | 4 |
| `stt:status` | invoke | `sttStatus()` | `ipc.ts:1206` | 3 |
| `stt:listModels` | invoke | `sttListModels()` | `ipc.ts:1207` | 3 |
| `stt:setModel` | invoke | `sttSetModel(id)` | `ipc.ts:1208` | 3 |
| `stt:downloadModel` | invoke | `sttDownloadModel(id)` | `ipc.ts:1211` | 3 |
| `stt:pauseDownload` | invoke | `sttPauseModelDownload()` | `ipc.ts:1216` | 3 |
| `stt:cancelDownload` | invoke | `sttCancelModelDownload(id)` | `ipc.ts:1217` | 3 |
| `stt:deleteModel` | invoke | `sttDeleteModel(id)` | `ipc.ts:1220` | 3 |
| `stt:ensure` | invoke | `sttEnsure()` start whisper server | `ipc.ts:1223` | 3 |
| `stt:getTuning` | invoke | `sttGetTuning()` | `ipc.ts:1224` | 3 |
| `stt:setTuning` | invoke | `sttSetTuning(patch)` | `ipc.ts:1225` | 3 |
| `stt:transcribe` | invoke | `sttTranscribe(wav,{language})` (≤50 MB) | `ipc.ts:1251` | 3 |
| `emb:status` | invoke | `embStatus()` — **dead** | `ipc.ts:1234` | 3 |
| `emb:listModels` | invoke | `embListModels()` | `ipc.ts:1235` | 3 |
| `emb:setModel` | invoke | `embSetModel(id)` | `ipc.ts:1236` | 3 |
| `emb:downloadModel` | invoke | `embDownloadModel(id)` | `ipc.ts:1239` | 3 |
| `emb:pauseDownload` | invoke | `embPauseModelDownload()` | `ipc.ts:1244` | 3 |
| `emb:cancelDownload` | invoke | `embCancelModelDownload(id)` | `ipc.ts:1245` | 3 |
| `emb:deleteModel` | invoke | `embDeleteModel(id)` | `ipc.ts:1248` | 3 |
| `db:bulkUpsert` | invoke | `saveInterceptedPosts(posts,platform)` → `{inserted,skipped}`; merge rules DATA-04 | `ipc.ts:366` → `db.ts:2144` | 2 |
| `db:deleteLocalFiles` | invoke | `deleteLocalFiles(postId)` free disk, keep post (DATA-32) | `ipc.ts:299` | 1 |
| `db:deletePosts` | invoke | `deletePosts(ids)` rows + files (DATA-31) | `ipc.ts:358` → `db.ts:5450` | 1 |
| `dialog:openFile` | invoke | `openFile()` JSON picker; arms the import path (DATA-36) | `ipc.ts:1283` | 1 |
| `shell:openPath` | invoke | `openPath(path)` open local file (userData-confined) | `ipc.ts:1300` | 5 |
| `shell:showItemInFolder` | invoke | `showItemInFolder(path)` | `ipc.ts:1309` | 5 |
| `shell:openExternal` | invoke | `openExternal(url)` SSRF-checked | `ipc.ts:1317` | 5 |
| `feedback:send` | invoke | `sendFeedback(message,attachments)` | `ipc.ts:1330` | 5 |
| `getWebviewInjectedScript` | invoke | MAIN-world capture script source | `ipc.ts:1280` | 2 |
| `getWebviewSelectScript` | invoke | MAIN-world selection overlay source | `ipc.ts:1281` | 2 |
| `window:maximizeChanged` | event | → `onWindowMaximizeChange(bool)` | emit `ipc.ts:132` | 5 |
| `updater:state` | event | → `onUpdaterState(state)` | emit `updater.ts:93` | 5 |
| `binaries:progress` | event | → `onBinariesProgress({phase,fraction,error?})` | emit `ipc.ts:880`, `main.ts:705` | 5 |
| `ai:variantFallback` | event | → `onVariantFallback({failedVariant})` GPU→CPU fallback | emit `main.ts:732` | 3 |
| `interceptor:newPosts` | event | → `onNewPosts`: generic "library changed" (`{count,platform}`, `{source}`, `{refresh}`) (DATA-19) | emit `ipc.ts:148,175,386,390,739`; `main.ts:649,673` | 1 |
| `download:progress` | event | → `onDownloadProgress(job)` | emit `ipc.ts:137` | 2 |
| `analyze:progress` | event | → `onAnalyzeProgress(job)` | emit `ipc.ts:140` | 3 |
| `web:progress` | event | → `onWebProgress(job)` | emit `ipc.ts:143` | 4 |
| `search:chatToken` | event | → `onChatToken({start,runId}` / `{token,runId})` | emit `ipc.ts:1135` | 3 |
| `ai:remoteStatus` | event | → `onAiRemoteStatus(status)` | emit `ipc.ts:1110` | 3 |
| `analyze:modelProgress` | event | → `onModelProgress({id,progress,label})` | emit `ipc.ts:658` | 3 |
| `stt:modelProgress` | event | → `onSttModelProgress(...)` | emit `ipc.ts:1213` | 3 |
| `emb:modelProgress` | event | → `onEmbModelProgress(...)` | emit `ipc.ts:1241` | 3 |
| `aitags:clusterProgress` | event | → `onClusterProgress(p)` | emit `ipc.ts:931` | 3 |
| `aitags:aliasProgress` | event | → `onAliasProgress(p)` | emit `ipc.ts:956` | 3 |

## Data touched (area summary)
| store | written by | read by |
|---|---|---|
| `posts` | 1 (user layer, deletes, resets, preview/blur), 2 (`bulkUpsert`, paths), 3 (`ai_*`), 4 (web, manual) | all |
| `post_media` | 1, 2 (`local_path`), 4 | 1, 2, 3 |
| `collections`, `post_collections` | 1 (+2 folder sync, +3 promote views) | 1, 5 |
| `post_tags`, `post_entities` | 1 (manual tier, resets), 3 (AI tiers, rename/merge, aliases) | 1 (filters/search), 3 |
| `tag_alias`, `tag_cluster`, `tag_cluster_membership` | 3 (wiped by 1's resets) | 3 (alias read by 1 for manual tags) |
| `web_snapshots` | 4 (cascade-deleted by 1) | 4 |
| `jobs` | 2/3/4 via `jobstore` | the same |
| `downloads` | none | none |

Files under `<userData>/`:
- `shelfy.sqlite` (+ `-wal`, `-shm`)
- `assets/{thumbnails,images,videos,files,previews,web}`
- `thumb-cache/`
- `logs/main.log(.1)`
- settings JSON (DATA-49)
- `Partitions/social/` (cookies, area 2)

Other stores: renderer localStorage keys (DATA-49); OS Keychain / env for AI API keys (area 3).

## Background jobs, queues & concurrency
1. **`jobs` mirror:**
   - Durable state for the download, analyze and web queues (one upsert per state transition, batched for bulk).
   - Recovered at boot (`electron/main.ts:683`) before the UI interacts.
   - Recovered jobs restart from zero; there is no byte or step resume.
2. **Preview-cover queue** (`electron/preview-cache.ts:142`):
   - in-memory FIFO, concurrency 3, dedupe on (id, url)
   - not persisted; re-derived from candidates on the next upsert, and Twitter-only at +5 s after start
3. **Cover-repair queue** (`electron/preview-repair.ts:154`): concurrency 2, ≤80 pending, 15-minute per-post cooldown, 30-minute global block after HTTP 429; in-memory.
4. **Thumbnail generation:**
   - 8 slots (4 on Linux), LIFO, in-flight dedupe.
   - On Linux the decode is synchronous and blocks the main thread.
   - Startup chain: +15 s pre-warm (sequential) → blur backfill (sequential) → `interceptor:newPosts`.
5. **All SQL runs synchronously on the Electron main thread:**
   - Long work yields with `setImmediate`: bulk loops in batches of 200 (`electron/ipc.ts:60`), deletes in batches of 200, export in chunks of 500.
   - Only tag clustering is offloaded, to `worker_threads` (120 s timeout, in-process fallback; area 3).
6. **Caches:** stats TTL 5 s; IDF / tag / keyword / alias memos invalidated on every content write (DATA-48).
7. **Renderer pacing:**
   - list reload coalescing: 400 ms quiet / 2 s max (`src/hooks/usePosts.ts:296`)
   - stats bump window 800 ms (`src/App.tsx:558`)
   - Downloads stats poll every 5 s while active
   - search debounce 300 ms

## External services & endpoints
- **Preview cache** (DATA-44):
  - Hosts: `https://*.cdninstagram.com`, `https://*.fbcdn.net`, `https://pbs.twimg.com`, `https://i.pinimg.com` / `*.pinimg.com`.
  - Image GETs from an in-memory, cookie-less partition with a desktop Chrome User-Agent and a platform Referer.
- **Cover repair** (DATA-45): HTML GETs on `https://www.instagram.com/{p,reel,tv}/<code>/`, `https://x.com|twitter.com/<user>/status/<id>`, `https://www.pinterest.com/pin/<id>/`; parses `og:image` / `og:url`.
- **Shared infra:** `serverUtils.downloadFile` → `huggingface.co` / `*.hf.co` (models, area 3); `archive-utils.readFeedUrl` → update feed from `app-update.yml` (https only, area 5).
- **No network:** db, IPC, `asset://` and the logger.

## Web-migration risks & notes
1. **No FTS. Search = multi-column `LIKE '%term%'` + correlated `EXISTS` + a JS UDF + per-term IDF full scans.** (`electron/db.ts:1399-1761`)
   - On Postgres, `LIKE` is case-sensitive, while SQLite's is ASCII-case-insensitive. Every search, tag LIKE and IDF probe must become `ILIKE` / `lower()`.
   - `word_match` needs a regex or tsvector equivalent.
   - Recommended: `pg_trgm` + `unaccent`, or `tsvector` on a generated searchable column.
   - The IDF memo must become per-user statistics.
2. **SQLite-only SQL to rewrite:**
   - `INSERT OR IGNORE` / `INSERT OR REPLACE` → `ON CONFLICT`.
   - `UPDATE OR IGNORE` (alias re-canonicalization, `db.ts:3988`, `db.ts:4128`) has no Postgres equivalent.
   - `unixepoch()`, `strftime(...,'unixepoch')`.
   - `SUM(<boolean>)` in `getStats` (`db.ts:2513`).
   - Select aliases inside `HAVING` (`db.ts:4253`, `db.ts:4916`).
   - A non-aggregated `p.timestamp` in a `GROUP BY` query (`searchPostsByTags`, `db.ts:5039-5044`).
   - `ORDER BY … rowid` (`db.ts:5542`); `lastInsertRowid` → `RETURNING`.
   - `PRAGMA table_info` / `user_version` migrations; `.iterate()` streaming; 500-id `IN` chunking (`db.ts:1024`) → `= ANY($1)`.
3. **Synchronous better-sqlite3 assumptions.** Several logical operations are multiple statements outside one transaction. They are safe today only because JS is single-threaded, and become races under an async multi-request server:
   - `upsertWebReference`: archive → `bulkUpsert` tx → replace media → UPDATE (`db.ts:2422-2467`)
   - `addManualBookmark`: two transactions (`db.ts:2763-2785`)
   - `importFromJSON`: posts and collections committed separately (`db.ts:5137-5149`)
   - Wrap each in an explicit transaction.
4. **Multi-tenant keys:**
   - `posts.id` is a global PK holding platform-native ids. They collide across users and potentially across platforms (all numeric).
   - IG rows may be keyed by media id, pk or shortcode, and there is no `UNIQUE(platform, shortcode)`, so duplicates are possible.
   - `tag_alias(alias_norm)`, `tag_cluster_membership(tag_norm)` and `jobs(kind,key)` are global PKs.
   - Import matches collections by `external_id` without platform (`db.ts:5258`).
   - Add `user_id` to every PK and index, plus `UNIQUE(user_id, platform, external_id)`.
5. **Process-global caches** (stats, IDF, tag counts, keyword tokens, alias map) are invalidated only by in-process writes. They are wrong with multiple server instances or users; scope them per user, or replace them with SQL aggregates.
6. **Absolute local paths are persisted in the DB and in exports:**
   - `posts.*_path`, `post_media.local_path`, manual `post_media.source_url`
   - `screenshotPath` / `chunks[]` inside `web_pages_json` and `web_snapshots.web_pages_json`
   - A desktop→cloud migration must upload the files and rewrite these to object keys. The paths are OS-specific and include the OS username.
7. **JSON-in-TEXT columns** (`ai_*` arrays, `user_tags`, 6× `web_*_json`, snapshots) map to JSONB or `text[]`. `post_tags` must stay a maintained table, because tiers and alias canonicalization are applied at write time.
8. **Timestamps & paging:**
   - `timestamp` is ISO TEXT with `''` / NULL sentinels and lexicographic sort; move to `timestamptz`.
   - Replace OFFSET paging and the per-page `COUNT(*)` (`db.ts:1824`) with keyset cursors and cached or estimated totals.
   - "Select all" ships every matching id to the client and back (≤100k); replace it with server-side bulk operations by filter.
9. **Heavy list payloads.** `getPosts` does `SELECT *`: web rows carry `web_pages_json` (page text) and ≤20k `text`, plus a blur data URI per row; the Websites view requests 500 rows at once. Project list columns server-side.
10. **Trust boundary.**
    - The main-side `db:bulkUpsert` accepts any object fields (`*_path`, `ai_*`, `web_*`). Only the renderer whitelist (`src/lib/browserSanitize.ts`) protects it.
    - A crafted payload storing an arbitrary path would later be `unlink`ed by `deletePosts` if it ever bypassed the sanitizer.
    - The web API must whitelist fields server-side, never accept storage keys from clients, and use small batch limits (100k today).
11. **Export/import is not a safe migration vehicle yet.**
    - `normalizeImportedPost` (`db.ts:5096`) sends every non-IG record through the Twitter parser (`electron/tw-parser.ts:311`).
    - Result: Pinterest, web and manual posts come back as `twitter`. `updateMeta` even flips existing un-downloaded rows (`db.ts:2171`).
    - `userNote`, `userTags` and `web_*` are dropped; snapshots, aliases and clusters are not exported.
    - Exports leak absolute paths and blur data. Define a versioned export v2 before relying on it.
12. **Reset/delete leave orphans:**
    - `db:clearAll` keeps collections and every file.
    - `clearAllAssets` and post deletes never purge `thumb-cache/`, which has no GC.
    - Define explicit server-side purge jobs and their semantics.
13. **Derived-index inconsistency.** `post_tags` has PK (post_id, tag_norm) shared by the AI and manual tiers with `INSERT OR IGNORE`:
    - The same tag can't be both AI and manual.
    - Clearing manual tags drops an AI tag of the same name from the index until the next analysis (`db.ts:2716-2727`, `db.ts:2883-2927`).
14. **Security (desktop today; a requirement for the web):**
    - The `asset://` root is the whole userData (`electron/main.ts:174-177`), so renderer code can read `shelfy.sqlite`, `ai-providers.json` and the `Partitions/social` cookie store.
    - `shell:openPath` is likewise confined only to userData, which includes `runtime-bin/` executables.
    - On the web: per-user object ACLs and short-lived signed URLs.
15. **Server-side media fetching.**
    - IG CDN URLs expire, so covers must be persisted at ingest (DATA-44).
    - From datacenter IPs, IG/X/Pinterest CDN and HTML fetches (DATA-45) are likely throttled or blocked.
    - Plan for client-side or browser-extension capture (a capture-on-view spike exists: DATA-56).
16. **Native image pipeline.** `nativeImage`, OS thumbnailers and the blur generation map to sharp/libvips workers at ingest, or to an image CDN (`?w=` → resize params).
17. **Per-user volume (order-of-magnitude estimates, not measured; code comments mention libraries >10k posts and "multi-hundred-MB" exports):**

    | item | size |
    |---|---|
    | social row (with AI + blur) | ~2–6 KB |
    | web row | 20–300 KB (+ snapshots) |
    | post_media | ~0.3 KB/slide |
    | post_tags | ~10–25 rows/post |
    | DB for a 10k-post library | ~50–150 MB |
    | preview cover | ~40–120 KB |
    | thumb tile | ~30–80 KB |
    | original image | 0.2–5 MB |
    | video | 2–100 MB |
    | web screenshot | 0.2–3 MB/page |
    | covers only, 10k posts | ~0.5–1 GB |
    | full offline archive | tens to hundreds of GB |
18. **Quick wins:**
    - keyset pagination
    - bulk operations by filter
    - one typed SSE stream replacing the overloaded `interceptor:newPosts`
    - renditions and blur generated at ingest
    - `jobs` maps 1:1 onto a Postgres-backed queue
    - drop dead pieces: the `downloads` table, the 11 uncalled bridge channels, `getTopTagsForTextQuery`, `upsertWebReferences`
    - expose "remove from collection" (DATA-27)
    - fix `getStats` omitting `manual` from `byPlatform` (`db.ts:2529`)
