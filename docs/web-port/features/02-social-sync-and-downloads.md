# Social sources sync & media downloads — feature index
> Scope: `electron/webview-injected.ts`, `webview-preload.ts`, `webview-select.ts`, `interceptor.ts`, `anonymous-media.ts`, `preview-cache.ts`, `preview-repair.ts`, `capture-mvp.ts`, `downloader.ts`, `binaries.ts` (yt-dlp/ffmpeg part), `ig-parser.ts` / `tw-parser.ts` (response parsers), webview/session/CDP setup in `electron/main.ts`, sync/download IPC in `electron/ipc.ts` + `electron/preload.ts`; renderer `src/views/Browser.tsx`, `src/views/Downloads.tsx`, `src/hooks/useBrowser{Webview,Sync,Intercept,Scripts,Selection}.ts`, `useSourceSync.ts`, `useDownloads.ts`, `useDownloadPrefs.ts`, `src/lib/browser{Scripts,Urls,Sanitize}.ts`; build tooling `provision-binaries.ps1`, `scripts/make-binary-packs.ts`, `build/postinstall.ts`; tests in `tests/` + `e2e/`. Snapshot: working tree of `dev`, 2026-10-02.
> Out of scope (one-line refs only): `capture-engine.ts` + `adblock.ts` = Playwright web-capture engine (area 4); `bookmarks.ts` = manual file bookmarks (area 4); JSON import normalizers `normalizeExportedPost` (area 4); DB internals / collections CRUD (area 1); `asset://` protocol, thumbs, Activity Center rendering, PostModal (area 5); VLM analysis of downloaded media, llama/whisper provisioning (area 3).

## Overview
- **Sync = scraping the user's own logged-in browser session, not an API client.** Instagram, X and Pinterest each run in an always-mounted `<webview partition="persist:social">` tab (`src/views/Browser.tsx`). The renderer injects `webview-injected.ts` (fetch/XHR patch + in-page parsers) and `webview-select.ts` (selection overlay) into the page MAIN world; batches flow `contextBridge` → `sendToHost('intercepted')` → `useBrowserIntercept` → `sanitizeInterceptedBatch` → `db:bulkUpsert` → SQLite.
- **Sync orchestration lives in the renderer, not in main:** `useBrowserSync` (manual "Auto-import": IG REST replay + gradual 2-pass scroll scripts from `src/lib/browserScripts.ts`), `useSourceSync` (background multi-step runs launched from the Library), plus plumbing hooks. Main only hardens the webview, serves the scripts and persists.
- **Downloads are a separate, on-demand pipeline** (`electron/downloader.ts`): in-memory queue mirrored to the `jobs` table; thumbnails/images fetched directly and cookie-less from platform CDNs; videos via pinned, anonymous-only yt-dlp; files in `<userData>/assets/`, paths in `posts`/`post_media`; UI = Downloads view + Activity Center via `download:progress`.
- **After a sync nothing is downloaded automatically** except a small cover preview cache (`preview-cache.ts`); expired covers are repaired on demand via `og:image` (`preview-repair.ts`). AI analysis (area 3) only works on media already on disk.
- **Runtime binaries** (yt-dlp pinned `2026.08.19`, ffmpeg) are provisioned into `<userData>/runtime-bin` by `electron/binaries.ts`.
- **Working tree vs HEAD (v1.0.2-beta.4):** the authenticated (cookie) yt-dlp fallback + login lane were removed (anonymous-only now); X moved to `/i/history` with a DOM fallback; preview cache/repair + anonymous media session added (untracked files). Capture-on-view via CDP (`capture-mvp.ts`) is still an env-gated spike.
- **Platforms in code: Instagram, X/Twitter, Pinterest only.**

## Platform matrix

| Platform | What can be synced | How it is captured | Login / session | Parser | Fields extracted | Media download | Anti-bot / ban mitigations |
|---|---|---|---|---|---|---|---|
| **Instagram** | Saved "All posts" (`/<user>/saved/all-posts/`) and native saved collections (`/<user>/saved/<slug>/<numericId>/`, mapped to a tag by numeric folder id). Not: likes, own posts, stories, discovery of not-yet-imported folders. | 1) Passive fetch/XHR hook on URLs containing `/graphql/query`, `/api/v1/feed/saved/`, `/api/v1/feed/collection/` (no doc_id pinning). 2) Active REST replay `GET /api/v1/feed/saved/posts/?max_id=` or `/api/v1/feed/collection/<id>/posts/?max_id=` with `X-IG-App-ID` (scraped from page, fallback `936619743392459`), `credentials:'include'`, 700 ms gap, max 100 pages. 3) Gradual 2-pass scroll (650 ms settle) to lazy-load tiles. Username via `GET /api/v1/accounts/current_user/`. | Required. Manual login in webview; Meta/Facebook OAuth popup allowed; login wall detected via `/accounts/(login\|signup\|emailsignup)`. | `parseInstagramResponse` `electron/webview-injected.ts:143` (REST `items`/`feed_items` + GraphQL `edges[].node` walker, 200k-node budget). | id (`item.id`→`pk`→shortcode; GraphQL `node.id`), shortcode, postUrl `/p/<sc>/`, profileUrl, authorUsername (authorName always empty), caption, thumbnailUrl, mediaType image/video/carousel, media[] (image URL per slide; video slides only typed `video` + cover URL — `video_versions`/`video_url` are ignored), timestamp (`taken_at` or decoded from shortcode). | Images/thumbnails: direct cookie-less CDN fetch (`*.cdninstagram.com`, `*.fbcdn.net`, Referer instagram.com); 403/404 on expired signed URL → anonymous yt-dlp URL refresh. Videos/reels/carousel video slides: anonymous yt-dlp (`--use-extractors Instagram`, `--playlist-items N`). Covers: preview cache. | Mostly passive capture of the page's own traffic; replay paced 700 ms; human-like gradual scroll; fixed Chrome UA; downloads never use account cookies; image jitter 120–400 ms; global 4 download slots. |
| **X / Twitter** | Bookmarks (default tab `x.com/i/history`, Bookmarks sub-tab; legacy `/i/bookmarks`). Not: Likes tab (explicitly ignored), bookmark folders (response shape not parsed), lists. | Passive hook on `/i/api/graphql/…` URLs containing "bookmark" (case-insensitive); DOM fallback scan of rendered `article[data-testid="tweet"]` on every scroll step (no extra requests); gradual 2-pass scroll (750 ms settle). No replay. | Required. Google/Apple OAuth popups allowed; login wall detected at `/i/flow/login` or `/login`. | `parseTwitterResponse` `electron/webview-injected.ts:329` (`bookmark_timeline_v2` / `bookmarks_timeline` → `TimelineAddEntries`, `cursor-bottom` paging); `scanTwitterBookmarksDom` `:684`. | id (`rest_id`), postUrl `x.com/<user>/status/<id>`, profileUrl, authorUsername + authorName (`user.core` then `legacy`), `full_text`, mediaType text/image/images/video, media (`media_url_https`; video/GIF → poster only, `video_info.variants` ignored), thumbnailUrl (first media else avatar), `created_at` → ISO. DOM fallback: text, own images (quoted-post media excluded), video poster, `<time datetime>`. | Images: `pbs.twimg.com` rewritten to `?name=orig`. Videos/GIFs: anonymous yt-dlp (`twitter` extractor, `--no-playlist`, `x.com//status` → `/i/status` fix). | Purely passive + DOM read; no authenticated replay; no image pacing; anonymous downloads. |
| **Pinterest** | Boards and board sections (`/<user>/<board>/…`, tag keyed by `<user>/<slug>` — not rename-safe). Not: profile `_saved`/`_created` tabs, "all pins", board discovery. | Passive hook on resource RPC `/resource/<X>Resource/get/` for BoardFeed / BoardSectionPins / UserPins / UserActivityPins / UserActivityFeed; SSR first page parsed from inline JSON `<script>` (no network replay: RPC is bot-guarded and would 403); gradual 2-pass scroll (650 ms). | Session in `persist:social`; source sync aborts with `login` on a `/login` redirect. | `parsePinterestResponse` `:586` / `mapPin` `:493`; `replayPinterestSSR` `:768`. | id, postUrl `/pin/<id>/`, profileUrl, authorUsername/Name (`pinner`/`native_creator`), text = title — description + outbound link, thumbnailUrl (largest `images` size), mediaType image/video/carousel (idea/story pins, carousels), media (progressive MP4 preferred over HLS), `created_at` (often missing → import time). | Images: try `/originals/` rewrite, fall back to served size (`i.pinimg.com`). Videos: anonymous yt-dlp (`Pinterest` extractor) — stored direct MP4 URL unused. | No RPC replay; end via cursor sentinels (`''`, `-end-`, `Y2JOb25lO…`) + stuck-cursor guard; image jitter 120–400 ms. |
| **TikTok, YouTube, Reddit, Facebook, Threads, LinkedIn…** | Not implemented. | — | — | — | — | Downloader rejects any video host outside IG/X/Pinterest (`VIDEO_POST_HOSTS`). | Facebook/Google/Apple appear only as OAuth hosts; TikTok/LinkedIn only in the web-capture adblock list (area 4). |

