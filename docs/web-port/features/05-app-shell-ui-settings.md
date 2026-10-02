# App shell, gallery/browsing UX, settings & platform services — feature index

> Scope: `electron/main.ts` (minus webview/session setup and `asset://` internals), `electron/updater.ts` (+ `readFeedUrl` in `electron/archive-utils.ts`), `electron/feedback.ts`, `workers/feedback/*`, `electron/hardware.ts` (non-AI uses: **none found** — every importer is analyzer/stt/embeddings/binaries), the window/app/updater/shell/feedback handlers in `electron/ipc.ts` and their bridge in `electron/preload.ts`, `electron/logger.ts` (crash/log handling), `src/main.tsx`, `src/App.tsx`, `src/views/Gallery.tsx`, `src/views/Settings.tsx`, `src/components/*` (except AddSiteModal, AddBookmarkModal, ImportModal, ImportFolderModal, postmodal/AiPanel, postmodal/WebMetaPanel), `src/hooks/{useActivity,useGridSize,useViewMode,useRangeSelect,useToast}.ts`, `src/lib/{exportMarkdown,duration,imagePreview.worker}.ts`, `src/disclaimer.ts`, `src/i18n/*`, `src/index.css`, `tailwind.config.ts`, `index.html`, `vite.config.ts` (build-time plugin). Snapshot: working tree of `dev`, 2026-10-02.

## Overview

- Shell: one frameless Electron 31 `BrowserWindow` hosting a React 18 SPA — a fixed 240 px Sidebar plus keep-alive view layers; the social Browser view (area 2) stays mounted under an opaque overlay so its webviews keep syncing.
- Navigation is in-memory React state (no router, no URLs): `gallery`, `downloads`, `aiqueue`, `aiweb`, `aisearch`, `aitags`, `settings`, `browser` (instagram / twitter / pinterest).
- Gallery: a filtered, paged post list rendered as a row-virtualized square grid or an infinite pan/zoom canvas, with multi-select (click, Shift-range, drag-sweep, select-all-matching), a bulk Actions menu and a post modal (carousel, lightbox, folders, per-post actions).
- Activity Center aggregates background work from main→renderer push channels (downloads, analysis, web capture, model/binary provisioning, OTA updates) plus renderer-only Browser state (social sync, selection save).
- Platform services: custom updater (GitHub Releases; stable/beta; macOS DMG, Windows self-rebuild, Linux manual), feedback via a Cloudflare Worker relay to Resend, versioned legal-disclaimer gate, in-house i18n (it/en), local file log; **no telemetry or analytics**.
- Absent (grep over `electron/`): single-instance lock, deep links / protocol client, tray, power/sleep hooks, dock/taskbar badge or progress, crash reporter, auto-launch.
- All settings are local: renderer `localStorage` (UI prefs) or JSON files in `userData` (main-process prefs); there is no settings table in SQLite.
- Renderer coupling: 14 in-scope files call `window.electronAPI` directly (82 distinct members = 81 methods + `platform`); app-wide, 39 files use 162 of the 173 bridged members.

## Navigation map

**Shell layout** (`src/App.tsx:649-890`):

- `<Sidebar>` sits on the left.
- `<main>` stacks:
  - `RemoteAiBanner`
  - the always-mounted `Browser` (z 0)
  - an opaque keep-alive overlay (z 2) holding every visited view
  - the AI-onboarding gate (z 5, AI tabs only)
- Global modals render after `<main>`.
- `WindowControls` floats top-right on Windows/Linux.

| View id | Reached from | What it shows | Owner |
| --- | --- | --- | --- |
| `gallery` (default) | Library rows (All posts / platform / folder); FilterDrawer source rows; Activity "save" item; after Add file succeeds | Post grid or canvas for the active source + filters, floating toolbar, filter drawer, post modal, bulk actions | 5 |
| `browser` + `instagram`/`twitter`/`pinterest` | Connections rows; Activity sync item; "open browser" CTA on a sync login error | Logged-in social webviews used for capture/sync (always mounted) | 2 |
| `downloads` | Library → Downloads; Activity download item | Download queue | 2 |
| `aiqueue` ("Auto-tag") | AI group; Activity analysis item | AI analysis queue | 3 |
| `aiweb` ("Website Analyzer") | AI group; AddSite success; modal "open in Websites"/"re-analyze"; Activity web item | Website capture list/jobs | 4 (+3) |
| `aisearch` ("Chat") | AI group | AI chat / search | 3 |
| `aitags` ("Tags Explorer") | AI group; tag chip in the global post modal | Tag analytics and browsing | 3 |
| `settings` | Sidebar footer; Activity model/binaries/stt/update items; onboarding "open settings" | Settings page (sections below) | 5 (+2, 3) |

**Sidebar entries** (`src/components/Sidebar.tsx:375-843`), in order:

1. Header: logo, "SHELFY", total posts. It is also the window drag handle.
2. **Connections**: Instagram, X / Twitter, Pinterest (→ `browser` sub-tab, with new-post badge and sync spinner); "+ Add website" (AddSiteModal, area 4); "+ Add file" (AddBookmarkModal, area 4).
3. **Library**:
   - **All posts** with a disclosure chevron. It expands into:
     - Instagram, X / Twitter, Pinterest and Websites rows, each with a count. Folders tied to a platform nest under its row.
     - Custom folders.
     - "New folder".
   - **Downloads**, with a live done/total badge.
4. **AI**: Auto-tag, Website Analyzer, Chat, Tags Explorer. Auto-tag and Website Analyzer carry live badges.
5. Footer: **Feedback** (FeedbackModal), **Activity** strip and popover, **Settings**.

**Overlays / modals / transient surfaces:**

- **Startup and gates**
  - DisclaimerGate: a blocking gate at launch. It also has a review mode opened from Settings → Legal.
  - AI onboarding gate (area 3).
  - RemoteAiBanner, a bottom floating banner (area 3).
  - Dev-only "build refreshed" bar.
- **Gallery and post modal**
  - CollectionModal: create or edit a folder. Opened from the Sidebar, from gallery bulk "Create new source", and from the post modal.
  - PostModal, opened from Gallery, AI Search, Tags Explorer, Browser, or App (global, for AI queue/Websites rows). It contains:
    - CollectionsMenu popover
    - ActionsMenu (⋮)
    - ImageLightbox
    - a nested CollectionModal
  - FilterDrawer, a right slide-in panel inside the gallery.
  - Bulk "Actions" popover.
  - Bulk feedback toast.
- **Sidebar and Settings**
  - FeedbackModal, with internal confirm/sent overlays.
  - Activity popover.
  - ExportModal (Settings).
  - ImportModal (area 4, opened from Settings).

## Features

### APP-01 · Startup sequence and fatal-error dialogs
- **What:** `whenReady` runs: file logger → SQLite init → `asset://` → CSP → hardening → mic permission → window → deferred warm-ups → queue recovery → updater → binaries. If the DB fails to open, or any startup exception occurs, the app shows "Shelfy cannot start" and quits.
- **Entry points:** `electron/main.ts:578-787` (DB fatal `:586-602`, generic fatal `:773-787`)
- **Data:** shows the `shelfy.sqlite` path in the dialog
- **Local deps:** Electron `app`/`dialog`, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink. Becomes server boot/health checks plus a "service unavailable" page. The renderer has no React error boundary today, so one needs to be added.

### APP-02 · Main window creation and load
- **What:** 1280×800 window (min 900×600), titled SHELFY. Dev loads `http://localhost:5173` with 10×500 ms retry and auto-opens DevTools. Prod uses `loadFile(dist/index.html)`. Vite `base:'./'` makes the build file:// compatible.
- **Entry points:** `electron/main.ts:507-576`; `vite.config.ts:37`; `index.html:1-12`
- **Data:** none
- **Local deps:** Electron `BrowserWindow`, preload `electron/preload.ts`
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). The SPA is served by the web host. Layout assumes ≥900 px width (see risks).

### APP-03 · Frameless chrome and drag regions
- **What:** macOS uses `hiddenInset` traffic lights at (18,11) inside a 36 px draggable sidebar strip; Windows/Linux use `frame:false`. CSS `.drag-region` marks drag areas (interactive elements auto-excluded via `no-drag`), and the gallery toolbar reserves 144 px on the right for the window controls.
- **Entry points:** `electron/main.ts:512-515`; `src/components/Sidebar.tsx:27,383,388`; `src/index.css:885-918`; `src/views/Gallery.tsx:162-166`
- **Data:** `electronAPI.platform` (`electron/preload.ts:24`)
- **Local deps:** Electron window chrome, `-webkit-app-region`
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). Already guarded by `!!window.electronAPI`, so it degrades to 0 inset.

### APP-04 · Custom window controls (Windows/Linux)
- **What:** Minimize, maximize/restore toggle and close buttons. The icon tracks the native maximize state pushed from main. Labels are hard-coded English.
- **Entry points:** `src/components/WindowControls.tsx:10-69`, `src/App.tsx:662-666` → `window:minimize|maximizeToggle|close|isMaximized`, push `window:maximizeChanged` → `electron/ipc.ts:827-842,132-133`
- **Data:** none
- **Local deps:** Electron `BrowserWindow` API
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only)

### APP-05 · Application menu
- **What:** The application menu has a single "Edit" menu with role items (undo, redo, cut, copy, paste, select all) and a dev-only "Toggle DevTools" (⌥⌘I / Ctrl+Shift+I). There are no app/File/View/Window menus. On macOS this likely means no Quit, Cmd+W or Cmd+M menu bindings (verify).
- **Entry points:** `electron/main.ts:264-293,561`
- **Data:** none
- **Local deps:** Electron `Menu`
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). The browser provides native editing shortcuts.

### APP-06 · Window lifecycle (re-activate / all-closed)
- **What:** On macOS dock re-activate with no windows, the window is recreated and the updater re-pointed; progress emitters always target the current window (`currentWindow` / `_window`). The app quits when all windows close, except on macOS.
- **Entry points:** `electron/main.ts:86-93,760-771,819-823`; `electron/updater.ts:1068-1070`; `electron/ipc.ts:124-149`
- **Data:** none
- **Local deps:** Electron `app` events
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). Tabs and reloads replace this; SSE must reconnect on reload.

### APP-07 · Graceful shutdown on quit
- **What:** `before-quit` SIGKILLs the llama/whisper/embedding servers, cancels web captures, closes the shared Playwright browser, and checkpoints/closes the SQLite WAL.
- **Entry points:** `electron/main.ts:789-817`
- **Data:** `shelfy.sqlite` (WAL checkpoint)
- **Local deps:** child processes, Playwright, better-sqlite3
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). The server worker lifecycle replaces it.

### APP-08 · Background queue recovery at boot
- **What:** Calls `recover()` on downloader, analyzer and weborchestrator, after IPC emitters are wired, so persisted jobs resume and stream progress. Each recovery is isolated so one failure doesn't block the others.
- **Entry points:** `electron/main.ts:683-689` → `downloader/analyzer/weborchestrator.recover()` (areas 2/3/4)
- **Data:** `jobs` table (area 1)
- **Local deps:** in-process queues
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Server job queues persist and resume natively (areas 2/3/4).

### APP-09 · Deferred startup warm-ups
- **What:** +5 s: backfill X preview images for remote-only cards; +15 s: pre-warm 640 px grid-tile thumbnails, then backfill 24 px blur placeholders. Each triggers one coalesced grid refresh via `interceptor:newPosts`; `PERF_NO_PREWARM` skips the pre-warm.
- **Entry points:** `electron/main.ts:642-654,664-677` → `preview-cache.enqueuePreviews`, `thumbs.prewarmThumbCache/backfillThumbBlurs` (`electron/thumbs.ts:240-337`)
- **Data:** `posts.thumb_blur`, `userData/thumb-cache/`, `userData/assets/`
- **Local deps:** Electron `nativeImage`, filesystem
- **External calls:** X media CDN (preview backfill)
- **Status:** shipped (pre-warm can be turned off by env var)
- **Web port:** object-storage. Generate thumbnails and blur hashes at ingest (worker or CDN transform). No client warm-up.

