# Websites capture, bookmarks & imports — feature index
> Scope: `electron/{weborchestrator,webcapture,webcapture-playwright,capture-engine,adblock,net-safety,web-enrich,bookmarks}.ts`, web/bookmark/import parts of `electron/{ipc,preload,db,main,jobstore}.ts`, `src/views/AiWebsites.tsx`, `src/components/{AddSiteModal,AddBookmarkModal,ImportModal,ImportFolderModal}.tsx`, `src/components/postmodal/WebMetaPanel.tsx`, `src/lib/{bookmarkFiles,imagePreview.worker}.ts`, `src/hooks/useWebJobs.ts`, `build/prepare-playwright.ts`, `scripts/{web-capture-eval,smoke-web*.ts,import-ai.ts}`, related tests. Snapshot: working tree of `dev`, 2026-10-02.

## Overview
- **Website references**: the user pastes ONE URL → `weborchestrator` creates a placeholder post (`platform='web'`, `media_type='website'`) immediately and queues a job: discovery (`webcapture.discoverPages`: robots.txt → sitemap → home-link crawl, Node `fetch`) → capture of ≤8 representative pages via `capture-engine` (Playwright **chromium-headless-shell** by default, Electron offscreen `BrowserWindow` as fallback) with injected DOM-prep scripts → ffmpeg WebP bands in `<userData>/assets/web` → `web-enrich` (regex content extraction, palette/fonts/tech via live-page probes, award badges — zero network) → VLM screenshot QC + one re-capture + og:image fallback → `db.upsertWebReference` (posts `web_*` JSON columns; previous capture archived in `web_snapshots`) → hand-off to the AI analyzer (area 3).
- Progress streams on `web:progress` (full job record + "behind the scenes" event timeline) into the Websites panel (`AiWebsites`), the Activity Center and the post modal (`WebMetaPanel`).
- **Imports**: (1) manual bookmark from local files (renderer builds previews incl. pdfjs PDF page 1; main writes bytes to `assets/`; `platform='manual'`); (2) JSON import (own export `{posts,collections}`, bare array, or Chrome-extension IG/X exports) with collections round-trip; (3) destination chooser for in-app Instagram-folder / Pinterest-board imports (sync itself = area 2).
- **Not present (verified by grep)**: batch/list URL add, clipboard paste, browser-bookmarks HTML (Netscape), CSV, platform GDPR archives (IG/TikTok/X), folder-of-media import, PDF capture/generation, HTML/MHTML/WARC archiving (raw HTML is discarded after extraction).

## Features

### WEB-01 · Add a website by URL
- **What:** Modal takes one URL (+ optional max pages 1–8, default 6); client prepends `https://`, requires a dotted host, fires and forgets, then jumps to the Websites view.
- **Entry points:** Sidebar "+ Website" `src/components/Sidebar.tsx:474`, Websites header/empty state `src/views/AiWebsites.tsx:2097,2235` → `AddSiteModal.handleSubmit` `src/components/AddSiteModal.tsx:71` → `addWebReference` `electron/preload.ts:201` → `web:add` `electron/ipc.ts:674` → `enqueueWeb` `electron/weborchestrator.ts:1143`
- **Data:** returns `{id, finalUrl, domain, queued}`; nothing persisted by the modal.
- **Local deps:** Electron IPC
- **External calls:** none at submit (capture is async)
- **Status:** shipped
- **Web port:** client-only + api+db — `POST /sites` that validates and enqueues.

### WEB-02 · Single-page capture mode
- **What:** Checkbox captures only the pasted URL (no discovery, cap=1, source `single-page`); flag persisted in `web_meta_json.singlePage` and replayed on reanalyze when the caller passes `undefined`.
- **Entry points:** `src/components/AddSiteModal.tsx:156` → `web:add {singlePage}` → tri-state resolve `electron/weborchestrator.ts:1196` → `captureWebReference` `electron/weborchestrator.ts:483`; persisted `:969`; read back `electron/db.ts:941` (`webSinglePage`)
- **Data:** `posts.web_meta_json.singlePage`, `jobs.payload.singlePage`
- **Local deps:** SQLite
- **External calls:** the target URL only
- **Status:** shipped
- **Web port:** api+db — job parameter + stored flag.

### WEB-03 · URL validation & SSRF gate at intake
- **What:** `assertSafeUrl` allows only http(s) and rejects literal loopback/private/link-local/metadata hosts (dotted, integer/hex/octal IPv4, IPv4-mapped/compatible IPv6, fc00::/7, fe80::/10, `localhost`, `*.localhost`); no DNS resolution (comment `net-safety.ts:26`), so names resolving to private IPs pass.
- **Entry points:** `enqueueWeb` `electron/weborchestrator.ts:1154`, `discover` `:1403`, `normalizeInputUrl` `electron/webcapture.ts:170`, `capturePage` `electron/webcapture.ts:1696` / `electron/webcapture-playwright.ts:720`, `shell:openExternal` `electron/ipc.ts:1317` → `electron/net-safety.ts:27,87`
- **Data:** none
- **Local deps:** Node `net`
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink — server must resolve+pin IPs or route egress through a filtering proxy; extend ranges (see risks).

### WEB-04 · Placeholder-first card
- **What:** Enqueue upserts a bare `platform='web'` row at once (idempotent by id) so a gallery/Websites card appears, then emits `interceptor:newPosts` (count 1 if new; the renderer never badges `web`).
- **Entry points:** `electron/weborchestrator.ts:1183-1207` → `db.createWebPlaceholder` `electron/db.ts:2483` → emitter `electron/ipc.ts:147` → `src/App.tsx:575`, `src/hooks/usePosts.ts:339`
- **Data:** `posts` (id, platform, post_url, author_username=domain, web_url, web_domain, web_final_url, timestamp)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + realtime-push — insert in the job-create transaction, push "list changed".

### WEB-05 · Deterministic site identity & dedup
- **What:** Post id = `web:` + sha1(normalized PASTED url: lowercase host, no `www.`, no fragment/trailing slash, utm_*/gclid/fbclid/ref removed), fixed across redirects; re-paste while a job is active is a no-op (`queued:false`); after completion it re-captures (new version). http vs https are different ids.
- **Entry points:** `db.webPostId` `electron/db.ts:2311`, `normalizeWebUrl` `electron/db.ts:2277`, dedup `electron/weborchestrator.ts:1159-1172`
- **Data:** `posts.id`, `jobs.key = web:<id>`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — same key, unique per user/tenant.

### WEB-06 · Page discovery (robots → sitemap → home crawl)
- **What:** Resolves the home (redirects → canonical origin; one-shot https→http fallback), reads robots.txt `Sitemap:` lines (else `/sitemap.xml`), parses urlset/sitemapindex (depth ≤2, ≤5 children, `.gz` via async gunzip, ≤200 raw URLs), falls back to regex `href` crawl of the home; source = sitemap|crawl|seed-only. robots `Disallow` is NOT honored.
- **Entry points:** `electron/weborchestrator.ts:499-509` → `webcapture.discoverPages` `electron/webcapture.ts:665` (`fetchRobotsSitemaps` :542, `parseSitemap` :581, `parseSitemapXml` :566, `extractLinks` :633)
- **Data:** in-memory (job `source`, `pagesTotal`, timeline event with the page list)
- **Local deps:** Node fetch, zlib
- **External calls:** target origin `/`, `/robots.txt`, same-origin sitemap URLs
- **Status:** shipped
- **Web port:** background-job — plain HTTP in a queue consumer behind the egress policy; no browser needed.