## Features

### SYNC-01 · Embedded social browser (3 persistent tabs)
- **What:** IG / X / Pinterest each in a `<webview partition="persist:social">`; all stay mounted (stacked, inactive at `opacity:0`, `backgroundThrottling=false`) so syncs continue in background; back/forward/reload/URL bar.
- **Entry points:** `src/views/Browser.tsx:1010` (webviews), `:431` (nav buttons) → `src/hooks/useBrowserWebview.ts:300`; always mounted under every view `src/App.tsx:705`.
- **Data:** none (live page state, per-tab React state).
- **Local deps:** Electron `<webview>`, webview/session.
- **External calls:** instagram.com, x.com, pinterest.com + their CDNs.
- **Status:** shipped.
- **Web port:** browser-extension — the user's real browser tabs replace webviews; an SPA cannot frame these sites (frame-ancestors/X-Frame-Options).

### SYNC-02 · Persistent social login session
- **What:** user logs in manually inside the webview; cookies persist on disk in `persist:social`, shared by the 3 tabs, OAuth popups, web capture and the post-modal fallback webview. There is no logout / clear-session UI.
- **Entry points:** `electron/interceptor.ts:19` (`setupInterceptor`, called `electron/main.ts:564`); `src/views/Browser.tsx:1023` (partition attr).
- **Data:** `<userData>/Partitions/social/` (cookie jar + site storage = credential store).
- **Local deps:** Electron session/partition, filesystem.
- **External calls:** platform login endpoints.
- **Status:** shipped.
- **Web port:** browser-extension — session stays in the user's browser; the backend must never receive platform cookies.

### SYNC-03 · OAuth popups, navigation confinement, permissions, UA
- **What:** OAuth popups (Google/Apple/Facebook) open as windows sharing `persist:social`; webview + popup navigation confined to IG/X/Pinterest/CDN/auth hosts; other links go to the OS browser after an SSRF check; only `notifications` + `clipboard-sanitized-write` granted; fixed UA "Chrome/124 macOS" on every OS.
- **Entry points:** `electron/main.ts:340` (host regex), `:379` (window-open), `:417`, `:421` (will-navigate); `electron/interceptor.ts:7`, `:13`, `:23-30`.
- **Data:** none.
- **Local deps:** Electron webContents events, session.
- **External calls:** accounts.google.com, appleid.apple.com, facebook.com.
- **Status:** shipped.
- **Web port:** drop (desktop-only) — the real browser handles OAuth; extension `host_permissions` replace the allowlist; real UA removes the Chromium-126-vs-"Chrome 124" mismatch.

### SYNC-04 · Webview hardening & capture-script delivery
- **What:** main forces `nodeIntegration=false`, `contextIsolation=true`, `webSecurity`, strips `disablewebsecurity`, pins the preload to `webview-preload.js`; MAIN-world scripts are read from disk by main, fetched once by the renderer, cached; status `loading/ready/error` gates Auto-import/Select with a retry chip.
- **Entry points:** `electron/main.ts:464`; `src/hooks/useBrowserScripts.ts:51` → `getWebviewInjectedScript` / `getWebviewSelectScript` `electron/ipc.ts:1280-1281` (readers `:74`, `:93`); retry `src/views/Browser.tsx:777`.
- **Data:** none.
- **Local deps:** Electron, filesystem (`dist-electron/*.js`).
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — scripts ship inside the extension package; no runtime delivery.

### SYNC-05 · MAIN-world injection per document load
- **What:** on `dom-ready`/`did-finish-load` the renderer `executeJavaScript`s capture hook + overlay (bypasses page CSP) once per load (guard reset on `did-start-loading`) and re-enables select mode after reloads; injection happens after page start, so early API responses can be missed (hence SYNC-11/12/16).
- **Entry points:** `src/hooks/useBrowserIntercept.ts:214` (inject), `:248` (listeners); guard reset `src/hooks/useBrowserWebview.ts:197`.
- **Data:** page globals `__socialSavedInjected`, `__ssSelectInjected`.
- **Local deps:** webview `executeJavaScript`.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — manifest `content_scripts` with `world:"MAIN"`, `run_at:"document_start"` (earlier than today).