### APP-10 · Runtime binaries auto-provisioning trigger (ref. area 3)
- **What:** After the first `did-finish-load`, main ensures the yt-dlp/ffmpeg/llama/whisper sidecars and streams `binaries:progress`; when a GPU llama build fails it emits `ai:variantFallback` and force-re-provisions the CPU build.
- **Entry points:** `electron/main.ts:701-758` → `binaries.ensureBinaries`
- **Data:** `userData/runtime-bin/`, `llama-variant.json`
- **Local deps:** filesystem, child processes
- **External calls:** upstream binary hosts / GitHub Releases (area 3)
- **Status:** shipped
- **Web port:** drop (desktop-only). Local models are replaced by API providers.

### APP-11 · `asset://` privileged protocol (registration and confinement)
- **What:** Registers `asset:` as a standard/secure/fetch/stream/bypassCSP scheme; every request is realpath-confined to `userData` (else 403), `?w=` serves cached thumbnails and Range requests stream video. Thumbnail/ETag internals belong to area 1.
- **Entry points:** `electron/main.ts:99-130,164-257`; renderer URL builder `src/lib/asset.ts:4-20`
- **Data:** absolute file paths stored in post rows (thumbnail/image/video/media paths)
- **Local deps:** Electron `protocol`, `net`, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage. Replace with signed CDN URLs built from storage keys (data-model change, area 1).

### APP-12 · Content-Security-Policy for app documents
- **What:** Injects a CSP header on `defaultSession` (prod `script-src 'self'`, dev adds `unsafe-inline`/`unsafe-eval` + localhost ws; `img-src`/`media-src 'self' asset: data: blob: https:`; `connect-src 'self' asset: data:`; `object-src 'none'`; `frame-src`/`base-uri 'self'`). Caveat: Electron docs say header CSP can't apply to `file://` loads and `index.html` has no meta CSP, so the packaged build is likely uncovered (verify).
- **Entry points:** `electron/main.ts:299-331`
- **Data:** none
- **Local deps:** Electron `session.webRequest`
- **External calls:** none
- **Status:** shipped (prod effectiveness unverified)
- **Web port:** rethink. Serve CSP as real HTTP headers from the web host (CDN img/media origins, API `connect-src`, `frame-ancestors 'none'`).

### APP-13 · Web-contents hardening (window-open, navigation lock, webview attach)
- **What:** Window-open: OAuth popups from the social webview are allowed (sharing `persist:social`), other http(s) URLs are SSRF-checked and opened in the OS browser, everything else is denied. Navigation: capture windows unrestricted, popups/webviews confined to the IG/X/Pinterest/auth host regex, the app window to dev localhost or prod `file://`/`asset://`; webview attach forces safe prefs, strips `disablewebsecurity` and pins the preload (it also hooks the area-2 capture spike).
- **Entry points:** `electron/main.ts:335-505` (open handler `:379-412`, navigate `:421-459`, attach `:464-503`)
- **Data:** none
- **Local deps:** Electron `web-contents-created`
- **External calls:** none
- **Status:** shipped (capture spike behind `SHELFY_CAPTURE_MVP`, area 2)
- **Web port:** drop (desktop-only). The browser sandbox replaces it; use `rel="noopener noreferrer"` on external links.

### APP-14 · Microphone permission gate
- **What:** On `defaultSession`, request/check handlers grant only `media`/`audioCapture`, and only to app origins (null/empty, dev localhost, `file://`, `asset://`). Every other permission is denied. It is used by AI-search dictation (area 3).
- **Entry points:** `electron/main.ts:619-636`
- **Data:** none
- **Local deps:** Electron permission handlers, OS microphone
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Uses the browser `getUserMedia` permission prompt; needs a secure context.

### APP-15 · Open URL in the system browser
- **What:** `openExternal` accepts only http(s) URLs and rejects loopback, private, numeric and metadata hosts (net-safety) before calling `shell.openExternal`. Used by the modal "Open original" and the lightbox "Open page".
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:293-305`, `src/components/ImageLightbox.tsx:106-116` → `shell:openExternal` → `electron/ipc.ts:1317-1327`
- **Data:** `posts.post_url`
- **Local deps:** Electron `shell`
- **External calls:** the target URL (OS browser)
- **Status:** shipped
- **Web port:** client-only. Use `window.open(url, '_blank', 'noopener')` with a scheme allowlist.

### APP-16 · Reveal / open local files
- **What:** "Open file" reveals the primary local file in Finder/Explorer. The meta-column links open the thumbnail/image/video in the OS default app. Both are confined to `userData` via `confineToUserData`.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:278-290`, `src/components/postmodal/MetaColumn.tsx:136-147` → `shell:showItemInFolder`, `shell:openPath` → `electron/ipc.ts:117-122,1300-1316`
- **Data:** local asset paths in post rows
- **Local deps:** Electron `shell`, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). Replace with "Download original" (signed URL).

### APP-17 · File logging and crash diagnostics
- **What:** Writes `<userData>/logs/main.log` (rotated at 5 MB to `.1`), mirroring main and renderer `console.*` (webview consoles skipped for privacy), uncaught exceptions/rejections, `did-fail-load` (query stripped), `render-process-gone` and `preload-error`. No auto-reload, no crash reporter, nothing leaves the machine.
- **Entry points:** `electron/logger.ts:113-150,163-172`; `electron/main.ts:534,581`
- **Data:** `userData/logs/main.log`
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink. Server logs plus optional client error reporting (consent needed). Add a React error boundary.

### APP-18 · Test/perf environment hooks
- **What:** `SHELFY_TEST_USER_DATA` redirects `userData` (unpackaged only), `SHELFY_THUMB_NO_CACHE` forces no-store asset responses, `PERF_NO_PREWARM` skips warm-ups, `PLAYWRIGHT_E2E` suppresses DevTools and `ELECTRON_DEV` switches to dev mode.
- **Entry points:** `electron/main.ts:19-27,154,557,664`
- **Data:** none
- **Local deps:** env vars
- **External calls:** none
- **Status:** behind flag/env var
- **Web port:** drop (desktop-only). Re-create equivalents in the web test harness.

### APP-19 · App version reporting
- **What:** The app version (`app.getVersion()`) is shown in the Settings header pill and the update card, and is appended to feedback payloads.
- **Entry points:** `src/views/Settings.tsx:1798-1817,965-981` → `app:getVersion` → `electron/ipc.ts:846`; `electron/ipc.ts:1330-1336`
- **Data:** `package.json` version
- **Local deps:** Electron `app`
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Inject the build version at build time; expose the API version from `/health`.

### APP-20 · Update channel (stable/beta)
- **What:** The channel is read once from `update-channel.json` (default stable) and cached; switching resets any non-in-flight pending state and triggers an immediate check. Beta reads the rolling `…/releases/download/beta` tag, stable reads `…/releases/latest/download`.
- **Entry points:** `src/views/Settings.tsx:955-1001` → `app:getUpdateChannel`/`app:setUpdateChannel` → `electron/ipc.ts:847-855` → `electron/updater.ts:137-213`
- **Data:** `userData/update-channel.json` `{channel}`
- **Local deps:** filesystem
- **External calls:** github.com Releases
- **Status:** shipped
- **Web port:** drop (desktop-only). A web deploy has no client update channel; use feature flags or a beta environment instead.

### APP-21 · Update feed polling and version compare
- **What:** Checks at boot, hourly, on channel switch and on "Check for updates" (skipped in dev/unpackaged builds); the HTTPS-only feed comes from `resources/app-update.yml` (package.json `build.publish`, GitHub Releases). Versions are regex-validated and compared as semver with pre-release ordering (docs/architecture.md wrongly says a 60 s poll).
- **Entry points:** `electron/updater.ts:62-65,221-255,937-952,1072-1096`; `electron/archive-utils.ts:17-38`; `src/views/Settings.tsx:1003-1012` → `updater:check`, push `updater:state`
- **Data:** in-memory `_state`
- **Local deps:** filesystem, Node fetch
- **External calls:** github.com Releases (`*.yml`, `source*.json`)
- **Status:** shipped
- **Web port:** drop (desktop-only). Server deploys replace it; optionally a "new version — reload" prompt via service worker.

### APP-22 · macOS update: verified DMG download and install prompt
- **What:** Reads `{latest|beta}-mac.yml`, picks the `.dmg` and requires its sha512; downloads in-app to `userData/updates/` (30 s stall watchdog, per-percent progress), verifies the hash, opens the DMG and offers "Close SHELFY and install" (falls back to a browser download on failure). Dialog strings are hard-coded Italian.
- **Entry points:** `electron/updater.ts:410-480,978-1063` ← `updater:openDownload` (`electron/ipc.ts:862`)
- **Data:** `userData/updates/*.dmg`
- **Local deps:** filesystem, Electron `dialog`/`shell`
- **External calls:** github.com Releases
- **Status:** shipped
- **Web port:** drop (desktop-only)

### APP-23 · Windows update: self-rebuild from source
- **What:** Reads `source[-beta].json` `{version, zip, sha512}`; "Update now" finds Node ≥20 (fnm/nvm/volta/scoop/system), downloads and sha512-checks the source zip, extracts it with `tar` and runs `build-windows.ps1` (`-ExecutionPolicy Bypass`) to reach "built". "Restart and install" spawns the NSIS installer (`/S --updated --force-run`) and quits; old rebuild directories are cleaned up.
- **Entry points:** `electron/updater.ts:537-934,956-973` ← `updater:rebuild`/`updater:quitAndInstall` (`electron/ipc.ts:861-863`)
- **Data:** `userData/rebuild/` (src, logs, `node-probe.log`)
- **Local deps:** PowerShell, Node.js, `tar`, child processes
- **External calls:** github.com Releases
- **Status:** shipped
- **Web port:** drop (desktop-only)

### APP-24 · Linux update: manual prompt
- **What:** Reads `{latest|beta}-linux.yml`. If a newer version exists, it shows a "manual" state whose action opens the Releases page in the browser. There is no AppImage self-replace.
- **Entry points:** `electron/updater.ts:488-535,982-990`
- **Data:** none
- **Local deps:** Electron `shell`
- **External calls:** github.com Releases
- **Status:** shipped
- **Web port:** drop (desktop-only)

### APP-25 · Update-ready native notification
- **What:** When a Windows installer is built, the app shows a native OS notification ("Aggiornamento pronto", hard-coded Italian); clicking it focuses the window. It also calls `flashFrame(true)` (taskbar flash / dock bounce). These are the only native notifications in the app.
- **Entry points:** `electron/updater.ts:101-134,740,925`
- **Data:** none
- **Local deps:** Electron `Notification`, OS notification center
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). Reuse the pattern for job-done Web Notifications/Push (see risks).

### APP-26 · Feedback send (main process)
- **What:** `feedback:send` adds the app version, sanitizes the payload (message ≤5000 chars; ≤5 attachments, each ≤7 MB and ≤16 MB base64 in total, base64 charset only) and POSTs JSON to the relay with a 20 s timeout, returning `{ok,id|error}`. With the relay disabled and `SHELFY_RESEND_API_KEY` set it calls Resend directly (dev only); error strings are hard-coded Italian.
- **Entry points:** `src/components/FeedbackModal.tsx:326-347` → `feedback:send` → `electron/ipc.ts:1330-1336` → `electron/feedback.ts:113-219`
- **Data:** none persisted
- **Local deps:** Node `fetch` (no CORS in main)
- **External calls:** `shelfy-feedback.*.workers.dev` (default relay, overridable via `SHELFY_FEEDBACK_RELAY_URL`); `api.resend.com` (dev direct)
- **Status:** shipped (direct-Resend mode behind env var)
- **Web port:** api+db. Send from the browser to the backend or relay; this needs CORS, auth and bot protection (e.g. Turnstile).

### APP-27 · Feedback relay Worker (Cloudflare)
- **What:** Rejects non-POST (405), bodies >24 MB (413), more than 3 requests per IP per 60 s (429), invalid payloads (400, same caps as the app) and requests over a soft global KV budget of `DAILY_CAP`=90 per UTC day (429); then forwards to Resend (502 on failure) and bumps the budget (48 h TTL). No auth, no CORS, attachment MIME not checked.
- **Entry points:** `workers/feedback/src/index.ts:106-212`; `workers/feedback/wrangler.jsonc:19-39`
- **Data:** KV `BUDGET`; vars `FROM`, `TO`, `DAILY_CAP`; secret `RESEND_API_KEY`
- **Local deps:** none (Cloudflare Workers)
- **External calls:** `api.resend.com`
- **Status:** shipped
- **Web port:** api+db. Reusable as-is if called server-side; if called from the browser, add CORS allowlist, user auth/Turnstile and a per-user rate limit.

