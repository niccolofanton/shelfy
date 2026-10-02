# Shelfy online — implementation plan

> **Status:** adopted plan of record (2026-10-02). It was written by an independent reviewer who had no access to the earlier proposal, and it was adopted after a side-by-side comparison: see [review/plan-comparison.md](review/plan-comparison.md). The superseded proposal is in [review/plan-v1-superseded.md](review/plan-v1-superseded.md).
>
> **Date:** 2026-10-02 · **Scope:** bring Shelfy online as an invite-only, self-hosted web product on the existing Hetzner CX33, with a Rust server core, while the Electron desktop app keeps working and later converges on the same core.
>
> **Inputs:** `docs/web-port/README.md` and `features/01…05` (381 verified features), the `dev` working tree (including the uncommitted capture v2 in `electron/webcap/`), `docs/website-analyzer-audit.md`, the `osn` infra repo (read for conventions only), and aggregate counts from a reference desktop library (Appendix C).
>
> **Fixed by the product owner (treated as requirements):** invite-only; maximize users per VPS; self-hosted on the CX33 (Cloudflare only at the edge, R2 only for backups); Rust server core; capture stays Node/Playwright and reuses the TS code; BYOK AI only, no local models; sync only through a browser extension; media on local disk, videos on demand only; the desktop app stays and converges on the Rust core after the AI phase; every feature ported unless inherently desktop-only; all artifacts in English.

**How to read.** §0 lists every decision with a one-line justification. §2 is the architecture, §3 the operations on the VPS, §4 the desktop-library migration, §5 the roadmap, §6 testing and the capacity model, §7 security and privacy, §8 risks, §9 the spikes, §10 the first two weeks as ordered tasks. `SPIKE-n` marks a choice that must be validated before anything depends on it.

---

## 0. Decisions at a glance

| # | Area | Decision | Why (one line) |
|---|---|---|---|
| D1 | Topology | One Rust binary `shelfy-server` (API, SSE, scheduler, workers) + one Node capture service + one egress proxy | Fewest moving parts on a shared 4-vCPU box; everything except browser rendering lives in one ~100 MB process |
| D2 | Database | SQLite everywhere: one `control.sqlite` plus one `library.sqlite` per user (rusqlite, bundled SQLite with FTS5) | No DB-server RAM, physical tenant isolation, per-user bm25 statistics, and the same engine the desktop already uses (convergence) |
| D3 | Media storage | Per-user content-addressed files on local disk; the DB stores object references, never paths | Immutable URLs cache forever, dedupe is free, refcount GC fixes today's orphan leaks, and a Hetzner Volume can be mounted later without code changes |
| D4 | Media policy | Archive covers and image slides at the platform's bounded source size (IG 1080 px, X `name=large`, Pinterest `1200x`); one 480 px WebP grid rendition; ThumbHash placeholder; videos only on demand into an LRU cache | Disk is the binding capacity limit; the reference library is 75 % video posts (~68 GB if every video were archived) |
| D5 | Media serving | Same origin (`/media/*`), session-cookie auth, `Cache-Control: private, max-age=31536000, immutable`, `Content-Security-Policy: sandbox`, `nosniff` | No cross-user sharing means no cross-user XSS; avoids signed URLs and a second TLS hostname |
| D6 | API | REST + JSON under `/api/v1`, OpenAPI generated from Rust types (utoipa), TS client generated with `openapi-typescript`, keyset pagination, bulk actions by selector | Typed contract for SPA and extension; 155 IPC channels collapse into ~95 endpoints |
| D7 | Realtime | One SSE stream per tab (`GET /api/v1/events`), typed events, 20 s heartbeat, per-user broadcast | Works through Cloudflare Tunnel and nginx with no extra infra; we only need server→client push |
| D8 | Auth | Invite links → passkeys (webauthn-rs) with email magic-link fallback (SMTP via lettre); opaque server-side sessions; scoped, hashed, revocable tokens for the extension, the iOS Shortcut and the migration CLI | No passwords to store or reset; invite-only by construction |
| D9 | Jobs | In-process DB-backed scheduler: `jobs` table in the control DB, per-item work state in the user DB ("drain" jobs), weighted round-robin across users, leases, exponential backoff | Durable and fair without Redis or a broker; survives restarts |
| D10 | Search | FTS5 (`unicode61 remove_diacritics 2`, prefix 2/3) with bm25 column weights mirroring today's weights plus an exact-tag boost; gated by the existing `scripts/search-eval` gold set | Indexed search instead of `LIKE` + JS UDF, without a relevance regression |
| D11 | AI | BYOK provider layer with two wire protocols (OpenAI-compatible, Anthropic Messages); prompts and strict JSON schemas shared as files with the desktop; per-user concurrency and token buckets; keys sealed with XChaCha20-Poly1305 | Two adapters cover OpenAI, Gemini, OpenRouter, Groq, Mistral, Anthropic and custom endpoints; server CPU is ~40 ms per call |
| D12 | Video analysis | Video posts are cataloged from poster + caption by default; frame-based "deep" analysis runs only when the video is already cached | Avoids mass video downloads from a datacenter IP |
| D13 | Sync | Chrome MV3 extension: the existing parsers as a `world: "MAIN"`, `run_at: "document_start"` content script; the sync controller runs in the tab, not in the service worker; incremental stop on already-known items | Reuses `webview-injected.ts` almost verbatim, survives service-worker suspension, and cuts the ban surface |
| D14 | Image fetch | Server-first anonymous CDN fetch through the egress proxy; per-host circuit breaker hands the work to the extension (it fetches and uploads the bytes) | Uses the user's uplink only where the datacenter IP is refused (SPIKE-2) |
| D15 | On-demand video | Fresh direct URL kept by the parser → extension fetch + upload → anonymous yt-dlp (X, Pinterest; IG opt-in) → "open original" | Platform cookies never leave the browser and IG traffic stays off the datacenter IP |
| D16 | Capture | Node 24 service reusing `electron/webcap/*` through an Env shim; NDJSON streaming protocol; one site at a time; Chromium on an `internal: true` network with all egress through the proxy | Reuse as mandated; SSRF solved at the network layer instead of hostname lists |
| D17 | Egress | Smokescreen as the single outbound proxy for capture, media fetch, AI providers and yt-dlp | One SSRF policy (deny private, CGNAT, link-local and metadata ranges after DNS resolution) for every untrusted URL |
| D18 | Mobile | Responsive SPA + PWA; Android Web Share Target; iOS Shortcut with a `links:create` token; shared links hydrated by the server (public X/Pinterest data) or by the extension (IG) | iOS has no Web Share Target, and IG pages are login-walled from datacenters |
| D19 | Web client | Reuse the React renderer behind a `ShelfyClient` interface (HTTP for web, IPC for desktop); `wouter` routes; `vite-plugin-pwa` | 39 files call `window.electronAPI`; one seam keeps both apps building from one UI |
| D20 | Deploy | Images on GHCR; compose services, edge route, DNS, SOPS keys, timers and alerts added to osn through two PRs (P1: API; P4: capture + egress); the osn refactor is done (§3, "Current osn baseline"); no permanent staging on the VPS | Follows osn conventions; staging and capacity tests run on throwaway CX33s |
| D21 | Backups | Hourly consistent DB snapshots (SQLite online backup API) and daily media, restic to R2 under `restic/shelfy`, monthly automated restore drill | RPO 1 h for data, 24 h for media, RTO 2 h, verified rather than assumed |
| D22 | Guardrails | api 768 MiB / 1.5 CPU; capture 1.5 GiB / 1.5 CPU with `cpu_shares: 256` and `oom_score_adj: 600`; proxy 96 MiB | Hermes keeps its 2 GiB / 2 CPU; capture is the first to yield CPU and the first to die under memory pressure |
| D23 | Convergence | After the AI phase: `crates/core` exposed to Electron main through napi-rs; desktop data migrated to the same schema and CAS layout; desktop local llama-server becomes an OpenAI-compatible provider | One domain logic and one schema; moving a library between desktop and web becomes copying one directory |
| D24 | Migration | `shelfy-migrate` CLI (Rust, built on the same legacy reader P6 needs) → bundle (DB + CAS objects) → resumable upload → server install job | Bypasses the lossy JSON export/import (DATA-36) and keeps notes, tags, AI, aliases, clusters and site versions |
| D25 | Repo | Additive monorepo: `crates/*`, `shared/ai/`, `web/`, `extension/`, `capture/`, `deploy/`; the desktop stays at the root untouched until P6 | Zero churn for the desktop build and release flow |

---

## 1. Scope and parity

### 1.1 Parity summary

The feature index counts 381 features (208 direct port, 89 needing platform infra, 30 redesign, 54 drop). This plan keeps every behavior that is not inherently desktop-only.

| Area | Ported behavior (same semantics) | Redesigned for the web | Dropped (desktop-only or dead code) |
|---|---|---|---|
| 1 Data & IPC (58) | filters, sort, collections CRUD, notes, manual tags, stats, deletes, web versions, select-all-matching | storage keys instead of paths, FTS5 search, bulk-by-selector, export/import v2, resets as account operations, SSE refresh, soft delete with 30-day trash | DATA-03 (migration only), 34, 42, 53, 54, 57, 58 |
| 2 Sync & downloads (60) | parsers, sanitizer, merge-never-clobber rules, folder/board → collection, selection overlay, live counters, source-sync planner | every webview concern → MV3 extension; downloads → archive jobs + on-demand video; run history persisted | SYNC-03, 29; DL-13, 26, 29, 30 (SYNC-28 duplicates merged into one parser module) |
| 3 AI (63) | prompts and schemas, tag tiers, aliases, clusters, merge/rename/health, chat search, suggestion chips, manual edits, deterministic fallback | provider layer (BYOK), queue on the job system, dictation as single-shot STT, onboarding = "connect a provider" (AI-47), concurrency = per-user provider concurrency (AI-51) | AI-04, 44, 46, 48–50, 52–54, 56, 61, 62 |
| 4 Websites & imports (64) | discovery, prep scripts, palette/fonts/tech/awards, versions, delete modes, manual bookmarks | capture service + egress proxy, resumable uploads (tus), import/export as jobs | WEB-09, 10, 12, 13, 14 (security), 43, 52 (→ CI), 53; IMP-11 |
| 5 Shell & UI (136) | gallery grid and canvas, density zoom, selection (click, shift, sweep, select-all), post modal, lightbox, activity center, Markdown export and copy links, keyboard shortcuts, i18n, design tokens, feedback | URL routing, responsive and touch shell, PWA, persisted notifications, settings for account/keys/tokens, server-side consent | APP-02…07, 10, 13, 16, 18, 20…25; UI-72, 76, 87, 93, 94 |

### 1.2 Defects fixed by construction (acceptance criteria)

| # | Defect in the index | Fix in this plan |
|---|---|---|
| 1 | JSON import re-labels Pinterest/web/manual as `twitter` and drops notes, manual tags and web fields (DATA-36, IMP-07) | import reads an explicit `platform`; export/import v2 carries every layer; desktop libraries move through the migration CLI, not JSON |
| 2 | IG posts keyed by media id, `pk` or shortcode → duplicates (02 risk 4) | canonical key `ig_<pk>`; shortcodes decoded to `pk`; duplicates merged at migration |
| 3 | `post_tags` PK `(post_id, tag_norm)` shared by AI and manual tiers (01 risk 13) | PK `(post_id, tag_norm, source)` |
| 4 | http/https variants of one site get different ids (WEB-05) | scheme-insensitive URL identity |
| 5 | Bands/frames beyond the hero are never deleted (WEB-48, audit G4) | assets are CAS objects referenced from `web_capture_assets`; refcount GC |
| 6 | Websites view lists only the 500 oldest sites (WEB-45) | cursor pagination |
| 7 | Bulk analysis sends web posts through the social prompt (AI-02) | analysis always loads the full post |
| 8 | `purpose` enum lacks `other` (AI-09) | schema v2 adds it |
| 9 | Chat history unbounded; dictation quadratic, capped at 60 s, Italian only (AI-35, AI-45) | history truncated to 8 turns / 6k tokens; one transcription per recording, ≤120 s, language from the UI |
| 10 | Favicons hot-linked from Google and from each site (WEB-46, UI-66) | favicon stored at capture time; the SPA makes zero third-party requests |
| 11 | Header CSP likely not applied to the packaged renderer; no error boundary (APP-12, APP-01) | real HTTP CSP from the server; React error boundary + client error endpoint |
| 12 | No "remove from collection" UI (DATA-27); `getStats` omits `manual` (DATA-20) | endpoint + UI; stats grouped over all platforms |
| 13 | Media-type filter cannot isolate `images`/`text`/`website`/`file`; drawer hides custom folders (UI-34, UI-35) | full facet list; drawer and sidebar share one source list |
| 14 | Chromium runs without sandbox; captures carry the user's social cookies (audit A1, WEB-14) | sandbox on (SPIKE-4) or a hardened no-sandbox container; no cookies server-side, ever |
| 15 | Cancelled first capture leaves a blank placeholder; og:image >2 MB truncated (WEB-37, WEB-36) | placeholder removed on cancel; oversized og:image rejected, not truncated |
| 16 | `bookmark:add` limits checked only in the renderer (WEB risks) | server enforces count, size, MIME by magic bytes |
| 17 | Danger-zone resets keep collections and every file (DATA-37) | explicit scopes (library / stored media / AI) with an explicit "keep folders" option and a purge job |

### 1.3 Explicitly deferred

Semantic or vector post search (planned path: `sqlite-vec` in the same per-user DB), "similar sites", X bookmark folders, new platforms (TikTok, YouTube, Reddit), public sharing, Firefox and Safari extensions (Firefox after GA), and the desktop acting as a sync client of the server.

---

## 2. Architecture

### 2.1 Topology

```
                     Cloudflare: DNS, TLS, Tunnel (edge only)
                                   │ shelfy.niccolofanton.dev
                          cloudflared (host, osn)
                                   │ http://127.0.0.1:80
                  ┌────────── edge nginx (osn) ──────────┐
                  │ Host routing; SSE unbuffered by       │
                  │ X-Accel-Buffering: no from the app    │
                  └──────────────────┬────────────────────┘
                                     │ :8080 (edge network)
 SPA / PWA (cookie) ──────────► ┌────▼──────────────────┐   :9464 /metrics (internal network)
 MV3 extension (Bearer) ──────► │ shelfy-api (Rust)     │◄── VictoriaMetrics, blackbox
 iOS Shortcut (Bearer) ───────► │ API · SSE · scheduler │
 shelfy-migrate CLI (Bearer) ─► │ media · AI · yt-dlp   │──► /data/shelfy (control DB, user DBs,
                                └──┬─────────────────┬──┘    CAS media, caches, work dirs)
          POST /v1/captures (NDJSON)│                 │ all outbound HTTP via proxy
              shelfy_capture net    │                 │
                ┌───────────────────▼───┐        ┌────▼──────────────┐
                │ shelfy-capture (Node) │───────►│ shelfy-egress     │──► Internet
                │ Playwright + Chromium │ proxy  │ Smokescreen :4750 │    (public IPs only)
                │ internal-only network │        └───────────────────┘
                └───────────────────────┘
```

### 2.2 Components

| Component | Tech | Responsibility | Scaling knob |
|---|---|---|---|
| `shelfy-api` | Rust (axum, tokio, rusqlite) | SPA hosting, REST API, SSE, auth, ingest, search, media serving and processing, job scheduler and workers, AI calls, yt-dlp/ffmpeg orchestration, admin CLI | per-queue concurrency, open-DB cache size, CPU/memory limits |
| `shelfy-capture` | Node 24, playwright-core, chromium-headless-shell, ffmpeg | Website capture (discovery, prep, screenshots, scroll video, page probes, metadata) into a work dir | sites in parallel (default 1), pages per site (default 2) |
| `shelfy-egress` | Smokescreen (pinned commit) | Outbound HTTP(S)/CONNECT proxy that resolves DNS and refuses non-public destinations | none needed |
| MV3 extension | TypeScript, esbuild | Capture saved items in the user's browser, run syncs, selection overlay, upload bytes on request, hydrate shared IG links | pacing flags from server config |
| SPA / PWA | React 18, Vite 5, Tailwind 3 (existing renderer) | All library, AI, websites and settings UI; share target | code splitting, SW caching |
| `shelfy-migrate` | Rust CLI | Desktop library → bundle → upload | resumable, dedupe by hash |
| Desktop (later) | Electron 31 + `crates/napi` | Same domain core, local CAS, webviews kept | — |

### 2.3 Process and concurrency model