### SYNC-06 · Passive fetch/XHR interception + relay bridge
- **What:** patches `window.fetch` (clone → text) and `XMLHttpRequest.open/send` (one `load` listener) for URLs passing `matchPlatform`; parses in-page; stashes items in `__ssCapturedItems` (FIFO 5000 keys) + monotonic `__ssCapturedOrder`; relays `{items, hasNextPage, platform}` via `__socialSavedBridge.send` → `sendToHost('intercepted')` (same-origin postMessage fallback).
- **Entry points:** `electron/webview-injected.ts:833` (fetch), `:847` (XHR), `:621`, `:641` (emit); `electron/webview-preload.ts:32`, `:47`, `:58` → `src/hooks/useBrowserIntercept.ts:98`.
- **Data:** in-page globals only.
- **Local deps:** webview preload (contextBridge, `ipcRenderer.sendToHost`).
- **External calls:** none extra (reads the page's own responses).
- **Status:** shipped.
- **Web port:** browser-extension — reusable as-is; the existing postMessage fallback becomes the relay to an ISOLATED content script → `chrome.runtime` → backend upload.

### SYNC-07 · Instagram saved-feed parser
- **What:** REST (`items`/`feed_items`, `{media}` wrapper, `more_available`) and GraphQL (`edges[].node.shortcode` walker, `page_info.has_next_page`), carousel children, shortcode→date decode (IG epoch), node/depth budgets against hostile payloads.
- **Entry points:** `electron/webview-injected.ts:143`, `:116` (shortcode date), `:211` (walk budget).
- **Data:** produces InterceptItem (see matrix).
- **Local deps:** none (pure JS).
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — pure, reusable verbatim; extend to keep `video_versions`/`video_url`; canonicalize ids (see risks).

### SYNC-08 · X bookmarks GraphQL parser
- **What:** reads `bookmark_timeline_v2` / `bookmarks_timeline` `TimelineAddEntries`, unwraps `tweet_results.result(.tweet)`, author from `user.core` with `legacy` fallback, guarded date parsing, `cursor-bottom` → hasNextPage (empty page with no cursor ⇒ end).
- **Entry points:** `electron/webview-injected.ts:329`, `:443` (`toIsoHttpDate`).
- **Data:** InterceptItem (see matrix).
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — reusable verbatim; extend to keep `video_info.variants` (direct MP4).

### SYNC-09 · X bookmarks DOM fallback scan
- **What:** on `/i/history` (only while the Bookmarks tab is selected) or `/i/bookmarks`, reads rendered tweet cards each scroll step; skips ids already captured from the API; excludes quoted-post media; emits `hasNextPage=null`.
- **Entry points:** `electron/webview-injected.ts:684` (exposed `:741`) ← scroll hook `src/lib/browserScripts.ts:86`; test `tests/electron/webview-injected-twitter.test.ts:5`.
- **Data:** InterceptItem with reduced fields.
- **Local deps:** none (DOM).
- **External calls:** none.
- **Status:** shipped (new, uncommitted in working tree).
- **Web port:** browser-extension — reusable verbatim.

### SYNC-10 · Pinterest resource-RPC parser
- **What:** maps pins from `resource_response.data` (or `.results`): idea-pin pages, carousel slots, `video_list` (V_720P…HLS), largest image size, pinner, title/description/link; skips board/ad rows; cursor end via sentinels or a repeated cursor (`__ssPinLastCursor`).
- **Entry points:** `electron/webview-injected.ts:586`, `:493`, `:455`, `:469`, `:605-614`.
- **Data:** InterceptItem.
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — reusable verbatim; or replace with Pinterest API v5 (official API).

### SYNC-11 · Pinterest SSR first-page replay
- **What:** at sync start scans inline `script[type="application/json"]` / `script[id^="__PWS"]` blobs for an embedded `BoardFeedResource` (two shapes, 200k-node budget) and emits page 1 with `hasNextPage=null`; no network call.
- **Entry points:** `electron/webview-injected.ts:768` (exposed `:829`) ← `src/hooks/useBrowserSync.ts:344`.
- **Data:** none persisted beyond ingest.
- **Local deps:** none (DOM).
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — reusable verbatim (likely unnecessary with `document_start` injection).

### SYNC-12 · Instagram REST feed replay (active pagination)
- **What:** in the page origin, re-fetches the current listing's REST feed from the top (`max_id` cursor, `X-IG-App-ID`, cookies included, 700 ms gap, ≤100 pages) so the SSR-inline first page and every page are captured through the same hook; overlap dedups on upsert.
- **Entry points:** `src/lib/browserScripts.ts:103` ← `src/hooks/useBrowserSync.ts:336`.
- **Data:** none (feeds SYNC-06).
- **Local deps:** webview `executeJavaScript`.
- **External calls:** `www.instagram.com/api/v1/feed/saved/posts/`, `/api/v1/feed/collection/<id>/posts/`.
- **Status:** shipped.
- **Web port:** browser-extension — runs with the user's cookies from the user's IP; MV3 needs it as a function/file (no code strings).

### SYNC-13 · Auto-import (manual sync start/stop)
- **What:** on a saved listing, "Auto-import" starts a per-tab sync (reset counters/timer, flush pre-sync buffer, clear `__syncStop`, run replay/scroll); same button stops; inside an IG folder / Pinterest board it opens the folder→tag modal first; refused while a background source-sync owns the tab. IG, X and Pinterest can sync concurrently.
- **Entry points:** `src/views/Browser.tsx:384` (`handleImportClick`), `:584` (button) → `src/hooks/useBrowserSync.ts:272` (`startSync`), `:355` (`stopSync`).
- **Data:** per-tab React state/refs.
- **Local deps:** webview `executeJavaScript`.
- **External calls:** see SYNC-12/14.
- **Status:** shipped.
- **Web port:** browser-extension (popup/side-panel trigger) + realtime-push of progress to the SPA.

### SYNC-14 · Gradual two-pass auto-scroll
- **What:** scrolls ~0.55 viewport per step (settle 650 ms IG/Pinterest, 750 ms X), pulls the last tile into view when stuck, 2 passes (2nd from top for virtualized misses); a pass ends after 60 no-growth steps or 3×20 s stalls; hard caps 16 000 steps / 30 min; optional per-step scan hook (X DOM).
- **Entry points:** `src/lib/browserScripts.ts:30`, `:84`.
- **Data:** reads `__ssCapturedOrder`, `__lastInterceptAt`, `__syncStop`.
- **Local deps:** webview `executeJavaScript`.
- **External calls:** whatever the page lazy-loads.
- **Status:** shipped (designed for the flag-gated capture-on-view SYNC-27, yet every sync pays the slower 2-pass cost).
- **Web port:** browser-extension — logic reusable; repackage as `func`/file and handle background-tab timer throttling.

### SYNC-15 · Sync termination & lifecycle guards
- **What:** X/Pinterest end on authoritative `hasNextPage===false`; IG ends only when replay AND scroll settle; a generation token ignores late completions of replaced syncs; idempotent `finishSync` (sets `__syncStop`, clears timer/collection target); auto-stop when a tab leaves its listing (post/pin detail modals exempt).
- **Entry points:** `src/hooks/useBrowserIntercept.ts:144`; `src/hooks/useBrowserSync.ts:193`, `:338-350`; `src/hooks/useBrowserWebview.ts:181`; `src/views/Browser.tsx:308`.
- **Data:** none.
- **Local deps:** webview.
- **External calls:** none.
- **Status:** shipped (tests `tests/hooks/useBrowserSync.test.tsx:56`).
- **Web port:** browser-extension.

### SYNC-16 · Pre-sync capture buffer
- **What:** batches intercepted while not syncing (e.g. the first page shown on opening a saved list) are buffered per tab (≤3000, scoped to the listing URL, reset on leaving it) and deduped/flushed when Auto-import starts; browsing without Auto-import persists nothing.
- **Entry points:** `src/hooks/useBrowserIntercept.ts:115`; `src/hooks/useBrowserWebview.ts:162`; flush `src/hooks/useBrowserSync.ts:300`.
- **Data:** in-memory.
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — product decision: passive always-on capture becomes possible.

### SYNC-17 · Cross-world payload sanitization
- **What:** page payloads are hostile: ≤1000 items/batch, id ≤256 chars, text ≤20k, URLs ≤4096 and http(s) only, ≤60 media/post, platform must be one of 3 and is stamped on every item (in auto-import it is the page-declared batch platform, not the tab id); main caps bulk arrays at 100 000 and drops non-objects.
- **Entry points:** `src/lib/browserSanitize.ts:69`, `:115` ← `src/hooks/useBrowserIntercept.ts:114`, `src/hooks/useBrowserSelection.ts:130`; `electron/ipc.ts:366-379`.
- **Data:** none.
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped (tests `tests/lib/browserSanitize.test.ts`).
- **Web port:** api+db — reuse as the server-side ingest validator (extension uploads are untrusted).

### SYNC-18 · Batch ingest, dedup & post-ingest hooks
- **What:** each batch → `db:bulkUpsert`: INSERT OR IGNORE by `id`; existing rows refresh metadata/CDN URLs only while nothing is downloaded; `post_media` merged without touching downloaded slides; then covers queued to the preview cache (DL-24) and `interceptor:newPosts {count, platform}` pushed (gallery reload, sidebar badge). No auto-download, no auto-AI.
- **Entry points:** `src/hooks/useBrowserSync.ts:221` → `electron/preload.ts:270` → `electron/ipc.ts:366` → `electron/db.ts:2144`, `:2113` (area 1).
- **Data:** `posts`, `post_media`.
- **Local deps:** SQLite.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** api+db + realtime-push — idempotent upsert keyed by (user, platform, canonical id).

### SYNC-19 · Live sync status & counters
- **What:** sync bar shows fetching vs scrolling, scanned / new / existing, library total, elapsed timer; idle "N captured" chip (last sync's inserts); sidebar badge accumulates new posts per platform (suppressed when viewing that tab unless syncing).
- **Entry points:** `src/views/Browser.tsx:834`, `:815`; counters `src/hooks/useBrowserSync.ts:242-259`, `:166`; badge `src/App.tsx:575`.
- **Data:** `db:getStats`.
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** realtime-push — extension → backend → SPA (SSE/WebSocket) or SPA↔extension messaging.

### SYNC-20 · Folder/board → tag import
- **What:** inside an IG saved folder or Pinterest board, Auto-import / Import-selected first asks where to file posts: existing tag matched by `external_id`, new tag (name from page `h1`, else de-slugified URL; `ig_name` kept) or no tag; each batch then `collections:addPosts`.
- **Entry points:** `src/views/Browser.tsx:321`, `:345`; `src/lib/browserUrls.ts:73`, `:93`; `src/lib/browserScripts.ts:199`; `src/components/ImportFolderModal.tsx:39`; `src/hooks/useBrowserSync.ts:260`.
- **Data:** `collections.platform/external_id/ig_name`, `post_collections`.
- **Local deps:** SQLite, webview.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** api+db (+ extension for the folder title); Pinterest API gives stable board ids.

### SYNC-21 · Selection-mode overlay
- **What:** checkboxes on each tile (IG `/p/` `/reel/` `/tv/`, tweets, pins); shift-click ranges that survive virtualization (persisted grid offsets); "Già in database" badge via `db:savedByKeys` (id OR shortcode) with click-to-open the saved post; record = captured API item else minimal DOM fallback. Overlay strings are hard-coded Italian.
- **Entry points:** `src/views/Browser.tsx:640` → `src/hooks/useBrowserSelection.ts:89` → `electron/webview-select.ts:693` (enable), `:290`, `:564`, `:753`; check `src/hooks/useBrowserIntercept.ts:152` → `electron/ipc.ts:169` → `electron/db.ts:1996`.
- **Data:** reads `posts.id/shortcode`.
- **Local deps:** webview, SQLite.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension — overlay reusable as-is (postMessage relay already present); saved-check becomes an API call.

### SYNC-22 · Import selected posts
- **What:** `collectJSON()` → sanitize (tab platform forced) → `db:bulkUpsert` → optional tag → mark saved + clear selection; Activity "saving" item and "N posts saved". Does not download media, although the folder modal labels this action "Download".
- **Entry points:** `src/views/Browser.tsx:403` → `src/hooks/useBrowserSelection.ts:114` → `electron/webview-select.ts:787`; `electron/ipc.ts:366`.
- **Data:** `posts`, `post_media`, `post_collections`.
- **Local deps:** webview, SQLite.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** browser-extension + api+db.

### SYNC-23 · Background source sync from the Library
- **What:** Gallery header button runs a sequential job in the hidden webviews: IG = all-posts + every already-imported native folder; X = bookmarks; Pinterest = every already-imported board. Per step: navigate + settle (login-page detection), ensure hook (one reload retry), take over a manual sync, wait ≤35 min; failed steps skipped, login aborts; one run per platform; re-click stops.
- **Entry points:** `src/views/Gallery.tsx:1153` → `src/App.tsx:252` → `src/hooks/useSourceSync.ts:394` (start), `:356`, `:244`, `:278`, `:306`, `:127` (planner), `:495` (stop).
- **Data:** `collections` (native = platform + external_id); localStorage `ig-saved-url`.
- **Local deps:** hidden always-mounted webviews.
- **External calls:** IG/X/Pinterest listing pages.
- **Status:** shipped (tests `tests/hooks/useSourceSync.test.ts`).
- **Web port:** browser-extension + background-job — extension drives a tab per step; backend can only schedule/remind, not run it.

### SYNC-24 · IG username & folder URL discovery
- **What:** username from the persisted saved URL, else `GET /api/v1/accounts/current_user/` from the page origin (empty ⇒ login error); folder href found on the saved index by numeric id (12 tries × 500 ms, scrolling).
- **Entry points:** `src/hooks/useSourceSync.ts:329`; `src/lib/browserScripts.ts:146`, `:170`.
- **Data:** localStorage `ig-saved-url`; `collections.external_id`.
- **Local deps:** webview.
- **External calls:** `www.instagram.com/api/v1/accounts/current_user/`.
- **Status:** shipped.
- **Web port:** browser-extension.

### SYNC-25 · Source-sync job reporting & login CTA
- **What:** per-platform state machine (navigating/syncing/done/stopped/error), step X/Y, current folder, live scanned/new, skipped steps; Activity Center live item with Stop; login failure leaves an error item with "open browser"/dismiss; done/stopped rows auto-removed after 4 s.
- **Entry points:** `src/hooks/useSourceSync.ts:195`, `:458-490`; `src/hooks/useActivity.ts:529` (area 5 UI); actions `src/App.tsx:453-466`.
- **Data:** React state only (no run history persisted).
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** realtime-push + api+db (persist run history server-side).

### SYNC-26 · Saved-URL memory & dead-URL recovery
- **What:** remembers the last IG saved URL and Pinterest board; tabs reopen there; after an IG login redirect jumps back to it; if the persisted URL fails to load (main frame, not aborted) the key is cleared and the tab falls back home.
- **Entry points:** `src/hooks/useBrowserWebview.ts:102-108`, `:136-155`, `:209`, `:270`.
- **Data:** localStorage `ig-saved-url`, `pin-board-url`.
- **Local deps:** none.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** client-only (extension storage / user prefs).

### SYNC-27 · Capture-on-view via CDP (spike)
- **What:** attaches `webContents.debugger` (CDP 1.3) to every webview and reads image bodies from IG/X/Pinterest CDNs with `Network.getResponseBody` (zero extra requests); dumps ≤60 samples + `urls.txt`/`misses.txt`; with WRITE, correlates by file basename and saves single images as image+thumbnail, carousel/video covers as thumbnail (retry 8×1.2 s). Pinterest never passes `isPostMedia`.
- **Entry points:** `electron/main.ts:370` → `electron/capture-mvp.ts:208`, `:127`, `:78`; `electron/db.ts:2646`; harness `scripts/capture-spike/{test,correlate,savecheck}.ts`.
- **Data:** `<userData>/capture-mvp/`, `assets/images|thumbnails`, `posts.*_path`, `post_media.local_path`.
- **Local deps:** CDP (Electron debugger), filesystem, SQLite.
- **External calls:** none.
- **Status:** spike — no-op unless `SHELFY_CAPTURE_MVP=1`; DB writes only with `SHELFY_CAPTURE_WRITE=1`.
- **Web port:** rethink — `chrome.debugger` works but shows a "debugging this browser" bar and draws store scrutiny; prefer service-worker re-fetch of the same CDN URL.

### SYNC-28 · Main-process IG/X response parsers (duplicates)
- **What:** `ig-parser.parseResponseBody` and `tw-parser.parseBookmarkResponse`/`extractTweet` mirror the injected parsers for offline use but have no production caller; only `normalizeExportedPost`/`igDateFromShortcode` from these files are used (JSON import, migrations — area 4/1). No Pinterest equivalent.
- **Entry points:** `electron/ig-parser.ts:366`, `electron/tw-parser.ts:186`, `:255`; callers only `tests/electron/ig-parser.test.ts`, `tests/electron/tw-parser.test.ts`.
- **Data:** none.
- **Local deps:** none.
- **External calls:** none.
- **Status:** dead (test-only).
- **Web port:** rethink — keep one parser package shared by extension and server ingest.

### SYNC-29 · Social session reuse by other subsystems (cross-area)
- **What:** `persist:social` is also used by web capture (OSR window renders arbitrary user-pasted sites in the logged-in session; the Playwright engine copies all its cookies into its context — area 4) and by the post-modal fallback `<webview>` that shows the original post logged-in (area 5).
- **Entry points:** `electron/webcapture.ts:60`; `electron/webcapture-playwright.ts:445`, `:723`; `src/components/postmodal/MediaCarousel.tsx:95`, `:210`.
- **Data:** `<userData>/Partitions/social/`.
- **Local deps:** Electron session.
- **External calls:** arbitrary sites (web capture).
- **Status:** shipped.
- **Web port:** drop — no shared cookie jar on the web; post fallback → link-out / official embeds.

### DL-01 · Download queue engine
- **What:** in-memory ordered queue with O(1) dedupe; job key `postId:assetType[:position]`; statuses pending/downloading/done/error/cancelled; 4 global slots; per-platform cap map exists but is empty; jobs of a saturated platform are skipped, not dropped; re-enqueue of an active key is ignored.
- **Entry points:** `electron/downloader.ts:1065` (pump), `:984` (runJob), `:1111` (enqueueJob), `:220`, `:44`, `:47`.
- **Data:** in-memory `jobsMap`/`postCache`; mirrored to `jobs` (DL-19).
- **Local deps:** main process, filesystem.
- **External calls:** see DL-07..11.
- **Status:** shipped (`PLATFORM_CONCURRENCY` dead config; `downloadPost`/`downloadMany` legacy exports `:1418`, `:1428` used only by tests).
- **Web port:** background-job — durable queue with per-platform rate limits (or client-side in the extension).

### DL-02 · Download a single post
- **What:** post modal "Download" queues thumbnail+image+video for that post (ignores asset-type prefs); returns the queued count so 0 shows "nothing to download" (web/text/manual); spinner cleared on progress or a grace timeout.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:119` → `download:post` `electron/ipc.ts:465` → `electron/downloader.ts:1149`.
- **Data:** reads `posts`, `post_media`.
- **Local deps:** SQLite.
- **External calls:** CDNs / yt-dlp.
- **Status:** shipped.
- **Web port:** background-job (API trigger) or extension fetch+upload.

### DL-03 · Bulk download of selected posts
- **What:** Gallery multi-select download and the analyze banner's "download remote-only posts" queue the ids with the user's asset-type prefs, `missingOnly` default true, hydrated in 200-post batches with event-loop yields, ≤100 000 ids.
- **Entry points:** `src/views/Gallery.tsx:771`, `:751` → `download:posts` `electron/ipc.ts:508` → `electron/downloader.ts:1216`.
- **Data:** reads `posts`, `post_media`.
- **Local deps:** SQLite.
- **External calls:** CDNs / yt-dlp.
- **Status:** shipped.
- **Web port:** background-job — also the prerequisite for AI on media (area 3).

### DL-04 · Download all / Download missing
- **What:** Downloads-view buttons over the whole library with the prefs; "missing" skips assets whose recorded file exists; "all" still skips files already present at their deterministic path (never forces a re-download). Disabled when no asset type is enabled.
- **Entry points:** `src/views/Downloads.tsx:537`, `:550`, `:622-659` → `download:all` `electron/ipc.ts:477`.
- **Data:** reads all post ids.
- **Local deps:** SQLite, filesystem.
- **External calls:** CDNs / yt-dlp.
- **Status:** shipped.
- **Web port:** background-job.

### DL-05 · Missing-only & dedupe semantics
- **What:** per asset type and per slide: skip when `*_path` / `post_media.local_path` exists on disk; executors also return early if the deterministic file exists (legacy un-suffixed slide 0 honored); text-only tweets skip thumbnails; manual posts and posts being deleted never queue.
- **Entry points:** `electron/downloader.ts:1149-1211`, `:801`, `:824-829`, `:904`.
- **Data:** `posts.*_path`, `post_media.local_path`, filesystem.
- **Local deps:** filesystem.
- **External calls:** none.
- **Status:** shipped (tests `tests/electron/downloader.test.ts:850`).
- **Web port:** api+db — "missing" becomes an object-storage/DB flag check.

### DL-06 · Asset-type preferences
- **What:** thumbnail / image / video toggles (default all on) in Settings, shared with the Downloads view and Gallery; same-window sync via a custom event.
- **Entry points:** `src/views/Settings.tsx:1866`, `:196`; `src/hooks/useDownloadPrefs.ts:32`.
- **Data:** localStorage `download:assetTypes`.
- **Local deps:** none (renderer storage).
- **External calls:** none.
- **Status:** shipped (e2e `e2e/downloads.spec.ts:48`).
- **Web port:** api+db (per-user settings, multi-device).

### DL-07 · Thumbnail (cover) download
- **What:** fetches `thumbnailUrl` into `assets/thumbnails/<platform>-<ident>.<ext>`; IG goes through expired-URL repair (DL-10); generates the blur-up placeholder from the new cover before emitting `done`.
- **Entry points:** `electron/downloader.ts:795`, `:1014-1032`; `electron/thumbs.ts:280` (area 5).
- **Data:** `posts.thumbnail_path`, `posts.thumb_blur`.
- **Local deps:** filesystem, Electron `nativeImage`.
- **External calls:** platform CDNs.
- **Status:** shipped.
- **Web port:** object-storage + background-job (or extension upload at capture time).

### DL-08 · Per-slide image download
- **What:** one job per image slide for `image`/`images`/`carousel` posts → `assets/images/<platform>-<ident>-<pos>.<ext>`; slide 0 also sets the post's `image_path`; posts without a media array fall back to the cover URL.
- **Entry points:** `electron/downloader.ts:813`, `:235`, `:942`.
- **Data:** `post_media.local_path`, `posts.image_path`.
- **Local deps:** filesystem, SQLite.
- **External calls:** platform CDNs.
- **Status:** shipped.
- **Web port:** object-storage + background-job.

### DL-09 · Platform image URL upgrades
- **What:** X → `?name=orig`; Pinterest → `/originals/` rewrite with fallback to the served size; extension taken from the URL (jpg/jpeg/png/webp, else `.jpg`).
- **Entry points:** `electron/downloader.ts:294`, `:302`, `:835-846`, `:279`.
- **Data:** none.
- **Local deps:** none.
- **External calls:** `pbs.twimg.com`, `i.pinimg.com`.
- **Status:** shipped (tests `tests/electron/downloader.test.ts:994`).
- **Web port:** reusable pure helpers (server or extension).

### DL-10 · Instagram expired-URL repair (yt-dlp)
- **What:** on HTTP 403/404 for an IG CDN URL of a `/p|reel|tv/` post, runs anonymous yt-dlp `--flat-playlist --skip-download --print "%(playlist_index)s\t%(thumbnail)s"` (25 s, 256 KB cap), caches fresh URLs 5 min (≤100 posts) and retries the slide once.
- **Entry points:** `electron/downloader.ts:658`, `:589`, `:644`.
- **Data:** none persisted (fresh URL used only for the file).
- **Local deps:** yt-dlp.
- **External calls:** `www.instagram.com` (anonymous).
- **Status:** shipped (new, uncommitted; test `tests/electron/downloader.test.ts:419`).
- **Web port:** rethink — anonymous IG extraction from datacenter IPs is unreliable; get fresh URLs from the extension or upload bytes at capture.

### DL-11 · Video download via yt-dlp (anonymous-only)
- **What:** https `postUrl` on the platform host allowlist; yt-dlp with `--ignore-config --no-cookies --no-cookies-from-browser --no-cache-dir --no-plugin-dirs --use-extractors <Instagram|twitter|Pinterest> --newline --user-agent`, `--no-playlist` (or IG `--playlist-items N` per carousel slide), `--` before the URL; `[download] NN%` progress; SIGTERM→SIGKILL 5 s; fragments purged on failure.
- **Entry points:** `electron/downloader.ts:879`, `:683`, `:860`, `:24-40`.
- **Data:** `assets/videos/<platform>-<ident>[-<pos>].mp4`, `posts.video_path` / `post_media.local_path`.
- **Local deps:** yt-dlp.
- **External calls:** instagram.com, x.com, pinterest.com + video CDNs (via yt-dlp).
- **Status:** shipped (test `tests/electron/downloader.test.ts:273`, `:354`).
- **Web port:** container (native binary) for public X/Pinterest; IG from datacenter IPs = high block/ban risk → extension-side bytes instead.

### DL-12 · Video format & remux (implicit)
- **What:** no `-f`, `--merge-output-format`, `--remux-video` or `--ffmpeg-location` are passed: yt-dlp's default format choice, output name fixed to `.mp4`, merging depends on yt-dlp finding ffmpeg on its own (per-OS behaviour unverified); no transcode; job fails if the expected file is missing.
- **Entry points:** `electron/downloader.ts:701-720`, `:773`.
- **Data:** `assets/videos/`.
- **Local deps:** yt-dlp, ffmpeg (indirect).
- **External calls:** via yt-dlp.
- **Status:** shipped (implicit).
- **Web port:** container — pin format/merge flags and ffmpeg explicitly.

### DL-13 · Authenticated fallback for login-walled media (removed)
- **What:** HEAD (v1.0.2-beta.4) retried age-restricted/private/login-walled videos with `persist:social` cookies exported to a Netscape file, in a serialized "login lane" with yt-dlp `--sleep-*` pacing; the working tree deletes it — such videos now end in `error`, and a test asserts no cookie retry.
- **Entry points:** `electron/downloader.ts:917-919`; `tests/electron/downloader.test.ts:273`; old code `git show HEAD:electron/downloader.ts` (~L336–L850).
- **Data:** (was) `<userData>/tmp-cookies/shelfy-cookies-*.txt`.
- **Local deps:** (was) Electron session cookies, yt-dlp.
- **External calls:** (was) authenticated platform requests.
- **Status:** dead (removed in working tree; still present in the published beta).
- **Web port:** drop — never hold platform cookies server-side; private media only via the extension.

### DL-14 · Anti-ban pacing & request hygiene
- **What:** image/thumbnail fetches send browser-like `Accept` + `Sec-Fetch-*`, platform Referer, the shared UA, `credentials:'omit'`; IG and Pinterest images wait a random, abortable 120–400 ms first; X unpaced; videos bounded only by the 4 global slots (no yt-dlp sleep flags any more).
- **Entry points:** `electron/downloader.ts:60`, `:72`, `:966`, `:19`.
- **Data:** none.
- **Local deps:** none.
- **External calls:** platform CDNs.
- **Status:** shipped (pacing disabled under tests `:79`).
- **Web port:** background-job — needs per-platform limiters shared across all users/egress IPs.

### DL-15 · Safe streaming fetch
- **What:** SSRF guard (http/https only, no loopback/private/link-local literals) on the first URL and every manual redirect hop (≤5); 60 s timeout composed with abort; streamed to `.part` with backpressure, atomic rename; partial file deleted on error.
- **Entry points:** `electron/downloader.ts:476`; `electron/net-safety.ts:87`.
- **Data:** files under `assets/`.
- **Local deps:** filesystem.
- **External calls:** any public host from post data.
- **Status:** shipped (tests `tests/electron/downloader.test.ts:942`).
- **Web port:** background-job — add DNS-resolution/IP checks server-side (current guard is hostname-literal only).

### DL-16 · Pause / resume all
- **What:** pause aborts in-flight jobs and puts them back at the queue head as pending (restart from scratch) and stops the pump; resume restarts it; paused rows dimmed; also toggled from the Activity Center.
- **Entry points:** `src/views/Downloads.tsx:663`; `src/App.tsx:441` → `download:pauseAll|resumeAll` `electron/ipc.ts:537-538` → `electron/downloader.ts:1236`, `:1248`.
- **Data:** in-memory + `jobs`.
- **Local deps:** child processes.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** background-job + api+db.

### DL-17 · Cancel job / cancel all / clear finished / clear queue
- **What:** per-job cancel (queued or live); cancel-all flips pending/downloading/error to cancelled in 200-job chunks with yields; clear-finished drops done+cancelled from memory and `jobs`; clear-queue = cancel-all + clear-finished.
- **Entry points:** `src/views/Downloads.tsx:272`, `:689`, `:711`; `src/hooks/useDownloads.ts:209-237` → `electron/ipc.ts:539-543` → `electron/downloader.ts:1253`, `:1306`, `:1344`.
- **Data:** `jobs` (kind `download`).
- **Local deps:** SQLite.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** background-job + api+db.

### DL-18 · Manual retry
- **What:** retry on error/cancelled rows re-queues the same key; there is no automatic retry, and `error` rows are terminal at boot (not resumed).
- **Entry points:** `src/views/Downloads.tsx:292` → `download:retryJob` `electron/ipc.ts:545` → `electron/downloader.ts:1335`; `electron/jobstore.ts:35`.
- **Data:** `jobs`.
- **Local deps:** SQLite.
- **External calls:** as the original job.
- **Status:** shipped.
- **Web port:** background-job — add bounded auto-retry with backoff for transient errors.

### DL-19 · Durable queue & boot recovery
- **What:** every job transition mirrored to `jobs` (bulk transactions for batches); at startup pending/downloading rows are grouped per post and re-enqueued with `missingOnly` (deleted post ⇒ rows dropped, DB error ⇒ rows kept).
- **Entry points:** `electron/downloader.ts:460`, `:448`, `:1368`; `electron/main.ts:683`; `electron/jobstore.ts:80`, `:182`.
- **Data:** `jobs` table (kind `download`, compact JSON payload).
- **Local deps:** SQLite.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** background-job (queue as source of truth).

### DL-20 · Progress streaming to the UI
- **What:** main emits `download:progress` immediately on status changes and coalesces progress-only updates every 100 ms; renderer upserts into a Map with leading+trailing 100 ms publish, refreshes stats on `done` (≤1/800 ms) and polls `db:getStats` every 5 s while the queue is active.
- **Entry points:** `electron/downloader.ts:430`; `electron/ipc.ts:136`; `electron/preload.ts:293`; `src/hooks/useDownloads.ts:158`, `:185`.
- **Data:** none.
- **Local deps:** Electron IPC.
- **External calls:** none.
- **Status:** shipped (tests `tests/hooks/useDownloads.test.tsx`).
- **Web port:** realtime-push (SSE/WebSocket).

### DL-21 · Downloads view
- **What:** header actions (download all/missing, pause/resume, clear finished, clear queue), stat pills (posts + downloaded thumbnails/images/videos), "done/total files across N posts", virtualized list grouped per post (thumb, platform, @author, Preview/Content stage counters, bar), expandable per-file rows with %/status/error and cancel/retry.
- **Entry points:** `src/views/Downloads.tsx:502`, `:341`, `:391`, `:210`.
- **Data:** jobs snapshot + `db:getStats`.
- **Local deps:** none.
- **External calls:** remote thumbnail URL fallback for rows without a local thumb.
- **Status:** shipped (tests `tests/views/Downloads.test.tsx`, `e2e/downloads.spec.ts`).
- **Web port:** client-only UI + api+db + realtime-push.

### DL-22 · Writer suspension during deletions
- **What:** before deleting posts or a post's local files, cancels their jobs, awaits in-flight writers, forgets their job rows and blocks re-enqueue until the delete releases (ref-counted).
- **Entry points:** `electron/downloader.ts:1267` ← `electron/ipc.ts:302`, `:334`.
- **Data:** `jobs`, files under `assets/`.
- **Local deps:** filesystem, SQLite.
- **External calls:** none.
- **Status:** shipped (tests `tests/electron/downloader.test.ts:880`).
- **Web port:** background-job (cancel jobs before object-storage deletes).

### DL-23 · Clear all downloaded assets
- **What:** Settings danger zone: cancel all, await writers, delete files in `assets/{thumbnails,images,videos,previews}` except manual/web protected files, recreate dirs, then null social `*_path`, `preview_path`, `post_media.local_path`.
- **Entry points:** `src/views/Settings.tsx:2291` → `db:clearAssets` `electron/ipc.ts:293` → `electron/downloader.ts:324`; `electron/db.ts:5612`, `:5626`.
- **Data:** `assets/`, `posts`, `post_media`.
- **Local deps:** filesystem, SQLite.
- **External calls:** none.
- **Status:** shipped (test `tests/electron/downloader.test.ts:920`).
- **Web port:** object-storage + api+db.

### DL-24 · Automatic cover preview cache
- **What:** after every `bulkUpsert`, posts with no local cover and an https thumbnail on the platform CDN allowlist get a 640 px JPEG (q82) fetched anonymously (3 concurrent, 15 s, ≤8 MB, redirects re-checked) into `assets/previews/<sha256(id+url)>.jpg`, set only if the URL is unchanged and nothing was downloaded meanwhile; X-only backfill 5 s after startup.
- **Entry points:** `electron/ipc.ts:384` → `electron/preview-cache.ts:142`, `:48`, `:99`; `electron/main.ts:642`; `electron/db.ts:1899`, `:1936`.
- **Data:** `posts.preview_path`, `assets/previews/`.
- **Local deps:** Electron `nativeImage`, filesystem, SQLite.
- **External calls:** `*.cdninstagram.com`, `*.fbcdn.net`, `pbs.twimg.com`, `*.pinimg.com`.
- **Status:** shipped (new, untracked file).
- **Web port:** object-storage + background-job — must happen within the CDN URL lifetime; best done by the extension at capture time.

### DL-25 · On-demand expired-cover repair
- **What:** a failing social card calls `preview:repair`: drops missing local paths, fetches the canonical post page anonymously (12 s, ≤2 MB, redirects must stay on the same post URL shape), checks `og:url`, takes `og:image`, swaps `thumbnail_url` (+slide 0) only if unchanged, queues a preview; 2 concurrent, ≤80 pending, 15 min per-post cooldown, 30 min global pause after HTTP 429.
- **Entry points:** `src/components/PostCard.tsx:656` → `electron/preload.ts:42` → `electron/ipc.ts:173` → `electron/preview-repair.ts:154`, `:119`, `:69`, `:56`; `electron/db.ts:1950`.
- **Data:** `posts.thumbnail_url`, `post_media.source_url`, `posts.preview_path`.
- **Local deps:** Electron session, SQLite.
- **External calls:** IG/X/Pinterest post pages (anonymous).
- **Status:** shipped (new, untracked file; test `tests/electron/preview-repair.test.ts:26`).
- **Web port:** rethink — server-side `og:image` scraping of IG from datacenter IPs is login-walled/blocked; do it in the extension.

### DL-26 · Anonymous media session
- **What:** fetch helper over an in-memory partition `shelfy-anonymous-media`, stripping `cookie`/`authorization`/`proxy-authorization` and forcing `credentials:'omit'` (used by DL-24/25; the downloader uses Node `fetch` with `credentials:'omit'`).
- **Entry points:** `electron/anonymous-media.ts:8`; test `tests/electron/anonymous-media.test.ts:14`.
- **Data:** none (ephemeral jar).
- **Local deps:** Electron session.
- **External calls:** platform CDNs and post pages.
- **Status:** shipped (new, untracked file).
- **Web port:** drop — server fetches are anonymous by construction; keep the "no credentials" invariant.

### DL-27 · yt-dlp resolution & availability probe
- **What:** binary = `YTDLP_BIN` → `<userData>/runtime-bin/bin/` → `resources/bin/` → `../bin/` → PATH; async `--version` probe, positive result cached, negative re-probed after 30 s; missing ⇒ video jobs fail "yt-dlp not installed".
- **Entry points:** `electron/downloader.ts:129`, `:152-178`, `:885`.
- **Data:** none.
- **Local deps:** yt-dlp, filesystem, env.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** container.

### DL-28 · Runtime provisioning of yt-dlp / ffmpeg
- **What:** on first window load (packaged only) and from Settings "Runtime": yt-dlp pinned `2026.08.19` + per-OS SHA-256 + version marker; ffmpeg = Windows gyan.dev rolling zip (hash not pinned), macOS arm64 / Linux x64 = CI mini-pack via `binaries.json` (sha512) on the GitHub feed (beta first); final-host allowlist, 3 retries, `.part` rename, serialized passes. llama/whisper share it (area 3).
- **Entry points:** `electron/main.ts:701`; `src/views/Settings.tsx:1156` → `binaries:ensure|status` `electron/ipc.ts:866-893` → `electron/binaries.ts:1111`, `:607`, `:825`, `:953`, `:408`, `:86-95`, `:159`.
- **Data:** `<userData>/runtime-bin/bin/{yt-dlp,ffmpeg}`, `runtime-bin/yt-dlp-version.txt`; reads `update-channel.json`, `app-update.yml`.
- **Local deps:** filesystem, archive extraction (tar/zip).
- **External calls:** github.com/yt-dlp releases, www.gyan.dev, GitHub release feed (`*.githubusercontent.com`).
- **Status:** shipped.
- **Web port:** container — bake pinned yt-dlp + ffmpeg into the worker image; drop client provisioning.

### DL-29 · Build-time / offline binary tooling
- **What:** `provision-binaries.ps1` (manual Windows recovery: pinned yt-dlp + SHA, gyan.dev ffmpeg, llama/whisper; does not re-check an existing yt-dlp's version), `scripts/make-binary-packs.ts` (CI mac/Linux ffmpeg+whisper tar.gz + manifest fragment), `build-windows.ps1` (yt-dlp into source `bin/`); `build/postinstall.ts` only installs Playwright Chromium (area 4).
- **Entry points:** `provision-binaries.ps1:164-190`; `scripts/make-binary-packs.ts:57-127`; `build-windows.ps1:163-183`.
- **Data:** `release/shelfy-bin-<key>.tar.gz`, `release/binaries-<key>.json`.
- **Local deps:** PowerShell, tar, ffmpeg-static.
- **External calls:** github.com, www.gyan.dev.
- **Status:** shipped (dev/CI tooling).
- **Web port:** drop (desktop-only) → container image build.

### DL-30 · Legacy cookie-file sweep
- **What:** on module load deletes orphan `shelfy-cookies-*.txt` files from `<userData>/tmp-cookies` and the OS temp dir (left by the removed authenticated fallback).
- **Entry points:** `electron/downloader.ts:380`, `:408`.
- **Data:** `<userData>/tmp-cookies/`, OS temp.
- **Local deps:** filesystem.
- **External calls:** none.
- **Status:** shipped (legacy cleanup).
- **Web port:** drop.

### DL-31 · Asset naming & storage layout
- **What:** `<userData>/assets/{thumbnails,images,videos,previews}`; names `<platform>-<ident>[-<pos>].<ext>`, `ident = shortcode || id` sanitized to `[A-Za-z0-9_-]` ≤128 chars, unknown platforms sanitized too; absolute paths stored in the DB; served via `asset://` (area 5).
- **Entry points:** `electron/downloader.ts:261-277`, `:313-320`.
- **Data:** filesystem; `posts.*_path`, `post_media.local_path`.
- **Local deps:** filesystem.
- **External calls:** none.
- **Status:** shipped.
- **Web port:** object-storage — keys like `u/<uid>/<platform>/<id>/<kind>-<pos>.<ext>`; store keys, not absolute paths.

## Data touched (area summary)
- **SQLite (`electron/db.ts`, owned by area 1):**
  - `posts`: written by sync ingest (`id, platform, shortcode, post_url, profile_url, author_username, author_name, text, thumbnail_url, media_type, timestamp, media_count`) and by downloads (`thumbnail_path, image_path, video_path, preview_path, thumb_blur`).
  - `post_media (post_id, position, media_type, source_url, local_path)`: slides from sync; `local_path` from downloads/capture-mvp.
  - `collections (platform, external_id, ig_name)` + `post_collections`: folder/board → tag import and source-sync planning.
  - `jobs` (kind `download`): durable queue mirror.
  - `downloads` table (`electron/db.ts:344`): legacy schema, never written (dead).
- **IPC used by this area:** `db:bulkUpsert`, `db:savedByKeys`, `db:getPostsByIds`, `db:getStats`, `collections:create|addPosts`, `preview:repair`, `download:{post,posts,all,status,isPaused,pauseAll,resumeAll,cancelAll,clearCompleted,cancelJob,retryJob}`, `db:clearAssets`, `binaries:{status,ensure}`, `getWebviewInjectedScript`, `getWebviewSelectScript`; `db:existingIds` is exposed but unused by the renderer (dead). Push: `download:progress`, `interceptor:newPosts`, `binaries:progress`; webview channels `intercepted`, `ss-select`.
- **Filesystem:** `<userData>/assets/{thumbnails,images,videos,previews}/`; `<userData>/runtime-bin/bin/` + `yt-dlp-version.txt`; `<userData>/Partitions/social/` (cookies = credential store); `<userData>/capture-mvp/` (spike only); `<userData>/tmp-cookies/` (legacy sweep).
- **Renderer localStorage:** `ig-saved-url`, `pin-board-url`, `download:assetTypes`.
- **In-memory only:** download `jobsMap`/`postCache`, preview-cache and repair queues, IG fresh-URL cache, source-sync jobs (React state), page globals (`__ssCapturedItems`, `__ssCapturedOrder`, `__syncStop`, `__lastInterceptAt`, `__ssPinLastCursor`).

## Background jobs, queues & concurrency
- **Download queue (main):** 4 global slots, per-platform caps unused; IG/Pinterest image jitter 120–400 ms; fetch timeout 60 s; ≤5 redirects; yt-dlp kill grace 5 s; progress coalescing 100 ms; IG fresh-URL cache 5 min / 100 posts; yt-dlp negative probe retry 30 s; persisted in `jobs`, recovered at boot (`electron/main.ts:683`); no auto-retry.
- **Preview cache (main):** 3 concurrent, 15 s, 8 MB cap, in-memory queue (not persisted); X backfill once 5 s after startup.
- **Preview repair (main):** 2 concurrent, ≤80 pending, 15 min per-post cooldown, 30 min global block on HTTP 429.
- **Sync (renderer + page context):** one sync per tab, 3 tabs concurrent; IG replay 700 ms × ≤100 pages; scroll settle 650/750 ms, 2 passes, 60 no-growth steps, 3×20 s stalls, 16 000 steps / 30 min caps; pre-sync buffer ≤3000; captured-item store 5000 keys.
- **Source sync (renderer):** one run per platform, sequential steps; navigation settle 45 s; scripts ready ≤20 s; injection ≤8 s, then one reload + ≤20 s; takeover wait ≤10 s; step cap 35 min; IG folder discovery 12 × 500 ms; finished jobs cleared after 4 s.
- **Binary provisioning (main):** serialized tail promise with piggyback; 3 attempts, linear backoff; triggered on first `did-finish-load` (packaged) and from Settings.
- **capture-mvp (flag only):** CDP buffers 256 MB total / 32 MB per resource; persist retry 1.2 s × 8, queue cap 800.

## External services & endpoints
- **Instagram:** pages `www.instagram.com/<user>/saved/all-posts/`, `/<user>/saved/<slug>/<id>/`; `GET /api/v1/feed/saved/posts/?max_id=`, `GET /api/v1/feed/collection/<id>/posts/?max_id=`, `GET /api/v1/accounts/current_user/` (header `X-IG-App-ID`); passive `/graphql/query`; post pages `/p|reel|tv/<sc>/` (yt-dlp, `og:image`); CDNs `*.cdninstagram.com`, `*.fbcdn.net`.
- **X:** `x.com/i/history`, `x.com/i/bookmarks`; passive `/i/api/graphql/<queryId>/Bookmarks…`; status pages (yt-dlp, `og:image`); `pbs.twimg.com`; video CDN via yt-dlp.
- **Pinterest:** `www.pinterest.com/<user>/<board>/[<section>/]`; passive `/resource/{BoardFeed,BoardSectionPins,UserPins,UserActivityPins,UserActivityFeed}Resource/get/`; `/pin/<id>/`; `*.pinimg.com` (images incl. `/originals/`, videos).
- **Auth popups:** `accounts.google.com`, `appleid.apple.com`, `facebook.com`.
- **Binary sources:** `github.com/yt-dlp/yt-dlp/releases/download/2026.08.19/{yt-dlp.exe,yt-dlp_macos,yt-dlp_linux}`; `www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.zip`; GitHub release feed (`app-update.yml` URL) `binaries.json` + `shelfy-bin-<platform>-<arch>.tar.gz`.

## Web-migration risks & notes

### Per-platform viability

| Platform | (a) Server-side scraping with stored user cookies from datacenter IPs | (b) Browser extension in the user's logged-in browser (MV3) | (c) Official API | Server-side yt-dlp from datacenter IPs |
|---|---|---|---|---|
| **Instagram** | Not viable. Saved feeds need `sessionid`/`csrftoken`/`ds_user_id` + `X-IG-App-ID`; a session replayed from a cloud ASN typically triggers checkpoints / forced logout / action blocks and puts the user's account at risk; violates Meta's terms on automated collection; the backend would custody session cookies. | Viable, recommended. MAIN-world `document_start` content script reuses SYNC-06/07/12 from the user's IP and session. Media: IG REST items already carry `video_versions[]` (GraphQL: `video_url`) which the parser currently drops → extension can fetch images AND MP4s (signed CDN URLs, no cookies) and upload them without yt-dlp. | None for saved posts: Instagram Graph API only covers professional accounts' own media; Basic Display API was retired (Dec 2024). | High risk: anonymous IG extraction from datacenter IPs is commonly rate-limited or login-walled; adding cookies risks account bans; ToS breach. Avoid. |
| **X / Twitter** | Fragile/risky. Bookmarks GraphQL needs `auth_token` + `ct0`, bearer token, CSRF header and an anti-bot transaction-id header; session reuse from cloud IPs invites lockouts; ToS forbids scraping. | Viable. Reuse SYNC-06/08/09; images from `pbs.twimg.com` need no cookies; GraphQL `extended_entities.media[].video_info.variants` (currently dropped) give direct MP4 URLs → no yt-dlp. | X API v2 `GET /2/users/:id/bookmarks` (OAuth 2.0 PKCE, `bookmark.read`), documented cap of the ~800 most recent bookmarks, media expansions incl. video variants; requires paid access — pricing/tiers and folder endpoints must be re-verified. | Medium–high: guest-token/syndication paths are rate-limited on datacenter IPs; protected/sensitive media need login. Prefer API variants or client-side upload. |
| **Pinterest** | Not needed and risky: the resource RPC is bot-guarded (the code itself avoids replaying it); cookie reuse from cloud IPs can flag the account. | Viable. Reuse SYNC-10/11; `i.pinimg.com` / video URLs fetchable anonymously; `mapPin` already stores direct MP4/HLS URLs. | Best candidate for a fully server-side connector: Pinterest API v5 `GET /v5/boards`, `/v5/boards/{id}/pins`, `/v5/boards/{id}/sections[/{sid}/pins]`, `/v5/pins/{id}` (scopes `boards:read`, `pins:read`, `*_secret` for secret boards); needs app review for standard access; gives stable board ids and board discovery (both missing today). | Low–medium: public pins/videos are generally reachable anonymously; still apply rate limits. |
| **TikTok** (not in code) | Not viable (signed requests, aggressive bot detection). | Would be the only route for favourites/collections. | Display API exposes the user's own videos only. | Medium–high (signature/anti-bot, IP blocks). |
| **YouTube** (not in code) | Not needed. | Possible. | YouTube Data API covers playlists/liked videos (Watch Later not exposed). | High: datacenter IPs routinely hit "confirm you're not a bot"/PO-token walls. |
| **Reddit** (not in code) | Not needed. | Possible. | Official API `GET /user/{name}/saved` (OAuth `history` scope) — server-side friendly. | Medium (v.redd.it is DASH → needs ffmpeg merge). |

### Extension (MV3) design notes
- Manifest `content_scripts` with `"world": "MAIN"`, `"run_at": "document_start"` patch fetch/XHR before page code runs (Chrome 111+, Firefox 128+; Safari to verify) — fixes today's late-injection gaps.
- Relay: MAIN → `window.postMessage` → ISOLATED content script (keep the `event.source === window` check from `webview-preload.ts:58`) → `chrome.runtime` port → service worker → backend API with a Shelfy token. Page scripts can forge postMessages, so the backend must validate everything (SYNC-17).
- `chrome.scripting.executeScript` in MV3 accepts only `func`/`files` (no code strings): `SCROLL_SCRIPTS`, `IG_FEED_REPLAY`, `IG_GET_USERNAME`, `findIgFolderHref`, `readIgFolderName` must become functions/files.
- Long syncs must be driven from the tab (content script), not the service worker (MV3 SW is suspended when idle); hidden tabs get timer throttling (intensive throttling after ~5 min) → use a visible/minimized dedicated window or accept slower runs (verify lazy-load in hidden tabs).
- Capture-on-view bytes: `chrome.debugger` + `Network.getResponseBody` (port of SYNC-27) works but shows a debugging infobar and needs the `debugger` permission; preferred: service-worker `fetch` of the same CDN URL with `host_permissions` (`*.cdninstagram.com`, `*.fbcdn.net`, `pbs.twimg.com`, `video.twimg.com`, `*.pinimg.com`), `credentials:'omit'` (CDN URLs are signed, no cookies needed; likely served from HTTP cache), then upload via presigned object-storage URLs. In-page canvas readback fails (CORS taint, validated by `scripts/capture-spike/test.ts`).
- Store review: broad host permissions + MAIN-world scripts on social sites need a clear single-purpose justification; users without the extension (mobile, other browsers) get a read-only library.

### Reuse inventory
- **As-is in the extension MAIN world:** `electron/webview-injected.ts` (whole IIFE: parsers, `emit`, `replayPinterestSSR`, `scanTwitterBookmarksDom`, fetch/XHR patches; its relay already falls back to `postMessage({type:'SOCIAL_SAVED_INTERCEPT'})`); `electron/webview-select.ts` (overlay; relay falls back to `SOCIAL_SAVED_SELECT`, host calls via `window.__ssSelect.*`).
- **Adapt:** `electron/webview-preload.ts` → ISOLATED content script; `src/lib/browserScripts.ts` scripts → functions/files; the orchestration logic of `useBrowserSync` / `useSourceSync` (termination rules, step planner, login detection) → extension controller.
- **As-is in a shared TS package (extension + server):** `src/lib/browserUrls.ts`, `src/lib/browserSanitize.ts` (server ingest validator), `buildSyncSteps` (`src/hooks/useSourceSync.ts:127`), downloader pure helpers (`twitterOrigUrl`, `pinterestOrigUrl`, `normalizeVideoUrl`, `safeIdent`, `imageMediaOf`, `VIDEO_POST_HOSTS`), `metaContent` (`electron/preview-repair.ts:56`).
- **Rewrite/drop:** webview/session plumbing (`useBrowserWebview`, `useBrowserIntercept`, `interceptor.ts`, main.ts webview hardening), `downloader.ts` queue (→ worker queue), `binaries.ts` (→ container image), `capture-mvp.ts`, duplicate parsers in `ig-parser.ts`/`tw-parser.ts`.

### Cross-cutting blockers & risks
1. **No server-side path for Instagram saved posts** (no API; cookie scraping from cloud IPs endangers user accounts) → the web product needs an extension for IG and X sync; multi-device is read-only without it.
2. **Expiring media URLs:** IG CDN URLs are signed and expire (the desktop already copes via preview cache + repair); bytes must be captured close to ingest — ideally uploaded by the extension.
3. **Videos depend on yt-dlp today** (private/age-restricted ones already fail since DL-13 was removed); server-side yt-dlp from datacenter IPs is high-risk for IG, medium for X → extract the direct video URLs the parsers currently drop and upload client-side; keep a containerized yt-dlp only as a fallback for public X/Pinterest.
4. **Identity & dedup:** IG `id` differs by capture path (REST `id` is typically `<pk>_<ownerId>`, GraphQL `node.id` is `<pk>`, DOM fallback uses the shortcode) and there is no unique `(platform, shortcode)` constraint → duplicates are possible; Pinterest board ids are `user/slug` (rename breaks the link). Define canonical keys before migrating data.
5. **No incremental sync:** every run re-walks the whole listing (IG replay ≤100 pages, 30-min ceilings) → add per-source watermarks / "stop after N known items" to cut load and ban surface.
6. **Legal/ToS:** automated collection violates each platform's terms; hosting copies of third-party media in the cloud changes the posture versus a local archive (takedown process, private per-user storage, retention policy).
7. **Security carry-overs:** never accept platform cookies server-side; validate all extension payloads; any server fetch needs SSRF protection with DNS/IP checks (today's guard is hostname-literal only); Windows ffmpeg is downloaded unpinned (rolling gyan.dev zip); `persist:social` is shared with web capture of arbitrary sites (SYNC-29).
8. **Ops:** yt-dlp is pinned (`2026.08.19`); extractor breakage needs fast image rebuilds; per-platform rate limiting must be global across users/egress IPs, not per process.
9. **State that is local-only today:** sync jobs/run history (React state), asset-type prefs and saved URLs (localStorage), absolute file paths in the DB → move to user settings, job tables and object-storage keys.

### Recommended sync architecture (summary)
1. An MV3 browser extension is the sync engine: MAIN-world `document_start` hook reusing `webview-injected.ts` + an in-tab scroll/replay controller, running in the user's own logged-in browser; it uploads sanitized post JSON to the backend with a Shelfy token (platform cookies never leave the browser).
2. Media is captured client-side: the extension fetches CDN images and the direct video URLs (IG `video_versions`, X `variants`, Pinterest `video_list`) right after capture and uploads them to object storage via presigned URLs; server-side yt-dlp is only a fallback for public X/Pinterest; Pinterest (API v5) and optionally X (API v2 bookmarks) can be server-side official connectors.
3. Backend: idempotent ingest keyed by canonical (user, platform, id), object storage, a job queue for derivatives (previews, blur, AI) and realtime push of sync/download progress to the SPA.