### WEB-07 · Representative page ranking & per-section dedup
- **What:** Same-origin normalization (asset extensions incl. .pdf dropped, tracking params stripped, query sorted), locale-aware (keeps only the resolved `/xx/` locale), IT/EN keyword buckets (about, work, case-study, pricing, services, contact, blog), ≤2 pages per top-level section, home forced first, cap ≤8.
- **Entry points:** `selectRepresentative` `electron/webcapture.ts:360`, `scorePath` :288, `sectionKey` :337, `normalizeUrl` :182, `PATH_RULES` :251
- **Data:** none (pure)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — pure TS, port verbatim.

### WEB-08 · SSRF-hardened discovery fetcher
- **What:** `fetchText` follows redirects manually (≤5 hops), re-validates every `Location` and the final host, desktop Chrome UA, 10 s timeout, 2 MB body cap, gunzip output cap 16 MB (gzip-bomb guard), abortable.
- **Entry points:** `electron/webcapture.ts:437` (discovery + og:image fallback `:805`)
- **Data:** none
- **Local deps:** Node fetch (undici)
- **External calls:** arbitrary user-supplied origins
- **Status:** shipped
- **Web port:** background-job — reuse logic, add DNS pinning/egress proxy (hop checks alone are insufficient server-side).

### WEB-09 · Standalone discovery preview
- **What:** Returns the ranked page list for a URL without capturing.
- **Entry points:** `discoverWebPages` `electron/preload.ts:211` → `web:discover` `electron/ipc.ts:744` → `discover` `electron/weborchestrator.ts:1399`
- **Data:** none
- **Local deps:** none
- **External calls:** target origin
- **Status:** dead (no renderer caller)
- **Web port:** drop — or reuse as a "preview pages" endpoint.

### WEB-10 · Capture engine selection & fallback
- **What:** Playwright engine loaded lazily and used by default; `SHELFY_CAPTURE_ENGINE=osr|playwright` overrides; missing browser binary → latch OSR for the session; transient launch failure → OSR for that page; per-URL errors propagate (tested).
- **Entry points:** `electron/weborchestrator.ts:561,761` → `electron/capture-engine.ts:139` (`preferred` :95, `isBrowserMissing` :111, `isLaunchFailure` :128, `activeEngine` :182); `tests/electron/capture-engine.test.ts:81`
- **Data:** none
- **Local deps:** Playwright Chromium; Electron BrowserWindow (fallback)
- **External calls:** none
- **Status:** shipped (env override = flag)
- **Web port:** drop (desktop-only) — one server engine.

### WEB-11 · Playwright headless capture session
- **What:** One shared headless Chromium; a fresh context+page per captured page: 1280×900 @1×, Chrome 124 desktop UA, no reducedMotion, HTTPS errors not ignored; GL args per OS (ANGLE Metal / D3D11, SwiftShader on Linux, `--enable-unsafe-swiftshader` fallback); sandbox on unless `SHELFY_DISABLE_SANDBOX=1`; `goto` waitUntil `load` (45 s) + best-effort `networkidle` (20 s); route-level SSRF abort for every request + final-host re-check.
- **Entry points:** `capturePage` `electron/webcapture-playwright.ts:701` (`getBrowser` :363, `LAUNCH_ARGS` :342, `newContext` :725, SSRF route :763, `goto` :822, final host :836-849)
- **Data:** tmp dirs `shelfy-pw-*` in OS tmp
- **Local deps:** Playwright Chromium (playwright-core + headless shell), GPU
- **External calls:** captured pages + all their sub-resources
- **Status:** shipped
- **Web port:** headless-browser — Playwright/Puppeteer Workers binding; Chromium flags (GPU/ANGLE) not settable on managed services.

### WEB-12 · Electron offscreen (OSR) fallback engine
- **What:** Hidden `offscreen:true` sandboxed BrowserWindow on the `persist:social` session (30 fps paint, audio muted); load settles on did-finish-load/did-stop-loading or dom-ready+2 s (≤30 s); 15 s pre-shot settle; tagged `__shelfyCapture` so `will-navigate` lets it leave the app origin; no adblock/preloader wait/filmstrip; redirects and sub-resources NOT SSRF-gated; WebGL often blank.
- **Entry points:** `webcapture.capturePage` `electron/webcapture.ts:1675` (window :1703, `waitForLoad` :1535); bypass `electron/main.ts:421-428`
- **Data:** same files as WEB-26
- **Local deps:** Electron BrowserWindow/debugger, ffmpeg
- **External calls:** captured pages + sub-resources
- **Status:** shipped (fallback only)
- **Web port:** drop (desktop-only).

### WEB-13 · Chromium provisioning & self-heal
- **What:** Build downloads chromium-headless-shell into `build/ms-playwright` (extraResources → `resources/ms-playwright`); runtime prefers the bundle, else `<userData>/ms-playwright`; dev self-heals with `playwright install` (≤10 min); packaged build cannot (RunAsNode fuse off) → OSR.
- **Entry points:** `build/prepare-playwright.ts:75`, `build/postinstall.ts`, `ensureBrowsersPath` `electron/webcapture-playwright.ts:185`, `installChromium` :268
- **Data:** `resources/ms-playwright`, `<userData>/ms-playwright`, `PLAYWRIGHT_BROWSERS_PATH`
- **Local deps:** filesystem, child_process, Playwright CLI
- **External calls:** Playwright browser CDN (build/dev only)
- **Status:** shipped
- **Web port:** drop (desktop-only) — managed service / container image ships the browser.

### WEB-14 · Logged-in capture via social session cookies
- **What:** Captures run with the user's social login: OSR uses `persist:social` directly; Playwright copies ALL its cookies (IG/X/Pinterest/OAuth) into every capture context (sameSite mapped, None→Secure).
- **Entry points:** `cookiesForPlaywright` `electron/webcapture-playwright.ts:445` (applied :736); OSR `electron/webcapture.ts:1698,1708`
- **Data:** Electron cookie jar (`Partitions/social`)
- **Local deps:** Electron session
- **External calls:** cookies sent to their own domains whenever a captured page requests them
- **Status:** shipped
- **Web port:** rethink — no user cookies server-side; drop or add explicit per-site auth (also a security smell today).

### WEB-15 · Ad/tracker/CMP request blocking
- **What:** Playwright route aborts non-document requests to 93 ad/analytics/session-replay/CMP/chat-widget host suffixes, 5 path fragments (`/gtag/js`, `/gtm.js`, `/collect?`, `/pixel?`, `/ga.js`) and the Meta `/tr` pixel; counts blocks. OSR has none.
- **Entry points:** `attachAdblock` `electron/adblock.ts:173` (`BLOCKED_HOST_SUFFIXES` :18, `shouldBlock` :148) ← `electron/webcapture-playwright.ts:745`
- **Data:** none (static list)
- **Local deps:** Playwright
- **External calls:** none (offline list)
- **Status:** shipped
- **Web port:** headless-browser — `page.route` in a binding, or REST `rejectRequestPattern`.

### WEB-16 · Cookie-banner dismissal
- **What:** Clicks 10 known CMP selectors (OneTrust, Cookiebot, Quantcast, Didomi, Usercentrics, Osano…), then IT/EN "accept" labels, hides large fixed consent overlays, re-enables vertical scroll if locked.
- **Entry points:** `JS_DISMISS_COOKIES` `electron/webcapture.ts:1211` (run `:1860`, `electron/webcapture-playwright.ts:860`)
- **Data:** none
- **Local deps:** in-page JS (either engine)
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — `page.evaluate`; accepting consent on the user's behalf needs legal review server-side.

### WEB-17 · Preloader wait & pre-shot settle
- **What:** Polls (500 ms, ≤30 s) for a fixed/absolute full-viewport `*preload*/*loader*/*loading*/*splash*` overlay BEFORE killing animations; then fixed settle (Playwright 4 s, OSR 15 s, 30 s on re-capture).
- **Entry points:** `waitForReady` `electron/webcapture-playwright.ts:518` (`JS_LOADER_VISIBLE` :501), settle :982; OSR `electron/webcapture.ts:1894`
- **Data:** none
- **Local deps:** Playwright / Electron
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — budget 1–3 min per page against session limits.