### UI-01 · Root render and providers
- **What:** Mounts `<App/>` inside `I18nProvider`. App wraps everything in `AnalysisProvider` (area 3) and `ActivityProvider`. Memoized view wrappers avoid re-rendering on every progress flush.
- **Entry points:** `src/main.tsx:7-14`; `src/App.tsx:138-161,650-657`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable unchanged.

### UI-02 · Keep-alive view switching with cross-fade
- **What:** Each non-browser view mounts on first visit and then stays mounted; switching only toggles `visibility`/opacity (`--dur-3`), preserving scroll, state and virtualization layout. Gallery and Tags Explorer get `active=false` while hidden, which pauses paging and live reloads.
- **Entry points:** `src/App.tsx:115-123,174,212-215,726-803`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable. Consider URL routing (deep links, back button) on top.

### UI-03 · Always-mounted social Browser layer (ref. area 2)
- **What:** `Browser` renders permanently under the overlay so its webviews keep syncing. It reports `syncing`/`saving`/`saved`/source-sync jobs up to App, which feed sidebar badges and the Activity Center.
- **Entry points:** `src/App.tsx:242-262,701-717`
- **Data:** renderer state
- **Local deps:** Electron `<webview>` (area 2)
- **External calls:** instagram.com, x.com, pinterest.* (area 2)
- **Status:** shipped
- **Web port:** rethink (area 2). Webview capture is impossible in a browser tab.

### UI-04 · Global post modal from AI queues and tag hand-off
- **What:** AI queue and Websites rows open a post by id; App loads the full post and shows PostModal. Clicking an AI tag chip there jumps to Tags Explorer with that tag; a nonce re-applies the same tag.
- **Entry points:** `src/App.tsx:316-330,862-886` → `db:getPostsByIds` → `electron/ipc.ts:163`
- **Data:** `posts` (area 1)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. `GET /posts?ids=`.

### UI-05 · Web-post shortcuts: open in Websites / re-analyze (ref. area 4)
- **What:** From a web post's modal, "open in Websites" switches to `aiweb`; "re-analyze" re-queues capture plus analysis with overwrite, then opens `aiweb`.
- **Entry points:** `src/App.tsx:386-401` → `web:add` → `electron/ipc.ts:675`
- **Data:** `posts` web fields (area 4)
- **Local deps:** none
- **External calls:** target site (area 4)
- **Status:** shipped
- **Web port:** api+db (area 4)

### UI-06 · Dev build-refresh bar
- **What:** In dev, a yellow bar shows "DEV — last update: {time}" for 1 s after each HMR rebuild (`virtual:build-time`).
- **Entry points:** `src/App.tsx:539-548,618-622,667-671`; `vite.config.ts:9-27`
- **Data:** none
- **Local deps:** Vite dev server
- **External calls:** none
- **Status:** behind flag (`import.meta.env.DEV`)
- **Web port:** client-only. Optional dev aid.

### UI-07 · Sidebar header (brand and total count)
- **What:** Shows the logo, "SHELFY" and the total post count (locale-formatted). The header doubles as the window drag handle.
- **Entry points:** `src/components/Sidebar.tsx:388-398`; stats from `src/App.tsx:381`
- **Data:** `db:getStats` (`electron/ipc.ts:172`)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. `GET /stats`.

### UI-08 · Collapsible sidebar groups (persisted)
- **What:** The Connections, Library, AI and "All posts" groups expand and collapse independently. All are open by default. State is saved to localStorage and restored at launch.
- **Entry points:** `src/components/Sidebar.tsx:168-190,245-265,411-501,504-728,731-804`
- **Data:** localStorage `shelfy.sidebar.expandedGroups`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Per-device preference.

### UI-09 · Platform folder subtree (nested collections)
- **What:** Folders with a `platform` value (e.g. Instagram saved folders, Pinterest boards) nest under their platform row with tree guides. Each platform's chevron collapse state is persisted. New rows get a staggered entrance animation.
- **Entry points:** `src/components/Sidebar.tsx:258-276,291-362,584-658`
- **Data:** `collections.platform`; localStorage `shelfy.sidebar.expandedPlatforms`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (data via api+db)

### UI-10 · Library source navigation
- **What:** Clicking All posts, a platform (instagram, twitter, pinterest, web) or a folder sets `activeSource` and bumps a nonce so re-clicking re-applies it. It switches to the gallery. Active rows show a colored accent bar. Folders whose platform is not one of the listed rows appear as "custom".
- **Entry points:** `src/components/Sidebar.tsx:278-289,530-672`, `src/App.tsx:403-407,610-616` → Gallery `src/views/Gallery.tsx:238-256`
- **Data:** `collections`, `db:getStats.byPlatform`, collection counts
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Make it a URL route for shareable views.

### UI-11 · Connections tabs with new-post badge and sync spinner
- **What:** The Instagram/X/Pinterest rows open the Browser sub-tab and clear its badge, which counts posts captured this session (display capped at "99999+") and is suppressed while that tab is focused unless a sync is running; a spinner shows during sync.
- **Entry points:** `src/components/Sidebar.tsx:428-466`, `src/App.tsx:233-237,409-416,575-599`
- **Data:** push `interceptor:newPosts` `{count,platform}` (`electron/ipc.ts:389-391`)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push (area 2 owns sync)

### UI-12 · "Add website" / "Add file" actions (ref. area 4)
- **What:** Action rows in Connections open AddSiteModal (→ `aiweb` on success) and AddBookmarkModal (→ the All-posts gallery on success); both refresh stats.
- **Entry points:** `src/components/Sidebar.tsx:473-496`; `src/App.tsx:331-332,839-861`
- **Data:** area 4
- **Local deps:** area 4
- **External calls:** area 4
- **Status:** shipped
- **Web port:** api+db (area 4)

### UI-13 · Live navigation badges
- **What:** Downloads shows a spinner plus done/total while active, Auto-tag shows done/total for social analysis jobs only, Website Analyzer counts capture jobs plus web-post analysis jobs; projections are memoized so badges re-render only when counts change.
- **Entry points:** `src/App.tsx:287-302,341-367`; `src/components/Sidebar.tsx:692-723,748-797`
- **Data:** push `download:progress`, `analyze:progress`, `web:progress` (via hooks owned by areas 2/3/4)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push

### UI-14 · Stats refresh coalescing
- **What:** Bursts of `interceptor:newPosts` refresh `getStats` at most once per 800 ms, immediately on the leading edge plus one trailing refresh. Payloads without a numeric count (thumb-blur, preview-cache, web placeholders) refresh only.
- **Entry points:** `src/App.tsx:551-604`
- **Data:** `db:getStats`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push (debounced stats endpoint or pushed counters)

### UI-15 · Create folder (CollectionModal)
- **What:** Name (≤60 chars, Enter saves) with a live preview, color from a 10-swatch palette or the native custom picker, a case-insensitive duplicate-name guard for manual folders, and inline errors. Opened from the sidebar "New folder", the gallery bulk "Create new source" and the post modal.
- **Entry points:** `src/components/CollectionModal.tsx:6-17,106-135,182-271`; `src/App.tsx:486-522` → `collections:create` → `electron/ipc.ts:400`
- **Data:** `collections` (area 1)
- **Local deps:** native color input
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-16 · Edit folder name/color
- **What:** A pencil appears on folder-row hover and opens CollectionModal in edit mode. Save calls rename/update and refreshes stats.
- **Entry points:** `src/components/Sidebar.tsx:344-354`; `src/App.tsx:497-500,515-522` → `collections:update` → `electron/ipc.ts:419`
- **Data:** `collections`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-17 · Delete folder (label only vs label and posts)
- **What:** A confirmation panel offers an ARIA radio choice: remove only the label, or the label plus its N posts (DB rows and files); a partial file-deletion failure keeps the modal open with a warning, and an active source falls back to All posts.
- **Entry points:** `src/components/CollectionModal.tsx:137-157,277-403`; `src/App.tsx:524-537` → `collections:delete` → `electron/ipc.ts:428`
- **Data:** `collections`, `post_collections`, `posts`, asset files
- **Local deps:** filesystem (file delete)
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage (delete objects)

### UI-18 · Legal disclaimer gate
- **What:** Blocking, non-dismissible gate at launch until disclaimer version `2026-06-07` is accepted via a checkbox; "Don't show again" is pre-ticked (if unticked, the gate returns every launch). Shows a ToS/copyright/privacy/warranty/affiliation summary plus the expandable full `DISCLAIMER.md` (raw import).
- **Entry points:** `src/App.tsx:170,887`; `src/components/DisclaimerGate.tsx:35-233`; `src/disclaimer.ts:9-69`
- **Data:** localStorage `app:disclaimerAcceptance` `{version, acceptedAt, dontShowAgain}`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Record consent per account server-side (version + timestamp), shown at signup and re-prompted on version bump.

### UI-19 · Legal card and disclaimer review
- **What:** Settings → Legal shows the recorded acceptance date/version (or "not accepted") and opens DisclaimerGate in dismissible review mode (Esc or backdrop closes).
- **Entry points:** `src/views/Settings.tsx:1821-1859`; `src/components/DisclaimerGate.tsx:48-59,150-173`
- **Data:** localStorage `app:disclaimerAcceptance`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db (read the consent record)

### UI-20 · AI onboarding gate overlay (ref. area 3)
- **What:** A latched overlay covers only the AI tabs while the local AI pipeline is incomplete. "Skip for now" mutes it for the session; "Open settings" navigates to Settings.
- **Entry points:** `src/App.tsx:222-231,804-827`
- **Data:** `useAiSetupStatus` (area 3)
- **Local deps:** area 3
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink (area 3). Becomes an "add an API key" onboarding.

### UI-21 · Remote-AI-unreachable banner (ref. area 3)
- **What:** A bottom floating banner shows while a configured remote AI node is unreachable. Actions: Retry, Use local models (if downloaded), or Download local model (with %). It listens to `ai:remoteStatus`.
- **Entry points:** `src/components/RemoteAiBanner.tsx:11-93`; `src/App.tsx:700` → `ai:remoteStatus|retryRemote|useLocalModels` → `electron/ipc.ts:1101-1110`
- **Data:** area 3
- **Local deps:** area 3
- **External calls:** remote AI node (area 3)
- **Status:** shipped
- **Web port:** rethink (area 3). Becomes a provider outage/quota banner.

### UI-22 · Paged infinite scroll with prefetch
- **What:** The first page loads 50 posts, then +250 whenever the sentinel is within 3000 px (IntersectionObserver rooted at the scroller, re-checked after each load); paused while hidden or in canvas mode. `usePosts` (area 1) appends only the missing offset page, de-dupes by id and uses `startTransition`.
- **Entry points:** `src/views/Gallery.tsx:137-150,895-976`; `src/hooks/usePosts.ts:183-266` → `db:getPosts` → `electron/ipc.ts:157` → `electron/db.ts:1818`
- **Data:** `posts` (area 1)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Cursor pagination recommended; the loaded array never shrinks today.

### UI-23 · Live grid updates (push-driven)
- **What:** New-post, download-done and analyze-done pushes refresh the grid: one row is patched via `getPostsByIds` when its filter membership can't change, otherwise a coalesced reload (400 ms quiet, 2 s max wait) reconciles by id. Events while hidden mark the view dirty for a single reload on return.
- **Entry points:** `src/hooks/usePosts.ts:52-95,280-390` (area 1 hook) → push `interceptor:newPosts`, `download:progress`, `analyze:progress`
- **Data:** `posts`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push (SSE/WebSocket per user)

### UI-24 · Row-virtualized square grid
- **What:** TanStack `useVirtualizer` with a deterministic row height ((width − padding − gaps)/cols + gap, no per-row measuring), overscan 6 rows, `scrollMargin` = floating header height, a ≤24-row pre-measure fallback and a first-paint stagger. Shared by Gallery, AI Search and Tags Explorer.
- **Entry points:** `src/components/VirtualPostGrid.tsx:62-344`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable. Tune overscan for network latency.

### UI-25 · Responsive columns and grid density control
- **What:** Base columns from window width (≥1280→6, ≥1024→5, ≥768→4, ≥640→3, else 2) plus a density step −3…+4 (min 1 column) set by zoom buttons; the step persists and syncs live across grid, skeleton and canvas tile size.
- **Entry points:** `src/components/GridSizeControl.tsx:17-51`; `src/hooks/useGridSize.ts:10-44,101-122`; `src/components/VirtualPostGrid.tsx:22-28,78-105`
- **Data:** localStorage `gridSizeStep`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable. Pair with `srcset` per density.