- **Runtime.** tokio multi-thread with `worker_threads = 2` and `max_blocking_threads = 16`. Two async workers are enough because the API is I/O-bound and the host has 4 shared vCPUs that Hermes and capture also need.
- **SQLite access.** Every query runs in `spawn_blocking`. Per user, an `Arc<UserDb>` holds one writer connection behind a `tokio::sync::Mutex` and up to 2 reader connections that open lazily and close after 60 s idle. A `moka` cache keeps at most 64 open user DBs with a 10-minute time-to-idle; eviction runs `PRAGMA wal_checkpoint(TRUNCATE)` and closes. Worst case is ~4 MB of SQLite heap per open user (~256 MB at the cap); typical is far lower because idle users are evicted. The control DB keeps 1 writer and 4 readers open permanently.
- **Pragmas.** `journal_mode=WAL`, `synchronous=NORMAL`, `foreign_keys=ON`, `busy_timeout=5000`, `temp_store=MEMORY`, `cache_size=-2000` (writer) / `-1000` (readers), `mmap_size=268435456`. mmap serves hot pages from the kernel page cache, which is reclaimable under the container's memory limit instead of pinned heap.
- **CPU-heavy work.** A dedicated rayon pool of 2 threads runs image decode, resize and encode. Subprocesses are semaphore-gated: ffmpeg ≤2 and yt-dlp ≤2, both started with `nice -n 10`.
- **Outbound HTTP.** One shared `reqwest` client (rustls, HTTP/2, proxy = `shelfy-egress`), at most 64 requests in flight, per-host rate limiters (`governor`).
- **Push.** A per-user `tokio::sync::broadcast` channel (capacity 256) feeds every SSE connection of that user. A lagging receiver gets a `resync` event and the client refetches.
- **Shutdown.** Stop accepting, cancel job tokens, let open transactions finish, checkpoint WAL, exit in ≤25 s (`stop_grace_period: 30s`). Interrupted jobs are re-queued on the next boot.

| Resource | Limit | Where enforced |
|---|---|---|
| HTTP request handler time | 30 s (SSE, chat, uploads exempt) | `tower_http::timeout` |
| Request body | 64 KiB default; 8 MiB ingest; 16 MiB per tus chunk; 25 MiB STT | per-route `RequestBodyLimitLayer` |
| Open user DBs | 64 (time-to-idle 10 min) | moka |
| Image transforms in parallel | 2 | rayon pool |
| ffmpeg / yt-dlp processes | 2 / 2 | semaphores |
| Outbound requests in flight | 64 total; per host see §2.13 | semaphore + governor |
| AI requests in flight | 32 total, 4 per user (1–8 setting) | semaphores |
| Capture sites in flight | 1 (max 2) | dispatcher |

### 2.4 Repository layout and code reuse

```
Cargo.toml                 # [workspace], edition 2024; rust-toolchain.toml pins the current stable
crates/
  core/      sync domain library, no tokio: schema + migrations, repositories, ingest validation and
             merge rules, canonical ids, URL normalization, search, tags/aliases/clusters, web captures,
             AI prompt assembly + output normalization, legacy desktop reader
  ai/        async provider adapters (OpenAI-compatible, Anthropic), retries, limiters, embeddings, STT
  media/     CAS store, image pipeline (decode/resize/WebP/ThumbHash), ffmpeg and yt-dlp wrappers
  server/    axum app, auth, SSE, scheduler and workers, `shelfy-server admin …` CLI
  migrate/   `shelfy-migrate` CLI (desktop → web)
  napi/      (P6) napi-rs bindings for Electron main
shared/ai/   prompts (*.md), JSON schemas (*.json), golden fixtures; read by TS and by Rust (include_str!)
web/         Vite SPA entry; imports ../src through an `@ui` alias; HTTP ShelfyClient; PWA config
extension/   MV3 extension (esbuild): bundles electron/webview-injected.ts, electron/webview-select.ts,
             src/lib/browser{Urls,Sanitize,Scripts}.ts
capture/     Node service: imports electron/webcap/* through capture/src/env.ts
deploy/      Dockerfiles, dev/test compose files, nginx snippet, seccomp profile, Smokescreen ACL,
             osn patch set, Grafana dashboard and alert rules
```

| Existing code | Reused as | Change needed |
|---|---|---|
| `electron/webview-injected.ts` (894 lines) | extension MAIN-world content script | relay through `window.postMessage` (fallback already exists); keep IG `video_versions`/`video_url` and X `video_info.variants`; emit IG `oe` expiry |
| `electron/webview-select.ts` | extension overlay injected on demand | i18n the hard-coded Italian strings |
| `src/lib/browserSanitize.ts`, `browserUrls.ts` | extension (client-side) + ported to `crates/core::ingest` | golden fixtures keep TS and Rust identical |
| `src/lib/browserScripts.ts`, `useBrowserSync`/`useSourceSync` logic | extension sync controller | code strings → functions/files for `chrome.scripting` |
| `electron/webcap/*` (capture v2, 4.5k lines), `net-safety.ts`, discovery in `webcapture.ts` | `capture/` service | replace `electron.app` paths with `env.ts`; drop `electron-driver.ts` and `system-chrome.ts` on the server |
| prompts and schemas in `electron/analyzer.ts` | `shared/ai/` files | desktop reads the same files (no behavior change) |
| `electron/cluster-core.ts`, validators (`validateAliasPairs`, `intersectWithVocab`, `parseTagBlock`…) | ported to `crates/core::tags` | golden fixtures |
| `electron/db.ts` SQL (6.2k lines) | ported to `crates/core` (SQLite → SQLite) | add `source` to `post_tags`, CAS refs, FTS5; most statements port almost verbatim |
| React renderer (`src/`) | SPA through `ShelfyClient` | replace direct `window.electronAPI` calls (39 files) |
| `scripts/search-eval`, `extract-eval`, `cluster-eval` | parity gates | point runners at the Rust core / server |

### 2.5 Storage layout on disk

```
/data/shelfy/                              owner 10100:10100, 0750 (bind mount; Volume-ready)
  control/control.sqlite[-wal|-shm]
  users/<user_ulid>/
    library.sqlite[-wal|-shm]
    media/<aa>/<sha256>.<ext>              masters (covers, slides, posters, screenshots, uploads, kept videos)
    media/<aa>/<sha256>.g480.webp          480 px rendition (covers, carousel slides 1–3, site heroes)
    exports/<export_ulid>.zip              TTL 7 days
  cache/video/<user_ulid>/<sha256>.mp4     on-demand videos, global LRU (5 GiB), not backed up
  work/capture/<capture_ulid>/             capture scratch, shared with shelfy-capture (same uid)
  work/uploads/<upload_ulid>.part          tus uploads in progress, TTL 24 h
  backup-staging/db/                       hourly DB snapshots read by restic
```

Rules: object file names are lowercase hex SHA-256 plus a fixed extension from an allowlist, so path traversal is impossible by construction; objects are written to a temp file in the same directory, fsynced and renamed; a user is deleted by removing `users/<id>/` and `cache/video/<id>/`.

### 2.6 Control DB schema (`control.sqlite`)

```sql
CREATE TABLE users (
  id TEXT PRIMARY KEY,                               -- ULID
  email TEXT NOT NULL UNIQUE COLLATE NOCASE,
  display_name TEXT,
  role TEXT NOT NULL CHECK (role IN ('owner','member')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active','disabled','deleting')),
  quota_bytes INTEGER NOT NULL,                      -- media + DB; owner: 0 = unlimited
  capture_daily_limit INTEGER NOT NULL DEFAULT 20,
  usage_bytes INTEGER NOT NULL DEFAULT 0, usage_updated_at INTEGER,
  disclaimer_version TEXT, disclaimer_accepted_at INTEGER,
  created_at INTEGER NOT NULL, last_seen_at INTEGER
);
CREATE TABLE invites (token_hash BLOB PRIMARY KEY, email TEXT, role TEXT NOT NULL DEFAULT 'member',
  created_by TEXT NOT NULL REFERENCES users(id), created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL, used_at INTEGER, used_by TEXT);
CREATE TABLE passkeys (id INTEGER PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  cred_id BLOB NOT NULL UNIQUE, passkey_json TEXT NOT NULL, label TEXT,
  created_at INTEGER NOT NULL, last_used_at INTEGER);
CREATE TABLE sessions (id_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, last_seen_at INTEGER NOT NULL,
  reauth_at INTEGER, user_agent TEXT);
CREATE TABLE magic_links (token_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL CHECK (purpose IN ('login','verify','reauth')),
  expires_at INTEGER NOT NULL, used_at INTEGER);
CREATE TABLE api_tokens (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (kind IN ('extension','shortcut','migrate')),
  token_hash BLOB NOT NULL UNIQUE, label TEXT, scopes TEXT NOT NULL,
  created_at INTEGER NOT NULL, last_used_at INTEGER, revoked_at INTEGER);
CREATE TABLE pairing_codes (code_hash BLOB PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, expires_at INTEGER NOT NULL, used_at INTEGER);
CREATE TABLE provider_keys (user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  provider_id TEXT NOT NULL, key_version INTEGER NOT NULL, nonce BLOB NOT NULL, ciphertext BLOB NOT NULL,
  last4 TEXT NOT NULL, created_at INTEGER NOT NULL, last_used_at INTEGER,
  PRIMARY KEY (user_id, provider_id));
CREATE TABLE jobs (
  id INTEGER PRIMARY KEY,
  user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  kind TEXT NOT NULL, dedupe_key TEXT,
  state TEXT NOT NULL CHECK (state IN ('queued','running','succeeded','failed','cancelled')),
  priority INTEGER NOT NULL DEFAULT 100,
  payload_json TEXT NOT NULL DEFAULT '{}',
  attempts INTEGER NOT NULL DEFAULT 0, max_attempts INTEGER NOT NULL,
  run_at INTEGER NOT NULL, lease_until INTEGER,
  progress REAL, stage TEXT, error_code TEXT, error_detail TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, finished_at INTEGER
);
CREATE UNIQUE INDEX jobs_active_dedupe ON jobs(user_id, kind, dedupe_key)
  WHERE state IN ('queued','running') AND dedupe_key IS NOT NULL;
CREATE INDEX jobs_ready ON jobs(kind, state, run_at);
CREATE INDEX jobs_user ON jobs(user_id, state, kind);
CREATE TABLE queue_state (user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE, kind TEXT NOT NULL,
  paused INTEGER NOT NULL DEFAULT 0, PRIMARY KEY (user_id, kind));
CREATE TABLE uploads (id TEXT PRIMARY KEY, user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
  purpose TEXT NOT NULL, length INTEGER NOT NULL, upload_offset INTEGER NOT NULL DEFAULT 0, meta_json TEXT,
  created_at INTEGER NOT NULL, expires_at INTEGER NOT NULL, completed_at INTEGER);
CREATE TABLE idempotency (user_id TEXT NOT NULL, key TEXT NOT NULL, status INTEGER NOT NULL, body BLOB,
  created_at INTEGER NOT NULL, PRIMARY KEY (user_id, key));          -- pruned after 24 h
CREATE TABLE usage_daily (user_id TEXT NOT NULL, day TEXT NOT NULL,
  ai_calls INTEGER NOT NULL DEFAULT 0, ai_in_tokens INTEGER NOT NULL DEFAULT 0, ai_out_tokens INTEGER NOT NULL DEFAULT 0,
  captures INTEGER NOT NULL DEFAULT 0, ingest_items INTEGER NOT NULL DEFAULT 0, bytes_in INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (user_id, day));
CREATE TABLE audit_log (id INTEGER PRIMARY KEY, at INTEGER NOT NULL, actor_user_id TEXT, action TEXT NOT NULL,
  target TEXT, ip_hash BLOB, meta_json TEXT);
CREATE TABLE feature_flags (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
```

All secrets (sessions, tokens, invite and magic-link tokens, pairing codes) are 256-bit random values; only their SHA-256 is stored. All timestamps are unix milliseconds.

### 2.7 User library schema (`library.sqlite`)

```sql
CREATE TABLE posts (
  id INTEGER PRIMARY KEY,                    -- internal rowid: joins + FTS rowid
  key TEXT NOT NULL UNIQUE,                  -- public id, see §2.8
  platform TEXT NOT NULL CHECK (platform IN ('instagram','twitter','pinterest','web','manual')),
  native_id TEXT NOT NULL,
  shortcode TEXT, post_url TEXT, profile_url TEXT, author_username TEXT, author_name TEXT,
  caption TEXT,                              -- ≤ 20 000 chars
  media_type TEXT NOT NULL,                  -- image|images|carousel|video|text|website|file
  media_count INTEGER NOT NULL DEFAULT 1,
  posted_at INTEGER,                         -- ms; NULL when unknown (was ISO TEXT with '' sentinels)
  imported_at INTEGER NOT NULL,
  sort_ts INTEGER NOT NULL,                  -- COALESCE(posted_at, imported_at)
  cover_object INTEGER REFERENCES media_objects(id),
  cover_url TEXT, cover_url_expires_at INTEGER,
  thumbhash BLOB,                            -- ≤ 25 bytes (was a ~530-byte data URI)
  archive_state TEXT NOT NULL DEFAULT 'pending'
    CHECK (archive_state IN ('pending','partial','done','failed','client','link_only')),
  ai_status TEXT, ai_attempts INTEGER NOT NULL DEFAULT 0, ai_next_at INTEGER, ai_error TEXT,
  ai_provider TEXT, ai_model TEXT, ai_schema_version INTEGER,
  ai_description TEXT, ai_save_reason TEXT, ai_language TEXT, ai_category TEXT, ai_content_type TEXT,
  ai_tags_json TEXT, ai_entities_json TEXT, ai_keywords_json TEXT, ai_web_json TEXT, ai_analyzed_at INTEGER,
  user_note TEXT, user_tags_json TEXT,
  web_url TEXT, web_domain TEXT, web_final_url TEXT,
  current_capture_id INTEGER REFERENCES web_captures(id) ON DELETE SET NULL,
  updated_at INTEGER NOT NULL, deleted_at INTEGER,
  UNIQUE (platform, native_id)
);
CREATE INDEX posts_sort      ON posts(sort_ts DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_platform  ON posts(platform, sort_ts DESC, id DESC) WHERE deleted_at IS NULL;
CREATE INDEX posts_shortcode ON posts(shortcode) WHERE shortcode IS NOT NULL;
CREATE INDEX posts_ai        ON posts(ai_status, ai_next_at) WHERE ai_status IS NOT NULL;
CREATE INDEX posts_domain    ON posts(web_domain) WHERE web_domain IS NOT NULL;
CREATE INDEX posts_trash     ON posts(deleted_at) WHERE deleted_at IS NOT NULL;

CREATE TABLE post_media (
  post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  position INTEGER NOT NULL,
  kind TEXT NOT NULL CHECK (kind IN ('image','video','file','page')),
  source_url TEXT, source_url_expires_at INTEGER,
  video_url TEXT, video_url_expires_at INTEGER,     -- direct MP4 kept by the parser
  width INTEGER, height INTEGER, duration_ms INTEGER, label TEXT,
  object_id INTEGER REFERENCES media_objects(id),   -- archived image / poster
  video_object_id INTEGER REFERENCES media_objects(id), -- kept ("offline") video
  fetch_attempts INTEGER NOT NULL DEFAULT 0, fetch_next_at INTEGER, fetch_error TEXT,
  PRIMARY KEY (post_id, position)
) WITHOUT ROWID;
CREATE INDEX post_media_pending ON post_media(fetch_next_at) WHERE object_id IS NULL AND kind IN ('image','video');

CREATE TABLE media_objects (
  id INTEGER PRIMARY KEY, sha256 BLOB NOT NULL UNIQUE,
  ext TEXT NOT NULL, mime TEXT NOT NULL, bytes INTEGER NOT NULL,
  width INTEGER, height INTEGER, duration_ms INTEGER,
  role TEXT NOT NULL,          -- image|poster|video|file|preview|screenshot|band|section|footer|filmstrip|favicon|og
  variants INTEGER NOT NULL DEFAULT 0,   -- bitmask: 1 = g480
  origin TEXT NOT NULL,        -- server|extension|upload|capture|migration
  created_at INTEGER NOT NULL, unreferenced_since INTEGER
);

CREATE TABLE collections (id INTEGER PRIMARY KEY, name TEXT NOT NULL, color TEXT NOT NULL DEFAULT '#3d5afe',
  platform TEXT, external_id TEXT, source_name TEXT, position INTEGER, created_at INTEGER NOT NULL,
  UNIQUE (platform, external_id));
CREATE TABLE post_collections (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  collection_id INTEGER NOT NULL REFERENCES collections(id) ON DELETE CASCADE, added_at INTEGER NOT NULL,
  PRIMARY KEY (post_id, collection_id)) WITHOUT ROWID;
CREATE INDEX post_collections_c ON post_collections(collection_id, post_id);

CREATE TABLE post_tags (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  tag_norm TEXT NOT NULL, tag_form TEXT NOT NULL,
  source TEXT NOT NULL CHECK (source IN ('ai','manual')),
  tier TEXT CHECK (tier IN ('general','specific')),
  PRIMARY KEY (post_id, tag_norm, source)) WITHOUT ROWID;
CREATE INDEX post_tags_norm ON post_tags(tag_norm, post_id);
CREATE TABLE post_entities (post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  ent_norm TEXT NOT NULL, ent_form TEXT NOT NULL, PRIMARY KEY (post_id, ent_norm)) WITHOUT ROWID;
CREATE INDEX post_entities_norm ON post_entities(ent_norm, post_id);
CREATE TABLE tag_alias (alias_norm TEXT PRIMARY KEY, canonical_norm TEXT NOT NULL, canonical_form TEXT NOT NULL,
  status TEXT NOT NULL CHECK (status IN ('proposed','accepted')), created_at INTEGER NOT NULL);
CREATE TABLE tag_cluster (id INTEGER PRIMARY KEY, label TEXT NOT NULL, label_norm TEXT,
  status TEXT NOT NULL DEFAULT 'proposed', run_id INTEGER, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE tag_cluster_membership (tag_norm TEXT PRIMARY KEY,
  cluster_id INTEGER NOT NULL REFERENCES tag_cluster(id) ON DELETE CASCADE);
CREATE TABLE tag_embeddings (tag_norm TEXT NOT NULL, model TEXT NOT NULL, dim INTEGER NOT NULL, vec BLOB NOT NULL,
  PRIMARY KEY (tag_norm, model)) WITHOUT ROWID;

CREATE TABLE web_captures (id INTEGER PRIMARY KEY,
  post_id INTEGER NOT NULL REFERENCES posts(id) ON DELETE CASCADE,
  captured_at INTEGER NOT NULL, requested_url TEXT, final_url TEXT,
  status TEXT NOT NULL, partial INTEGER NOT NULL DEFAULT 0, engine TEXT, viewport TEXT,
  title TEXT, palette_json TEXT, fonts_json TEXT, tech_json TEXT, awards_json TEXT,
  meta_json TEXT, pages_json TEXT, traits_json TEXT,   -- pages_json holds text + probes, never file paths
  hero_object INTEGER REFERENCES media_objects(id), favicon_object INTEGER REFERENCES media_objects(id),
  ai_snapshot_json TEXT,                               -- frozen AI layer of this version
  created_at INTEGER NOT NULL);
CREATE INDEX web_captures_post ON web_captures(post_id, captured_at DESC);
CREATE TABLE web_capture_assets (capture_id INTEGER NOT NULL REFERENCES web_captures(id) ON DELETE CASCADE,
  page_index INTEGER NOT NULL, role TEXT NOT NULL, seq INTEGER NOT NULL,
  object_id INTEGER NOT NULL REFERENCES media_objects(id), css_top INTEGER, css_height INTEGER,
  PRIMARY KEY (capture_id, page_index, role, seq)) WITHOUT ROWID;

CREATE VIRTUAL TABLE posts_fts USING fts5(
  tags, keywords, entities, description, note, caption, author, web_text,
  content='', contentless_delete=1,
  tokenize="unicode61 remove_diacritics 2", prefix='2 3');

CREATE TABLE settings (key TEXT PRIMARY KEY, value_json TEXT NOT NULL, updated_at INTEGER NOT NULL);
CREATE TABLE notifications (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, code TEXT NOT NULL,
  params_json TEXT, target TEXT, created_at INTEGER NOT NULL, read_at INTEGER);
CREATE TABLE sync_runs (id TEXT PRIMARY KEY, platform TEXT NOT NULL, source_kind TEXT NOT NULL, source_key TEXT,
  started_at INTEGER NOT NULL, finished_at INTEGER, status TEXT NOT NULL,
  scanned INTEGER NOT NULL DEFAULT 0, inserted INTEGER NOT NULL DEFAULT 0, updated INTEGER NOT NULL DEFAULT 0,
  known INTEGER NOT NULL DEFAULT 0, error_code TEXT, client_version TEXT);
CREATE TABLE sync_sources (platform TEXT NOT NULL, source_key TEXT NOT NULL,
  collection_id INTEGER REFERENCES collections(id) ON DELETE SET NULL,
  last_run_at INTEGER, newest_native_id TEXT, PRIMARY KEY (platform, source_key));
CREATE TABLE ai_cache (kind TEXT NOT NULL, key_hash BLOB NOT NULL, value_json TEXT NOT NULL, created_at INTEGER NOT NULL,
  PRIMARY KEY (kind, key_hash)) WITHOUT ROWID;
CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
```