### WEB-18 · Animation freeze & video frame nudge
- **What:** Injects a style killing CSS animations/transitions/smooth-scroll, disables autoplay and seeks videos ~25% in before pausing (avoids black first frames); Playwright shots use `animations:'disabled'`.
- **Entry points:** `JS_DISABLE_ANIMATIONS` `electron/webcapture.ts:1263` (run `:1865`, `electron/webcapture-playwright.ts:878`)
- **Data:** none
- **Local deps:** in-page JS
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate script verbatim.

### WEB-19 · Virtual smooth-scroll neutralization
- **What:** Kills GSAP ScrollSmoother, un-transforms Locomotive v3 and generic translated tall wrappers, unlocks Lenis, restores native vertical scroll so height is measurable.
- **Entry points:** `JS_NEUTRALIZE_VIRTUAL_SCROLL` `electron/webcapture.ts:1299` (run `:1873`, `electron/webcapture-playwright.ts:961`)
- **Data:** none
- **Local deps:** in-page JS
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate script verbatim.

### WEB-20 · Lazy-load auto-scroll
- **What:** Sets `img.loading=eager`, promotes `data-src/data-srcset`, scrolls in 80%-viewport steps (220 ms, ≤40 iterations, stop on stable height or 12 000 px), returns to top, awaits `document.fonts.ready` and pending `img.decode()` (30 s eval cap).
- **Entry points:** `jsAutoScroll` `electron/webcapture.ts:1392` (run `:1880`, `electron/webcapture-playwright.ts:968`)
- **Data:** none
- **Local deps:** in-page JS
- **External calls:** the page's lazy assets
- **Status:** shipped
- **Web port:** headless-browser — evaluate script verbatim.

### WEB-21 · Scroll-animation end-state forcing
- **What:** After auto-scroll, sets every GSAP ScrollTrigger animation to `progress(1)` and disables triggers without revert; completes active non-looping tweens so below-fold sections aren't shot at opacity 0.
- **Entry points:** `JS_FORCE_SCROLL_ANIM_END` `electron/webcapture.ts:1442` (run `:1885`, `electron/webcapture-playwright.ts:974`)
- **Data:** none
- **Local deps:** in-page JS
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate script verbatim.

### WEB-22 · Fixed/sticky flattening
- **What:** Turns fixed/sticky elements static (keeps full-viewport heroes and canvas holders) and hides small (<160 px) fixed widgets such as custom cursors, so headers aren't repeated down the full-page shot.
- **Entry points:** `JS_NEUTRALIZE_FIXED` `electron/webcapture.ts:1475` (run `:1923`, `electron/webcapture-playwright.ts:997`)
- **Data:** none
- **Local deps:** in-page JS
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate script verbatim.

### WEB-23 · Full-page screenshot
- **What:** Playwright `fullPage` bounded by `documentElement.scrollHeight`; >12 000 px → fullPage+clip to 12 000 (`capped`), fallback viewport-grow. OSR: CDP `Page.captureScreenshot` captureBeyondViewport @1×, fallback viewport slices (80 px overlap) stitched with ffmpeg `vstack`.
- **Entry points:** `electron/webcapture-playwright.ts:1003-1067`; OSR `captureFullPage` `electron/webcapture.ts:1582`, slices `:1961`, `stitchSlices` :1154
- **Data:** tmp PNG
- **Local deps:** Playwright / Electron debugger, ffmpeg
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — `page.screenshot` in the same session as the prep (REST `/screenshot` only without prep).

### WEB-24 · Scroll-jacked WebGL "filmstrip" capture
- **What:** If the document was scroll-locked (read before cookie dismissal) AND a ≥60% fixed/absolute canvas exists without tall DOM, drives `mouse.wheel(0,1500)` and grabs ≤12 viewport frames (950 ms settle), stopping when a 16×16 gray signature differs by <6 (first step static → hero only).
- **Entry points:** `electron/webcapture-playwright.ts:899-957`, `captureScrollJourney` :631, `JS_DOC_LOCKED` :560, `JS_HAS_BIG_FIXED_CANVAS` :572, `frameSignature` :587
- **Data:** frames stored as chunks (WEB-26)
- **Local deps:** Playwright, ffmpeg
- **External calls:** none
- **Status:** shipped (Playwright engine only)
- **Web port:** headless-browser — needs a binding (mouse.wheel); frame diff via wasm instead of ffmpeg.

### WEB-25 · WebGL detection & adaptive parallelism
- **What:** Canvas/WebGL probe sets `webglHeavy`; the home page is captured alone first and, if heavy, remaining pages run 2-up instead of 4-up; OSR forces `invalidate()` + 2 rAF before grabbing.
- **Entry points:** `JS_DETECT_CANVAS` `electron/webcapture.ts:1375`; `electron/webcapture-playwright.ts:884-892`; OSR `electron/webcapture.ts:1933-1943`; `electron/weborchestrator.ts:665-672`
- **Data:** `pageCtx.webglHeavy` (memory)
- **Local deps:** GPU
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — keep as a per-site concurrency hint (browser-session quota instead of GPU).

### WEB-26 · Screenshot encoding, banding & file naming
- **What:** ffmpeg PNG→WebP q82 downscaled to ≤1280 px wide; tall pages cut into ≤2000 px bands (chunk[0] = thumbnail), trailing flat bands (gray stddev <1) trimmed, sizes probed from ffmpeg stderr; files `<captureEpoch>-<host>-<sha256(finalUrl#cap=<tmpdir>[#c<i>|#f<i>|#og])[:16]>.webp` (versions/concurrency never collide).
- **Entry points:** `encodeImage` `electron/webcapture.ts:911`, `encodeImageChunks` :970, `encodeFrames` :1120, `probeImageFlatness` :1053, `probeImageSize` :1192, `screenshotPathForUrl` :756, `resolveFfmpeg` :833
- **Data:** `<userData>/assets/web/*.webp`
- **Local deps:** ffmpeg native binary (bundled / ffmpeg-static / PATH), filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage + rethink — no ffmpeg in Workers: shoot each band with `clip`, convert via image service/wasm; else container.

### WEB-27 · Download suppression during capture
- **What:** Any download the page triggers is cancelled (OSR session `will-download` filtered by webContents; Playwright `page.on('download')`).
- **Entry points:** `electron/webcapture.ts:1747,1838`; `electron/webcapture-playwright.ts:778`
- **Data:** none
- **Local deps:** Electron session / Playwright
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — keep explicit.

### WEB-28 · Live DOM & response-header harvest
- **What:** Grabs post-hydration `outerHTML`, `document.title` and main-document response headers (Playwright `resp.headers()`; OSR session-wide `onHeadersReceived`, clobbered by concurrent OSR captures); context stays alive for `evaluate()` probes until `dispose()`. HTML is not persisted.
- **Entry points:** `electron/webcapture-playwright.ts:822-827,989`; OSR `electron/webcapture.ts:1767,1901`; dispose `electron/weborchestrator.ts:651`
- **Data:** memory only
- **Local deps:** Playwright / Electron webRequest
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — `page.content()` + response headers in the same session (REST `/content` = second render).

### WEB-29 · Content extraction (meta / OG / Twitter / JSON-LD / headings / text)
- **What:** Regex extraction (no DOM lib, HTML capped at 4 MB): title, meta description, `og:*`/`twitter:*` maps, `article:section`, JSON-LD nodes filtered to 14 @types (@graph flattened), h1–h3 (≤12 each), `<html lang>`/og:locale, "readability-lite" mainText (drops script/style/nav/header/footer/aside/svg, prefers main/article/body, ≤2000 chars). No favicon extraction.
- **Entry points:** `extractContent` `electron/web-enrich.ts:622` (called `electron/weborchestrator.ts:607,782`); `extractMainText` :531, `extractJsonLd` :485, `extractMetaTags` :436
- **Data:** `web_pages_json[i].contentText`, `web_pages_json[i].meta.ogImage`
- **Local deps:** none (pure TS)
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — port verbatim (or swap mainText for `/markdown`).