### UI-26 · Density zoom animation (FLIP)
- **What:** When the column count changes, the new layout renders pre-scaled to the old card size (anchored at the viewport center) and animates to scale 1 over ~320 ms. Width-only changes re-measure without animating.
- **Entry points:** `src/components/VirtualPostGrid.tsx:138-189,295-307`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-27 · Infinite canvas view
- **What:** Tiles a ≤500-post pool infinitely (cols = ⌈√n⌉, modular mapping, golden-ratio column offsets, viewport culling with 2 overscan rings). Drag pans with inertia, wheel/two-finger swipe pans, pinch or ⌘/Ctrl+wheel zooms 0.45–2.6× at the cursor (5 px click slop); edge vignette, reduced-motion aware; no drag-select and no multi-touch pinch.
- **Entry points:** `src/components/InfiniteCanvas.tsx:43-607`; `src/views/Gallery.tsx:1012-1021,1609-1620`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable. Add touch pinch; remote images repeat across tiles (cache headers matter).

### UI-28 · Grid ↔ canvas toggle (persisted)
- **What:** A toolbar toggle switches modes; its icon previews the target mode. Canvas hides the date sort and grows the loaded pool to 500. The choice persists and syncs across views.
- **Entry points:** `src/views/Gallery.tsx:209-217,1091-1104`; `src/hooks/useViewMode.ts:16-66`
- **Data:** localStorage `galleryViewMode`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-29 · Date sort toggle
- **What:** One button flips Newest ↔ Oldest; the DB orders by post timestamp (undated last, id tiebreak), with relevance first during text search. Not persisted; hidden in canvas mode.
- **Entry points:** `src/views/Gallery.tsx:154-157,1109-1127` → `db:getPosts` → `electron/db.ts:1811-1816,1818-1843`
- **Data:** `posts.timestamp`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-30 · Search bar (debounced, relevance-ranked)
- **What:** The search pill updates `filters.search` 300 ms after typing (× clears). The DB matches the whole phrase (LIKE over caption, author, shortcode, AI description/tags/keywords, user note/tags) or any content term (stopwords removed, also via `post_tags`) and ranks by IDF-weighted relevance, then date. Not AI search (area 3).
- **Entry points:** `src/components/FilterBar.tsx:47-70,96-118` → `db:getPosts` → `electron/db.ts:1537-1645`
- **Data:** `posts`, `post_tags`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Full-text index (FTS/Postgres tsvector) recommended.

### UI-31 · AI suggested filter chips with AND/OR (ref. area 3)
- **What:** Opt-in (default off): 600 ms after a non-empty query, `search:suggest` proposes related tags as chips; toggling a chip adds a concept block (OR by default, AND/OR toggle with ≥2 chips). A new query clears the concepts; responses are race-guarded.
- **Entry points:** `src/views/Gallery.tsx:283-352,1498-1559` → `search:suggest` → `electron/ipc.ts:1073`; DB concept blocks `electron/db.ts:1649-1663`
- **Data:** localStorage `aiSearchSuggestions`; `posts`, `post_tags`
- **Local deps:** local LLM (area 3)
- **External calls:** AI provider (area 3)
- **Status:** shipped (opt-in)
- **Web port:** api+db (area 3 provider call)

### UI-32 · Tag filter chip
- **What:** Clicking an AI or manual tag in the post modal applies `filters.tag` and closes the modal. A floating "#tag" chip with an × removes it. Matching is case-insensitive via `post_tags.tag_norm`.
- **Entry points:** `src/views/Gallery.tsx:1703-1710`; `src/components/FilterBar.tsx:124-138`; DB `electron/db.ts:1459-1464`
- **Data:** `post_tags`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-33 · Active folder chip
- **What:** When a folder source is active, the toolbar shows its color dot and name.
- **Entry points:** `src/views/Gallery.tsx:1129-1140`
- **Data:** `collections`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-34 · Filter drawer: source picker
- **What:** A right drawer mirrors the sidebar Library: All posts, Instagram, X, Pinterest and Websites with counts, nested platform folders, and custom folders. Selecting routes through the same `onSelectSource` path; the drawer stays open. Note: folders whose platform value is not one of those four are hidden here but shown in the sidebar.
- **Entry points:** `src/components/FilterDrawer.tsx:169-229,282-344`; `src/views/Gallery.tsx:1684-1693`
- **Data:** `collections`, stats
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (data api+db)

### UI-35 · Filter: media type
- **What:** Segmented All / Video / Image / Carousel. It is an exact `media_type` match, so `images`, `text`, `website` and `file` posts can't be isolated, and "Image" excludes multi-image `images`.
- **Entry points:** `src/components/FilterDrawer.tsx:150-155,347-355` → `electron/db.ts:1443-1446`
- **Data:** `posts.media_type`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-36 · Filter: download status
- **What:** Segmented All / Downloaded / Link only. The UI value `linkonly` maps to API `missing`; the filter checks for presence or absence of a local thumbnail, image or video path.
- **Entry points:** `src/components/FilterDrawer.tsx:157-161,357-365`; `src/lib/postFilters.ts:46-63` → `electron/db.ts:1693-1701`
- **Data:** `posts.*_path`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db (becomes "stored copy" vs "link only")

### UI-37 · Filter: AI-tagged
- **What:** Segmented All / Tagged / Untagged. A post counts as tagged if it has a `post_tags` row of AI tier general/specific (or legacy NULL). Manual tags don't count.
- **Entry points:** `src/components/FilterDrawer.tsx:163-167,367-375` → `electron/db.ts:1522-1535`
- **Data:** `post_tags.tier`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-38 · Filter drawer chrome
- **What:** The drawer slides open on a width-animated track and is inert/aria-hidden when closed. "Reset" clears the three segmented filters. The toolbar "Filters" button shows an active-filter count badge. Filter state is not persisted.
- **Entry points:** `src/components/FilterDrawer.tsx:182-188,232-278`; `src/components/FilterBar.tsx:72-79,156-174`
- **Data:** in-memory filters
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Consider URL query params.

### UI-39 · Total count pill
- **What:** Shows "{n} posts", the unpaged total matching the current query.
- **Entry points:** `src/components/FilterBar.tsx:145-147`
- **Data:** `db:getPosts.total`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-40 · Manual gallery refresh
- **What:** A refresh icon button re-fetches the current window and spins while loading.
- **Entry points:** `src/views/Gallery.tsx:1196-1205`
- **Data:** `posts`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-41 · Toolbar source-sync button (ref. area 2)
- **What:** A cloud button appears when the current source maps to a connector: IG/X platform, Pinterest when board folders exist, or a folder with `externalId`. It starts a background sync, or stops it while running (spinner). Routed to the Browser's registered API via App.
- **Entry points:** `src/views/Gallery.tsx:1152-1194`; `src/App.tsx:252-262`
- **Data:** `collections.platform/externalId`
- **Local deps:** Electron webview (area 2)
- **External calls:** instagram.com / x.com / pinterest (area 2)
- **Status:** shipped
- **Web port:** rethink (area 2)

### UI-42 · Ordered search transition
- **What:** On a query change, the old results stay on screen until the fetch settles, then play an "out" dissolve (column-rippled) and an "in" bloom of the new set. Canvas uses a whole-wall fade plus a 400 ms settle. Infinite-scroll growth does not animate.
- **Entry points:** `src/views/Gallery.tsx:384-398`; `src/hooks/useSearchTransition.ts:35-124`; `src/components/VirtualPostGrid.tsx:238-260`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-43 · Skeleton, loading, empty and error states
- **What:** First load shows 18 shimmering square tiles (same columns/density), paging shows a mid-scroll spinner, and the empty state shows the error or "No posts found. Import a JSON file or capture posts via the Browser tab" — gated on the displayed list so it never flashes mid-transition.
- **Entry points:** `src/components/PostGridSkeleton.tsx:14-39`; `src/views/Gallery.tsx:996,1645-1676`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (copy needs updating for web)

### UI-44 · Floating frosted toolbar layout
- **What:** Apple-Maps-style translucent pills (`backdrop-blur` on the toolbar only) over the grid. Gaps pass clicks through to the grid. The header height is measured with a ResizeObserver to inset the grid (`topInset`/`scrollMargin`). In select mode the strip becomes the selection pill.
- **Entry points:** `src/views/Gallery.tsx:168-175,903-917,1061-1218`; `src/components/FilterBar.tsx:81-182`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Rework for narrow/mobile widths.

### UI-45 · Select mode
- **What:** "Select" swaps the toolbar to count, select-all, Actions menu and ✕; cards show checkboxes, unselected cards dim to 80% and the post modal is suppressed. Exiting clears the selection, the optimistic membership overlay and pending confirmations.
- **Entry points:** `src/views/Gallery.tsx:355-357,400-415,1207-1215,1220-1487`; `src/components/PostCard.tsx:647-649,692-705`
- **Data:** in-memory selection Set
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-46 · Hover quick-select
- **What:** Outside select mode, a checkbox appears on card hover/focus. Clicking it (or Enter/Space) enters select mode with that post selected. It does not open the modal or start a drag-select.
- **Entry points:** `src/components/PostCard.tsx:514-525,710-725`; `src/views/Gallery.tsx:637-645`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Needs a long-press equivalent on touch.

### UI-47 · Click toggle and Shift range select
- **What:** In select mode, a click toggles a card and moves the anchor. Shift+click adds every card between the anchor and the click (additive, inclusive). The anchor resets whenever the list is refetched or reordered. Keyboard activation forwards Shift too.
- **Entry points:** `src/hooks/useRangeSelect.ts:29-107`; `src/views/Gallery.tsx:263-266,610-632`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable, and also used by AiWebsites (area 4).

### UI-48 · Drag-select sweep
- **What:** In select mode, pressing on a card and sweeping selects the range from the press point to the hovered card. It deselects instead if the first card was already selected, and shrinks when moving back. The trailing click is suppressed and the Shift anchor reset. Grid only and mouse only.
- **Entry points:** `src/views/Gallery.tsx:480-575`; `src/components/VirtualPostGrid.tsx:291-292`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Needs a touch/pointer-events rework.

### UI-49 · Select all matching / deselect all
- **What:** Instantly selects the loaded cards. If `total` exceeds the loaded count, it then fetches every matching id (`getPostIds`), with a guard against stale filter changes, and shows a toast "{n} selected". It flips to "Deselect all" once everything is selected.
- **Entry points:** `src/views/Gallery.tsx:860-893,1244-1260` → `db:getPostIds` → `electron/ipc.ts:160` → `electron/db.ts:1857`
- **Data:** `posts` ids (up to 100k per bulk call, `electron/ipc.ts:49`)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Prefer server-side "bulk by query" over shipping id arrays.

### UI-50 · Auto-exit selection on filter or tab change
- **What:** Any change to the result-defining filters (everything except limit and sort) exits select mode or clears stale selection state. Leaving the gallery tab also exits select mode.
- **Entry points:** `src/views/Gallery.tsx:427-463`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-51 · Bulk: analyze selected (ref. areas 3/2)
- **What:** Stops with "download a model first" if no model is ready; otherwise splits the selection into local-media posts (queued) and posts needing download, toasts queued/partial, and shows a dismissible amber banner offering to download the missing media.
- **Entry points:** `src/views/Gallery.tsx:699-767,1296-1307,1564-1591` → `analyze:modelStatus|split|posts`, `download:posts` → `electron/ipc.ts:637,603,573,509`
- **Data:** `jobs`, `posts`
- **Local deps:** local VLM (area 3)
- **External calls:** AI provider (area 3)
- **Status:** shipped
- **Web port:** api+db. Remote vision APIs can analyze remote media, so the "needs download" split may disappear.

### UI-52 · Bulk: download missing media (ref. area 2)
- **What:** Queues downloads of missing assets only, for the asset types enabled in Settings. With no types enabled it shows a toast instead. Toast "{n} downloading".
- **Entry points:** `src/views/Gallery.tsx:769-786,1309-1327` → `download:posts` (missingOnly) → `electron/ipc.ts:509`
- **Data:** localStorage `download:assetTypes`; `downloads`
- **Local deps:** yt-dlp/ffmpeg (area 2)
- **External calls:** platform CDNs (area 2)
- **Status:** shipped
- **Web port:** api+db + object-storage (server-side fetch)