| Change vs desktop | Reason |
|---|---|
| integer rowid + public `key`, `UNIQUE(platform, native_id)` | collision-free canonical identity; compact joins and FTS rowids |
| `posted_at`/`sort_ts` in ms instead of ISO TEXT with `''` sentinels | keyset pagination and correct ordering |
| CAS references (`media_objects`) instead of 5 path columns + paths inside JSON | portable, dedupe, GC, no OS user names in data |
| `post_tags.source` in the PK | an AI and a manual tag with the same name can coexist |
| `web_captures` (+ assets) instead of `posts.web_*` + `web_snapshots` duplicates | one model for current and previous versions; delete-latest = repoint `current_capture_id` |
| per-item work state (`ai_*`, `fetch_*`) | the job system drains it (§2.12) |
| `deleted_at` | trash with undo (30 days) |
| `posts_fts` | indexed search with per-user statistics |
| `sync_runs`, `sync_sources`, `notifications`, `settings` | state that was React-only or localStorage on the desktop |
| `downloads`, `jobs` dropped from the user DB | dead table; jobs live in the control DB |

FTS maintenance is explicit in the same transaction as every write that changes searchable text (delete by rowid, re-insert); no triggers, so the core controls exactly what is indexed. `web_text` = title + meta description + per-page digests (≤ 8 k chars), which also makes inner-page text searchable (audit F2).

### 2.8 Canonical identity

| Platform | `native_id` | `key` | Derivation |
|---|---|---|---|
| Instagram | media `pk` as a decimal string | `ig_<pk>` | REST `id` = `<pk>_<owner>` → split; GraphQL `node.id` = pk; DOM fallback shortcode → base64url-decoded to pk (store as text; long private shortcodes may exceed 64 bits) |
| X | tweet id | `x_<id>` | `rest_id` / status URL |
| Pinterest | pin id | `pin_<id>` | `/pin/<id>/` |
| Web | SHA-1 of the normalized URL (lowercase host, no `www.`, **no scheme**, no fragment, no trailing slash, no `utm_*`, `gclid`, `fbclid`, `ref`) | `web_<sha1:20>` | changes WEB-05: http and https collapse |
| Manual | ULID | `m_<ulid>` | created server-side |

Collections from platforms are keyed `(platform, external_id)`: IG saved-folder numeric id, Pinterest board id when the parser exposes it, else `user/slug` (rename-unsafe, as today).

### 2.9 API contract

**Conventions**

| Topic | Rule |
|---|---|
| Base and format | `/api/v1`, JSON UTF-8, camelCase fields, timestamps in unix ms, posts addressed by `key` |
| Errors | `application/problem+json` with a stable `code` (e.g. `quota_exceeded`, `invalid_cursor`, `provider_key_invalid`, `capture_blocked`); the SPA maps codes to i18n strings, the server never returns UI prose (05 §i18n) |
| Auth | SPA: session cookie. Extension, iOS Shortcut, migration CLI: `Authorization: Bearer shx_…` with scopes (`ingest`, `tasks`, `uploads`, `lookup`, `links:create`, `migrate`); a token can never call cookie-only routes |
| CSRF | unsafe cookie-authenticated requests must carry `X-Shelfy-Client: web` and an `Origin` equal to the public URL; bodies are JSON except tus `PATCH` |
| Pagination | `limit` (default 60, max 200) + opaque `cursor`; responses `{items, nextCursor}`; `includeTotal=true` adds `total` (exact `COUNT` is cheap at per-user scale) |
| Conditional GET | list, stats and collections responses carry `ETag = hash(user generation, query)`; unchanged views answer 304 without touching SQLite |
| Idempotency | job-creating `POST`s accept `Idempotency-Key` (24 h); ingest batches carry a batch id |
| Rate limits | per user 20 req/s burst 60; search 5/s; AI suggest 1/s; auth endpoints 10/min per IP (`CF-Connecting-IP`, trusted because the tunnel is the only ingress); 429 + `Retry-After` |
| Compatibility | additive changes only within v1; `GET /api/v1/extension/config` returns `minVersion`; older extensions get 426 |

**List filters** (`GET /posts`, also the `filter` of a bulk selector): `platform`, `source=web|social`, `collection`, `mediaType` (repeatable), `stored=yes|no`, `aiTagged=yes|no`, `aiStatus`, `tag` (repeatable) + `tagMode=and|or`, `entity`, `category`, `contentType`, `q`, `concept` (repeatable) + `conceptMode`, `sort=newest|oldest|relevance`, `trash=1`.

**Bulk selector** (`POST /posts/bulk`): `{"selector": {"keys": [...]} | {"filter": {...}, "exceptKeys": [...]}, "action": "...", "params": {...}}`. Actions: `delete`, `restore`, `addToCollections`, `removeFromCollection`, `clearAiDescription`, `clearAiTags`, `analyze`, `fetchMedia`, `removeStoredMedia`. Small selections run inline; anything above 500 posts or touching files becomes a job. This replaces the "fetch every id, send it back" round trip (DATA-17, UI-49).

**Endpoints**

| Group | Endpoints |
|---|---|
| Auth | `POST /auth/invites/{token}/accept` · `POST /auth/passkeys/register/{start,finish}` · `POST /auth/passkeys/login/{start,finish}` · `POST /auth/magic-links` (always 202) · `GET /auth/magic/{token}` (sets cookie, redirects) · `POST /auth/reauth/{start,finish}` · `POST /auth/logout` · `POST /auth/device/{start,poll}` (CLI) + `POST /auth/device/approve` (cookie) |
| Account | `GET /me` (profile + `capabilities`) · `GET,PUT /me/settings` · `GET,POST,DELETE /me/passkeys` · `GET,DELETE /me/sessions` · `GET,POST,DELETE /me/tokens` · `POST /me/tokens/pairing-code` · `GET,PUT,DELETE /me/providers/{id}` · `POST /me/providers/{id}/test` · `GET /me/usage` · `POST /me/consent` · `POST /me/reset` (reauth) · `DELETE /me` (reauth) |
| Extension | `POST /extension/pair` (code → token) · `GET /extension/config` · `POST /sync-runs` · `PATCH /sync-runs/{id}` · `POST /ingest/batches` · `GET /ingest/tasks?wait=25` · `POST /ingest/tasks/{id}/complete` |
| Library | `GET /posts` · `GET /posts/count` · `POST /posts/batch-get` (≤200 keys) · `GET /posts/{key}` · `PATCH /posts/{key}` (note, manual tags, manual AI edit) · `POST /posts/lookup` (≤1000 platform keys, cookie or `lookup` token) · `POST /posts/bulk` · `GET /stats` · `GET /sync-runs` · `GET /trash` · `POST /trash/restore` · `POST /trash/empty` |
| Collections | `GET,POST /collections` · `PATCH,DELETE /collections/{id}` · `POST /collections/{id}/posts` (selector) · `DELETE /collections/{id}/posts/{key}` · `POST /collections/from-query` |
| Media | `GET /media/{sha}.{ext}` and `GET /media/{sha}.g480.webp` (outside `/api`, cookie) · `POST /posts/{key}/media/fetch` (`{what: "video", keep: bool}`) · `DELETE /posts/{key}/media` (remove stored copy) |
| Uploads (tus 1.0 core + creation + termination) | `POST /uploads` · `HEAD,PATCH,DELETE /uploads/{id}` (16 MiB chunks) |
| Links | `POST /links` (`{url, note?, tags?}`; cookie or `links:create` token) |
| Jobs | `GET /jobs?kind&state&cursor` · `GET /jobs/summary` · `POST /jobs/{id}/cancel` · `POST /jobs/{id}/retry` · `POST /queues/{kind}/{pause,resume,cancel-all,clear-finished}` |
| AI | `POST /ai/analyze` (selector + mode `missing` / `selected` / `all` + `deep`; returns an estimate and a confirm token, second call enqueues) · `GET /ai/queue?state&cursor` · `POST /search/suggest` · `POST /search/chat` (SSE response) · `POST /search/chat/{runId}/cancel` · `POST /stt/transcriptions` |
| Tags | `GET /tags/overview` · `GET /tags?tier&limit` · `GET /entities` · `GET /tags/{tag}/related` · `GET /tags/health` · `GET /tags/merge-suggestions` · `POST /tags/rename` · `POST /tags/merge` · `POST /tags/post-keys` · `GET /tag-clusters` · `POST /tag-clusters/regenerate` (job) · `PATCH,DELETE /tag-clusters/{id}` · `DELETE /tag-clusters/{id}/tags/{tag}` · `GET /tag-aliases?status` · `POST /tag-aliases/propose` (job) · `POST /tag-aliases/{alias}/{accept,dismiss}` · `POST /tag-aliases/accept-all` |
| Search | `GET /search` (hybrid tags + text, `scope` = `all` / `sites` / `social`, `tagMode`) |
| Websites | `POST /sites` (`{url, maxPages≤8, singlePage}`) · `GET /sites` · `GET /sites/{key}/captures` · `DELETE /sites/{key}/captures/{id}` · `POST /sites/{key}/recapture` · `POST /sites/delete-latest` |
| Bookmarks, import, export, migration | `POST /bookmarks` (`{uploadIds, note, tags}`) · `POST /imports` · `POST /exports` · `GET /exports/{id}/download` · `POST /migrations` · `POST /migrations/missing-objects` |
| Platform | `GET /events` (SSE) · `GET /notifications` · `POST /notifications/read` · `POST /feedback` · `POST /client-errors` · `GET /version` · `GET /openapi.json` · `GET /health`, `GET /health/capture` (outside `/api`) |
| Admin (owner) | `GET,POST,DELETE /admin/invites` · `GET /admin/users` · `PATCH /admin/users/{id}` (quota, limits, disable) · `GET /admin/overview` · `PUT /admin/flags/{key}` |

Appendix A maps all 155 invoke and 15 push channels onto this table.

### 2.10 Realtime (SSE)

`GET /api/v1/events?topics=…` streams `text/event-stream` with `X-Accel-Buffering: no` and `Cache-Control: no-store`. The server sends `hello` on connect, a comment heartbeat every 20 s (Cloudflare drops idle proxied connections after 100 s), and supports `Last-Event-ID` against a per-user ring buffer (last 256 events, 5 minutes); older gaps get `resync`.

| Event | Payload | Replaces | Throttle |
|---|---|---|---|
| `posts.changed` | `{keys?, reason}` (`ingest`, `archive`, `ai`, `edit`, `delete`, `capture`, `import`) | `interceptor:newPosts` | coalesced 400 ms quiet / 2 s max (same as `usePosts`) |
| `stats.changed` | `{}` | stats bump in `App.tsx` | 1 s |
| `job.updated` | `{id, kind, state, progress, stage, postKey?, errorCode?}` | `download:progress`, `analyze:progress`, `web:progress`, `aitags:clusterProgress`, `aitags:aliasProgress` | latest per job every 250 ms |
| `ai.stream` | `{postKey, text}` | `streamText` inside `analyze:progress` | 4 Hz per job; only with `topics=ai.stream` (AI queue view open) |
| `capture.event` | `{jobId, postKey, kind, code, params}` | timeline inside `web:progress` | ≤250 per job; codes, not Italian prose |
| `sync.progress` | `{runId, platform, source, scanned, inserted, known, state}` | renderer-only sync state | 1 s |
| `notification` | `{id, kind, code, params, target}` | in-memory activity history | — |
| `provider.status` | `{providerId, state}` with state `ok` / `degraded` / `down` / `invalid_key` | `ai:remoteStatus` | on change |
| `extension.status` | `{connected, version, lastSeenAt}` | — | on change |

Chat tokens stream on the `POST /search/chat` response itself, not on the shared stream (replaces `search:chatToken`).

### 2.11 Authentication, sessions and tokens

- **Owner bootstrap.** `shelfy-server admin create-owner --email …` prints a one-time invite URL. No signup route exists.
- **Invite.** Owner creates an invite in `/admin` (optional email lock, role, TTL 7 days). The invitee opens `/invite/<token>`, accepts the disclaimer and privacy notice (version and timestamp stored, UI-18), verifies an email with a magic link, then registers a passkey (discoverable credential, user verification preferred, RP ID `shelfy.niccolofanton.dev`). Registration state lives in a 5-minute in-memory TTL cache, so `webauthn-rs` needs no state-serialization feature.
- **Login.** Passkey (username-less) or "email me a link" (token TTL 15 min, single use, rate-limited to 3 per hour per address). Magic links double as account recovery when the last passkey is lost.
- **Session.** Cookie `__Host-shelfy_session` (Secure, HttpOnly, SameSite=Lax, Path=/), 256-bit random, SHA-256 stored, 30-day sliding and 90-day absolute expiry; lookups cached in moka for 60 s. Re-authentication (a passkey assertion or magic link younger than 5 minutes) is required for account deletion, resets, email change and token creation.
- **Device tokens.** The extension pairs without exposing the long-lived token to page JavaScript: the SPA requests a 60-second pairing code (`POST /me/tokens/pairing-code`), hands it to the extension through `chrome.runtime.sendMessage(EXTENSION_ID, …)` (`externally_connectable`), and the extension exchanges it at `POST /extension/pair`. The iOS Shortcut token (`links:create` only) is created in Settings and shown once. The migration CLI uses a device-code flow: `shelfy-migrate login` shows a short code, the user approves it in the SPA, and the CLI's poll returns a `migrate` token (7-day TTL).
- **Admin.** Owner-only routes under `/api/v1/admin`; every destructive or admin action writes `audit_log`.
- **Edge layer.** During P0–P1 (owner only, read-only) the hostname also sits behind Cloudflare Access, reusing the osn allow-list; Access is removed in P2 because extension and Shortcut tokens cannot pass an interactive Access login.

### 2.12 Jobs and scheduling

**Model.** Two kinds of work, one scheduler:

1. **Item work** (thousands per user: archive an image, catalog a post) keeps its state in the user DB (`post_media.fetch_*`, `posts.ai_*`). A per-user **drain job** (`archive.drain`, `ai.drain`, dedupe key = kind) claims items in chunks, respects per-user and global concurrency, and when only backed-off items remain it re-arms itself with `run_at = min(next_at)`. Enqueuing is idempotent, so the user-DB write and the control-DB enqueue do not need a distributed transaction; a sweeper every 10 minutes re-arms any user with pending items and no active drain job.
2. **Task work** (capture a site, fetch one video, export, import, migrate, purge, cluster run, alias run, hydrate a link) is one `jobs` row per task.