### WEB-30 · Site text aggregation & prompt-injection sanitization
- **What:** Builds one ≤1200-char caption (siteName — title, description, JSON-LD text fields, h1/h2, deduped per-page mainText) through `sanitizeForPrompt` (neutralizes `<<<CAPTION>>>` markers, strips tags/entities, zero-width/bidi/control chars) plus `webMeta` {siteName,title,description,lang,ogImage,pageCount,jsonldTypes,entities}. AI-flag: sanitizer protects the area-3 prompt.
- **Entry points:** `aggregateSiteText` `electron/web-enrich.ts:708`, `sanitizeForPrompt` :397; used `electron/weborchestrator.ts:880,973`
- **Data:** `posts.text` (title+description+caption, ≤20 000), `posts.web_meta_json`, `web_pages_json[0].contentText`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — keep the sanitizer in front of any third-party AI API.

### WEB-31 · Brand palette extraction
- **What:** CSS-first: computed background/text/accent colors of html/body/header/nav/main/section/hero, headings/p, links/buttons weighted by element area → top 8 `{hex, role, weight}` (role background / background-dark if luminance ≤0.55 / text / accent); fallback ffmpeg `palettegen` on the screenshot + built-in PNG decoder (10 s ffmpeg cap). Home page only.
- **Entry points:** `buildWebMetadata` `electron/web-enrich.ts:1557` ← `electron/weborchestrator.ts:588,789`; `extractPalette` :1004, `PALETTE_EVAL_SRC` :979, `paletteFromScreenshot` :1080, `readPngPalette` :1130
- **Data:** `posts.web_palette_json`
- **Local deps:** live page (evaluate), ffmpeg (fallback)
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate in session; wasm quantizer for the fallback.

### WEB-32 · Font detection & provider attribution
- **What:** Computed `font-family` of h1–h3 (heading), p/body (body), code/pre/kbd (mono) skipping generics, plus `document.fonts` families (other); provider google/adobe (link/host presence), system (known list), self (`@font-face`), unknown.
- **Entry points:** `extractFonts` `electron/web-enrich.ts:1334` (`FONTS_EVAL_SRC` :1258, `fontProvider` :1310)
- **Data:** `posts.web_fonts_json` `[{family,usage,provider}]`
- **Local deps:** live page (evaluate)
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — evaluate in session.

### WEB-33 · Tech-stack fingerprinting
- **What:** 19 rules (Next.js, Nuxt, React, Vue, Svelte, Gatsby, Astro, WordPress, Shopify, Webflow, Framer, Wix, Squarespace, GSAP, three.js, Lenis, Vercel, Netlify, Cloudflare) over HTML markers, response headers and runtime `window` globals, plus `<meta name=generator>` slug fallback.
- **Entry points:** `detectTechStack` `electron/web-enrich.ts:1497` (`TECH_RULES` :1381, `TECH_RUNTIME_EVAL_SRC` :1451)
- **Data:** `posts.web_tech_json` (also merged into AI entities, area 3)
- **Local deps:** live page (evaluate)
- **External calls:** none
- **Status:** shipped
- **Web port:** headless-browser — runtime probe needs the session; static rules port verbatim.

### WEB-34 · Award / badge detection
- **What:** Awwwards, CSSDA, FWA, Godly, Land-book, SiteInspire via self-link to a specific entry (slug match 0.9 / single entry 0.7), footer badge image (0.6) or badge script (0.55); text-only off by default; level/date parsed; strong evidence → tags (`award-winning`, `<platform>-<level>`) + entities.
- **Entry points:** `detectAwards` `electron/web-enrich.ts:1715`, `AWARD_DETECTORS` :1575, `awardsToTagsEntities` :1909 ← `electron/weborchestrator.ts:886-895`
- **Data:** `posts.web_awards_json`; `web_meta_json.awardTags/awardEntities` (written, never read)
- **Local deps:** none
- **External calls:** none
- **Status:** shipped (award tags dead-ended)
- **Web port:** background-job — pure function.

### WEB-35 · Screenshot QC + single re-capture (AI)
- **What:** Hero band (plus a middle band when ≥3 chunks) assessed by the vision model in a pool of 3; a flagged page is re-captured once (30 s settle, 180 s budget), home branding re-harvested; fail-open when no model. AI part = area 3.
- **Entry points:** `assessPage` `electron/weborchestrator.ts:710`, loop `:739-850` → `analyzer.assessScreenshot` `electron/analyzer.ts:1475`
- **Data:** in-memory page record; timeline events
- **Local deps:** local VLM or remote provider (area 3), ffmpeg (downscale)
- **External calls:** remote AI provider when configured (area 3)
- **Status:** shipped
- **Web port:** third-party AI API + background-job — vision call per page; re-capture = second browser session.

### WEB-36 · og:image fallback for blank captures
- **What:** If the re-capture is still flagged (or failed, unless only the middle band was bad), downloads `og:image`/`twitter:image` (SSRF-gated, 2 MB cap — larger images are truncated) and encodes it as the page screenshot.
- **Entry points:** `applyOgFallback` `electron/weborchestrator.ts:688` → `fetchImageToWebp` `electron/webcapture.ts:774`
- **Data:** `<userData>/assets/web/<epoch>-…(#og).webp`
- **Local deps:** Node fetch, ffmpeg, filesystem
- **External calls:** og:image host (arbitrary)
- **Status:** shipped
- **Web port:** background-job + object-storage — egress-controlled fetch + image service.

### WEB-37 · Partial-success & failure semantics
- **What:** Failed/timed-out pages are skipped (`partial:true`); zero screenshots → retryable `error` and the placeholder created by this enqueue is deleted; save failure keeps the placeholder; AI enqueue failure never fatal; a cancelled first capture leaves a blank placeholder card.
- **Entry points:** `electron/weborchestrator.ts:633-649,853-862,997-1002,1077-1119`
- **Data:** `posts` delete via `db.deletePosts`; job `error`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — same rules; also clean the placeholder on cancel.

### WEB-38 · Persist reference + archive previous version
- **What:** Archives the current capture into `web_snapshots` (if it has pages), upserts the post (hero = thumbnail/image path, one `post_media` slide per page, searchable `text`), replaces media, force-writes all `web_*` columns; `overwriteAi` gates AI overwrite.
- **Entry points:** `electron/weborchestrator.ts:927-1004` → `db.upsertWebReference` `electron/db.ts:2422` (`webRefToPost` :2338, `archiveCurrentWebSnapshot` :5790)
- **Data:** `posts` (web_url/domain/final_url, web_palette/fonts/tech/awards/pages/meta_json, web_captured_at, thumbnail_path, image_path, text, author_name, timestamp), `post_media`, `web_snapshots`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — same schema with object keys instead of absolute paths.

### WEB-39 · AI analysis hand-off
- **What:** The persisted post is enqueued on the shared analyzer (category/tags/description/entities); the Websites panel renders that analyzer job inline (stepper + streamed JSON). Area 3.
- **Entry points:** `electron/weborchestrator.ts:1010-1021` → `analyzer.enqueuePost`; UI `AiAnalysis` `src/views/AiWebsites.tsx:626`
- **Data:** `posts.ai_*`, `post_tags`, `post_entities` (area 3)
- **Local deps:** area 3
- **External calls:** area 3
- **Status:** shipped
- **Web port:** third-party AI API — emit an AI-job event.