### UI-53 · Bulk: add to folder (with create and assign)
- **What:** The Actions menu lists folders, with a green check where all selected posts are already members. Assignment is optimistic (overlay), refreshes sidebar counts, and rolls back on failure with a toast. "Create new source" opens CollectionModal, then creates the folder and assigns.
- **Entry points:** `src/views/Gallery.tsx:650-696,1023-1044,1331-1378,1741-1747` → `collections:addPosts` → `electron/ipc.ts:442`
- **Data:** `post_collections`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-54 · Bulk: clear AI descriptions
- **What:** Two-step confirm (amber). Clears AI descriptions on the selected posts, shows a toast, clears the selection and reloads.
- **Entry points:** `src/views/Gallery.tsx:788-807,1383-1411` → `analyze:clearDescriptions` → `electron/ipc.ts:1060`
- **Data:** `posts.ai_description`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-55 · Bulk: remove AI tags
- **What:** Two-step confirm. Clears AI tags on the selected posts, shows a toast, clears the selection and reloads.
- **Entry points:** `src/views/Gallery.tsx:809-826,1413-1439` → `analyze:clearTags` → `electron/ipc.ts:1063`
- **Data:** `posts.ai_tags`, `post_tags`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-56 · Bulk: delete selected posts
- **What:** Two-step red confirm with an irreversibility hint; deletes DB rows and on-disk files, toasts the deleted count merged with "{n} files not removed", exits select mode and reloads the grid, sidebar counts and stats.
- **Entry points:** `src/views/Gallery.tsx:828-858,1443-1474` → `db:deletePosts` → `electron/ipc.ts:358-365`
- **Data:** `posts` + related rows, asset files
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage. Consider soft-delete/undo.

### UI-57 · Bulk-action feedback toast
- **What:** `useToast` shows a message for 3 s. It renders inside the selection pill in select mode, or as a fixed bottom toast in browse mode (so the "N deleted" message survives exiting selection).
- **Entry points:** `src/views/Gallery.tsx:376-378,1048-1060,1227-1235`; `src/hooks/useToast.ts:8-46`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-58 · Card cover thumbnail loading
- **What:** Local thumbnail/image/preview paths load as `asset://…?w=640` (cached downscale), otherwise the remote `thumbnailUrl`; eager loading with async decode (suits the virtualizer overscan), object-cover with 3 px overscan, top-anchored web screenshots, fade-in on load.
- **Entry points:** `src/components/PostCard.tsx:17-21,36-51,355-357,629-662`; `src/lib/asset.ts:12-14`
- **Data:** post path columns, `thumbnail_url`
- **Local deps:** `asset://` protocol
- **External calls:** remote CDN thumbnails (IG/X/Pinterest)
- **Status:** shipped
- **Web port:** object-storage. CDN-resized URLs (`srcset`); re-host the expiring IG/X URLs.

### UI-59 · Blur-up placeholder
- **What:** The post row carries `thumbBlur`, a ~24 px JPEG data URI. It paints blurred (12 px, scale 1.08) at mount and unmounts after the fade. A failsafe drops it after 1.2 s even if the image never fires load or error.
- **Entry points:** `src/components/PostCard.tsx:424-437,560-570,614-628`; generated by `electron/thumbs.ts:270-337`
- **Data:** `posts.thumb_blur`
- **Local deps:** Electron `nativeImage` (generation)
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage. Compute at ingest (blurhash/data URI) and ship in the API row.

### UI-60 · Hover video preview
- **What:** For a downloaded single video, a `<video>` mounts on first hover. Its `src` is set only while hovering; it plays muted in a loop, fades in on `playing`, rewinds on leave, and releases the source on unmount. Streams via `asset://` with Range support.
- **Entry points:** `src/components/PostCard.tsx:400-402,449-495,673-688`
- **Data:** `posts.video_path`
- **Local deps:** `asset://` Range streaming
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage. Range-capable CDN; consider short preview clips or posters.

### UI-61 · Hover carousel slideshow
- **What:** For non-web posts with ≥2 image slides, hovering cycles the slides every 800 ms, using the local 640 px thumbnail or the remote URL.
- **Entry points:** `src/components/PostCard.tsx:324-334,405-409,440-447,527-531`
- **Data:** `post_media`
- **Local deps:** `asset://`
- **External calls:** remote CDN images
- **Status:** shipped
- **Web port:** object-storage (touch has no hover)

### UI-62 · Hover info overlay
- **What:** Lazily mounted on first hover/focus: identity (@author, domain or user note), up to 5 web palette swatches, up to 3 tags (AI first, then user tags) or the first caption line, the localized date, an AI sparkle when description and tags exist, and a downloaded vs link-only icon.
- **Entry points:** `src/components/PostCard.tsx:367-398,739-808`
- **Data:** post fields
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Needs a tap/long-press equivalent on touch.

### UI-63 · Rest-state identity chips
- **What:** Always-visible chips: bottom-left shows the platform glyph (or favicon plus domain for web posts); bottom-right shows the media-type icon with a count for multi-slide posts. They deliberately have no `backdrop-filter`, for scroll performance.
- **Entry points:** `src/components/PostCard.tsx:74-78,115-167,810-837`
- **Data:** `platform`, `media_type`, `media_count`, `web_domain`
- **Local deps:** none
- **External calls:** site favicon (see UI-66)
- **Status:** shipped
- **Web port:** client-only

### UI-64 · Content fallbacks
- **What:** Instead of a grey box: a typographic quote card for text or caption-only posts, a web fallback (favicon + title/domain), a manual-bookmark fallback (file icon + note), or a social fallback (glyph, @author, "media unavailable" when the media existed).
- **Entry points:** `src/components/PostCard.tsx:185-311,533-552,664-670`
- **Data:** post fields
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-65 · Broken-thumbnail auto-repair request
- **What:** When the cover `<img>` fails on a social post that has a remote `thumbnailUrl`, the card fires `preview:repair`. Main re-caches the preview and pushes a grid refresh; the card shows the fallback meanwhile.
- **Entry points:** `src/components/PostCard.tsx:656-661` → `preview:repair` → `electron/ipc.ts:173-176`
- **Data:** `posts.preview_path`
- **Local deps:** filesystem (preview cache)
- **External calls:** IG/X CDN (re-fetch)
- **Status:** shipped
- **Web port:** api+db + object-storage (server re-fetch job)

### UI-66 · Website favicon and award badge
- **What:** Web cards fetch `https://<domain>/favicon.ico` directly, falling back to a Globe icon on error. Web posts with awards show an amber badge (level or count) top-right.
- **Entry points:** `src/components/PostCard.tsx:80-125,361-365,727-737`
- **Data:** `web_domain`, `web_awards`, `web_palette`
- **Local deps:** none
- **External calls:** every saved site's `/favicon.ico` (third-party requests)
- **Status:** shipped
- **Web port:** object-storage. Store favicons at capture time; avoid leaking each domain from the client.

### UI-67 · Card accessibility and keyboard activation
- **What:** Each card is a `role=button`, tabbable, with an `aria-label` from caption/domain/author and `aria-pressed` in select mode. Enter/Space opens the post (or toggles it in select mode) and forwards Shift for range select. Focus mounts the hover chrome.
- **Entry points:** `src/components/PostCard.tsx:497-510,572-607`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-68 · Post modal shell
- **What:** Closes on backdrop, Esc or × (`window.confirm` first if the AI panel has unsaved edits); focus moves into the dialog with a Tab trap, and layers above (lightbox, assign popover, create dialog) own the keyboard. Layout: 88vh, max-w-5xl, media column plus a 380 px meta column.
- **Entry points:** `src/components/PostModal.tsx:43-88,157-223,317-473`
- **Data:** post object
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (needs a stacked layout on mobile)

### UI-69 · Previous/next post navigation
- **What:** Edge buttons and ←/→ step through slides first, then move to the previous/next post in the gallery's loaded list. Indexing uses an id→index map. Only available when opened from the Gallery.
- **Entry points:** `src/components/PostModal.tsx:165-175,324-350`; `src/views/Gallery.tsx:577-603,1699-1702`
- **Data:** loaded `posts`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-70 · Media carousel
- **What:** Slides come from `post.media` (preview path substituted for an un-downloaded first image) with arrows, counter and dots; video plays with controls/autoplay and a persisted mute preference (default muted), images open the lightbox. Media resolves local `asset://` → remote URL → webview fallback.
- **Entry points:** `src/components/postmodal/MediaCarousel.tsx:27-50,176-268`; `src/components/postmodal/helpers.ts:62-70,130-172`
- **Data:** `post_media`; localStorage `postModal:videoMuted`
- **Local deps:** `asset://`
- **External calls:** remote CDN media
- **Status:** shipped
- **Web port:** object-storage

### UI-71 · Web page screenshots viewer
- **What:** For web posts, each captured page is a full-width, vertically scrollable screenshot. The homepage is pinned as the first slide. A page chip labels each slide ("Home" or a prettified slug), with arrows, dots and a counter. Tall captures open in the lightbox as lazy-stacked chunks.
- **Entry points:** `src/components/postmodal/MediaCarousel.tsx:52-173`; `src/components/postmodal/helpers.ts:83-124,137-145`; `src/components/PostModal.tsx:477-503`
- **Data:** `post_media`, `web_pages` chunks (area 4)
- **Local deps:** `asset://`
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage

### UI-72 · Live-page webview fallback
- **What:** When no media can be shown (or for an un-downloaded carousel video slide), the modal embeds the live post URL in `<webview partition="persist:social">`, http(s) only. X URLs without a username are canonicalized to `/i/web/status/<id>`.
- **Entry points:** `src/components/postmodal/MediaCarousel.tsx:91-98,205-216`; `src/components/postmodal/helpers.ts:27-47,157-172`
- **Data:** `post_url`
- **Local deps:** Electron `<webview>`, the `persist:social` login session
- **External calls:** instagram.com / x.com / pinterest
- **Status:** shipped
- **Web port:** drop (desktop-only). IG/X refuse framing, so use "Open original" or oEmbed.

### UI-73 · Image lightbox
- **What:** Full-screen (z 120) image-only viewer: ←/→ wrap, Esc closes just the lightbox (capture phase), tall captures stack lazy-loaded chunks, "Open page" for web posts, load-failed state. Reused by AiWebsites (area 4).
- **Entry points:** `src/components/ImageLightbox.tsx:26-199`; `src/components/PostModal.tsx:134-156,475-508` → `shell:openExternal`
- **Data:** slide URLs
- **Local deps:** `asset://`
- **External calls:** page URL (OS browser)
- **Status:** shipped
- **Web port:** client-only (`openExternal` → `window.open`)

### UI-74 · Modal header identity and "Local" badge
- **What:** Shows the platform icon in its accent color, the author name/@username (or domain for web posts), and a green "Local" badge when the current slide is served from `asset://`.
- **Entry points:** `src/components/PostModal.tsx:225-256,366-405`
- **Data:** post fields
- **Local deps:** `asset://` detection (`src/lib/asset.ts:18-20`)
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. The "Local" badge semantics become "stored copy".

### UI-75 · Add single post to folder
- **What:** A FolderPlus popover lists folders with a green check for membership; assignment is optimistic with rollback and a navigation guard, and "Create new source" creates the folder, refreshes the list and assigns. No remove-from-folder (`removePostFromCollection` is bridged but unused).
- **Entry points:** `src/components/postmodal/CollectionsMenu.tsx:20-92`; `src/components/PostModal.tsx:90-119,270-315` → `collections:list|addPosts|create` → `electron/ipc.ts:398-442`
- **Data:** `collections`, `post_collections`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db (add remove-from-folder)

### UI-76 · Actions ⋮: open file location
- **What:** "Open file" reveals the primary local file. For manual bookmarks this is the original file; otherwise it is the video, then the current slide, then the image, then the thumbnail.
- **Entry points:** `src/components/PostModal.tsx:258-268`; `src/components/postmodal/ActionsMenu.tsx:278-290` → APP-16
- **Data:** local paths
- **Local deps:** Electron `shell`
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only). Use "Download original".