**Scheduler.** In memory, rebuilt from `jobs` at boot (`running` rows go back to `queued`). For each kind it keeps a per-user FIFO and serves users in round-robin order, so one user's 6,000-post backlog never starves another user's single request. A claim is `UPDATE jobs SET state='running', lease_until=? WHERE id=? AND state='queued'`. Workers renew the lease every third of its length; an expired lease re-queues the job with `attempts + 1`. Delayed jobs sit on a timer keyed by the smallest `run_at`.

| Kind | Unit | Global | Per user | Attempts / backoff | Lease | Notes |
|---|---|---|---|---|---|---|
| `archive.drain` | 25 slides per chunk | 4 fetches, 2 encodes | 2 | 5 per item, 30 s × 2ⁿ (max 6 h) | 5 min | per-host limiters and circuit breaker (§2.13) |
| `ai.drain` | 1 post per call | 32 in flight | 4 (setting 1–8) | 3 per item, honors `Retry-After` | 10 min | provider token bucket; paused while the provider circuit is open |
| `ai.run` | cluster or alias run | 4 | 1 | 1 | 30 min | cancellable |
| `media.video` | 1 post | 2 | 1 | 3 | 15 min | ≤300 MB, faststart remux |
| `capture.site` | 1 site | 1 (max 2) | 1 | 2 | 20 min | daily quota per user; round-robin |
| `link.hydrate` | 1 link | 2 | 1 | 5 | 2 min | server path for X/Pinterest, extension task for IG |
| `import`, `export`, `migrate` | 1 | 1 | 1 | 2 | 60 min | streaming, progress events |
| `purge`, `gc` | 1 | 1 | — | 3 | 60 min | nightly 03:00 UTC |

**Errors.** Each worker classifies failures as `transient` (network, 5xx, 429, timeouts → retry with jitter) or `permanent` (4xx, validation, blocked, quota, invalid key → fail now, visible with a retry button). `provider_key_invalid` pauses the user's AI queue and posts a notification instead of failing thousands of items.