### WEB-40 · Capture queue & orchestration
- **What:** In-memory FIFO, 1 site at a time; home first, other pages via a 4-wide pool (2 if WebGL); phase weights discovering 10% / capturing 55% / extracting 25% / analyzing 10%; per-phase timeouts (discover 15 s, page 150 s, re-capture 180 s, extract 20 s) composed with the job `AbortController` via `AbortSignal.any`; one capture epoch per job.
- **Entry points:** `pumpQueue` `electron/weborchestrator.ts:1127`, `runJob` :1041, `captureWebReference` :448, `runPool` :361, constants :215-262
- **Data:** `jobsMap`/`pendingQueue` (memory) + `jobs` mirror
- **Local deps:** Node timers
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — Queues/Workflows with per-user concurrency and browser-session quota.

### WEB-41 · Live progress & behind-the-scenes timeline
- **What:** Each transition pushes the FULL job record on `web:progress` (status, phase, progress, stage, pages as they land, palette/fonts/tech/awards, ≤250 events read/artifact/branding/awards/write/info/error incl. step timings ≥2 s); renderer coalesces at 100 ms, keeps ≤200 finished; feeds Websites panel, Activity Center and the AI-tab badge. Stage/event/error texts are hard-coded Italian.
- **Entry points:** `setJob`/`pushEvent` `electron/weborchestrator.ts:286,332` → `electron/ipc.ts:142` → `onWebProgress` `electron/preload.ts:303` → `useWebJobs` `src/hooks/useWebJobs.ts:49`; `src/hooks/useActivity.ts:483-505,921-961`; `src/App.tsx:351-367`
- **Data:** memory only (events not persisted)
- **Local deps:** Electron IPC
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push — SSE/WebSocket deltas; optional persisted event log; i18n the texts.

### WEB-42 · Cancel / retry / clear finished jobs
- **What:** Cancel (aborts running work or dequeues), retry (error/cancelled → restart from scratch), clear terminal jobs from the live list; from queue rows, detail header and Activity Center; all web jobs cancelled on "clear all data" and app quit (Playwright browser closed).
- **Entry points:** `QueueRow` `src/views/AiWebsites.tsx:876`, `Detail` :1561-1582, Clear :2134, `src/App.tsx:447-452` → `web:cancel`/`web:retryJob`/`web:clearCompleted` `electron/ipc.ts:688,694,697` → `electron/weborchestrator.ts:1275,1300,1327`; `electron/ipc.ts:251`, `electron/main.ts:803-810`
- **Data:** jobs memory + `jobs` rows
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + background-job — cancellation must reach the remote browser session.

### WEB-43 · Pause/resume all & cancel-all channels
- **What:** `pauseAll` aborts running jobs and re-queues them from scratch, `resumeAll` restarts the pump, `cancelAll`; preload exposes `pauseWeb/resumeWeb/getWebIsPaused/cancelAllWeb`, none called by the renderer (cancelAll only used internally by WEB-42).
- **Entry points:** `electron/preload.ts:204-208` → `electron/ipc.ts:687,691-693` → `electron/weborchestrator.ts:1258,1269,1286`
- **Data:** memory
- **Local deps:** none
- **External calls:** none
- **Status:** dead (no UI caller; backend live)
- **Web port:** drop — or admin queue controls.

### WEB-44 · Job persistence & boot recovery
- **What:** Job records are mirrored (minus events/pages) into `jobs`; at boot non-terminal web jobs restart from discovery (`recovered:true`, overwrite/singlePage kept), stale rows are forgotten after re-enqueue.
- **Entry points:** `jobstore.mirror` `electron/jobstore.ts:80` (HEAVY_KEYS :41) ← `electron/weborchestrator.ts:288`; `recover` `electron/weborchestrator.ts:1341` ← `electron/main.ts:683-689`; `tests/electron/weborchestrator-recover.test.ts:88`
- **Data:** `jobs` (kind='web', key `web:<postId>`, payload JSON)
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** background-job — durable queue makes it native.

### WEB-45 · Websites panel: archive + live queue
- **What:** Merges persisted web posts (`getPosts({platform:'web', search, sortOrder:'oldest', limit:500})`, 250 ms debounce) with live jobs (live wins), pins active jobs, auto-selects running/first, header counters, onboarding empty state, per-site version badge. Only the 500 OLDEST sites are ever listed.
- **Entry points:** `AiWebsites` `src/views/AiWebsites.tsx:1743` (fetch :1799-1821, merge :1835, sort :1856) → `db:getPosts`, `web:snapshotCounts` `electron/ipc.ts:757` → `db.getWebSnapshotCounts` `electron/db.ts:5900`
- **Data:** `posts`, `web_snapshots` (counts)
- **Local deps:** none (renderer)
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only + api+db — paginate instead of the 500 cap.

### WEB-46 · Site detail view
- **What:** Header (favicon, title, external link, palette strip, elapsed, cancel/retry/open reference), phase stepper with error/partial notice, live timeline vs archived layout, artefact sections (480 px screenshot grid, palette roles, fonts usage/provider, tech chips, awards with evidence), lightbox stacking chunk bands, inline AI block (area 3).
- **Entry points:** `Detail` `src/views/AiWebsites.tsx:1286`, `Timeline` :1046, `AnalysisMeta` :1149, lightbox :1699; links → `shell:openExternal` `electron/ipc.ts:1317`
- **Data:** job record / post / snapshot; `asset://…?w=480` thumbnails
- **Local deps:** asset:// protocol (`electron/main.ts:165`)
- **External calls:** favicons from `https://www.google.com/s2/favicons` (`src/views/AiWebsites.tsx:236`); site opened in the system browser
- **Status:** shipped
- **Web port:** client-only — screenshots from CDN; consider self-hosted favicons (privacy).

### WEB-47 · Version history (snapshots)
- **What:** Version bar lists the current capture + archived snapshots (newest first, dated chips); selecting one shows its frozen screenshots/branding/AI; hover × deletes an archived snapshot and unlinks its files. No "restore as current" UI.
- **Entry points:** `VersionBar` `src/views/AiWebsites.tsx:1196`, fetch :1898-1923, `handleDeleteSnapshot` :2034 → `web:getSnapshots` `electron/ipc.ts:752`, `web:deleteSnapshot` `electron/ipc.ts:782` → `db.getWebSnapshots` `electron/db.ts:5869`, `db.deleteWebSnapshot` :5912
- **Data:** `web_snapshots`; `assets/web` files (incl. chunk bands)
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage — delete objects with the row.

### WEB-48 · Multi-select & delete sites (two modes)
- **What:** Select mode with Shift-range; "delete report only" drops the current capture and promotes the newest snapshot (or resets to a re-analyzable placeholder keeping manual tags); "delete completely" cancels the job, unlinks current+snapshot files, deletes rows. Current-capture bands/frames beyond the hero (`#c1..`, `#f1..`) are in neither path list → leak on disk.
- **Entry points:** `src/views/AiWebsites.tsx:1964-2045,2304` → `web:deleteLatestReport` `electron/ipc.ts:799` / `web:deleteSites` `electron/ipc.ts:761` → `db.deleteLatestReport` `electron/db.ts:6041` (`promoteSnapshotToPost` :5939, `clearWebPostToPlaceholder` :6013), `getWebSiteFilePaths` :5926, `getCurrentCaptureFilePaths` :5685
- **Data:** `posts`, `post_media`, `post_tags`, `post_entities`, `web_snapshots`; files
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db + object-storage — derive the object set from `web_pages_json` incl. chunks.

### WEB-49 · Re-capture / reanalyze from the post modal
- **What:** Web post modal offers "Reanalyze" (full re-capture with `overwrite=true`, single-page mode replayed) and "Open in Websites"; re-adding the same URL via WEB-01 also creates a new version (`overwrite=false`).
- **Entry points:** `WebMetaPanel` `src/components/postmodal/WebMetaPanel.tsx:149` → `goReanalyzeWeb` `src/App.tsx:387` (also from AiTags/AiSearch) → `web:add` → `enqueueWeb`
- **Data:** as WEB-38
- **Local deps:** none
- **External calls:** as capture
- **Status:** shipped
- **Web port:** api+db + background-job.

