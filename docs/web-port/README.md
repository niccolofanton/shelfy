# Shelfy web port — feature index

Granular inventory of every Shelfy capability, built from the `dev` working tree on 2026-10-02 (uncommitted changes included).

| Document | Role |
|---|---|
| [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md) | **Plan of record:** self-hosted on the osn VPS, Rust core, Node capture service, MV3 extension, BYOK AI. Phases, spikes and the first two weeks. |
| [ONLINE-MIGRATION.md](ONLINE-MIGRATION.md) | Earlier analysis (Cloudflare-native option) and the product decisions. Its feature analysis and defect list still apply; its platform choice is superseded. |
| [review/](review/) | How the plan of record was chosen: the superseded v1 plan and the side-by-side comparison. |

Every entry was verified against the code and cites `path:line`. Line numbers drift as the code changes; search by symbol if a reference no longer matches.

## Areas

| # | Area | File | Features | Highlights |
|---|---|---|---|---|
| 1 | Library data, persistence & IPC surface | [01-library-data-and-ipc.md](features/01-library-data-and-ipc.md) | 58 (`DATA-*`) | full SQLite schema (12 tables), complete IPC contract (155 invoke + 15 push channels) |
| 2 | Social sources sync & media downloads | [02-social-sync-and-downloads.md](features/02-social-sync-and-downloads.md) | 60 (`SYNC-*`, `DL-*`) | platform matrix (Instagram, X, Pinterest), per-platform viability of extension / server / official API |
| 3 | AI features | [03-ai.md](features/03-ai.md) | 63 (`AI-*`) | model inventory and online replacements, LLM call sites, per-action token costs |
| 4 | Websites capture, bookmarks & imports | [04-websites-bookmarks-imports.md](features/04-websites-bookmarks-imports.md) | 64 (`WEB-*`, `IMP-*`) | capture pipeline mapped to a managed headless browser vs container, SSRF notes |
| 5 | App shell, gallery/browsing UX, settings & platform services | [05-app-shell-ui-settings.md](features/05-app-shell-ui-settings.md) | 136 (`APP-*`, `UI-*`) | navigation map, settings inventory, renderer coupling to `window.electronAPI` |
| | **Total** | | **381** | |

## Entry format

Each feature lists: what it does, entry points (UI → IPC → main process), data touched, local dependencies, external calls, status (`shipped` / `flag` / `spike` / `dead`) and a **web port** class.

| Web port class | Meaning |
|---|---|
| `client-only` | runs in the browser as-is |
| `api+db` | needs an API endpoint over the per-user database |
| `object-storage` | media or files move to object storage |
| `realtime-push` | main→renderer events become server push |
| `background-job` | needs a durable job/queue |
| `headless-browser` | needs a managed headless browser |
| `container (native binary)` | needs yt-dlp / ffmpeg / Chromium in a container |
| `browser-extension` | must run in the user's own logged-in browser |
| `third-party AI API` | local model replaced by a provider API |
| `rethink` | the feature needs a redesign for the web |
| `drop (desktop-only)` | no web equivalent needed |

## Portability at a glance

Each feature is bucketed by its primary class.

| Area | Direct port | Needs platform infra | Redesign | Drop |
|---|---|---|---|---|
| 1 Data & IPC | 38 | 8 | 5 | 7 |
| 2 Sync & downloads | 15 | 35 | 4 | 6 |
| 3 AI | 28 | 16 | 7 | 12 |
| 4 Websites & imports | 21 | 30 | 5 | 8 |
| 5 Shell, UI & settings | 106 | 0 | 9 | 21 |
| **Total** | **208 (55%)** | **89 (23%)** | **30 (8%)** | **54 (14%)** |

The buckets group the classes as follows:

- **Direct port:** `client-only`, `api+db`, `object-storage`, `realtime-push`.
- **Needs platform infra:** jobs, headless browser, container, extension, AI API.
- **Drop:** mostly local models and sidecars, the updater, window chrome and binary provisioning.

## Not supported today

Checked by grep, so the port does not inherit them:

- TikTok, YouTube, Reddit and Facebook sync or downloads
- browser-bookmark HTML, CSV or GDPR-archive imports
- folder import
- favorite, pinned or hidden flags
- full-text index (search is `LIKE` plus a JS UDF)
- semantic or vector search