### UI-77 · Actions ⋮: open original
- **What:** Opens the post's permalink in the OS browser (X URL fix applied). Hidden when there is no URL (manual bookmarks).
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:293-305` → APP-15
- **Data:** `post_url`
- **Local deps:** Electron `shell`
- **External calls:** platform site
- **Status:** shipped
- **Web port:** client-only

### UI-78 · Actions ⋮: download locally (single post)
- **What:** For non-manual posts without local files: queues the download with a "Queued…" spinner, reports "nothing to download" when `queued=0` or no progress arrives within 8 s, refreshes the post on each completed asset and settles after 1.5 s of quiet; errors are shown.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:116-204,310-334` → `download:post`, push `download:progress` → `electron/ipc.ts:466`
- **Data:** `downloads`, post paths
- **Local deps:** yt-dlp/ffmpeg (area 2)
- **External calls:** platform CDN (area 2)
- **Status:** shipped
- **Web port:** api+db + realtime-push

### UI-79 · Actions ⋮: delete local files
- **What:** Two-step confirm. Suspends any downloads for the post, unlinks the current capture's files and clears their paths, keeping the post. Then refreshes the post and the grid.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:206-228,335-357` → `db:deleteLocalFiles` → `electron/ipc.ts:299-324`
- **Data:** post paths, asset files
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage ("remove stored copy")

### UI-80 · Actions ⋮: delete post
- **What:** Two-step red confirm. Deletes the DB record and files, closes the modal, shows the toast "Post deleted", and reloads the grid and counts.
- **Entry points:** `src/components/postmodal/ActionsMenu.tsx:230-254,362-382`; `src/views/Gallery.tsx:1716-1721` → `db:deletePosts`
- **Data:** `posts`, asset files
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage

### UI-81 · Meta column
- **What:** Shows the selectable caption, a facts byline (media type label, N items/pages, localized date-time) and, for social posts, Thumbnail/Image/Video links that open in the OS default app.
- **Entry points:** `src/components/postmodal/MetaColumn.tsx:28-155`; `src/components/postmodal/helpers.ts:49-60,73-81` → `shell:openPath`
- **Data:** post fields
- **Local deps:** Electron `shell`
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (drop the open-path links)

### UI-82 · Embedded AI and web metadata panels (ref. areas 3/4)
- **What:** AiPanel (area 3: AI categorization, edits, regeneration; tag chips apply the gallery tag filter) and WebMetaPanel (area 4: web metadata, re-analyze, open in Websites).
- **Entry points:** `src/components/postmodal/MetaColumn.tsx:73-84,114-119`
- **Data:** areas 3/4
- **Local deps:** areas 3/4
- **External calls:** areas 3/4
- **Status:** shipped
- **Web port:** api+db (areas 3/4)

### UI-83 · Activity strip
- **What:** Sidebar footer row: primary-kind icon (spinner when indeterminate, amber warning on error, glow when an action is needed), headline ("idle", the primary short label or "+N more"), count badge, primary progress bar, and an unread dot when idle.
- **Entry points:** `src/components/ActivityCenter.tsx:345-484`; `src/hooks/useActivity.ts:641-669`
- **Data:** in-memory activity state
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push

### UI-84 · Activity popover: live items and queue controls
- **What:** Live items: analysis (done/total, ETA, pause/resume, cancel all), downloads (done/total incl. partial progress, pause/resume, cancel all), per-job web capture (phase, cancel; retry/remove on error), per-platform source sync (folder, step, scanned/new, stop; login/error item with "open browser"), legacy sync, selection save, VLM/STT model and binaries downloads, OTA update. Sorted errors first, then by kind priority.
- **Entry points:** `src/hooks/useActivity.ts:229-239,318-669`; `src/components/ActivityCenter.tsx:207-284,392-409`; `src/App.tsx:432-484`
- **Data:** push `analyze:progress`, `analyze:modelProgress`, `download:progress`, `web:progress`, `updater:state`, `binaries:progress`, `stt:modelProgress`; renderer-only sync/save
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push. Drop the model/binaries/stt/update kinds; expose cancel/pause APIs.

### UI-85 · Activity click-through navigation
- **What:** Clicking an item routes by kind: analysis → Auto-tag, download → Downloads, web → Website Analyzer (the passed `postId` is ignored by App), save → Gallery, sync → that platform's Browser tab, model/binaries/stt/update → Settings.
- **Entry points:** `src/components/ActivityCenter.tsx:131-144,411-420`; `src/App.tsx:420-429`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (routes)

### UI-86 · Activity session history
- **What:** In-memory log (cap 50, de-duped by id) of edge-detected milestones: update ready/error, binaries/STT/model ready, analysis and download batches done, web capture done/partial/error, selection saved, source and legacy sync done/stopped/error. Unread count (cleared on open), per-entry dismiss, clear all, relative times; lost on restart.
- **Entry points:** `src/hooks/useActivity.ts:671-1070`; `src/components/ActivityCenter.tsx:153-162,295-336,486-554`
- **Data:** in-memory only
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Persisted per-user notifications for multi-device.

### UI-87 · Update CTA and dismiss in Activity
- **What:** The update item offers Update now (rebuild), Restart now (install), Download (manual), Retry (rebuild, which re-checks on macOS) and Later. "Later" hides the current update state, keyed by status:version:epoch, until the next real event.
- **Entry points:** `src/hooks/useActivity.ts:305-403,1122-1125`; `src/components/ActivityCenter.tsx:392-409` → `updater:rebuild|quitAndInstall|openDownload`
- **Data:** `updater:state`
- **Local deps:** updater (APP-21..24)
- **External calls:** none
- **Status:** shipped
- **Web port:** drop (desktop-only)

### UI-88 · Settings page shell and version pill
- **What:** One scrolling page (max-w-4xl) with sections Language, Artificial intelligence, Downloads and data, Updates, Danger zone, Legal, each with a staggered fade-in. The header shows the version pill.
- **Entry points:** `src/views/Settings.tsx:1764-1817,2203-2330`
- **Data:** `app:getVersion`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink. Most cards drop; add account, storage and provider-key sections.

### UI-89 · Language picker
- **What:** A select with Italiano / English. The change applies app-wide immediately and persists.
- **Entry points:** `src/components/LanguageCard.tsx:8-35`; `src/views/Settings.tsx:2226-2228`
- **Data:** localStorage `app:language`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (optionally an account preference)

### UI-90 · AI settings cards (ref. area 3)
- **What:** Cards for: AI search-suggestions toggle; remote providers editor (name, id, base URL, model, secret name, Pi key, vision flag, key) with default chat and vision provider; VLM/STT/embedding model pickers (download, pause, cancel, delete, activate); parallel classifications 1–10; performance/hardware (detected CPU/RAM/GPU/VRAM, Auto/Custom overrides, reset).
- **Entry points:** `src/views/Settings.tsx:490-950,1326-1716,1892-2197,2230-2255`
- **Data:** `ai-providers.json`, Keychain, `ai-model.json`, `stt-model.json`, localStorage `aiSearchSuggestions`
- **Local deps:** local models, hardware probe, macOS Keychain
- **External calls:** model hosts (area 3)
- **Status:** shipped
- **Web port:** rethink (area 3). Becomes per-user provider API keys.

### UI-91 · Asset types to download (ref. area 2)
- **What:** Thumbnail, image and video toggles (all on by default). They scope bulk downloads and the Downloads view, and stay in sync across mounted instances via a custom event.
- **Entry points:** `src/views/Settings.tsx:196-246,1861-1890`; `src/hooks/useDownloadPrefs.ts:1-57`
- **Data:** localStorage `download:assetTypes`
- **Local deps:** localStorage
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db (account pref; also drives storage quota)