### WEB-50 · Web metadata panel in the post modal
- **What:** For `platform='web'`: Open site, palette swatches that copy hex to clipboard, font chips with usage, tech chips, award chips linking to the award profile.
- **Entry points:** `WebMetaPanel` `src/components/postmodal/WebMetaPanel.tsx:108` ← `src/components/postmodal/MetaColumn.tsx:73` → `shell:openExternal` `electron/ipc.ts:1317`
- **Data:** `posts.web_*` (read)
- **Local deps:** clipboard API
- **External calls:** user-initiated navigation to site / award profile
- **Status:** shipped
- **Web port:** client-only — `window.open(…, 'noopener')`.

### WEB-51 · Delete local files of a web post
- **What:** Generic "delete local files" removes the current capture's files (posts paths + `post_media`), keeps the post and snapshot files; `web_pages_json` still points at the deleted screenshots.
- **Entry points:** `db:deleteLocalFiles` `electron/ipc.ts:299` → `db.getCurrentCaptureFilePaths` `electron/db.ts:5685`, `db.clearPostLocalFiles` `electron/db.ts:6061`
- **Data:** files + path columns
- **Local deps:** filesystem
- **External calls:** none
- **Status:** shipped (generic action, area 5)
- **Web port:** rethink — "free disk" is meaningless server-side; map to "delete capture".

### WEB-52 · Capture debug & evaluation tooling
- **What:** `SHELFY_CAPTURE_DEBUG=1` logs per-step timings (OSR); `eval:capture` offline fixtures (ScrollSmoother/Locomotive/Lenis/WebGL/pinned, `capture.test`→127.0.0.1 via host-resolver-rules), `eval:pw-engine`, `pw-*` probes, `smoke-web(-ai)` end-to-end runs writing to the real DB.
- **Entry points:** `electron/webcapture.ts:1818`; `scripts/web-capture-eval/run.ts`, `scripts/web-capture-eval/pw-engine-test.ts`, `scripts/smoke-web.ts`, `scripts/smoke-web-ai.ts`; `package.json:40-41`
- **Data:** `scripts/web-capture-eval/last-run.json`; test web posts in the DB
- **Local deps:** Electron, ffmpeg
- **External calls:** smoke default `https://nextjs.org`
- **Status:** flag (debug env) / spike (dev tooling)
- **Web port:** drop — rebuild as CI checks against the hosted pipeline.

### WEB-53 · Dead capture helpers
- **What:** No callers: `webcapture.captureMany`, `capture-engine.discoverPages` passthrough (orchestrator calls `webcapture` directly), `web-enrich.awardsQualityBoost` (F9 ranking) and `sanitizeUntrustedText` alias, `db.upsertWebReferences` (batch); `promoteSnapshotToPost` reachable only via delete-report.
- **Entry points:** `electron/webcapture.ts:2062`, `electron/capture-engine.ts:177`, `electron/web-enrich.ts:1940,427`, `electron/db.ts:2470`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** dead
- **Web port:** drop.

### IMP-01 · Manual bookmark from local files
- **What:** Picker or drag&drop of up to 12 files of any type plus a description and manual tags (Enter/comma, case-insensitive dedup, pending draft folded in); creates ONE post (multi-file = carousel), then opens the "All" gallery. No content dedup.
- **Entry points:** Sidebar "+ Manual bookmark" `src/components/Sidebar.tsx:488` → `AddBookmarkModal` `src/components/AddBookmarkModal.tsx:117` (`addFiles` :147, `onDrop` :213, `handleSubmit` :222) → `addManualBookmark` `electron/preload.ts:222` → `bookmark:add` `electron/ipc.ts:702` → `bookmarks.addManualBookmark` `electron/bookmarks.ts:91`
- **Data:** see IMP-04
- **Local deps:** File API (renderer), Electron IPC
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only + object-storage — same UI, direct upload.

### IMP-02 · Client-side file classification & previews (incl. PDF)
- **What:** MIME-then-extension → image/video/pdf/file; ≤768 px WebP preview q0.82: images in a Web Worker (OffscreenCanvas; main-thread fallback incl. SVG), video frame at ~0.1 s (15 s load timeout), PDF page 1 via lazily-imported `pdfjs-dist` + worker (only pdfjs usage in the repo), else a drawn document icon with the extension; any preview failure → icon.
- **Entry points:** `prepareFile` `src/lib/bookmarkFiles.ts:309` (`classifyFile` :61, `imagePreview` :185, `src/lib/imagePreview.worker.ts`, `videoPreview` :195, `pdfPreview` :234, `iconPreview` :262, `loadPdfjs` :16)
- **Data:** in-memory bytes
- **Local deps:** browser canvas/OffscreenCanvas, Web Worker, pdfjs-dist
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only — already browser-native.

### IMP-03 · Upload size limits
- **What:** 12 files (renderer only), 200 MB/file and 500 MB/submission checked in the modal, again in preload before the IPC clone, and in main (declared `size` + real byteLength of original+preview) → `too-large`.
- **Entry points:** `src/lib/bookmarkFiles.ts:34-36`, `src/components/AddBookmarkModal.tsx:152-170`, `electron/preload.ts:222-235`, `electron/ipc.ts:55-56,718-737`
- **Data:** none
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage — presigned-URL policy + per-user quota; enforce file count server-side.

### IMP-04 · Persist manual bookmark
- **What:** Writes originals verbatim to `assets/{images|videos|files}/<slug>-<i>.<ext>` and previews to `assets/thumbnails/<slug>-<i>.webp`; post `manual:<uuid>`, media_type video > carousel > file > image; slides render original (image/video) or preview (pdf/file) with `source_url` = original path; note+tags via `updateUserContent` (user_note, user_tags, `post_tags` tier `manual`, alias-canonicalized); written files unlinked if a write fails before the insert.
- **Entry points:** `electron/bookmarks.ts:91` → `db.addManualBookmark` `electron/db.ts:2741` → `db.updateUserContent` `electron/db.ts:2695`; refresh `interceptor:newPosts {platform:'manual'}` `electron/ipc.ts:739`
- **Data:** `posts` (platform `manual`), `post_media`, `post_tags`, files under `<userData>/assets/`
- **Local deps:** filesystem, SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** object-storage + api+db — create the row after uploads complete; store object keys.

### IMP-05 · Open / reveal / delete a manual bookmark's original
- **What:** Modal reveals/opens the ORIGINAL (slide `source_url`) through the OS shell, confined to userData; full delete also removes copied originals under `assets/`; originals are protected from "clear downloaded assets"; not auto-analyzed (manual AI trigger, area 3).
- **Entry points:** `src/components/PostModal.tsx:262-267`, `src/components/postmodal/ActionsMenu.tsx:278-289`, `src/components/postmodal/MetaColumn.tsx:140` → `shell:openPath`/`shell:showItemInFolder` `electron/ipc.ts:1300,1309`; `electron/db.ts:5706-5715`; `getProtectedAssetPaths` `electron/db.ts:5626`
- **Data:** `post_media.source_url`
- **Local deps:** OS shell, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** rethink — signed download / open in new tab, served from an isolated origin.

