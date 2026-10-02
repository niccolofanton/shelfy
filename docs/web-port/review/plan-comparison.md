# Plan review — v1 vs independent plan (2026-10-02)

## How the review was run

- **v1** (`plan-v1-superseded.md`) was written in the working session, as the platform decisions were taken.
- An independent agent with no conversation context then wrote a second plan. It had only:
  - the feature index (`../README.md`, `../features/`)
  - the source code
  - the product owner's fixed decisions
- It never saw v1 or ONLINE-MIGRATION.md.
- The two plans were compared, and the independent plan was adopted as [../IMPLEMENTATION-PLAN.md](../IMPLEMENTATION-PLAN.md).

## Errors in v1 that the adopted plan fixes

| # | v1 | Adopted plan |
|---|---|---|
| 1 | Video AI analysis used ffmpeg keyframes, so every analyzed video had to be downloaded. This contradicts the "videos on demand" decision. | Videos are cataloged from poster + caption; deep frame analysis only when the video is already cached (D12). |
| 2 | Web Share Target for mobile | iOS has no Web Share Target: Android uses `share_target`, iOS uses a Shortcut with a `links:create` token (§2.17). |
| 3 | Reused `webcapture-playwright.ts` | Reuses capture v2 (`electron/webcap/*`) behind an Env shim (§2.18). |
| 4 | 11–16 weeks | ~21 weeks for P0–P5 with one engineer (~12–13 with two), plus 6–8 for desktop convergence (§5). |
| 5 | Moved the desktop into `apps/desktop` | Additive monorepo: the desktop stays at the root, untouched until P6 (D25). |

## Improvements adopted

| Area | Adopted approach |
|---|---|
| Client seam | `ShelfyClient` (HTTP for web, IPC for desktop) instead of a fake `window.electronAPI` |
| API | REST + OpenAPI (utoipa → openapi-typescript), ~95 endpoints, bulk actions by selector, problem+json codes, ETag/304, idempotency keys |
| Realtime | SSE with a 20 s heartbeat (Cloudflare's 100 s idle timeout) and `Last-Event-ID`, instead of WebSocket |
| SSRF | Network-level: Smokescreen egress proxy for every outbound client; Chromium on an internal-only network |
| Media | Per-user CAS; bounded source sizes; one 480 px WebP rendition; ThumbHash; video LRU cache; ~0.4 MB per post |
| Media fetch | Server-first through the proxy, with a per-host circuit breaker that falls back to the extension (SPIKE-2) |
| On-demand video | Parser URL → extension → yt-dlp (X/Pinterest) → open original |
| Jobs | Control-DB `jobs` + per-item state with drain jobs, leases, round-robin across users, transient/permanent error classes |
| Resources | Concrete limits and OOM priorities (capture dies first, Hermes untouched) |
| Backups | Hourly DB snapshots, daily media, monthly automated restore drill; RPO 1 h / 24 h, RTO 2 h |
| Migration | `shelfy-migrate` CLI: dry run, upload of missing objects only, IG duplicate merge, reconciliation report |
| AI | Two wire protocols; base URL used verbatim (fixes the Gemini `/v1` bug); a cost estimate before enqueuing; an owner-only private provider route |
| Delivery | 12 spikes with pass criteria; day-by-day first two weeks; full IPC → API mapping; osn checklist; throwaway servers for capacity tests |
| Capacity | Model built from the real reference library (6,138 posts, 75 % video): ~10–12 users on today's disk, ~100 with a 200 GB volume |

## Updates made at adoption

- The osn refactor is done (§3 baseline).
- New R2 bucket `osn-backups` (§3.4).
- Owner prerequisites before PR 1 (Appendix B): DNS Edit on the tofu token, R2 bucket and token, throwaway servers, Grafana handler fix.