### UI-92 · Import/Export JSON card and export modal
- **What:** "Import JSON" opens ImportModal (area 4). "Export JSON" opens a modal with Instagram/X/Pinterest checkboxes and counts (web/manual posts can't be exported), then a native save dialog (`saved-posts.json`); the file is written in 500-post chunks with collections, and the result count/path or error is shown.
- **Entry points:** `src/views/Settings.tsx:174-184,341-488,1718-1762` → `db:exportJSON` → `electron/ipc.ts:198-236`
- **Data:** `posts`, `collections`
- **Local deps:** Electron `dialog`, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db. Streamed download (`Content-Disposition`) or an async export job.

### UI-93 · Update channel and controls card
- **What:** Stable/Beta select (a switch triggers a check), installed version, download progress bar, live build log, error text and status line, plus buttons Update now, Restart and install, Download (manual) and Check for updates (spinner ≥800 ms).
- **Entry points:** `src/views/Settings.tsx:952-1137` → `app:*`, `updater:*`, push `updater:state` → `electron/ipc.ts:846-863`
- **Data:** `update-channel.json`
- **Local deps:** updater
- **External calls:** github.com Releases
- **Status:** shipped
- **Web port:** drop (desktop-only)

### UI-94 · Runtime components card (ref. area 3)
- **What:** Shows sidecar status (ready or missing). A llama variant select (CPU, CUDA, Vulkan, Metal) forces a re-provision. Includes Redownload, phase progress and GPU-fallback/failed-variant warnings.
- **Entry points:** `src/views/Settings.tsx:1139-1324` → `binaries:*` → `electron/ipc.ts:866-891`
- **Data:** `llama-variant.json`, `runtime-bin/`
- **Local deps:** filesystem, child processes
- **External calls:** binary hosts (area 3)
- **Status:** shipped
- **Web port:** drop (desktop-only)

### UI-95 · Danger zone: delete saved files
- **What:** Two-step confirm row with busy/done/error states. Deletes downloaded media for Instagram, X and Pinterest only and clears those paths; manual-bookmark files and website captures are preserved. Posts stay as link-only. Then refreshes stats and badges.
- **Entry points:** `src/views/Settings.tsx:257-339,2284-2294` → `db:clearAssets` → `electron/ipc.ts:293-297` → `db.clearAllAssetPaths` (`electron/db.ts:5612-5621`)
- **Data:** social-post asset paths, `post_media.local_path`, `userData/assets`
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage (account-level, re-auth)

### UI-96 · Danger zone: delete AI descriptions and tags
- **What:** Two-step confirm. Aborts any running chat/cluster/alias jobs, then wipes all derived AI data. The user's manual tags and notes are kept.
- **Entry points:** `src/views/Settings.tsx:2296-2306` → `db:clearAiAnalysis` → `electron/ipc.ts:277-292` → `db.clearAllAiAnalysis` (`electron/db.ts:5573-5608`)
- **Data:** `posts.ai_*` columns, AI-tier `post_tags`, `post_entities`, `tag_cluster*`, `tag_alias`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db

### UI-97 · Danger zone: delete all saved posts
- **What:** Two-step confirm. Cancels the download, analysis and web queues, aborts AI jobs, wipes the library and the persisted job rows. Then resets badges and reloads stats and collections.
- **Entry points:** `src/views/Settings.tsx:2308-2318`; `src/App.tsx:509-513` → `db:clearAll` → `electron/ipc.ts:237-276`
- **Data:** entire DB
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage (account data deletion, GDPR flow)

### UI-98 · Feedback modal
- **What:** Message ≤5000 chars with counter; up to 5 images (picker or paste), ≤5 MB each and ≤16 MB base64 total, with removable thumbnails and warnings. The backdrop doesn't close it; Esc/×/Cancel confirm discarding input; Send always confirms; errors allow retry; success auto-closes after 1.4 s; focus trap and inert background.
- **Entry points:** `src/components/Sidebar.tsx:814-821,842`; `src/components/FeedbackModal.tsx:67-591` → APP-26
- **Data:** none persisted
- **Local deps:** FileReader, clipboard paste
- **External calls:** via APP-26/27
- **Status:** shipped
- **Web port:** client-only UI + api (see APP-26)

### UI-99 · i18n engine and language detection
- **What:** In-house, dependency-free: `import.meta.glob('./messages/*.ts')` merges 30 namespace files into `ns.key` tables for `it` (default) and `en`. Initial language: saved choice → `navigator.language` (it*/en*) → `it`; fallback lang → en → it → key; `{var}` interpolation, `{one,other}` plurals, `<html lang>` sync, `useT(ns)` plus pure `translate()`.
- **Entry points:** `src/i18n/index.tsx:37-169`; `src/i18n/messages/*.ts` (activity, addBookmark, addSite, aiOnboarding, aiQueue, aiSearch, aiTags, aiWebsites, app, browser, chip, collectionModal, common, dictation, disclaimer, downloads, errors, feedback, filterBar, filterDrawer, gallery, importFolder, importModal, language, lightbox, postCard, postModal, remoteAi, settings, sidebar)
- **Data:** localStorage `app:language`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable as-is.

### UI-100 · Locale-aware formatting
- **What:** `localeTag` maps the language to it-IT / en-US for dates (card date, modal date-time, disclaimer acceptance, dev bar). Counts use `toLocaleString()` with the OS default locale in Sidebar/FilterBar (inconsistent).
- **Entry points:** `src/i18n/index.tsx:92-95`; `src/components/PostCard.tsx:313-322`; `src/components/postmodal/helpers.ts:49-60`; `src/components/Sidebar.tsx:192-195`
- **Data:** none
- **Local deps:** Intl
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-101 · Markdown export and copy links (AI result lists)
- **What:** Tags Explorer and AI Search results offer "Export Markdown" (escaped `- [desc](<url>) #tags` bullets downloaded as `shelfy-aitags|aisearch-<ts>.md`) and "Copy links" (newline-joined permalinks to the clipboard, status toast). Not offered in the Gallery.
- **Entry points:** `src/lib/exportMarkdown.ts:8-63`; `src/views/AiTags.tsx:745,756`; `src/views/AiSearch.tsx:863,874`
- **Data:** result posts
- **Local deps:** `navigator.clipboard`
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable. Consider adding it to Gallery bulk actions.

### UI-102 · Duration and ETA formatting
- **What:** Compact durations (`42s`, `3m 5s`, `2h 10m`) and "≈ …" ETAs. Used for the Activity ETA and the AI queue stats.
- **Entry points:** `src/lib/duration.ts:4-20`; `src/components/ActivityCenter.tsx:240`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-103 · Off-thread image preview worker (ref. area 4)
- **What:** A dedicated Worker downsizes images with `createImageBitmap` and `OffscreenCanvas` to WebP (q 0.82, longest edge 768 px) and transfers the bytes zero-copy. The main thread falls back for SVG. It is used when adding manual bookmarks.
- **Entry points:** `src/lib/imagePreview.worker.ts:8-66`; consumer `src/lib/bookmarkFiles.ts:124`
- **Data:** none
- **Local deps:** Web Worker, OffscreenCanvas
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable for client-side upload previews.

### UI-104 · Anchored portal popover
- **What:** Portal at `<body>` with fixed positioning from the anchor rect; flips vertically and horizontally, clamps to the viewport, scrolls past max-height and re-places on scroll, resize and Resize/IntersectionObserver events. Closes on outside mousedown, Esc or anchor unmount; the `hoverBridge` option is unused.
- **Entry points:** `src/components/Popover.tsx:47-234`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped (the `hoverBridge` option is dead)
- **Web port:** client-only. Reusable.

### UI-105 · Chip component (ref. area 3)
- **What:** A pill with violet (tags) or sky (keywords) tone, an optional count, color dot, click and remove ×. Used by Tags Explorer and AI Search.
- **Entry points:** `src/components/Chip.tsx:53-103`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-106 · Transient toast hook
- **What:** `showToast` shows a message for 3 s, then sets a 200 ms `toastClosing` flag for the exit animation. Used by Gallery, Tags Explorer, AI Search, AI queue and Downloads.
- **Entry points:** `src/hooks/useToast.ts:8-46`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-107 · Design tokens, motion system and reduced motion
- **What:** CSS variables for colors (accent `#7b5cff`), easings, durations (120/200/320/440 ms) and a type scale; `u-*` entrance/press/progress/skeleton utilities; a global `prefers-reduced-motion` kill-switch; Space Grotesk display font; dark theme only. Tailwind has no plugins, so `scrollbar-*` classes are inert (a global WebKit scrollbar style applies instead); `.u-grid-zoom` is unused.
- **Entry points:** `src/index.css:5-71,92-118,400-875`; `tailwind.config.ts:1-7`
- **Data:** none
- **Local deps:** bundled `src/assets/fonts/space-grotesk-variable.woff2`
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only. Reusable; Firefox needs scrollbar styling.

### UI-108 · Text-selection policy
- **What:** App chrome is not selectable (`user-select:none` on `body`). Inputs and `.select-text` areas (the post modal) are selectable. Images use `draggable=false` to block native image drag.
- **Entry points:** `src/index.css:889-907`; `src/components/PostModal.tsx:362`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only

### UI-109 · Brand and platform icon set
- **What:** The SHELFY bookmark logo SVG. A single glyph source (`SourceIcon`) for IG, X, Pinterest, Web and Bookmark, plus `PLATFORM_COLORS`/`PLATFORM_LABELS`. A custom Pinterest glyph (lucide lacks one).
- **Entry points:** `src/components/Logo.tsx:8-38`; `src/components/SourceIcon.tsx:12-93`; `src/components/PinterestIcon.tsx:9-30`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only (also feeds PWA icons)

### Keyboard and pointer shortcuts (enumerated)

| Shortcut | Context | Action | Source |
| --- | --- | --- | --- |
| ⌘ (mac) / Ctrl + `=`, `+`, numpad + | Global while a GridSizeControl is mounted (Gallery, AI Search, Tags Explorer); overrides native zoom | Bigger cards (−1 density step) | `src/hooks/useGridSize.ts:59-85` |
| ⌘ / Ctrl + `-`, `_`, numpad − | Same | Smaller cards (+1 step) | same |
| Enter / Space | Focused post card | Open modal; toggles selection in select mode | `src/components/PostCard.tsx:505-510` |
| Shift + Enter / Space | Focused card in select mode | Range-select from the anchor | same + `src/views/Gallery.tsx:624-626` |
| Enter / Space | Hover quick-select checkbox | Enter select mode with that post | `src/components/PostCard.tsx:519-525` |
| Shift + click | Card in select mode | Additive range select | `src/hooks/useRangeSelect.ts:73-83` |
| Press + sweep (mouse) | Grid in select mode | Drag-select or deselect a range | `src/views/Gallery.tsx:505-575` |
| Drag / wheel / two-finger swipe | Canvas | Pan (drag has inertia) | `src/components/InfiniteCanvas.tsx:389-527` |
| Pinch / ⌘/Ctrl + wheel | Canvas | Zoom at cursor (0.45–2.6×) | `src/components/InfiniteCanvas.tsx:506-511` |
| Esc | Post modal | Close (confirms if AI edits are unsaved) | `src/components/PostModal.tsx:157-176` |
| ← / → | Post modal | Previous/next slide, then previous/next post | same |
| Enter / Space | Modal image | Open lightbox | `src/components/PostModal.tsx:150-155` |
| Tab / Shift+Tab | Post modal, feedback modal | Focus trap cycling | `src/components/PostModal.tsx:196-223`; `src/components/FeedbackModal.tsx:262-279` |
| Esc / ← / → | Lightbox (capture phase) | Close lightbox only / previous / next (wraps) | `src/components/ImageLightbox.tsx:51-75` |
| Esc | Any Popover (bulk actions, activity, assign) | Close popover | `src/components/Popover.tsx:174-176` |
| Esc / Enter | CollectionModal | Close / save (name field) | `src/components/CollectionModal.tsx:98-104,204-206` |
| ↓ → / ↑ ← | CollectionModal delete radios | Switch label-only vs label+posts | `src/components/CollectionModal.tsx:301-329` |
| Esc | FeedbackModal | Close confirm overlay, else request close | `src/components/FeedbackModal.tsx:314-324` |
| ⌘/Ctrl + V (image) | Feedback textarea | Attach pasted screenshot | `src/components/FeedbackModal.tsx:294-303` |
| Esc | Disclaimer review mode (the gate itself is not dismissible) | Close | `src/components/DisclaimerGate.tsx:52-59` |
| Enter / Space | Settings model row | Activate a downloaded model | `src/views/Settings.tsx:621-626` |
| Edit-menu roles (undo, redo, cut, copy, paste, select all) | Whole app | Native editing accelerators | `electron/main.ts:265-276` |
| ⌥⌘I / Ctrl+Shift+I | Dev builds only | Toggle DevTools | `electron/main.ts:278-289` |

Not implemented: Esc to exit select mode, ⌘A select-all, a search-focus key, view-switch keys.

## Settings inventory

Areas: 1 = library data, 2 = sources and downloads, 3 = AI, 4 = websites and imports, 5 = shell, UI and platform.

| # | Section (UI) | Setting / key | Type | Default | Effect | Persistence | Area |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 1 | Language | `app:language` | `it` / `en` | saved → `navigator.language` (it*/en*) → `it` | UI language and date locale | localStorage | 5 |
| 2 | AI | AI search suggestions `aiSearchSuggestions` | bool (`'true'`/`'false'`) | false | Gallery suggested-tag chips via `search:suggest` | localStorage | 3 |
| 3 | AI › Remote providers | `providers[]` {id, name, baseUrl, model, apiKeyEnv, apiKeyPiProvider, vision} | list | `[]` (Pi-agent providers auto-listed) | OpenAI-compatible endpoints | `userData/ai-providers.json` (mode 0600) | 3 |
| 4 | AI › Remote providers | API key per `apiKeyEnv` (`SHELFY_AI_*`, `ORNITH_API_KEY`) | secret (≤4096 chars) | unset | Provider auth | macOS Keychain (`security`); other OSes: env var only (saving throws) | 3 |
| 5 | AI › Remote providers | Default chat/search provider `searchProvider` | `local` / `custom:<id>` | `local` | Routes AI chat and search | `ai-providers.json` | 3 |
| 6 | AI › Remote providers | Vision provider `visionProvider` | `''` / `custom:<id>` (vision-capable only) | `''` (local VLM) | Routes image analysis | `ai-providers.json` | 3 |
| 7 | AI › Analysis model | `modelId` | preset id | `qwen3vl-4b` | Local VLM weights | `userData/ai-model.json` | 3 |
| 8 | AI › Parallel classifications | `concurrency` | int 1–10 | 1 | Analysis slots / queue width | `ai-model.json` | 3 |
| 9 | AI › Voice transcription model | `modelId` | preset id | `whisper-small` | Dictation STT | `userData/stt-model.json` | 3 |
| 10 | AI › Embedding model | (none) | fixed | `e5-small` | Tag-clustering embeddings | not persisted (single model; `setModel` is a no-op) | 3 |
| 11 | AI › Performance | `tuning.gpuLayers` | `auto` / int (UI: 99 all layers, 0 CPU only; clamp 0–999) | `auto` | llama `-ngl` | `ai-model.json` | 3 |
| 12 | AI › Performance | `tuning.threads` | `auto` / 1–64 | `auto` | llama `-t` | `ai-model.json` | 3 |
| 13 | AI › Performance | `tuning.ubatch` | `auto` / 512, 1024, 2048 (clamp 64–4096) | `auto` | llama `-ub` | `ai-model.json` | 3 |
| 14 | AI › Performance | `tuning.kvCache` | `auto` / `f16` / `q8_0` | `auto` | llama KV cache type | `ai-model.json` | 3 |
| 15 | AI › Performance | `tuning.threadsBatch` | `auto` / 1–64 | `auto` | llama `--threads-batch`; no UI control, only "reset to auto" writes it | `ai-model.json` | 3 |
| 16 | AI › Performance | Transcription threads `threads` | `auto` / 1–64 | `auto` | whisper `-t` (respawns) | `stt-model.json` | 3 |
| 17 | AI › Performance | Auto/Custom mode | UI state | derived from overrides | Locks or unlocks overrides; Auto resets them all | not persisted | 3 |
| 18 | Downloads and data | Asset types `download:assetTypes` {thumbnail, image, video} | bool map | all true | Which assets bulk/queue downloads fetch | localStorage | 2 |
| 19 | Downloads and data | Export platforms (modal checkboxes) | Instagram / X / Pinterest | all checked | Scope of the JSON export | not persisted | 1 |
| 20 | Updates | Update channel `update-channel.json` {channel} | `stable` / `beta` | `stable` | Feed: latest vs rolling beta tag | userData JSON | 5 |
| 21 | Updates › Runtime components | `llama-variant.json` {variant, explicit, failed[]} | `cpu` / `cuda` / `vulkan` / `metal` | hardware-recommended (failed GPU → cpu) | Which llama build is provisioned | userData JSON | 3 |
| 22 | Legal | Disclaimer acceptance `app:disclaimerAcceptance` {version, acceptedAt, dontShowAgain} | record | none (gate shown); "don't show again" pre-ticked | First-run gate; re-prompts when `DISCLAIMER_VERSION` changes | localStorage | 5 |
| 23 | Gallery toolbar | View mode `galleryViewMode` | `grid` / `canvas` | `grid` | Gallery layout | localStorage | 5 |
| 24 | Gallery / AI Search / Tags toolbar | Grid density `gridSizeStep` | int −3…+4 | 0 | Column offset and canvas tile size | localStorage | 5 |
| 25 | Gallery toolbar | Sort order `filters.sortOrder` | `newest` / `oldest` | `newest` | Date order | in-memory (resets on restart) | 5 |
| 26 | Gallery drawer/toolbar | Filters (mediaType, downloadStatus, aiTagged, search, tag, concepts, conceptMode) | strings / array | `all` / `''` / `or` | Query scope | in-memory | 5 |
| 27 | Sidebar | Group expansion `shelfy.sidebar.expandedGroups` {browser, bookmarks, ai, allposts} | bools | all true | Collapse sidebar groups | localStorage | 5 |
| 28 | Sidebar | Platform folder expansion `shelfy.sidebar.expandedPlatforms` | map id → bool | `{}` (expanded) | Collapse nested folders | localStorage | 5 |
| 29 | Post modal | Video mute `postModal:videoMuted` | `'true'` / `'false'` | muted | Initial mute of modal videos | localStorage | 5 |
| 30 | Browser (area 2) | Instagram saved URL `ig-saved-url` | URL (sanitized) | connector default | Reopens the last IG saved folder | localStorage | 2 |
| 31 | Browser (area 2) | Pinterest board URL `pin-board-url` | URL (sanitized) | connector default | Reopens the last board | localStorage | 2 |
| 32 | App shell | AI onboarding "skip for now" | ref flag | false | Mutes the AI gate for the session | in-memory | 3 |
| 33 | Activity | Session log / update dismiss | arrays / keys | empty | History and "Later" | in-memory | 5 |
| 34 | (env) Feedback | `SHELFY_FEEDBACK_RELAY_URL` | URL | built-in `*.workers.dev` relay; `''` disables it | Relay endpoint | env var | 5 |
| 35 | (env) Feedback | `SHELFY_RESEND_API_KEY` | secret | unset | Direct Resend sending (dev, relay off) | env var | 5 |
| 36 | (Worker) Feedback relay | `FROM`, `TO`, `DAILY_CAP`; secret `RESEND_API_KEY`; ratelimit `PER_IP`; KV `BUDGET` | vars / secret / bindings | `onboarding@resend.dev`, developer inbox, 90/day, 3 per 60 s | Relay behavior | `workers/feedback/wrangler.jsonc` + Cloudflare secrets | 5 |
| 37 | (build) Updates | Feed URL `resources/app-update.yml` (from package.json `build.publish`) | HTTPS URL | `github.com/niccolofanton/shelfy/releases/latest/download` | Updater feed base | packaged resource | 5 |
| 38 | (env) Dev | `ELECTRON_DEV` | `'true'` | unset | Dev renderer URL, DevTools, relaxed CSP, updater off | env var | 5 |
| 39 | (env) Test | `SHELFY_TEST_USER_DATA` | path | unset | Redirects `userData` (unpackaged only) | env var | 5 |
| 40 | (env) Perf/test | `SHELFY_THUMB_NO_CACHE`, `PERF_NO_PREWARM`, `PLAYWRIGHT_E2E` | `'1'` | unset | No-store assets, skip warm-ups, no DevTools | env var | 5 (+1) |
| 41 | (env) Spike | `SHELFY_CAPTURE_MVP` | `'1'` | unset | CDP image-capture spike on webviews | env var | 2 |

## Web-migration risks & notes

### Top risks (ranked)

1. **The bridge is the only data path.**
   - Every read, write and push goes through `window.electronAPI`:
     - in scope: 14 files, 82 members
     - app-wide: 39 files, 162 of 173 bridged members (11 bridged members are already unused, e.g. `removePostFromCollection`, `getTaxonomy`, `pauseWeb`)
   - Most calls assume the bridge exists and would throw without it (e.g. `src/App.tsx:381`, `src/hooks/usePosts.ts:224`). Only a few guard with explicit checks or optional chaining: the window chrome (`src/App.tsx:132-133`, `src/views/Gallery.tsx:163-166`, `src/components/Sidebar.tsx:27`), `useActivity`, `FeedbackModal`, `ImageLightbox` and the PostCard repair call.
   - Plan:
     - Keep `types/electron-api.d.ts` (`ElectronAPI`, line 229) as the contract.
     - Implement a `WebApi` with fetch for `invoke` calls and SSE/WebSocket for `on*` channels.
     - Inject it as `window.electronAPI`, or better, as a context.
     - Desktop-only members become feature-flagged no-ops.
2. **Media addressing and gallery performance with remote media.**
   - **Media addressing:** post rows store absolute local paths that are rendered as `asset://media/<path>`:
     - thumbnail/image/video/preview paths
     - `media.localPath`
     - web chunk `screenshotPath`

     Moving to the web requires these changes:
     - storage keys plus signed CDN URLs (area 1 data-model change)
     - 640 px resized tiles plus a `srcset` per density step
     - blur data URIs computed at ingest
     - Range-capable video for hover previews

     The expiring signed IG/X/Pinterest CDN URLs (`thumbnailUrl`) may also be hotlink/CORP-blocked from a web origin, so they must be re-hosted.
   - **Gallery performance at scale:**
     - The loaded `posts` array only grows: 50, then +250 per page, never trimmed; the canvas pool is 500.
     - Images load eagerly with overscan 6 rows (up to ~10 columns, so ~60 images per side).
     - "Select all" materializes up to 100k ids in the client and sends them in one request.

     For the web:
     - cursor pagination with a windowed or evicting cache
     - lower overscan plus fetch priority hints
     - server-side "bulk action by query" endpoints
3. **No identity, tenancy or session.** Settings, disclaimer consent, activity history, feedback and folders are all per-device and single-user.
   - The web needs:
     - auth on every endpoint and per-user scoping
     - server-side consent records
     - per-user rate limits (the feedback Worker is currently anonymous)
     - encrypted per-user AI provider keys (today: macOS Keychain, `electron/ai-providers.ts:360-397`)
4. **Background work and the Activity Center depend on in-process managers.**
   - The web needs a server job queue with per-user realtime push (SSE/WS) and pause/resume/cancel/retry APIs.
   - The model, binaries, STT and update activity kinds disappear.
   - Sync and save run in the Electron `<webview>` (area 2) and cannot run in a browser tab.
   - The session log should become persisted notifications for multi-device users.
   - The native update notification (APP-25) is the template for Web Notifications/Push on job completion.
5. **Desktop-only surfaces to drop or replace.** Drop or replace:
   - window controls and drag regions
   - the app menu
   - the whole updater (APP-20..25)
   - runtime binaries, hardware probe, performance tuning and local model pickers
   - show-in-folder and open-path
   - the `<webview>` live-page fallback in the post modal (IG/X refuse framing; use "Open original" or oEmbed)
   - the mic permission handler
   - the `asset://` protocol
   - session-level CSP

### Security re-baseline

- Header CSP is likely not applied to the packaged `file://` renderer (APP-12), and there is no meta CSP. For the web, serve a strict header CSP:
  - explicit CDN `img-src`/`media-src` instead of `https:`
  - `connect-src` limited to the API
  - `frame-ancestors 'none'`
- Favicons are fetched directly from each saved domain (UI-66), which leaks the library's domains and needs a broad `img-src`. Store them at capture time instead.
- Feedback relay: there is no auth or CORS. Attachments are only checked for base64 charset, not MIME; the 120-char filename passes through to Resend. Add MIME checks, auth or Turnstile, and per-user limits.
- The Windows self-update runs a downloaded PowerShell build with `-ExecutionPolicy Bypass`, trusting a same-feed sha512 rather than a signature. It is dropped on the web, but worth noting for the desktop.
- There is no single-instance lock, so two desktop instances can open the same SQLite DB and both run `recover()`. This becomes moot with a server DB.

### Mobile, offline and PWA

- The layout assumes desktop: a fixed 240 px sidebar, a 900 px minimum window, a 300 px search pill, and a two-column modal with a 380 px meta column. It needs a responsive shell: drawer sidebar, bottom navigation and a stacked modal.
- Several interactions are hover- or mouse-only: the info overlay, quick-select, video preview and slideshow are hover-only; drag-select uses mouse events; the canvas has no multi-touch pinch. Add long-press selection, tap-to-preview and pointer-based pinch.
- PWA plan:
  - a manifest with icons from `Logo`
  - a service worker caching the app shell and thumbnails; offline browsing of recent pages via IndexedDB is optional
  - a Web Share Target (`share_target`) so shared IG/X/Pinterest/web URLs go into ingestion (areas 2/4)
  - Web Push for job completion

### i18n and copy

- The i18n engine and its 30 namespaces are reusable as-is; `import.meta.glob` works in any Vite web build.
- Hard-coded strings to externalize or drop:
  - updater notification and DMG dialog (Italian)
  - feedback main and Worker errors (Italian)
  - startup fatal dialogs (English)
  - WindowControls labels (English)
  - `index.html` `lang="it"` (corrected at runtime)
- The server should return error codes, not prose.
- Some copy is desktop-specific, e.g. the empty-state "capture posts via the Browser tab" and the "media not downloaded" wording.

### Persistence mapping

- Keep per-device in localStorage:
  - view mode, grid density
  - sidebar expansion
  - video mute
- Promote to account preferences:
  - language
  - asset types
  - AI suggestions
  - disclaimer consent (server record)
- Drop or replace the `userData` JSON settings:
  - update channel (drop)
  - AI model, tuning and variant (replaced by area 3 provider settings)

### Destructive and data operations

- These become authenticated, audited account operations with re-auth or confirmation and preferably soft-delete or undo:
  - the danger zone (UI-95..97)
  - bulk and single delete (UI-56, UI-80)
  - folder delete with posts (UI-17)
  - "delete local files" (UI-79), which becomes "remove stored copy" and affects the storage quota
- JSON export moves from a native save dialog to a streamed download or an async export job.
- Markdown export (UI-101) already works in the browser.

### Correctness and debt found while indexing

- Gallery filter dimensions `source`, `category` and `contentType` are wired to the DB but no UI sets them; only `tag` is applied, from the modal.
- The media-type filter can't isolate `images`, `text`, `website` or `file` posts (UI-35).
- FilterDrawer hides folders whose `platform` isn't one of its four rows, while the sidebar shows them.
- ActivityCenter passes `postId` for web items but `App.handleNavigate` ignores it.
- `electron-updater` is a declared dependency but unused, since the updater is custom.
- docs/architecture.md says the updater polls every 60 s; the code polls hourly.
- Dead pieces:
  - `Popover.hoverBridge`
  - the `.u-grid-zoom` CSS class
  - Tailwind `scrollbar-*` classes (no plugin installed)
  - `Sidebar.onClearAlert` prop
- The renderer has no React error boundary; a render crash leaves a blank window with only a log line.
- On macOS the menu contains only Edit roles, so there are likely no Quit/Close/Minimize menu accelerators (verify on device).

### Component reuse matrix

- **Reusable as-is** (no `electronAPI`):
  - components: FilterBar, FilterDrawer, GridSizeControl, PostGridSkeleton, VirtualPostGrid, InfiniteCanvas (add touch pinch), Popover, Chip, CollectionModal, LanguageCard, Logo, PinterestIcon, SourceIcon, postmodal/CollectionsMenu
  - hooks: useGridSize, useViewMode, useRangeSelect, useToast
  - lib: exportMarkdown, duration, imagePreview.worker
  - i18n/*, index.css, tailwind.config
  - DisclaimerGate and disclaimer.ts: swap the storage for a server record
  - postmodal/helpers: swap `assetUrl` for a CDN URL builder
- **Reusable behind an API adapter:**
  - App, Gallery
  - PostCard (`repairPreview`, URL builder)
  - PostModal, postmodal/ActionsMenu (drop the OS actions), postmodal/MetaColumn (drop open-path links), postmodal/MediaCarousel (remove `<webview>`)
  - ImageLightbox (`openExternal` → `window.open`)
  - FeedbackModal (`sendFeedback` → fetch)
  - ActivityCenter and useActivity (SSE; drop desktop kinds)
  - RemoteAiBanner (area 3)
  - Settings (heavy rework: keep Language, Asset types, Import/Export, Danger zone and Legal; replace the AI cards; drop Updates, Runtime and Performance)
- **Drop:**
  - WindowControls
  - the platform-specific chrome (Sidebar mac drag strip, Gallery 144 px inset — already inert without the bridge)
  - the dev build bar (optional)