### IMP-06 · JSON import (pick file → import)
- **What:** Settings → Data → "Import JSON": native dialog (`.json`); main accepts only the last dialog-picked path (kept for retry, cleared on success); accepts the app's own export `{posts, collections}` (export = area 1), a bare array of posts, or Chrome-extension IG/X exports (`caption`/`text`).
- **Entry points:** `src/views/Settings.tsx:1739,1757` → `ImportModal` `src/components/ImportModal.tsx:20,27` → `openFile`/`importJSON` `electron/preload.ts:277,43` → `dialog:openFile` `electron/ipc.ts:1283`, `db:importJSON` `electron/ipc.ts:178` → `db.importFromJSON` `electron/db.ts:5106`
- **Data:** `_lastPickedImportPath` (memory)
- **Local deps:** Electron dialog, filesystem
- **External calls:** none
- **Status:** shipped
- **Web port:** client-only + background-job — file input/drop, upload, server-side processing.

### IMP-07 · JSON import normalization & dedup
- **What:** Async read then synchronous `JSON.parse` on the main thread; posts with `platform==='instagram'` or a shortcode → `ig-parser`, everything else → `tw-parser`, which hard-codes `platform:'twitter'` (web/manual/pinterest/tiktok records are coerced; `web_*`, `user_*`, local paths dropped); `bulkUpsert` INSERT OR IGNORE by id, meta refreshed only for path-less rows, imported AI applied (overwriteAi when it carries analysis).
- **Entry points:** `normalizeImportedPost` `electron/db.ts:5096`, `importFromJSON` :5106, `bulkUpsert` :2144; `electron/tw-parser.ts:308`, `electron/ig-parser.ts:426`; `tests/electron/db.test.ts:691`
- **Data:** `posts`, `post_media`, `post_tags`, `post_entities`
- **Local deps:** SQLite, filesystem
- **External calls:** none
- **Status:** shipped (lossy round-trip for non-IG/X posts)
- **Web port:** background-job — streaming parse, batched inserts, progress; fix platform mapping.

### IMP-08 · Collections round-trip on import
- **What:** Find-or-create collections by stable key (`x:<externalId>` for IG folders, else `n:<name>` among manual ones), rebuild missing defs from post keys, link posts in one transaction; failure is logged, posts stay imported.
- **Entry points:** `importCollections` `electron/db.ts:5272`, `findOrCreateCollection` :5253, `collectionKey` :5193
- **Data:** `collections`, `post_collections`
- **Local deps:** SQLite
- **External calls:** none
- **Status:** shipped
- **Web port:** api+db — same logic per user.

### IMP-09 · Import feedback & error handling
- **What:** Modal states idle/importing/done/error; shows only the count of NEW posts (`updated`, `collections`, `links` returned but hidden); error message + "try again"; "import another"; no progress or cancel.
- **Entry points:** `src/components/ImportModal.tsx:27-52,121-177`
- **Data:** `Shelfy.ImportResult {imported, updated, collections, links}`
- **Local deps:** none
- **External calls:** none
- **Status:** shipped
- **Web port:** realtime-push — job progress + full summary.

### IMP-10 · Folder/board import destination chooser
- **What:** When importing (full sync or "selected") inside an Instagram saved folder or Pinterest board in the in-app browser: import without tag, into an existing folder-tag (matched by external id) or a new tag (name ≤60, palette/custom color) created with platform/externalId/igName. Sync itself = area 2.
- **Entry points:** `src/views/Browser.tsx:321,395-405,1054` → `ImportFolderModal` `src/components/ImportFolderModal.tsx:39` → `handleFolderConfirm` `src/views/Browser.tsx:345` → `collections:create` + `startSync`/`importSelected` (area 2)
- **Data:** `collections` (platform, external_id, ig_name), `post_collections`
- **Local deps:** Electron webview (area 2)
- **External calls:** area 2
- **Status:** shipped
- **Web port:** rethink — depends on area-2 sync design; chooser UI is client-only.

### IMP-11 · AI-fields-only import (CLI)
- **What:** Dev CLI force-overwrites ai_* fields of local posts matched by id from an export JSON and rebuilds `post_tags` (accepted aliases); `--dry-run`; app must be closed.
- **Entry points:** `scripts/import-ai.ts`
- **Data:** `posts.ai_*`, `post_tags`
- **Local deps:** Electron node + better-sqlite3
- **External calls:** none
- **Status:** spike (dev-only, not in the app)
- **Web port:** drop — or admin endpoint.

## Data touched (area summary)
- **posts** — `platform='web'`, `media_type='website'`: `web_url`, `web_domain` (indexed), `web_final_url`, `web_palette_json` `[{hex,role,weight}]`, `web_fonts_json` `[{family,usage,provider}]`, `web_tech_json`, `web_awards_json` `[{platform,level,date,profileUrl,evidence,confidence}]`, `web_pages_json` `[{url,screenshotPath,chunks:[{screenshotPath,width,height}],contentText,meta:{ogImage}}]`, `web_meta_json` (siteName, title, description, lang, ogImage, pageCount, jsonldTypes, entities, awardTags, awardEntities, singlePage), `web_captured_at` (epoch s), plus `text`, `author_name`, `author_username`, `post_url`, `profile_url`, `thumbnail_url` (og:image), `thumbnail_path`/`image_path` (hero), `timestamp` (= capture time). Migration `electron/db.ts:600-623`.
- **posts** — `platform='manual'`: `media_type`, `thumbnail_path`, `image_path`, `video_path`, `media_count`, `user_note`, `user_tags`.
- **post_media** — web: one slide per page (`source_url` = page URL, `local_path` = hero band); manual: `source_url` = original file path, `local_path` = renderable original/preview.
- **web_snapshots** (`electron/db.ts:646-680`) — archived versions (web_* JSON + frozen ai_* fields), cascade on post delete.
- **jobs** (kind `web`) — resumable mirror; **post_tags** (tier `manual` for bookmarks); **collections / post_collections** (JSON import, folder-tag chooser).
- **Files**: `<userData>/assets/web/*.webp` (screenshots, bands, filmstrip frames, og fallbacks); `<userData>/assets/{images,videos,files,thumbnails}/manual*`; OS tmp `shelfy-web-*`, `shelfy-pw-*`, `shelfy-pwj-*`, `shelfy-og-*`, `shelfy-pal-*`, `shelfy-web-stitch-*`; `resources/ms-playwright` or `<userData>/ms-playwright`.
- **Settings keys**: none. **Env vars**: `SHELFY_CAPTURE_ENGINE`, `SHELFY_CAPTURE_DEBUG`, `SHELFY_DISABLE_SANDBOX`, `PLAYWRIGHT_BROWSERS_PATH`, `FFMPEG_BIN`.
- **IPC**: `web:add|status|isPaused|cancel|cancelAll|pauseAll|resumeAll|retryJob|clearCompleted|discover|getSnapshots|snapshotCounts|deleteSites|deleteSnapshot|deleteLatestReport`, push `web:progress`, `bookmark:add`, `dialog:openFile`, `db:importJSON`, reused `interceptor:newPosts`, `shell:openExternal|openPath|showItemInFolder`, `db:deleteLocalFiles`.

## Background jobs, queues & concurrency
- **Web capture queue** (`weborchestrator`): in-memory FIFO, `WEB_CONCURRENCY=1` site; per site the home page alone, then a pool of 4 pages (2 if WebGL-heavy), each in its own Playwright context inside ONE shared browser; QC pool of 3 + serial re-captures; dozens of short ffmpeg processes per capture (encode, crop per band, size probe, flatness), each SIGKILL-able.
- **Timeouts**: discovery 15 s total (10 s per fetch); page 150 s (goto 45 s + networkidle 20 s + preloader 30 s + autoscroll 30 s + settles + shot 30 s); re-capture 180 s; extract 20 s (sync code, not interruptible); palette ffmpeg 10 s; OSR nav 30 s / eval 15 s / CDP shot 20 s; dev Chromium install 10 min.
- **Retries**: no automatic retry; one QC-driven re-capture per flagged page; og:image fallback; manual retry restarts from scratch; pause = abort + re-queue from scratch; boot recovery re-runs interrupted jobs from discovery.
- **Cancellation**: one `AbortController` per job composed with phase timers (`AbortSignal.any`, listener cap lifted); closes Playwright contexts / destroys OSR windows / kills ffmpeg; cancel-all on library wipe and app quit, then `closeBrowser()`.
- **Hand-offs**: AI analysis + QC run on the analyzer queue (area 3). Manual bookmark and JSON import are single synchronous IPC requests (no queue, no progress, JSON parse blocks the main process).