**Controls.** Pause/resume per user and kind (`queue_state`), cancel (sets `cancelled` and trips the job's `CancellationToken`; capture cancellation closes the stream to the capture service), retry, clear finished. Finished task jobs are pruned after 14 days; item state lives with the post.

### 2.13 Media pipeline

**Archive policy**

| Asset | When | Source variant | Stored as | Rendition |
|---|---|---|---|---|
| Cover of an image/carousel post | at ingest | slide 0 (same object as the cover) | original bytes | `g480` WebP q75 + ThumbHash |
| Image slides | at ingest | IG as served (≤1080 px); X `?format=jpg&name=large` (≤2048 px, instead of today's `orig`); Pinterest `/1200x/` (instead of `/originals/`), falling back to the served size | original bytes if ≤2048 px and ≤1.5 MB, else WebP q82 at 2048 px | `g480` for slides 1–3 of multi-image posts (hover slideshow, AI input); the modal uses the master |
| Video poster | at ingest | cover URL | WebP q78, ≤1080 px | `g480` + ThumbHash |
| Video | on demand only | order of D15 | MP4 (stream copy + `+faststart`) in the LRU cache; "keep offline" moves it into the user's CAS (counts against quota) | none |
| Web capture assets | at capture | capture service | WebP bands/sections/hero, MP4 scroll video (720p, CRF 27) | hero `g480` |
| Manual uploads | on upload | original file | original bytes (≤200 MiB) | images: server `g480`; video/PDF: the client preview after server-side decode validation |
| Favicon, og:image | at capture | site | PNG/WebP ≤64 KB / ≤2 MB | — |

Image work uses `image` (decode, EXIF orientation), `fast_image_resize` (SIMD), `webp` (libwebp, lossy encode) and `thumbhash`. ThumbHash replaces the ~530-byte blur data URI with ≤25 bytes per row; the SPA decodes it client-side.

**Archive worker** (`archive.drain`). For each pending slide, ordered by soonest expiry (IG `oe=` hex timestamp parsed at ingest), fetch through the egress proxy with host allowlists (`*.cdninstagram.com`, `*.fbcdn.net`, `pbs.twimg.com`, `video.twimg.com`, `*.pinimg.com`), no cookies, browser-like `Accept` and platform `Referer`, 15 MB cap, 30 s timeout; hash while streaming; write to the CAS; link the object; render `g480` for covers and slides 1–3 of multi-image posts and ThumbHash for covers; emit `posts.changed`.

| Host group | Concurrency | Rate |
|---|---|---|
| Instagram CDN | 4 | 8 req/s, jitter 120–400 ms (as DL-14) |
| X (`pbs`, `video.twimg.com`) | 8 | 20 req/s |
| Pinterest CDN | 4 | 8 req/s, jitter 120–400 ms |

**Circuit breaker.** Per host group, when ≥20 % of the last 50 fetches (or 10 in a row) end in 403/429, the breaker opens for 30 minutes; pending items move to `archive_state='client'` and become extension upload tasks. The breaker state is a metric and an admin-visible flag. SPIKE-2 sets the per-platform default (`server`, `client` or `auto`).

**On-demand video** (`POST /posts/{key}/media/fetch`):

```
cached or kept? ─────────────────────────────► stream /media/<sha>.mp4 (Range)
post_media.video_url present and not expired ─► media.video job: server fetch via proxy
SPA sees the extension (externally_connectable ping) ─► extension resolves a fresh URL in a platform tab, uploads via tus
platform is X or Pinterest (IG only if the owner enables it) ─► yt-dlp anonymous via proxy, pinned version, 1 req/5 s per platform
otherwise ─► extension task queued for next wake-up + "Open original"
```

yt-dlp runs with the desktop's flags (`--ignore-config --no-cookies --no-cache-dir --no-plugin-dirs --use-extractors …`) plus explicit `-f "bv*[ext=mp4]+ba[ext=m4a]/b[ext=mp4]/b" --merge-output-format mp4 --ffmpeg-location /usr/bin/ffmpeg --proxy http://shelfy-egress:4750` (fixes DL-12's implicit format choice).

**Quota and GC.** Usage = sum of `media_objects.bytes` + DB file size, recomputed incrementally and nightly. Default quota 5 GiB per member (owner unlimited). Over quota: new items stay `link_only` (metadata and ThumbHash-less placeholder) and a notification explains it. The nightly GC deletes objects unreferenced for >24 h (file then row), empties trash older than 30 days, deletes exports older than 7 days and tus partials older than 24 h, and trims the video cache to its 5 GiB budget (least recently served first).

### 2.14 Search

**Query building** ports `extractContentTerms` (IT + EN stopwords, minimum 3 characters except the short-term whitelist `3d`, `ai`, `ux`…). For terms `t1 … tn` the FTS query is `{tags keywords entities description note caption author web_text}: ("t1"* OR … OR "tn"*)`; concept chips add AND/OR blocks; quotes in user input are escaped.

```sql
WITH hits AS (
  SELECT rowid, bm25(posts_fts, 6.0, 5.0, 4.5, 4.0, 4.0, 3.5, 2.0, 2.0) AS bm   -- lower = better
  FROM posts_fts WHERE posts_fts MATCH :query
), phrase AS (
  SELECT rowid FROM posts_fts WHERE posts_fts MATCH :phrase                      -- full-phrase bonus, as today
)
SELECT p.key,
       h.bm
       - 3.0 * (SELECT count(*) FROM post_tags t
                WHERE t.post_id = p.id AND t.tag_norm IN (SELECT value FROM json_each(:terms)))
       - 1.5 * (h.rowid IN (SELECT rowid FROM phrase)) AS score
FROM hits h JOIN posts p ON p.id = h.rowid
WHERE p.deleted_at IS NULL /* + list filters */
ORDER BY score, p.sort_ts DESC, p.id DESC
LIMIT :limit OFFSET :offset;
```

Column weights mirror today's CASE weights (exact tag 6, tag/keyword 5, description/note 4, caption 3.5). bm25 already includes IDF, replacing the process-global IDF memo with correct per-user statistics. Relevance results page by offset inside a snapshot (capped at 1,000 results); browsing pages by keyset `(sort_ts, id)`.

**Gate.** SPIKE-5 ports `scripts/search-eval/cases.ts` into a Rust test; FTS5 must reach nDCG@10 and MRR ≥ the desktop baseline in `last-report.json` minus 0.02 before the web search ships. Weights are tuned there, not in production.

**Filters** use the indexes in §2.7 (`post_tags(tag_norm, post_id)` for tags, `GROUP BY … HAVING count = n` for AND). Counts and stats are cached per `(user, generation, filter hash)`.

### 2.15 AI provider layer (BYOK)

**Adapters**

| Protocol | Covers | Structured output | Images | Extras |
|---|---|---|---|---|
| OpenAI-compatible (`{base}/chat/completions`, base used verbatim) | OpenAI, Gemini (`…/v1beta/openai`), OpenRouter, Groq, Mistral, Together, custom | `response_format: json_schema, strict: true`; fallback `json_object` + validation + one repair retry | `image_url` data URLs | `/embeddings`, `/audio/transcriptions`, `/models` |
| Anthropic Messages (`/v1/messages`) | Anthropic | forced tool use with `input_schema` = our schema (structured-output mode where available) | base64 image blocks | no embeddings or STT: route those tasks to another provider or disable them |

The base URL is used exactly as entered plus the path suffix; the desktop's "strip and re-append `/v1`" logic is not ported (it breaks Gemini, 03 §portability). Preset base URLs and the JSON-schema behavior of each provider are verified in SPIKE-6.

**Per-user configuration.** Providers `{id, kind, label, baseUrl, models: {catalog, chat, suggest, embed?, stt?}}` plus a sealed key. Task routing: `catalog` and `qc` need vision; `chat`, `suggest`, `cluster`, `alias` are text; `embed` and `stt` are optional (features degrade gracefully, as the desktop's deterministic fallbacks do). Keys are sealed with XChaCha20-Poly1305 under a per-user key derived by HKDF-SHA256 from `SHELFY_MASTER_KEY` and the user id (`key_version` enables rotation), held in `secrecy`/`zeroize` types, never returned (only `last4`), never logged. Custom base URLs must resolve to public IPs through the proxy; the owner may allow one private route (e.g. a tailnet model server) with an explicit `SHELFY_OWNER_PRIVATE_PROVIDER` setting.

**Prompts and schemas** move to `shared/ai/` (`catalog.system.md`, `catalog.schema.json`, `web_catalog.*`, `qc.*`, `chat.system.md`, `suggest.*`, `cluster_refine.*`, `aliases.*`) with a `schemaVersion`. The desktop reads the same files, so both products produce the same catalog. Schema v2 adds `other` to the web purpose enum. llama-only knobs (DRY sampler, `cache_prompt`, chat template kwargs) are dropped; reasoning models get `reasoning_effort: low` where supported.

**Inputs per post** (≤40 ms server CPU): image → its existing `g480` WebP (480 px vs the desktop's 448 px; every target provider accepts WebP), so most posts need no image work at all; carousel → `g480` of ≤4 slides; video → poster `g480`, plus 4 ffmpeg keyframes when `deep` and the video is cached; web → hero + ≤3 bands/sections downscaled to 768 px plus the sanitized page digest (`sanitizeForPrompt`); text → caption only; manual → image or preview. Captions stay wrapped as untrusted data (AI-11).

**Reliability.** Timeouts 120 s (catalog), 60 s (chat first token 20 s); three retries with jitter on 429/5xx/timeouts; a per-(user, provider) circuit breaker emits `provider.status` and holds the user's AI queue instead of failing it (replaces AI-57/58 and the global one-request mutex).

**Cost transparency.** `POST /ai/analyze` returns an estimate before enqueuing: posts × (≈1.5–2.4k input + 0.15–0.35k output tokens for multi-image posts; ≈0.9–1.3k for text-only) from 03 §cost, priced with the user's optional per-million-token prices. Actual token usage from provider responses goes to `usage_daily`.

**Features**

| Feature | Web behavior |
|---|---|
| Cataloging (AI-08/09) | `ai.drain` with the shared schemas; results applied through the same normalization (`cleanStringArray`, tier caps) and alias canonicalization; web posts always get the web prompt |
| Screenshot QC (AI-23) | pixel heuristics from capture v2 first; vision QC only if the user enables it (it costs 6–16 small calls per site) |
| Chat search (AI-35…38) | `POST /search/chat` streams SSE; the sentinel protocol (`[[GENERAL]]…`) is kept with the ported parsers; history truncated to 8 turns / 6k tokens; deterministic fallback when no provider is configured or its circuit is open |
| Suggestion chips (AI-41) | text task, 600 ms client debounce, 24 h cache in `ai_cache` |
| Clusters and aliases (AI-30…33) | `ai.run` jobs; `cluster-core` ported to Rust; tag embeddings from the user's embedding model cached in `tag_embeddings`; co-occurrence only when no embedding model is set |
| Dictation (AI-45) | MediaRecorder (Opus) in the browser, one `POST /stt/transcriptions` on stop (≤120 s, language from the UI), forwarded to the user's STT model; live interim text from the browser Web Speech API only if the user opts in |
| Onboarding (AI-47) | "Connect an AI provider" wizard: pick a preset, paste the key, test (chat, vision, schema), consent to sending post content to that provider |

### 2.16 Browser extension (Chrome MV3)

**Manifest essentials**

```json
{
  "manifest_version": 3,
  "minimum_chrome_version": "120",
  "permissions": ["storage", "unlimitedStorage", "alarms", "scripting", "sidePanel"],
  "host_permissions": [
    "https://www.instagram.com/*", "https://x.com/*", "https://twitter.com/*",
    "https://*.pinterest.com/*", "https://*.pinterest.it/*", "…one entry per supported ccTLD…",
    "https://*.cdninstagram.com/*", "https://*.fbcdn.net/*", "https://pbs.twimg.com/*",
    "https://video.twimg.com/*", "https://*.pinimg.com/*",
    "https://shelfy.niccolofanton.dev/*"
  ],
  "background": { "service_worker": "sw.js", "type": "module" },
  "content_scripts": [
    { "matches": ["<social hosts>"], "js": ["hook.main.js"], "world": "MAIN", "run_at": "document_start" },
    { "matches": ["<social hosts>"], "js": ["bridge.js"], "run_at": "document_start" }
  ],
  "externally_connectable": { "matches": ["https://shelfy.niccolofanton.dev/*"] },
  "side_panel": { "default_path": "panel.html" }
}
```

Manifest match patterns cannot express the `pinterest\.[a-z]{2,3}(\.[a-z]{2})?` regex used by `ALLOWED_HOSTS` in `src/lib/browserUrls.ts`, so an explicit, exported `PINTEREST_HOSTS` list (com, it, de, fr, es, co.uk, ca, com.au, jp, com.mx, …) feeds both that regex check and the manifest generator. Host permission on the Shelfy origin makes the service worker's API calls CORS-free; no cookies are used, only the bearer token.

| Part | Runs in | Built from | Job |
|---|---|---|---|
| `hook.main.js` | page MAIN world, `document_start` | `electron/webview-injected.ts` | patch fetch/XHR before page code runs, parse, `postMessage` batches |
| `bridge.js` | ISOLATED world | `electron/webview-preload.ts` | accept only `event.source === window` messages, forward over a `chrome.runtime` port |
| sync controller | ISOLATED world of the syncing tab, MAIN-world helpers via `chrome.scripting.executeScript({world: "MAIN", func})` | `src/lib/browserScripts.ts`, `useBrowserSync`, `useSourceSync` logic | IG REST replay, gradual two-pass scroll, termination rules, source-sync planner |
| `sw.js` | service worker | new | sanitize (`browserSanitize`), batch (≤500 items or 2 s), upload, offline queue (IndexedDB, ≤50k items), tasks, pairing |
| side panel | extension page | new UI with the shared i18n | pairing status, per-platform "Sync now", selection mode, passive-capture toggles, pending tasks, recent runs |
| selection overlay | MAIN world, injected on demand | `electron/webview-select.ts` | checkboxes, shift ranges, "already saved" badges via `POST /posts/lookup` |

**Rules that matter**

- **Passive capture is scoped.** IG's matcher accepts every `/graphql/query` response, so items are ingested passively only when `location` matches the saved-listing pattern (`SAVED_PATTERNS` in `browserUrls.ts`); Pinterest only on the logged-in user's own boards; X bookmarks always. Everything else is discarded, mirroring the desktop pre-sync buffer (SYNC-16).
- **Long work lives in the tab.** MV3 service workers stop after ~30 s idle, so replay and scroll loops run in the content script of the syncing tab; the service worker only relays and uploads.
- **Incremental stop.** Each ingest response returns `known`; the controller stops after a platform-specific run of consecutive known items (default IG 60, X 40, Pinterest 50, served by `/extension/config`). Daily syncs then touch one or two pages instead of re-walking the whole listing (02 risk 5).
- **Pacing kept from desktop.** IG replay 700 ms gap, ≤100 pages; scroll settle 650/750 ms; caps 16,000 steps / 30 min.
- **Folder/board mapping.** Sources are sent as `{kind, externalId, name}`; the server finds or creates the collection by `(platform, external_id)`. Explicit syncs show the chooser (IMP-10); passive capture follows a setting (`always` by default).
- **Background syncs.** `chrome.alarms` (daily, user-chosen hour) shows a notification "Sync now" by default; unattended runs in a minimized window are opt-in until SPIKE-3 confirms that throttled background tabs still lazy-load.
- **Tasks.** `GET /ingest/tasks?wait=25` is polled on wake-up (alarm every 5 min while the browser is open) and whenever the SPA pings the extension. Task types: `upload_media` (fetch CDN bytes with `credentials: "omit"`, upload via tus), `refresh_media` (IG: re-read the post in an open IG tab's MAIN world to get fresh URLs), `hydrate_link` (IG shared links). IG tasks never open hidden tabs; without an IG tab the panel shows "N items waiting — open Instagram".
- **Sync from the web UI.** The Gallery sync button (UI-41) and the Connections rows send `sync.start` to the extension through `externally_connectable`; progress comes back over the server's SSE (`sync.progress`). Without the extension (phone, other browser) the button explains how to install and pair it.
- **Server control.** `/extension/config` carries `minVersion`, per-platform kill switches (`passive`, `replay`, `scroll`), pacing and stop thresholds. Data only, no remote code (Chrome Web Store policy).
- **Ingest wire format.**

```json
POST /api/v1/ingest/batches   (Bearer, Idempotency-Key)
{ "syncRunId": "01J…", "platform": "instagram",
  "source": { "kind": "ig_collection", "externalId": "17890…", "name": "Recipes" },
  "client": { "ext": "0.4.0", "parser": "2026.10.1" },
  "items": [ { "id": "3141…_123", "shortcode": "C…", "postUrl": "…", "text": "…",
               "timestamp": "…", "thumbnailUrl": "…",
               "media": [ { "type": "video", "url": "<poster>", "videoUrl": "<mp4>" } ] } ] }
→ 200 { "inserted": 12, "updated": 3, "known": 485,
        "rejected": [ { "index": 7, "code": "bad_url" } ], "keys": ["ig_3141…", "…"] }
```

The server re-validates everything with the Rust port of `sanitizeInterceptedBatch` (≤500 items per batch, id ≤256 chars, text ≤20k, http(s) URLs ≤4096 on the platform host allowlist, ≤60 media per post), stamps the batch platform on every item, canonicalizes keys, and applies the desktop merge rules (DATA-04: never clobber archived media, never overwrite a known date, AI fields only when unanalyzed).

**Distribution.** Chrome Web Store, **unlisted** listing (auto-updates, invite-only discovery), with a single-purpose description ("save your own bookmarks into your private Shelfy library"). Submit a minimal build in week 3 because review time is unknown (SPIKE-7). Fallback: a signed zip for "Load unpacked" (developer mode, manual updates). Firefox (AMO unlisted) after GA.

### 2.17 Mobile and the share sheet

- **Responsive shell.** Drawer sidebar under 900 px, bottom navigation (Library, Search, AI, Settings), stacked post modal, long-press to select, tap-to-preview instead of hover, pointer-based pinch on the canvas (05 §mobile).
- **PWA.** `vite-plugin-pwa`: precached app shell; `CacheFirst` for `/media/*.g480.webp` (≤3,000 entries, ≤150 MB); network-only for `/api/*`.
- **Android.** `share_target` with `method: GET` (`/share?url=&text=&title=`) for links; the SPA posts to `POST /links`. Shared files (images, PDFs) use a POST share target handled in the service worker, stored in IndexedDB, then uploaded with tus (P4).
- **iOS.** No Web Share Target: Settings offers an iOS Shortcut ("Save to Shelfy", accepts URLs from the share sheet, `POST /links` with a `links:create` token) and a bookmarklet.
- **Link hydration** (`link.hydrate`). The link becomes a post immediately (platform and native id parsed from the URL, card shows a placeholder). X and Pinterest are hydrated server-side from public, unauthenticated endpoints (oEmbed / Open Graph / Pinterest widget data — exact endpoints fixed in SPIKE-9). IG links are hydrated by the extension in an open IG tab; until then the card links to the original. Web URLs become `capture.site` jobs.

### 2.18 Capture service

**Code.** `capture/src/server.ts` (plain `node:http`, `zod` validation) wraps `captureSite()` from `electron/webcap/capture.ts`. `capture/src/env.ts` replaces every `electron.app.getPath` call (`driver.ts`, `blocker.ts`, `web-enrich.ts`, `webcapture.ts`) with configured paths, the ffmpeg path and the prebuilt adblock engine (`build/prepare-adblock.ts` output). Only the Playwright driver is used; `electron-driver.ts` and `system-chrome.ts` (visible anti-bot window) stay desktop-only, so a blocked site ends as `capture_blocked` with the og:image fallback.

**Protocol**

```
POST http://shelfy-capture:8080/v1/captures            X-Shelfy-Internal-Token: <secret>
{ "captureId": "01J…", "url": "https://…", "maxPages": 6, "singlePage": false,
  "video": true, "workDir": "/work/01J…" }
→ 200 application/x-ndjson, one JSON object per line:
  {"type":"event","kind":"read|artifact|info|error","code":"…","params":{…}}
  {"type":"page","index":0,"url":"…","pageType":"home","assets":[{"role":"hero","file":"p0-hero.webp","w":2880,"h":1800}, …]}
  {"type":"done","manifest":"manifest.json"} | {"type":"failed","code":"capture_blocked|timeout|…"}
```

- The API dispatches only when `GET /health` reports a free slot; closing the response stream cancels the capture (AbortController).
- `manifest.json` is also written to the work dir, so the API can finish the ingest if the stream breaks after completion.
- The API ingests artifacts into the user's CAS, creates `web_captures` + `web_capture_assets`, updates the post, deletes the work dir, then enqueues `ai.drain` when auto-analysis is on and a vision provider exists.

**Isolation**

| Control | Setting |
|---|---|
| Network | only `shelfy_capture` (`internal: true`): no route to the internet, the host, Tailscale or other containers except the API and the proxy |
| Egress | Chromium `--proxy-server=http://shelfy-egress:4750`, `--disable-quic`; Node fetches (discovery, og:image) use the same proxy; DNS is resolved by the proxy, so DNS rebinding cannot reach private ranges |
| Proxy policy | Smokescreen denies loopback, RFC 1918, link-local (incl. 169.254.169.254), plus explicit `--deny-range` for 100.64.0.0/10 (Tailscale/CGNAT), 198.18.0.0/15, 192.0.0.0/24, 224.0.0.0/4, 240.0.0.0/4, 64:ff9b::/96, 2002::/16; ports 80/443 only (SPIKE-4) |
| Process | non-root uid 10100, `read_only: true`, `tmpfs /tmp 1g`, `shm_size: 512m`, `cap_drop: [ALL]`, `no-new-privileges`, Chromium sandbox on with Playwright's seccomp profile (SPIKE-4; fallback: hardened `--no-sandbox` with everything else unchanged) |
| Content | service workers blocked, downloads refused, dialogs and popups refused (already in capture v2); no cookies are ever imported |
| Resources | 1 site at a time, 2 pages in parallel (1 when WebGL-heavy), page budget 150 s, site budget 10 min, artifacts ≤80 MB per site, page height ≤30k CSS px, ffmpeg `-threads 1`; the browser closes after 120 s idle |

**Image.** `node:24-bookworm-slim` + `playwright-core` pinned to the repo version + `chromium-headless-shell` via `playwright install --with-deps` + Debian `ffmpeg` + `fonts-noto-core`, `fonts-noto-color-emoji`, `fonts-liberation` (CJK fonts optional). Expected size ~600 MB.

### 2.19 Web client (SPA)

- **Seam.** `src/api/ShelfyClient.ts` defines the operations the UI needs (async, transport-neutral); `src/api/electronClient.ts` wraps `window.electronAPI`; `web/src/api/httpClient.ts` implements it with `fetch` + `EventSource` and the generated OpenAPI types. A `ShelfyProvider` context replaces direct `window.electronAPI` access in the 39 files that use it, one view at a time, keeping the desktop green after every step.
- **Capabilities.** `GET /me` returns `capabilities` (`extension`, `ai.tasks`, `capture`, `video.onDemand`, `admin`); desktop-only UI (window controls, updater, runtime, local models, open-in-Finder, webview fallback) is hidden by capability, not by `!!window.electronAPI` checks.
- **Routes** (`wouter`): `/`, `/c/:collectionId`, `/p/:key`, `/sites`, `/sites/:key`, `/ai/queue`, `/ai/search`, `/ai/tags`, `/jobs`, `/settings/:section`, `/invite/:token`, `/share`, `/admin`.
- **Settings sections.** Account (passkeys, sessions, email), Connections (extension pairing and status, iOS Shortcut token), AI providers, Storage (quota bar, archive preferences, kept videos), Language, Data (import, export, resets), Legal. Updates, Runtime and Performance cards are dropped.
- **Performance rules.** Grid tiles use `g480` + ThumbHash, `decoding="async"`, viewport-first `fetchpriority`; overscan reduced from 6 to 3 rows on touch devices; the loaded list is windowed (keep ±1,000 items around the viewport); no `backdrop-filter` per card (known scroll-performance trap); code-split AI, Websites, Settings and Admin; initial JS ≤220 KB gzip.
- **Behavior changes.** Hover video preview (UI-60) plays only cached or kept videos, otherwise the poster stays; "Downloaded / Link only" becomes "Stored / Link only"; "Open file" becomes "Download original".
- **Robustness.** React error boundary per view + `POST /client-errors` (rate-limited, no post content); SSE reconnect with backoff; optimistic updates with rollback kept from the desktop hooks.

### 2.20 Desktop convergence (after the AI phase)

1. **`crates/napi`** exposes `crates/core` (sync API) and `crates/ai`/`crates/media` (async through napi's tokio integration) as `shelfy_core.node` for darwin-arm64, win32-x64 and linux-x64. N-API is ABI-stable, so no `@electron/rebuild` is needed.
2. **Data.** On first launch of the converged desktop, the legacy reader (the same code as `shelfy-migrate`) converts `shelfy.sqlite` + `assets/` into `library.sqlite` + `media/` with the server's layout; the old files stay as a backup. `asset://` resolves CAS object ids instead of absolute paths.
3. **IPC.** Handlers in `electron/ipc.ts` call the core area by area (library → collections → tags → search → AI → web); the renderer already talks through `ShelfyClient`.
4. **AI.** The desktop uses the same provider layer; the local llama-server/whisper sidecars become an OpenAI-compatible "Local" provider preset, so local models remain a desktop feature without a second code path.
5. **Capture.** The desktop uses the same `capture/` package in a `utilityProcess` (fixes audit G1) and keeps its Electron/system-Chrome unblock drivers.
6. **Exit.** `electron/db.ts` reduced to an adapter (<500 lines); domain tests run once, in Rust, for both products; desktop e2e suite green.

### 2.21 Library choices

| Need | Choice | Why (one line) |
|---|---|---|
| HTTP server, SSE | `axum` + `tokio` + `tower-http` (compression, limits, timeouts, `ServeDir` with precompressed assets, Range) | de-facto standard stack, small footprint, first-class SSE and streaming bodies |
| SQLite | `rusqlite` (`bundled`: recent SQLite with FTS5, `contentless_delete`), `rusqlite_migration` | synchronous API that napi can expose unchanged to Electron; migrations embedded in the binary |
| OpenAPI | `utoipa` (+ `utoipa-axum`) → `openapi-typescript` | contract generated from the Rust types, TS types generated from the contract |
| Outbound HTTP | `reqwest` (rustls, HTTP/2, proxy) | one client, one proxy, no OpenSSL in the image |
| Passkeys | `webauthn-rs` | maintained, passkey-first API, no state serialization needed with in-memory challenges |
| Email | `lettre` (SMTP, STARTTLS) | provider-neutral; Resend today, any SMTP tomorrow |
| Secrets | `chacha20poly1305` (XChaCha20-Poly1305), `hkdf`, `sha2`, `secrecy`, `zeroize` | misuse-resistant AEAD with random 192-bit nonces; keys never linger in memory |
| Caches, limits | `moka`, `governor` | bounded TTL caches and token buckets without a Redis |
| Observability | `tracing` + `tracing-subscriber` (JSON), `metrics` + `metrics-exporter-prometheus` | structured logs and Prometheus metrics for the existing VictoriaMetrics |
| Images | `image` (decode, EXIF orientation), `fast_image_resize` (SIMD), `webp` (libwebp), `thumbhash` | fast, memory-light, one small C dependency |
| Concurrency utils | `tokio-util` (`CancellationToken`), `rayon` (2-thread image pool) | cooperative cancellation; CPU work off the async workers |
| Ids, errors | `ulid`, `thiserror`, `anyhow` | sortable ids; typed domain errors, contextual app errors |
| Tests | `proptest`, `insta`, `cargo-fuzz`, k6 | property, snapshot and fuzz tests; scriptable load tests |
| Desktop bindings | `napi-rs` | ABI-stable N-API, async through tokio, no Electron rebuilds |
| Extension build | `esbuild` | already used for `dist-electron`; four small bundles |
| Capture service | `node:http` + `zod`, existing `playwright-core` | no framework needed for one streaming endpoint |
| SPA additions | `wouter`, `vite-plugin-pwa`, `tus-js-client`, `thumbhash` (npm) | tiny router, PWA/share target, resumable uploads, placeholder decoding |
| Egress proxy | Smokescreen | DNS-resolving CONNECT proxy built for SSRF egress control |

---

## 3. Deployment and operations on the CX33

**Current osn baseline (2026-10-02).** The osn refactor is merged and applied:
- AppFlowy was removed entirely, and the repo is now `niccolofanton/osn`.
- The compose project is `osn` (`/opt/osn/stack`): edge, Grafana, VictoriaMetrics, node-exporter, cAdvisor, blackbox-exporter and Hermes.
- The tunnel serves only `grafana.niccolofanton.dev`.
- Disk alerting is a single Grafana rule (root disk ≥ 85 % used).
- **No backup job runs today**: the AppFlowy-only backup role was deleted.
- Usable disk: ~33 GB free, plus ~15 GB of unused images that can be reclaimed.

All osn changes are prepared in `deploy/osn/` inside the Shelfy repo and land as **two osn PRs** (P1: `shelfy-api`, edge route, DNS, secrets, backups, monitoring; P4: `shelfy-capture`, `shelfy-egress`, seccomp profile); Shelfy work never edits osn directly. Until then the stack runs locally with `deploy/compose.dev.yml` and on throwaway CX33s for spikes and capacity tests.

### 3.1 Resource budget

| Service | Memory limit | CPU limit | `cpu_shares` | `oom_score_adj` | Typical use |
|---|---|---|---|---|---|
| hermes (existing) | 2 GiB | 2.0 | 1024 | 0 | unchanged |
| observability (existing, 5 containers) | ~1.5 GiB total | — | 1024 | 0 | unchanged |
| `shelfy-api` | 768 MiB (`memswap_limit` 1 GiB) | 1.5 | 1024 | -100 | 60–250 MiB RSS; ffmpeg/yt-dlp children count toward it |
| `shelfy-capture` | 1.5 GiB (no swap) | 1.5 | 256 | 600 | ~120 MiB idle (browser closed), 0.6–1.2 GiB while capturing |
| `shelfy-egress` | 96 MiB | 0.5 | 512 | 0 | ~20 MiB |

Shelfy adds 2.4 GiB of limits; expected peak use is ~2 GiB, inside the ~5.8 GiB currently available. Under memory pressure the kernel kills capture first (a capture job is simply retried), never Hermes or the API.

| Disk (of ~48 GB usable) | Budget | Enforcement |
|---|---|---|
| user libraries (`users/`) | 30 GB | per-user quotas; `SHELFY_MEDIA_BUDGET_GB` refuses new archives globally at 100 % |
| video cache | 5 GB | LRU trim (nightly and on insert) |
| work, uploads, exports, backup staging | 3 GB | TTL sweeps |
| headroom (OS, images, logs, metrics growth) | ≥10 GB | the existing osn root-disk alert (≥ 85 % used), plus a critical rule at 90 % added in PR 1 |

### 3.2 Compose services (added to osn `compose.yml`)

```yaml
  shelfy-api:
    image: ghcr.io/niccolofanton/shelfy-api:${SHELFY_VERSION:?}
    restart: unless-stopped
    user: "10100:10100"
    read_only: true
    tmpfs: ["/tmp:size=256m,mode=1777"]
    environment:
      - SHELFY_PUBLIC_URL=${SHELFY_PUBLIC_URL}
      - SHELFY_DATA_DIR=/data/shelfy
      - SHELFY_CAPTURE_URL=http://shelfy-capture:8080
      - SHELFY_EGRESS_PROXY=http://shelfy-egress:4750
      - SHELFY_SMTP_HOST=smtp.resend.com:587
      - SHELFY_SMTP_USER=resend
      - SHELFY_SMTP_PASSWORD=${RESEND_API_KEY}
      - SHELFY_SMTP_FROM=${SHELFY_SMTP_FROM}
      - SHELFY_MASTER_KEY=${SHELFY_MASTER_KEY}
      - SHELFY_INTERNAL_TOKEN=${SHELFY_INTERNAL_TOKEN}
      - SHELFY_MEDIA_BUDGET_GB=30
      - SHELFY_VIDEO_CACHE_GB=5
      - RUST_LOG=info
    volumes:
      - /data/shelfy:/data/shelfy
    networks: [edge, internal, shelfy_capture]
    mem_limit: 768m
    memswap_limit: 1g
    cpus: 1.5
    cpu_shares: 1024
    pids_limit: 256
    oom_score_adj: -100
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]
    stop_grace_period: 30s
    healthcheck:
      test: ["CMD", "/app/shelfy-server", "healthcheck"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 10s

  shelfy-capture:
    image: ghcr.io/niccolofanton/shelfy-capture:${SHELFY_VERSION:?}
    restart: unless-stopped
    user: "10100:10100"
    read_only: true
    tmpfs: ["/tmp:size=1g,mode=1777"]
    shm_size: 512m
    environment:
      - HOME=/tmp                       # Chromium profile and font cache on tmpfs (read-only root)
      - CAPTURE_WORK_DIR=/work
      - CAPTURE_PROXY=http://shelfy-egress:4750
      - CAPTURE_INTERNAL_TOKEN=${SHELFY_INTERNAL_TOKEN}
      - CAPTURE_SITES_PARALLEL=1
      - CAPTURE_VIDEO=true
    volumes:
      - /data/shelfy/work/capture:/work
    networks: [shelfy_capture]
    mem_limit: 1536m
    memswap_limit: 1536m
    cpus: 1.5
    cpu_shares: 256
    pids_limit: 1024
    oom_score_adj: 600
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true", "seccomp=/opt/osn/stack/shelfy/chromium-seccomp.json"]
    stop_grace_period: 20s

  shelfy-egress:
    image: ghcr.io/niccolofanton/shelfy-egress:${SHELFY_EGRESS_VERSION:?}
    restart: unless-stopped
    user: "10101:10101"
    read_only: true
    command: ["--listen-port=4750", "--deny-range=100.64.0.0/10", "--deny-range=198.18.0.0/15",
              "--deny-range=192.0.0.0/24", "--deny-range=224.0.0.0/4", "--deny-range=240.0.0.0/4",
              "--deny-range=64:ff9b::/96", "--deny-range=2002::/16"]
    networks: [shelfy_capture, shelfy_egress]
    mem_limit: 96m
    cpus: 0.5
    cap_drop: [ALL]
    security_opt: ["no-new-privileges:true"]

networks:
  shelfy_capture: { driver: bridge, internal: true }
  shelfy_egress: { driver: bridge }
```

The API joins `edge` (reached by nginx), `internal` (scraped by VictoriaMetrics, probed by blackbox) and `shelfy_capture` (talks to capture and the proxy). Its own outbound HTTP is configured to use the proxy; a unit test fails the build if any HTTP client is constructed without it.

**Images.** `shelfy-api`: multi-stage (`cargo-chef`, `rust:<pinned>-bookworm` builder) → `debian:bookworm-slim` with `ca-certificates`, `tini`, Debian `ffmpeg`, the pinned `yt-dlp_linux` binary (SHA-256 checked), the binary and `web/dist`; ~180 MB. `shelfy-egress`: Smokescreen built from a pinned commit on `golang` → `distroless/static`; ~20 MB.

### 3.3 Edge, tunnel and DNS

- **Hostname.** `shelfy.niccolofanton.dev` (first-level, so Universal SSL covers it; a second-level name such as `media.shelfy.…` would not be). Add it to `osn_public_hostnames` and a proxied CNAME in `tofu/cloudflare-access/dns.tf`.
- **nginx server block** (added to `edge/nginx.conf`, same pattern as Grafana):

```nginx
    server {
        listen 80;
        server_name shelfy.niccolofanton.dev;
        client_max_body_size 20m;           # tus chunks are 16 MiB; the app enforces per-route limits
        location / {
            set $upstream_shelfy "http://shelfy-api:8080";
            proxy_pass $upstream_shelfy;
            proxy_http_version 1.1;
            proxy_set_header Host $host;
            proxy_set_header X-Forwarded-Proto https;
            proxy_set_header X-Forwarded-Host $host;
            proxy_request_buffering off;     # stream uploads straight to the app
            proxy_read_timeout 3600s;        # SSE and chat streams; the app sends X-Accel-Buffering: no
            proxy_send_timeout 3600s;
        }
    }
```

- **Metrics** listen on a separate port (`:9464`) that only the `internal` network reaches; nginx never proxies it.
- **Portability.** Nothing in the application calls a Cloudflare API: replacing the tunnel with any reverse proxy and ACME certificates needs no code change.
- **Cloudflare (edge only).** Always Use HTTPS; Brotli; a cache rule for `/assets/*` (hashed, `immutable`) and bypass for everything else; one rate-limiting rule on `/api/v1/auth/*`; confirm that bot protection does not challenge extension or Shortcut API calls (SPIKE-10). HSTS is sent by the app for this host only. Limits designed around: 100 MB request body (uploads are chunked), 100 s origin timeout (long work is a job; streams heartbeat every 20 s).

### 3.4 Secrets

| Key (in `secrets/osn.sops.yaml`) | Use | Notes |
|---|---|---|
| `SHELFY_MASTER_KEY` | seals BYOK provider keys (HKDF per user, XChaCha20-Poly1305) | 32 random bytes, base64; also stored in the operator's password manager; never in backups |
| `SHELFY_INTERNAL_TOKEN` | API ↔ capture service | 32 random bytes, hex |
| `SHELFY_RESTIC_PASSWORD` | restic repo `restic/shelfy` | separate from the legacy `restic/osn` password |
| reused: `RESEND_API_KEY` | SMTP | already present |
| `R2_BUCKET`, `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `R2_ENDPOINT` | restic → R2 | **replace**: the old values point to `coolify-backups`, which was the AppFlowy bucket and is being deleted. Create bucket `osn-backups` and an R2 API token scoped to it (Object Read & Write), then update SOPS. |

Non-secret values go to `.env.example` / `env.j2`: `SHELFY_VERSION`, `SHELFY_EGRESS_VERSION`, `SHELFY_PUBLIC_URL`, `SHELFY_SMTP_FROM`. Chrome Web Store publishing credentials live only in GitHub Actions secrets of the Shelfy repo. Rotation: master key by `key_version` + `shelfy-server admin rekey`; internal token by redeploying both services; session secrets need none (hashed random ids).

### 3.5 Backups and restore

| Timer (host systemd, new `shelfy` Ansible role) | Schedule | Action | Metric (textfile collector) |
|---|---|---|---|
| `shelfy-db-snapshot` | hourly at :05 | `shelfy-server admin snapshot --changed --out /data/shelfy/backup-staging/db` (SQLite online backup API, only DBs changed since the last run), then `restic backup … --tag db` | `shelfy_backup_last_success{set="db"}`, `…_timestamp_seconds` |
| `shelfy-media-backup` | daily 03:30 UTC | `restic backup /data/shelfy/users --exclude '*.sqlite*' --exclude 'exports' --tag media` (CAS files never change, so dedupe keeps it cheap) | `set="media"` |
| `shelfy-restic-maintenance` | Sundays 04:30 | `forget --prune` (db: 48 hourly, 14 daily, 8 weekly, 6 monthly; media: 7 daily, 8 weekly, 6 monthly) + `check --read-data-subset=5%` | `shelfy_restic_check_last_success` |
| `shelfy-restore-drill` | monthly | restore the latest control DB + one random user DB to a temp dir, `PRAGMA integrity_check`, `admin verify` (row counts vs live, CAS references resolvable) | `shelfy_restore_drill_last_success` |

All run with `nice -n 19 ionice -c3` and restic `--limit-upload 20480`. Not backed up: the video cache, work dirs, exports (re-creatable). Targets: **RPO** 1 h for libraries and accounts, 24 h for media; **RTO** 2 h for full host loss.

**Restore runbooks** (`doc/RUNBOOK-shelfy.md` in osn):
- *One user, point in time:* `admin user lock <id>` → `restic restore <snapshot> --include …/users/<id>.sqlite` → `admin user restore-db <id> <file>` (integrity check, atomic swap, keeps the old file) → unlock; objects missing from the CAS are re-archived from live URLs or by the extension.
- *Full host:* new CX33 from `cloud-init.yaml` → `just bootstrap` → `restic restore` (db, media) → `admin install-snapshots` → `just deploy` → `just check`. The tunnel credentials come from SOPS, so DNS does not change.

### 3.6 Monitoring and alerting

**Metrics** (Prometheus format on `:9464`, no per-user labels): `shelfy_http_requests_total{route,method,status}`, `shelfy_http_request_duration_seconds{route}`, `shelfy_sse_connections`, `shelfy_jobs{kind,state}`, `shelfy_job_oldest_queued_seconds{kind}`, `shelfy_job_duration_seconds{kind,outcome}`, `shelfy_ai_requests_total{provider_kind,task,outcome}`, `shelfy_ai_tokens_total{direction}`, `shelfy_media_fetch_total{host_group,outcome}`, `shelfy_breaker_open{host_group}`, `shelfy_disk_bytes{area}`, `shelfy_open_user_dbs`, `shelfy_sqlite_busy_total`, `shelfy_capture_duration_seconds{outcome}`, `shelfy_capture_peak_rss_bytes`; plus the textfile backup metrics. Scrape: add `shelfy-api:9464` to `observability/victoriametrics/scrape.yml`; blackbox targets `http://shelfy-api:8080/health` and `http://shelfy-api:8080/health/capture` (blackbox cannot reach the internal capture network, so the API reports it).

**Dashboard** `03-shelfy.json`: request rate and p50/p95/p99 per route group; SSE connections; queue depth and oldest age per kind; job outcomes; AI calls, errors and tokens; media fetch outcomes and breaker state; capture durations and peak RSS; disk by area vs budget; container CPU/memory for api, capture, egress and Hermes side by side; backup ages.

| Alert | Condition | For | Severity |
|---|---|---|---|
| Shelfy DOWN / capture DOWN | existing `osn_probe_down` with the two new targets | 2m | critical |
| Queue stuck | `shelfy_job_oldest_queued_seconds{kind!="capture.site"} > 1800` (capture: > 7200) | 10m | warning |
| Job failures | failed / all jobs over 1 h > 20 % | 30m | warning |
| Slow API | p95 of non-stream routes > 500 ms | 10m | warning |
| Media breaker open | `shelfy_breaker_open == 1` | 30m | info |
| Disk | `shelfy_disk_bytes{area="users"}` > 85 % of budget; root disk via the existing rule | 15m | warning / critical |
| Backups stale | db > 3 h, media > 36 h, restore drill > 35 days | 10m | critical |
| Memory | container working set > 90 % of limit (api, capture) | 10m | warning |
| Host contention | host memory available < 800 MB or load > 6 | 15m | warning |

### 3.7 Logging

`tracing` JSON lines to stdout (Docker `json-file`, 10 MB × 3 per container as configured in osn). Fields: time, level, target, request id, route, status, latency, user ULID. Never logged: captions, post URLs, query strings, tokens, provider keys, email bodies (enforced by typed redaction wrappers and a test that greps a log capture for planted secrets). `just logs shelfy-api`.

### 3.8 CI/CD and releases

| Workflow (Shelfy repo) | Trigger | Steps |
|---|---|---|
| `ci.yml` | PR, push | Rust: fmt, clippy `-D warnings`, tests, `cargo deny`, `cargo audit`; TS: typecheck, lint, vitest (desktop + web + extension + capture); OpenAPI drift check (generated TS client committed); golden-fixture parity; extension e2e (Playwright persistent context with `--load-extension` against recorded fixtures); capture fixture e2e; web e2e against `deploy/compose.test.yml` (mock AI provider, fixture CDN, Smokescreen allowing only the fixture subnet); bundle-size budget |
| `release-server.yml` | tag `server-v*` | build `shelfy-api`, `shelfy-capture`, `shelfy-egress` for linux/amd64 → GHCR (`:vX.Y.Z` and `:sha-…`), SBOM (syft), cosign keyless signature |
| `release-extension.yml` | tag `ext-v*` | build, zip, upload to the Chrome Web Store (unlisted) |
| desktop release | unchanged | — |

- **Branches.** Keep the repo's model: `dev` = beta, `main` = stable, protected `main` with the `test` check. Server releases are cut from `main`.
- **Deploy.** Bump `SHELFY_VERSION` in osn `.env`/`env.j2` → `just apply "--tags app"` (or a `just shelfy-deploy <tag>` recipe) → healthcheck gate → `just check`. A restart is ~1 s; SSE clients reconnect; a running capture is retried.
- **Rollback.** Re-deploy the previous tag. Migrations follow expand/contract: release N only adds; anything removed in N is dropped in N+1, so N-1 always runs on N's schema. User DBs migrate lazily on open plus a low-priority sweep after boot; the control DB migrates at boot.
- **Release candidates** run the k6 capacity scenario on a throwaway CX33 (§6.3) before tagging.

### 3.9 Runbooks to write (osn `doc/RUNBOOK-shelfy.md`)

Deploy and rollback · invite, disable or delete a user · revoke extension/Shortcut tokens · restore one user · full restore · disk pressure (trim caches, raise quotas, attach a Hetzner Volume and move `/data/shelfy`) · capture stuck or Chromium crash-looping · platform parser broken (flip the kill switch in `/admin/flags`, ship an extension fix) · AI provider outage · master-key rotation · suspected account compromise (kill sessions, revoke tokens, audit log).

---

## 4. Migration of existing desktop libraries

### 4.1 Flow

1. **Get the tool.** `shelfy-migrate` binaries (macOS arm64/x64, Windows x64, Linux x64) are attached to `server-v*` releases. In P6 the same code runs from the desktop menu ("Move library to Shelfy Web") through napi.
2. **Authorize.** `shelfy-migrate login https://shelfy.niccolofanton.dev` prints a URL + code; the user approves it in the logged-in SPA, which mints a `migrate` token (7 days).
3. **Plan (dry run).** `shelfy-migrate plan [--user-data <dir>]` opens the desktop `shelfy.sqlite` read-only (refuses if the desktop app holds it open for writing) and prints: posts per platform, IG duplicate groups to merge, files found/missing, bytes to upload by class, videos excluded by default, and the result against the server quota.
4. **Run.** `shelfy-migrate run [--with-videos] [--merge]` builds a bundle in a temp dir — a new-schema `library.sqlite` plus CAS objects (each file hashed once) — then asks `POST /migrations/missing-objects` which hashes the server lacks, uploads only those through tus (resumable; re-running continues), uploads the DB last, and calls `POST /migrations`.
5. **Install** (server job `migrate`): `PRAGMA integrity_check`, schema version check, row and size limits, every referenced object present with a matching hash. An empty web library is replaced atomically; a non-empty one is merged through the normal ingest merge rules (`--merge`). Afterwards the job rebuilds FTS, renders missing `g480`/ThumbHash, and queues archive work for posts whose files were missing.

### 4.2 Field mapping

| Desktop | Web | Rule |
|---|---|---|
| `posts.id` (IG `<pk>_<owner>`, `pk` or shortcode) | `key = ig_<pk>`, `native_id = pk` | split or decode; rows mapping to the same pk merge (keep the row with archived files, then AI, then user layer; union collections, notes concatenated with a separator if both exist) |
| `posts.id` X / Pinterest | `x_<id>` / `pin_<id>` | — |
| `web:<sha1>` | `web_<sha1>` of the scheme-less normalized URL | recomputed; http/https twins merge |
| `manual:<uuid>` | `m_<ulid>` | new id; original kept in `meta` for traceability |
| `timestamp` (ISO, `''`, NULL) | `posted_at` ms / NULL; `sort_ts` | invalid strings → NULL |
| `thumbnail_path`, `image_path`, `video_path`, `preview_path`, `post_media.local_path`, manual `source_url` | `media_objects` + `cover_object` / `post_media.object_id` / `video_object_id` | hash the file; missing file → item stays pending for re-archive; videos only with `--with-videos` |
| `thumb_blur` | `thumbhash` | recomputed from the cover |
| `ai_*` JSON columns | `ai_*` (`ai_provider = 'desktop-local'`, `ai_schema_version = 1`) | verbatim |
| `post_tags` (tier general/specific/manual/NULL) | `post_tags` (`source` ai/manual, `tier`) | manual tier → `source='manual'`; NULL tier → `source='ai', tier=NULL` |
| `user_note`, `user_tags` | same | verbatim |
| `post_entities`, `tag_alias`, `tag_cluster*` | same | verbatim |
| `collections` (+ `platform`, `external_id`, `ig_name`) | `collections` (`source_name = ig_name`) | duplicates on `(platform, external_id)` merge |
| `web_*` columns + `web_snapshots` | `web_captures` + `web_capture_assets` | screenshot paths inside `web_pages_json` and chunks become CAS objects; current version → `current_capture_id` |
| `jobs`, `downloads` | — | dropped |
| localStorage/userData settings | `settings` | language and asset preferences only (others are desktop-only) |

### 4.3 Safety

The desktop data is never modified. The bundle is validated twice (CLI and server). Install is atomic (new DB file + rename) and the previous web library, if any, is kept for 7 days. The tool prints a reconciliation report (counts in, counts out, merges, missing files) that is also stored as a notification.

### 4.4 Reference library sizing

The reference library (Appendix C: 6,138 posts, 75 % video posts) migrates as ~1 GB of covers and images plus ~90 MB of DB; its 4 GB of downloaded videos are left out unless `--with-videos` is passed and the quota allows it. Upload time at 20 Mbit/s uplink: ~7 minutes without videos.

2,037 of its posts have no local cover at all (1,861 IG, 176 X). X covers on `pbs.twimg.com` do not expire and are archived server-side after install; most IG cover URLs of that age carry expired signatures, so they become `refresh_media` extension tasks that drain the next time the user has Instagram open (paced like a normal sync). Until then those cards show the platform fallback (UI-64), as they do on the desktop today.

---

## 5. Roadmap

Effort assumes one senior engineer comfortable with Rust and TypeScript. With two engineers (backend/Rust and extension/SPA in parallel) the GA date moves from ~21 to ~12–13 weeks.

| Phase | Effort | Main deliverables | Exit criteria |
|---|---|---|---|
| **P0 Foundations and spikes** | 2 wk | workspace, CI, schemas v1, legacy reader, read-only API slice, SPA slice, owner login (magic link), SPIKE-1, 2, 3, 5, 10 | §10 exit: the reference library, migrated locally, is browsable and searchable in the web SPA; spike reports committed |
| **P1 Library on the web** | 4 wk | migration CLI + install job; full library API (filters, FTS search, collections incl. remove, notes, manual tags, manual AI edits, trash, bulk-by-selector, stats); CAS + renditions + media serving; SSE; SPA on `ShelfyClient` for Gallery, Post modal, Sidebar, Settings (account, language, storage, legal); responsive shell; invites and passkeys (SPIKE-8); first osn PR (compose, edge, DNS, Access, scrape, alerts, backups) | owner uses the web daily on desktop and phone; budgets in §6.2 met on the reference library on the VPS; first restore drill green; search-eval gate passed |
| **P2 Extension sync and archiving** | 5 wk | SPIKE-9 first; MV3 extension (hook, bridge, SW, side panel, pairing, IG/X/Pinterest sync, source sync, selection overlay, folder/board mapping, incremental stop, kill switches); ingest API; archive worker with breaker and extension upload tasks; sync runs + Activity center on SSE; PWA, Android share target, iOS Shortcut, link hydration; Chrome Web Store unlisted submission; Access removed; closed beta with 2–3 invitees | a fresh invitee syncs all three platforms end to end; ≥99 % of the items the desktop captures on the same listings; ≥98 % of covers archived within 15 min of ingest; a daily incremental sync touches ≤2 pages per source |
| **P3 AI (BYOK)** | 4 wk | SPIKE-6 first; provider layer, key vault, onboarding wizard; `ai.drain` cataloging (social + web, schema v2); AI queue view with live stream; chat search over SSE; suggestion chips; tags explorer, health, merge, rename; clusters and aliases jobs; dictation; cost estimate and usage; `shared/ai/` read by the desktop too | `extract-eval` on two providers within ±5 % of the desktop baseline; the owner catalogs ≥5k posts with zero stuck items; chat search works with and without a provider (fallback); `cluster-eval` ARI ≥ desktop − 0.05 |
| **P4 Websites, bookmarks, data, video** | 4 wk | SPIKE-4 and SPIKE-11 first; capture service + egress proxy + isolation (second osn PR); Websites view (paginated) + detail + versions + delete modes + recapture; manual bookmarks via tus (+ Android file share); on-demand video chain + "keep offline"; Jobs view (replaces Downloads); import v1 (desktop export, bare arrays, Chrome-extension IG/X exports) and v2, export v2 bundle; quotas and GC; danger zone; feedback | SSRF suite green (§6.1); capture success on the audit corpus ≥ desktop with zero sandbox/egress exceptions; on-demand video success rate per platform measured and documented; export → import round trip lossless |
| **P5 Hardening and GA** | 2 wk | authz matrix tests, ZAP baseline, dependency audit; capacity run on a throwaway CX33; chaos tests (kill -9 during ingest, capture, AI); alert tuning; runbooks; privacy notice and disclaimer v2; Chrome Web Store listing live; invite 5–10 users | all §6.2 budgets green under the §6.3 scenario; 7 days without a critical alert; restore drill green; zero open high-severity findings |
| **P6 Desktop convergence** | 6–8 wk | §2.20 | desktop runs on `crates/core`; `electron/db.ts` <500 lines; shared domain tests; desktop e2e green |

P6 starts after P3 ("after the AI phase") if a second engineer is available; otherwise after P5. Parallel tracks with two engineers: Rust core/server (P1 API, P2 ingest + archive, P3 AI, P4 jobs/video/import) and TS (P1 SPA seam, P2 extension, P3 AI UI, P4 capture service + Websites UI).

---

## 6. Testing and performance verification

### 6.1 Test strategy

| Layer | Tooling | What is covered |
|---|---|---|
| Core (Rust) | `cargo test`, `proptest`, `insta` | repositories, merge rules, canonical ids (shortcode ↔ pk), URL normalization, sanitizer, FTS query builder, tag normalization and aliases, clustering, AI output normalization; migrations upgrade from every committed schema fixture |
| Parity | golden fixtures | `scripts/golden/*.ts` runs the desktop TS functions (`sanitizeInterceptedBatch`, `normalizeWebUrl`/`webPostId`, `extractContentTerms`, `bulkUpsert` merge on an in-memory DB, alias resolution, `cleanStringArray`, `validateAliasPairs`, `buildTagCommunities`, `parseTagBlock`, `igDateFromShortcode`) on fixture inputs; outputs in `shared/golden/`; Rust must match byte for byte until P6 deletes the TS copies |
| Quality gates | `search-eval`, `extract-eval`, `cluster-eval` | relevance (nDCG@10, MRR), extraction quality per provider, cluster ARI/purity; run in CI with the mock provider and manually with real keys before prompt changes |
| Server | axum `Router` + `tower::ServiceExt`, temp data dirs | every route: auth required, wrong-user access returns 404 (authz matrix generated from the OpenAPI document), limits, idempotency, ETag/304, SSE ordering and resync |
| Security | dedicated suites | SSRF (private, CGNAT, metadata, IPv4-mapped IPv6, decimal/octal IPs, redirects to private IPs, DNS rebinding via a test resolver) against the real Smokescreen config; upload magic-byte and size fuzzing (`cargo fuzz` on parsers); CSP and headers check; log-redaction test |
| AI | mock provider (axum) | schema-strict and `json_object` paths, streaming, 429/5xx/`Retry-After`, invalid key → queue paused, circuit breaker |
| Extension | vitest + Playwright (`launchPersistentContext` with `--load-extension`) | parsers on recorded, scrubbed platform responses (`tests/fixtures/platforms/…`); fixture pages that replay those responses; passive scoping; incremental stop; pairing; offline queue |
| Capture | vitest + fixture sites | the existing `scripts/web-capture-eval` fixtures (ScrollSmoother, Locomotive, Lenis, WebGL, pinned sections) through the service protocol |
| SPA | vitest + Testing Library (existing suites with a mock `ShelfyClient`), Playwright e2e | gallery, modal, selection, bulk actions, AI views, Websites, settings, mobile viewport |
| Resilience | scripted | kill -9 during ingest, archive, AI drain and capture → no lost or duplicated items; restore drill |
| Real platforms | manual weekly checklist by the owner | IG saved + folder, X bookmarks, Pinterest board on real accounts, latest Chrome stable |

### 6.2 Performance budgets

| Metric | Budget | Measured by |
|---|---|---|
| `GET /posts` page (60 items), 20k-post library, server time | p95 ≤ 40 ms, p99 ≤ 100 ms | route histogram |
| `GET /search` (FTS + filters) | p95 ≤ 60 ms | route histogram |
| `GET /posts/{key}` | p95 ≤ 15 ms | route histogram |
| `POST /ingest/batches` (500 items) | p95 ≤ 300 ms | route histogram |
| Media rendition served by the API | p95 ≤ 5 ms server time | route histogram |
| Write → SSE delivered | p95 ≤ 300 ms | e2e probe |
| TTFB through Cloudflare from the EU | p95 ≤ 250 ms | k6 |
| Gallery LCP, mobile profile (Lighthouse, Moto G Power, slow 4G) | ≤ 2.5 s | Lighthouse CI |
| Initial JS | ≤ 220 KB gzip | size-limit in CI |
| Grid scroll with real wheel events | no frame > 50 ms, ≥ 55 fps median | Playwright trace (`perf:gallery`) |
| `g480` rendition size | p50 ≤ 35 KB, p95 ≤ 60 KB | ingest metric |
| `shelfy-api` RSS with 100 active users | ≤ 300 MB | cAdvisor |
| Website capture (6 pages) | p50 ≤ 4 min, p95 ≤ 8 min, peak RSS ≤ 1.2 GB | capture metrics |
| Archive throughput during onboarding | ≥ 5 images/s sustained | metrics |
| API restart to healthy | ≤ 3 s | deploy log |

### 6.3 Load and capacity test

Run on two throwaway CX33s (server and load generator), Ubuntu 26.04, the release images and the osn-equivalent compose:

1. **Seed.** `shelfy-server admin synth --users 100 --posts 5000 --profile reference` creates libraries with the reference distribution (platform mix, media types, caption lengths, tags), real 25 KB `g480` files and sparse placeholder masters.
2. **Contention.** A `stress-ng --cpu 2 --vm 1 --vm-bytes 1500M` container with `cpu_shares: 1024` stands in for Hermes.
3. **Scenarios (k6), concurrently.** A: 100 virtual users browsing (list, scroll pages, search, open posts, open modal media; think time 2–5 s). B: an extension simulator ingesting 20k items in 500-item batches. C: `ai.drain` for 2,000 posts against the mock provider (2 s latency, 32 in flight). D: six fixture-site captures back to back. E: 300 idle SSE connections.
4. **Pass.** Every §6.2 server budget holds; zero 5xx; memory within limits; the stand-in Hermes keeps ≥1.8 CPU when it asks for it; no queue older than its alert threshold.

### 6.4 Capacity model

**Inputs** (Appendix C, measured on the reference library): 6,138 posts; 75 % video posts; 11,482 slides (5,855 images, 5,627 videos); desktop originals average 192 KB per cover, 401 KB per image, 14.8 MB per video; desktop DB 46 MB.

**Per-user cost under policy D4**

| Item | Unit cost | Reference library |
|---|---|---|
| Video poster (WebP ≤1080 px) + `g480` | ~0.115 MB per video post | 4,631 × 0.115 ≈ 0.53 GB |
| Image slide (bounded source) | ~0.30 MB | 5,855 × 0.30 ≈ 1.76 GB |
| `g480` for image-post covers and carousel slides 1–3 | ~0.025 MB each | ≈ 0.10 GB |
| DB + FTS | ~15 KB per post | ≈ 0.09 GB |
| **Total** | **≈0.40 MB per post** | **≈2.5 GB** (vs ~68 GB if every video were archived) |

| Resource | Limit for Shelfy | Per-user demand | Users supported | Binding? |
|---|---|---|---|---|
| Disk (`users/` budget 30 GB) | 30 GB | median 5k posts ≈ 2.0 GB; power 20k ≈ 8 GB | **~15 median or ~3–4 power users (~10–12 in a realistic mix)** | **yes** |
| Memory (api 768 MiB) | ~500 MB usable | ≤4 MB per open library + ~1 MB per SSE client | >100 concurrently active | no |
| CPU, interactive | ~1 core | ~1–2 ms per request; ~60 req/s for 20 active users ≈ 5–10 % of a core | >100 | no |
| CPU, onboarding | bursty | ~3 core-min per 5k-post library; ~20 min wall | queue absorbs ~1 new user/hour | no |
| CPU, AI | — | ~40 ms server CPU per post (provider-bound wall time) | >100 | no |
| CPU, capture | 1.5 cores | ~3–4 core-min per site (SPIKE-11) → ~20–25 sites/h | ~100 users at ≤2 sites/user/day | at ~100 users, as queue latency |
| Traffic | 20 TB/month | ~1–2 GB per user per month | thousands | no |

**Conclusion.** On today's disk the VPS serves about a dozen users; disk binds an order of magnitude before CPU or RAM. Scale steps, in order: (1) attach a Hetzner Volume and move `/data/shelfy` (a 200 GB volume raises the limit to ~100 median users; priced per GB-month, verify the current rate); (2) measure cross-user duplicate media and AVIF masters (−30–50 % bytes at ~10× encode CPU) before adopting either; (3) beyond ~100 users, move `shelfy-capture` to a second small server over the same HTTP protocol (WireGuard/Tailscale) and schedule bulk work off-peak; (4) per-user directories make moving users between hosts a file copy plus one control-DB row.

---

## 7. Security and privacy

### 7.1 Threat model and controls

| Surface | Threat | Controls |
|---|---|---|
| Accounts | phishing, credential stuffing, session theft | passkeys (phishing-resistant), no passwords, single-use rate-limited magic links, `__Host-` HttpOnly SameSite=Lax cookie, re-auth for destructive actions, session list with remote logout |
| Tenant isolation | user A reads or writes user B's data | the library DB and media root are derived only from the authenticated principal; no user id in URLs; CAS names are hex hashes; generated authz-matrix tests on every route |
| Extension / Shortcut tokens | token stolen from a browser profile or phone | narrow scopes (ingest, tasks, uploads, lookup / `links:create` only), hashed at rest, per-device revocation, last-used shown, rate limits; tokens cannot reach cookie routes |
| Ingest | hostile page payloads (XSS on a platform, forged `postMessage`) | Rust port of the sanitizer, platform host allowlists, size caps, platform stamped server-side, no client-supplied storage keys or paths, text rendered escaped |
| Capture | SSRF to metadata/internal services, Chromium exploit, resource exhaustion, abusive content | `internal: true` network + Smokescreen, sandbox + seccomp, non-root, read-only root fs, no capabilities, budgets and timeouts, per-user daily limit, takedown process |
| Uploads | XSS through SVG/HTML/PDF, polyglots, decompression bombs | magic-byte sniffing, MIME allowlist, size and count caps, decode with dimension limits, same-origin media with `Content-Security-Policy: sandbox`, `nosniff`, `Content-Disposition: attachment` for non-image/video |
| AI keys | leak through logs, DB or backup theft | per-user sealing with XChaCha20-Poly1305, master key only in SOPS and the container env, never returned or logged, zeroized; backups never contain the master key |
| AI outputs | prompt injection through captions or page text | untrusted-data markers, `sanitizeForPrompt`, strict schemas, validated outputs, no tool execution |
| SPA | XSS, clickjacking | CSP `default-src 'self'; img-src 'self' data: blob:; media-src 'self' blob:; connect-src 'self'; frame-ancestors 'none'; object-src 'none'; base-uri 'none'; form-action 'self'`; zero third-party requests |
| CSRF | cross-site state changes | SameSite=Lax + `X-Shelfy-Client` header + `Origin` check |
| Outbound HTTP | SSRF through media URLs or custom AI base URLs | every client goes through the proxy; DNS resolved by the proxy; redirects re-checked there |
| Host | exposed ports, lateral movement | osn baseline: tunnel only, ufw + `DOCKER-USER` drops, SSH keys only, unattended security updates; containers non-root, `cap_drop: ALL`, `no-new-privileges` |
| Supply chain | compromised dependencies or binaries | lockfiles, `cargo deny`/`cargo audit`, `pnpm audit`, SHA-256-pinned yt-dlp, Chromium pinned through Playwright, signed images, weekly dependency updates |
| Data at rest | stolen disk or backups | restic encrypts client-side; VPS disk unencrypted (accepted and documented); account deletion purges the user directory |
| Invitees | quota abuse, scanning through capture | quotas, daily limits, audit log, owner can disable a user instantly |

### 7.2 Privacy and legal

- **Data held:** email, passkey public keys, the user's saved posts (third-party captions and media), notes, AI outputs, sealed provider keys, sync and job metadata, logs (rotated within days), backups (≤6 months).
- **Processors:** Hetzner (hosting), Cloudflare (DNS, TLS, tunnel — traffic transits the edge), Resend (email delivery over SMTP). AI providers are chosen and contracted by each user; content goes there only for actions the user starts, after a per-provider consent screen.
- **No analytics, no telemetry, no third-party requests** from the SPA.
- **User rights:** export (v2 bundle), account deletion (immediate purge job; backups age out), corrections in the UI.
- **Retention:** trash 30 days, exports 7 days, video cache LRU, finished jobs 14 days, audit log 1 year.
- **Platform terms:** automated collection conflicts with platform terms (02 risk 6). Posture: invite-only, private per-user storage, no sharing, only the user's own saved items (passive capture limited to saved listings), no platform sessions server-side, a takedown contact and process, the disclaimer recorded per account. Get legal advice before inviting beyond a small circle.

---

## 8. Risks and mitigations

| # | Risk | Likelihood | Impact | Mitigation |
|---|---|---|---|---|
| R1 | Platform API/DOM changes break the parsers | high | high | recorded fixtures, weekly real-account checklist, server kill switches, parser version in every batch, fast extension releases |
| R2 | Chrome Web Store rejects or delays the unlisted listing | medium | high | submit in week 3, single-purpose description, minimal permissions, no remote code; fallback signed zip for developer-mode install; Firefox AMO unlisted |
| R3 | CDNs refuse the datacenter IP | medium | medium | per-host breaker → extension uploads; defaults set by SPIKE-2 |
| R4 | User accounts flagged for automation | low–medium | high | passive-first capture, desktop pacing, incremental stop, no hidden IG tabs, kill switches |
| R5 | Disk fills (~a dozen users) | high | high | quotas, global budget, video LRU, alerts at 80/90 %, Volume runbook ready before the 6th invitee |
| R6 | No Chromium sandbox in Docker on Ubuntu 26.04 | medium | medium | SPIKE-4; fallback: hardened no-sandbox container, network isolation unchanged |
| R7 | SSRF through capture, media or provider URLs | medium | high | Smokescreen for every client, internal network, SSRF suite in CI |
| R8 | Search relevance regresses | medium | medium | `search-eval` gate before shipping, weights tuned offline |
| R9 | Port velocity (≈15–20k lines of TS domain logic) | medium | medium | SQL ported nearly verbatim (SQLite → SQLite), golden fixtures, capture stays TS, phase gates; second engineer cuts GA to ~13 weeks |
| R10 | Contention with Hermes | medium | medium | CPU shares, limits, `oom_score_adj`, capture concurrency 1, `nice`, host-contention alert, capacity test with a Hermes stand-in |
| R11 | osn drift between the repo and the live host | low | medium | the refactor is done and applied; run `just apply "--check --diff"` before each osn PR; known check-mode-only noise (GPG keys, SSH-key placeholder) is documented in osn |
| R12 | Single VPS failure | low | high | hourly DB backups, monthly restore drill, 2 h RTO runbook |
| R13 | Many SQLite files to migrate and keep healthy | low | medium | lazy + sweep migrations, expand/contract, upgrade tests from every fixture version, open-handle cap |
| R14 | Provider differences (schemas, vision, streaming) | medium | medium | two adapters, capability probe, `json_object` + repair fallback, SPIKE-6 |
| R15 | Legal exposure from hosting third-party media | medium | high | invite-only, private storage, takedown process, legal review before widening |
| R16 | MV3 lifecycle and background-tab throttling | medium | medium | controller in the tab, alarms, notification-driven background sync, SPIKE-3 |
| R17 | IG signed URLs expire before archiving | medium | low | archive ordered by expiry, extension refresh task |
| R18 | Cloudflare limits (body size, timeouts, bot checks) | low | medium | chunked uploads, async jobs, heartbeats, SPIKE-10 |

---

## 9. Spikes

| ID | Question | Method | Pass criteria | Timebox | When |
|---|---|---|---|---|---|
| SPIKE-1 | Can the desktop DB be mapped losslessly (IG ids, files, site versions)? | legacy reader + `shelfy-migrate plan` on a copy of the reference library | every row accounted for, duplicate groups listed, no unmapped column | 1 d | week 1 |
| SPIKE-2 | Do IG, X and Pinterest CDNs serve anonymous fetches to a Hetzner IP? | ~300 fresh URLs per platform from a throwaway CX33 | ≥98 % 2xx → `server`; otherwise `client` or `auto` per platform | 0.5 d | week 1 |
| SPIKE-3 | Does a MAIN-world `document_start` hook capture what the desktop captures; does IG replay run from a content script; do background tabs keep lazy-loading? | minimal extension on the owner's accounts vs the desktop on the same listings | ≥99 % item parity; replay works; background behavior documented | 1 d (+0.5 d for background tabs at P2 start) | week 1 |
| SPIKE-4 | Chromium sandbox in Docker on Ubuntu 26.04; Smokescreen with CONNECT, WebSockets, deny ranges and port limits | throwaway CX33 with the compose fragment | sandbox on with the seccomp profile, or a documented fallback; every SSRF probe refused | 1 d | P4 start |
| SPIKE-5 | FTS5 relevance vs the desktop ranking | `search-eval` cases in Rust, weight tuning | nDCG@10 and MRR ≥ baseline − 0.02 | 0.5 d | week 1 |
| SPIKE-6 | JSON schema, images and streaming on OpenAI, Anthropic, Gemini (compat) and OpenRouter | `extract-eval` subset with the owner's keys, cost-capped | capability table per provider; preset base URLs confirmed | 1 d | P3 start |
| SPIKE-7 | Chrome Web Store review of this permission set | submit a minimal build | approved, or the fallback chosen | 1–3 weeks of calendar time | from week 3 |
| SPIKE-8 | Passkeys through the tunnel on macOS (Safari, Chrome), iOS (PWA) and Android | prototype relying party | registration and login on all four | 0.5 d | P1 week 3 |
| SPIKE-9 | On-demand video and link hydration: IG `video_versions` lifetime, X/Pinterest public endpoints, yt-dlp success from Hetzner | scripted runs on 30 posts per platform | success rates and endpoints documented | 0.5 d | P2 start |
| SPIKE-10 | SSE and bearer-token API calls through Cloudflare Tunnel + nginx | quick tunnel on a throwaway box | events delivered in <300 ms; no bot challenges on token calls | 0.5 d | week 1 |
| SPIKE-11 | Capture v2 cost in a 1.5 GiB / 1.5 CPU container | the 6 audit sites + 6 fixture sites | wall time, CPU seconds, peak RSS and bytes per site recorded; §6.2 budgets confirmed or revised | 1 d | P4 start |
| SPIKE-12 | napi-rs + rusqlite inside Electron 31 on macOS arm64 and Windows x64 | hello-world module calling the core | loads in packaged builds | 1 d | before P6 |

---

## 10. The first two weeks

Goal: the reference library, moved with the real migration tool, is browsable and searchable in the web SPA, and the five spikes that could change the architecture are answered. Every task ends with a commit on `web/foundations` (branched from `dev`) and green CI for both desktop and web.

| # | Day | Task | Output | Needs |
|---|---|---|---|---|
| T1 | D1 AM | Cargo workspace (`crates/core`, `crates/server`, `crates/media`, `crates/migrate`), `rust-toolchain.toml`, `deny.toml`, `deploy/` skeleton; CI job `rust` (fmt, clippy, test, deny) added to `ci.yml` | green CI | — |
| T2 | D1 PM – D2 AM | **SPIKE-1:** `crates/core::legacy` reader (read-only), canonical keys (IG split/decode, scheme-less web ids), `shelfy-migrate plan` on a copy of the reference library | `docs/web-port/spikes/01-legacy-mapping.md` | T1 |
| T3 | D2 PM – D3 | Schema v1 (§2.6–2.7) with `rusqlite_migration`; `UserDb` (pragmas, handle cache); repositories: posts list (keyset + filters), post detail, stats, collections; FTS maintenance on write; golden-fixture harness skeleton (`scripts/golden/`) | unit tests green on fixture DBs | T1 |
| T4 | D4 AM | **SPIKE-5:** port `scripts/search-eval/cases.ts` to `crates/core/tests/search_eval.rs`, FTS query builder, weight tuning against `last-report.json` | `spikes/05-fts-relevance.md` | T3 |
| T5 | D4 PM – D5 AM | **SPIKE-3:** minimal MV3 extension in `extension/` (esbuild) bundling `webview-injected.ts` as MAIN `document_start` + relay + side-panel log; compare with the desktop on IG saved, one IG folder, X bookmarks, one Pinterest board; keep the fresh CDN URLs | `spikes/03-extension-capture.md` + URL sample | — |
| T6 | D5 PM | Throwaway CX33: **SPIKE-2** (fetch the T5 URLs anonymously, record status/latency/expiry) and **SPIKE-10** (quick tunnel → nginx → test SSE + bearer calls); destroy the box | `spikes/02-cdn-from-datacenter.md`, `spikes/10-sse-tunnel.md` | T5 |
| T7 | D6 | `crates/server`: axum app, env config, JSON tracing, `/health`, `/metrics` on :9464, problem+json errors, body limits, compression, utoipa OpenAPI, `shelfy-server admin` (`create-owner`, `invite`, `snapshot`); `deploy/compose.dev.yml` | server boots locally | T3 |
| T8 | D7 | `crates/media`: CAS store, `g480` WebP + ThumbHash on the rayon pool, `/media/*` with Range, ETag, immutable cache and sandbox headers | rendition and serving tests | T7 |
| T9 | D8 | Migration v0: `shelfy-migrate run` (bundle + tus upload), `POST /migrations/missing-objects`, `POST /migrations`, install job; install the reference library into the local server | reference library installed | T2, T3, T8 |
| T10 | D9 AM | Owner auth v0: `admin create-owner`, magic-link login (lettre → local SMTP catcher), sessions, CSRF/Origin middleware (passkeys follow in week 3) | login works | T7 |
| T11 | D9 PM | Read API: `GET /posts` (filters, keyset, ETag), `GET /posts/{key}`, `GET /search`, `GET /stats`, `GET /collections`, `GET /events` (hello + heartbeat); generated TS client committed | OpenAPI + client | T4, T7 |
| T12 | D10 | SPA slice: `web/` Vite app, `ShelfyClient` + HTTP implementation, login screen, Gallery (grid, infinite scroll, filters, search), read-only Post modal, Sidebar sources; desktop-only UI hidden by capability; desktop build still green | the reference library browsable in a browser, desktop and mobile viewports | T10, T11 |

**Exit check (end of D10):** list p95 ≤40 ms server time on the reference library; search passes the SPIKE-5 gate; spike notes 1, 2, 3, 5 and 10 committed with their decisions (archive mode per platform, extension capture parity, FTS weights); open questions for week 3 listed (passkeys/SPIKE-8, first osn PR draft, Chrome Web Store submission).

---

## Appendix A — IPC → web API mapping (155 invoke + 15 push channels)

| Desktop channels | Count | Web | Replacement |
|---|---|---|---|
| `db:getPosts`, `db:getPostIds`, `db:getPostsByIds`, `db:getStats` | 4 | port | `GET /posts`, `GET /posts/count` + bulk selector, `POST /posts/batch-get`, `GET /stats` |
| `db:savedByKeys` | 1 | port | `POST /posts/lookup` |
| `db:existingIds` | 1 | drop (dead) | — |
| `db:bulkUpsert` | 1 | redesign | `POST /ingest/batches` (extension) |
| `db:importJSON`, `dialog:openFile` | 2 | redesign | tus upload + `POST /imports` |
| `db:exportJSON` | 1 | redesign | `POST /exports` + download |
| `db:clearAll`, `db:clearAiAnalysis`, `db:clearAssets` | 3 | port | `POST /me/reset {scope}` (re-auth) |
| `db:deletePosts`, `db:deleteLocalFiles` | 2 | port | `POST /posts/bulk {delete}` (trash), `DELETE /posts/{key}/media` |
| `preview:repair` | 1 | redesign | automatic archive retry / extension refresh task (no client call) |
| `collections:*` | 6 | port | `/collections…` (`removePost` finally exposed) |
| `post:updateUserContent`, `analyze:updateManual` | 2 | port | `PATCH /posts/{key}` |
| `download:*` | 11 | redesign | `POST /posts/{key}/media/fetch`, bulk `fetchMedia`, `/jobs`, `/queues/media.video/*` |
| `analyze:` post, all, posts, split, missing, status, cancelJob, cancelAll, clearAll, clearCompleted, pauseAll, resumeAll, isPaused, retryJob | 14 | redesign | `POST /ai/analyze`, `GET /ai/queue`, `/jobs`, `/queues/ai.drain/*` |
| `analyze:` modelStatus, listModels, setModel, get/setConcurrency, getHardware, get/setTuning, download/pause/cancel/deleteModel | 12 | drop | provider settings (`/me/providers`) |
| `analyze:taxonomy` | 1 | drop (dead) | — |
| `analyze:clearDescriptions`, `analyze:clearTags` | 2 | port | bulk `clearAiDescription` / `clearAiTags` |
| `aitags:*` | 21 | port | `/tags…`, `/tag-clusters…`, `/tag-aliases…` (`tagGraph`, dead, dropped) |
| `search:*` | 10 | port / redesign | `/search`, `/search/suggest`, `/search/chat` (SSE), `/me/providers` |
| `ai:remoteStatus`, `ai:retryRemote`, `ai:useLocalModels` | 3 | redesign / drop | `provider.status` event, `POST /me/providers/{id}/test` |
| `stt:transcribe` | 1 | redesign | `POST /stt/transcriptions` |
| other `stt:*`, all `emb:*` | 17 | drop | — |
| `web:*` | 15 | port / redesign | `/sites…`, `/jobs`, `/queues/capture.site/*` (`isPaused`, `pauseAll`, `resumeAll`, `discover` were dead) |
| `bookmark:add` | 1 | redesign | tus + `POST /bookmarks` |
| `feedback:send` | 1 | port | `POST /feedback` (SMTP from the server) |
| `shell:openExternal` | 1 | client | `window.open(url, "_blank", "noopener")` with a scheme allowlist |
| `shell:openPath`, `shell:showItemInFolder` | 2 | drop | "Download original" |
| `app:getVersion` | 1 | client | build constant + `GET /version` |
| `app:get/setUpdateChannel`, `updater:*`, `binaries:*`, `window:*`, `getWebview*Script` | 18 | drop | — |
| **Push:** `interceptor:newPosts` → `posts.changed` + `stats.changed`; `download:`/`analyze:`/`web:progress`, `aitags:cluster/aliasProgress` → `job.updated` (+ `ai.stream`, `capture.event`); `search:chatToken` → chat response stream; `ai:remoteStatus` → `provider.status` | 8 | port | §2.10 |
| **Push dropped:** `window:maximizeChanged`, `updater:state`, `binaries:progress`, `ai:variantFallback`, `analyze:`/`stt:`/`emb:modelProgress` | 7 | drop | — |

Totals: 155 invoke = 4+1+1+1+2+1+3+2+1+6+2+11+14+12+1+2+21+10+3+1+17+15+1+1+1+2+1+18; 15 push = 8 kept + 7 dropped.

## Appendix B — osn integration checklist

**Prerequisites (owner actions before PR 1):**
1. Grant **Zone DNS Edit** on `niccolofanton.dev` to the OpenTofu Cloudflare token (`CLOUDFLARE_API_TOKEN` in osn SOPS). Today it can manage Access but gets 403 on DNS changes.
2. Create the R2 bucket `osn-backups` and an R2 API token scoped to it, then replace `R2_BUCKET`/`AWS_*` in SOPS (§3.4).
3. Throwaway servers for spikes and capacity tests: either a Hetzner Cloud API token (scoped project) for `hcloud`, or create/destroy the CX33s by hand.
4. Fix the osn `monitoring` handler, which recreates the stack but does not restart Grafana when only provisioning files change. Dashboards and alert rules from PR 1 would otherwise not load until a manual `docker restart osn-grafana-1`.

**PR 1 (P1):**
1. `compose.yml`: `shelfy-api` service (§3.2), `shelfy_capture` network declared early so the API attaches once.
2. `edge/nginx.conf`: Shelfy server block (§3.3).
3. `ansible/group_vars/all.yml`: `osn_host_shelfy`, appended to `osn_public_hostnames`.
4. `tofu/cloudflare-access/dns.tf`: proxied CNAME `shelfy`; `access.tf`: temporary Access app for the P0–P1 owner-only period (removed in P2).
5. `secrets/osn.sops.yaml` (+ `.example`): `SHELFY_MASTER_KEY`, `SHELFY_INTERNAL_TOKEN`, `SHELFY_RESTIC_PASSWORD`.
6. `.env.example` and `ansible/roles/app/templates/env.j2`: `SHELFY_VERSION`, `SHELFY_PUBLIC_URL`, `SHELFY_SMTP_FROM` and the secrets above.
7. New role `ansible/roles/shelfy` (before `app`): `/data/shelfy/{control,users,cache/video,work/capture,work/uploads,backup-staging/db}` owned by 10100, mode 0750; restic env file (0600); systemd units/timers from §3.5 writing textfile metrics; copies `deploy/osn/shelfy/*` (seccomp profile, later) to `/opt/osn/stack/shelfy/`.
8. `observability/victoriametrics/scrape.yml`: job `shelfy` (`shelfy-api:9464`) and blackbox targets `/health`, `/health/capture`.
9. `observability/grafana/dashboards/03-shelfy.json` and alert rules from §3.6 in `provisioning/alerting/rules.yaml`.
10. `justfile`: `shelfy-deploy TAG`, `shelfy-logs`, `shelfy-admin *ARGS`, `shelfy-backup-now`, `shelfy-restore-drill`; `just check` gains the Shelfy host route (expect 200 on `/health`).
11. `doc/RUNBOOK-shelfy.md` (§3.9) and a `README.md`/`LINKS.md` entry.

**PR 2 (P4):** `shelfy-capture` and `shelfy-egress` services, the seccomp profile and Smokescreen settings, capture alerts and dashboard panels; `osn-docker-firewall` unchanged (no new published ports).

## Appendix C — reference desktop library (aggregate counts only)

Measured read-only on a real desktop library on 2026-10-02; no content was read.

| Measure | Value |
|---|---|
| Posts | 6,138 (Instagram 3,997, X 2,140, web 1) |
| Media types | video 4,631 · carousel 828 · image 391 · images 114 · text 173 · website 1 |
| Slides | 11,482 (5,855 image, 5,627 video) |
| Downloaded files (desktop) | covers 2,105 × avg 192 KB; images 1,496 × avg 401 KB; videos 284 × avg 14.8 MB (4.0 GB); 640 px previews 2,078 × avg 41 KB |
| Desktop DB | 46 MB (`thumb_blur` averages 533 bytes per row) |
| AI analyzed | 3 posts (the web port starts AI from scratch for this library) |
| Posts with a downloaded video | 504 |
| Posts with no local cover | 2,037 (Instagram 1,861, X 176) |
| Collections | 1 (2,264 memberships); `post_tags` rows: 27 |
| Other local data not migrated | local models 6.4 GB, thumbnail cache 1.0 GB, social session partition 1.5 GB |