## External services & endpoints
- Arbitrary user-supplied origins: Node `fetch` of `/`, `/robots.txt`, sitemaps; headless browser loads up to 8 pages + all sub-resources (minus 93 blocked ad/tracker hosts); spoofed desktop Chrome 124 UA.
- og:image / twitter:image hosts (only on QC failure).
- `https://www.google.com/s2/favicons?sz=64&domain=…` hot-linked by the renderer for every listed site (leaks the user's site list to Google).
- Playwright browser CDN (build time and dev self-heal only).
- Remote AI provider for screenshot QC / analysis when configured (area 3).
- System browser via `shell.openExternal` (site, award profile).
- `web-enrich.ts` itself makes **zero** network calls.

## Web-migration risks & notes

**Capture capability → managed headless browser (e.g. Cloudflare Browser Rendering) vs container** (verify current service limits: session length, concurrency, screenshot size, WebGL support):

| Capability | REST endpoints (`/screenshot` `/content` `/markdown` `/links` `/scrape` `/json` `/pdf`) | Playwright/Puppeteer Workers binding | Needs container |
|---|---|---|---|
| Discovery robots/sitemap (WEB-06/07/08) | not needed (`/links` could replace only the home crawl) | — | no — plain fetch in a queue consumer |
| Prep scripts: cookie banners, animation kill, virtual-scroll, autoscroll, GSAP end, fixed flatten (WEB-16…22) | `addScriptTag` only: no ordering, no conditional waits, no return values | yes — `page.evaluate` the existing IIFE strings verbatim | no |
| Preloader poll / networkidle / fixed settles (WEB-17) | only `waitUntil`/`waitForSelector`/timeouts | yes | no |
| Ad blocking (WEB-15) | `rejectRequestPattern`/`rejectResourceTypes` | `page.route` | no |
| Full-page shot ≤12 000 px, 1280@1× (WEB-23) | yes but PNG/JPEG only, without the prep pipeline | yes; shoot 2000 px bands with `clip` to skip cropping | no |
| WebP encode, banding, flat-tail trim, size probe (WEB-26) | — | — | ffmpeg → container; otherwise image service + wasm |
| Filmstrip wheel journey (WEB-24) | no | yes (`mouse.wheel`; diff via wasm) | no |
| GPU-quality WebGL (WEB-11/25) | software GL (verify) | same | GPU container if fidelity must match ANGLE/Metal |
| Post-hydration HTML + main response headers (WEB-28) | `/content` = second render, no headers | `page.content()` + `response.headers()` | no |
| Palette / fonts / runtime tech probes (WEB-31/32/33) | no (`/scrape` can't do area-weighted computed styles) | yes | no |
| Text / meta / JSON-LD / awards (WEB-29/30/34) | `/markdown`, `/scrape`, `/json` are optional replacements | either | no — pure TS |
| PDF of the page | `/pdf` (new capability; none today) | `page.pdf()` | no |
| Logged-in capture (WEB-14) | `cookies` param exists, but no user cookies server-side | `context.addCookies` | rethink |

- **Conclusion**: a REST-only integration covers "URL → screenshot/HTML/markdown" but NOT this pipeline's ordered prep → measure → shoot → probe sequence in one live page; use the Workers Playwright binding (one session per page, 1–3 min wall time, run from Queues/Workflows). Not covered by the managed service: ffmpeg encoding/banding (replace or containerize), GPU WebGL fidelity, Chromium flags, the OSR fallback (drop), QC vision (third-party AI API), logged-in captures.

**SSRF when capture moves to a server**
- Today: literal-host blocklist + per-hop redirect validation (Node fetch) + `context.route` gating of every Playwright request + final-host recheck; the OSR fallback gates only the initial URL. Not covered: hostnames resolving to private IPs / DNS rebinding (acknowledged `net-safety.ts:26`; the eval harness even relies on it), `localhost.` trailing-dot form (likely passes, unverified), ranges 100.64/10, 198.18/15, 192.0.0/24, 224/4, 240/4, NAT64 64:ff9b::/96, 6to4 2002::/16; any port allowed; WebSocket/service-worker traffic vs `context.route` (verify).
- Server-side the blast radius becomes cloud metadata (169.254.169.254) and internal services: resolve DNS and check every A/AAAA, pin the connection to the vetted IP or force all egress (fetch + browser) through a proxy/firewall that drops private ranges; cap redirects/bytes (already done); per-user rate limits and quotas; honest UA + robots `Disallow` policy (today ignored, UA spoofed); abuse handling (scanning, reflection, illegal content landing in your bucket). A managed browser can't reach your VPC, but your Worker/container fetches (discovery, og:image) can → keep the guard there.
- Cookie mirroring must not exist server-side; today every capture context carries the user's IG/X/Pinterest session cookies while running arbitrary third-party JS.

**Imports from a browser**
- Manual bookmarks already use the File API + client-side previews (canvas, OffscreenCanvas worker, pdfjs) → keep; replace the single ≤500 MB IPC payload with direct multipart/resumable uploads (presigned PUT, R2/S3 multipart) of original + preview, then a "finalize" API creating the post; enforce count/size/MIME server-side; serve user uploads (SVG/HTML/PDF) from an isolated origin with `Content-Disposition`. Video previews depend on browser codecs (mkv/avi/HEVC → icon), same as today.
- Folder import doesn't exist today; if wanted: `<input webkitdirectory>` (broad support) or File System Access `showDirectoryPicker` (Chromium-only) + directory drag&drop (`webkitGetAsEntry`), client-side queue, many posts per batch.
- JSON import: drop the native dialog + path allow-list; read via File API, upload, stream-parse in a background job with progress; fix the IG/X-only normalizer; manual/web exports reference machine-local paths → need a bundle format (zip with media) or object keys.

**Local filesystem path reliance**
- Absolute paths are persisted in `posts.thumbnail_path/image_path`, `post_media.local_path`, `post_media.source_url` (manual originals) and INSIDE JSON blobs (`web_pages_json[].screenshotPath`, `chunks[].screenshotPath`, `web_snapshots.web_pages_json`) → data migration must rewrite JSON, not just columns. Also `asset://` + `?w=` thumbnails, `shell.openPath/showItemInFolder`, `confineToUserData`, tmp dirs, ffmpeg/Playwright binary resolution.

**Bugs / surprises found (by reading)**
- Disk leak: current-capture bands/frames beyond the hero are never unlinked by delete-site, delete-report or delete-local-files (`getCurrentCaptureFilePaths` ignores `web_pages_json`; only snapshots' chunks are collected).
- JSON import coerces non-IG posts to `twitter` and drops `web_*`/`user_*` → export→import is lossy for web/manual/pinterest/tiktok.
- Websites panel lists only the 500 oldest web posts.
- `awardTags/awardEntities` written but never read; `awardsQualityBoost`, `captureMany`, `upsertWebReferences`, `web:discover`, pause/resume IPC have no callers.
- OSR fallback: no redirect/sub-resource SSRF gating, no adblock, session-global `onHeadersReceived` shared with concurrent captures and the social webview session.
- Hard-coded Italian job stage/event/error strings; stale comments (web new-posts badge, `docs/architecture.md` still describes OSR as the engine).
- Cancelled first capture leaves a blank placeholder card; http/https variants of one site get different ids; og:image >2 MB is truncated; `bookmark:add` doesn't enforce the 12-file limit or note/tag lengths in main.
