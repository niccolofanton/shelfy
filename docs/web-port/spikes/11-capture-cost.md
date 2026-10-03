# SPIKE-11 — What one capture v2 run costs in the capture container

**Question.** Capture v2 (`electron/webcap/*`) will run in the P4 capture service, in a 1.5 GiB / 1.5 CPU container behind Smokescreen (plan §2.18, §3.1). What does one capture cost there, measured on the 6 audit sites and the 6 fixture sites? The answer confirms or revises the §6.2 budget ("website capture (6 pages): p50 ≤ 4 min, p95 ≤ 8 min, peak RSS ≤ 1.2 GB") and the §6.4 capacity input ("~3–4 core-min per site → ~20–25 sites/h").

**Answer.** The budget is revised. Capture v2 runs outside Electron with an `electron` stand-in and a few wrappers, but on this CPU-only box it costs about twice the time and two to three times the CPU the plan assumed.

- **As written (2× device scale, 3 inner pages in parallel), it does not fit.** Of the 5 audit sites that were not blocked:
  - 4 filled the 1.5 GiB limit: the OOM killer killed renderers, 2–3 times per site;
  - only 3 finished, each with pages or bands missing.
- **At 2×, even one page at a time does not fit:** `apple.com` crashed in 12 s in both cases.
- **One page at a time at 1× fits.** `linear.app`, `itsnicethat.com` and `apple.com/airpods-pro` captured all 6 pages:
  - in 4.9–8.1 min (two runs each), with 6.8–11.3 core-min of CPU;
  - with 1.32–1.43 GiB held at peak, and no OOM kill.
- **Two sites fail on CPU, however the capture is configured:**
  - `stripe.com` needs about 11–12 min, so the 10 min site budget cut it with 4 and then 5 of its 6 pages done;
  - `lusion.co` (WebGL) never yields its first screenshot within 45 s: SwiftShader cannot render its scene in time on 1.5 shared vCPUs.
- **Blocked site:** `aesop.com` was blocked by Cloudflare in 20 s, at 0.2 core-min.
- **Fixtures:** 15–43 s and 0.33–0.82 GiB each.

| Budget (plan) | Measured | Verdict |
|---|---|---|
| Capture (6 pages) p50 ≤ 4 min, p95 ≤ 8 min | 1 page at a time at 1×: median 5.25 min, 8.1 min the slowest that finished, ~11–12 min for `stripe.com` | **revise:** p50 ≤ 6 min, p95 ≤ 12 min, site budget 12 min (was 10), and a timed-out capture keeps the pages already captured |
| Peak RSS ≤ 1.2 GB | 1.32–1.43 GiB held at 1×; the 1.5 GiB limit is reached at 2× | **revise:** ≤ 1.45 GiB at 1× and 1 page at a time, and raise the limit to 1.75 GiB for headroom (lead decision, below) |
| ~3–4 core-min per site, ~20–25 sites/h (§6.4) | 6.8–11.3 core-min for completed sites; 14.5 when `stripe.com` timed out | **revise:** ~10 core-min per site, ~8–9 sites/h at 1.5 CPU |
| Artifacts ≤ 80 MB per site | 4.4–14.5 MB at 1×, up to 21.3 MB at 2× | confirmed |
| Page budget 150 s | primary page 116–169 s at 1×, inner pages 18–85 s | **revise:** 180 s for the primary page (it records the scroll video); keep 150 s for the other pages |

Run on 2026-10-02/03, 23:39–01:58 UTC, on the osn VPS (E1):
- **Runs:** 35 captures, sequential, one fresh container each.
- **Hermes:** checked before and after every run, 76 times: running, 0 restarts, same start time, node online. Its cgroup's CPU pressure stayed at 0.00 (see [Hermes](#hermes)).
- **Other lanes:** SPIKE-9's yt-dlp containers ran alongside some runs. Other work never took more than 0.43 of the 2.5 vCPUs the capture left free, so no run was repeated for load (see [Load](#load)).

## Harness

[`scripts/spikes/capture-harness/`](../../../scripts/spikes/capture-harness/build.mjs) runs `captureSite()` and then the orchestrator's design-metadata phase:
- palette from pixels, typography, tech, awards, traits, page digests;
- the og:image and favicon downloads.

It skips the DB and AI steps of `captureWebReference()`. `build.mjs` bundles `src/run-site.ts` and the whole `electron/webcap` graph with esbuild, with `electron` aliased to `src/electron-shim.ts`. `electron/` is untouched (D25).

The result is a 225 KB bundle, plus the 5 MB adblock engine and the harness's own runtime dependencies (30 MB, in its own `package.json` and `pnpm-lock.yaml`): `playwright-core` 1.60.0, `@ghostery/adblocker` 2.18.2, `@duckduckgo/autoconsent` 16.43.2, `tldts-experimental` 7.4.16.

It writes what the §2.18 protocol describes:
- `events.ndjson` with `event`, `page`, and `done` or `failed` lines;
- `manifest.json`;
- the artifacts under `assets/web/`;
- `metrics.json`.

**What had to be shimmed, and what P4's `env.ts` should do instead:**

| Capture v2 needs | In Electron | Harness | P4 |
|---|---|---|---|
| `app.getPath('userData')` | asset root `<userData>/assets/web` | `CAPTURE_WORK_DIR` | one work dir per capture; files keep capture v2's `<stamp>-<host>-<hash>.<ext>` names |
| `app.getLocale()` | context locale, `Accept-Language` | `CAPTURE_LOCALE` (default `en-US`) | configure it. The VPS's IP is German, so `stripe.com` redirected to `/de` anyway (see [quality](#quality)) |
| `app.isPackaged` | adblock engine path; Chromium self-install | `true`, with `process.resourcesPath` pointing at `resources/adblock/engine.bin` | ship `build/adblock/engine.bin` in the image; never self-install Chromium |
| `session`, `BrowserWindow` | the Electron fallback driver when Playwright cannot launch, and the v1 engine | throw | a launch failure fails the capture; drop the fallback |
| Chromium launch options | fixed in `webcapture-playwright.ts` `getBrowser()` | wraps `playwright-core`'s `chromium.launch` to add the proxy and flags ([SPIKE-4](04-chromium-sandbox-egress.md)) | make them configurable |
| Inner pages in parallel | `PlaywrightSession.create()` takes no cap: 3, or 2 for WebGL | `src/session.ts` repeats `create()` with a cap, and a scale for the comparisons | a `pagesParallel` setting, default 1 (below) |
| ffmpeg with libwebp and libx264 | `ffmpeg-static` | `FFMPEG_BIN`, pointing at a static ffmpeg 6.0 (`ffmpeg-static` b6.1.1); the Playwright image's own ffmpeg only has VP8 | Debian `ffmpeg` (§2.18), through `FFMPEG_BIN` |
| Node `fetch` through the proxy | — | `NODE_USE_ENV_PROXY=1` and `HTTP(S)_PROXY` | the same; no code change |
| The shared browser | module singleton | `closeBrowser()` at the end | close it after 120 s idle (§2.18) |
| Events | Italian `text` | passed through | codes with parameters, as the §2.18 protocol already says (audit G5) |
| og:image and favicon | `weborchestrator.ts` (`pickIcon`, not exported) and the v1 module's `fetchImageToWebp` | `pickIcon` copied; v1 module bundled | move `discoverPages`, `fetchImageToWebp`, `screenshotPathForUrl` and `resolveFfmpeg` from `electron/webcapture.ts` into `webcap/`, so the service stops bundling the 2,100-line v1 Electron engine |
| Site deadline | job signal | `AbortController`, 10 min | when the site budget runs out, keep the pages already captured; today `captureSite()` throws and discards them |

## Method

**Container.** Each run had its own fresh container `shelfy-spike11-<label>`, configured as in SPIKE-4:
- **Image:** `mcr.microsoft.com/playwright:v1.60.0-noble` (Chromium 148 headless shell).
- **Limits:** `--cpus 1.5 --memory 1.5g --memory-swap 1.5g`, `--cpu-shares 256`, `--oom-score-adj 600`.
- **Hardening:** uid 10100, read-only root, `/tmp` on a 1 GiB tmpfs, `--shm-size 512m`, no capabilities, the SPIKE-4 seccomp profile, sandbox on.
- **Network:** only the isolated internal network, with `shelfy-spike4-egress` (Smokescreen) as its one way out.
- **Disk:** the work dir was on disk (`/var/tmp`), as `/data/shelfy/work/capture` will be; the VPS's `/tmp` is RAM-backed.

Runs were sequential, one site at a time (plan: 1 site in flight).

**Sites:**
- **Audit:** `linear.app`, `stripe.com`, `lusion.co`, `aesop.com`, `itsnicethat.com`, `apple.com/airpods-pro`, with 6 pages and the scroll video, as the desktop runs them (`DEFAULT_MAX_PAGES`).
- **Fixtures:** the six `scripts/web-capture-eval` pages, single page. Playwright's router serves them at `https://fixtures.shelfy.test/`, so they cost no network.

**Configurations:**

| | Device scale | Inner pages in parallel | Why |
|---|---|---|---|
| A | 2× | 3 (2 for WebGL) | capture v2 as it is |
| B | 1× | 1 | the cheapest setting that keeps every capture v2 step; run twice on the sites that finish |
| C | 1× | 2 | the plan's "2 pages in parallel" |
| D | 2× | 1 | isolates the scale from the parallelism |

**Measures.** Taken from inside the container, from the cgroup v2 files Docker mounts there:
- **CPU:** `cpu.stat` `usage_usec`, for every process: Node, Chromium, ffmpeg.
- **Memory held:** the peak, sampled every 250 ms, of `anon + shmem + kernel` from `memory.stat`. This is what page-cache reclaim cannot free, and the container has no swap. `memory.peak` also counts reclaimable page cache.
- **OOM:** `memory.events` `oom_kill`.
- **Bytes in and out:** on the container's only link, which goes to the proxy; the proxy's own log agrees per connection.
- **Output size:** the artifacts in the work dir.
- **Wall time:** in the harness, from launch to the end of the metadata phase.
- **Host:** busy CPU from `/proc/stat` (the "other host CPU" column: everything but the capture), the load average, the other containers that were running, and Hermes's CPU pressure.

## Results

### Audit sites

Configuration A, capture v2 as written (2×, 3 inner pages in parallel):

| Site | Outcome | Pages (skipped) | Bands / sections | Wall | CPU s (core-min) | Held peak GiB | OOM kills | In / out MB | Artifacts MB | Other host CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| linear.app | done | 4 (2) | 0 / 0 | 3.3 min | 294 (4.9) | 1.49 | 3 | 22 / 0.7 | 1.2 | 63.2 |
| stripe.com | done | 5 (1) | 18 / 20 | 8.3 min | 731 (12.2) | 1.50 | 3 | 207 / 0.6 | 11.7 | 130.2 |
| lusion.co | capture_failed | 0 | — | 7.9 min | 707 (11.8) | 0.92 | 0 | 31 / 0.3 | 0 | 77.1 |
| aesop.com | capture_blocked | 0 | — | 0.3 min | 9 (0.2) | 0.48 | 0 | 1 / 0.1 | 0 | 5.8 |
| itsnicethat.com | done | 5 (1) | 22 / 37 | 6.5 min | 577 (9.6) | 1.50 | 3 | 104 / 0.2 | 21.3 | 68.8 |
| apple.com/airpods-pro | capture_failed | 0 | — | 0.2 min | 18 (0.3) | 1.50 | 2 | 15 / 0.1 | 0 | 3.5 |

Configuration B, one inner page at a time, 1×:

| Site | Outcome | Pages (skipped) | Bands / sections | Wall | CPU s (core-min) | Held peak GiB | OOM kills | In / out MB | Artifacts MB | Other host CPU s |
|---|---|---|---|---|---|---|---|---|---|---|
| linear.app, run 1 | done | 6 (0) | 22 / 26 | 5.0 min | 415 (6.9) | 1.40 | 0 | 93 / 1.1 | 4.4 | 53.2 |
| linear.app, run 2 | done | 6 (0) | 22 / 26 | 4.9 min | 411 (6.8) | 1.43 | 0 | 100 / 1.1 | 4.5 | 50.3 |
| stripe.com, run 1 | timeout, 4 of 6 pages done | 0 | — | 10.1 min | 870 (14.5) | 1.09 | 0 | 193 / 0.5 | 6.3 | 100.0 |
| stripe.com, run 2 | timeout, 5 of 6 pages done | 0 | — | 10.0 min | 864 (14.4) | 1.10 | 0 | 159 / 0.6 | 8.3 | 99.0 |
| lusion.co | capture_failed | 0 | — | 4.3 min | 385 (6.4) | 0.78 | 0 | 22 / 0.2 | 0 | 43.1 |
| aesop.com | capture_blocked | 0 | — | 0.3 min | 10 (0.2) | 0.48 | 0 | 1 / 0.1 | 0 | 5.5 |
| itsnicethat.com, run 1 | done | 6 (0) | 32 / 63 | 5.3 min | 418 (7.0) | 1.32 | 0 | 115 / 0.2 | 13.9 | 53.6 |
| itsnicethat.com, run 2 | done | 6 (0) | 32 / 63 | 5.2 min | 412 (6.9) | 1.32 | 0 | 106 / 0.2 | 14.1 | 55.4 |
| apple.com/airpods-pro, run 1 | done | 6 (0) | 67 / 66 | 8.0 min | 672 (11.2) | 1.42 | 0 | 283 / 1.7 | 13.9 | 91.2 |
| apple.com/airpods-pro, run 2 | done | 6 (0) | 67 / 66 | 8.1 min | 678 (11.3) | 1.37 | 0 | 281 / 1.6 | 14.5 | 86.9 |

On `stripe.com`, the harness had written the finished pages' artifacts (the "Artifacts" column), but `captureSite()` threw at the deadline, so the capture reports no page. Repeated runs agree within 3 % on time and CPU, and within 0.05 GiB on memory.

Configurations C (1×, 2 in parallel) and D (2×, 1 at a time), next to B, on the sites that complete:

| Site, configuration | Outcome | Pages (skipped) | Bands / sections | Wall | CPU s (core-min) | Held peak GiB | OOM kills | In / out MB | Artifacts MB |
|---|---|---|---|---|---|---|---|---|---|
| linear.app, B | done | 6 (0) | 22 / 26 | 5.0 min | 415 (6.9) | 1.40 | 0 | 93 / 1.1 | 4.4 |
| linear.app, C | done | 6 (0) | 22 / 26 | 4.8 min | 419 (7.0) | 1.46 | 0 | 104 / 1.1 | 4.6 |
| linear.app, D | done | 6 (0) | 22 / 26 | 6.6 min | 558 (9.3) | 1.50 | 1 | 115 / 1.1 | 9.7 |
| stripe.com, B | timeout | 0 | — | 10.1 min | 870 (14.5) | 1.09 | 0 | 193 / 0.5 | 6.3 |
| stripe.com, C | done | 6 (0) | 39 / 46 | 9.1 min | 813 (13.6) | 1.25 | 0 | 159 / 0.6 | 9.8 |
| itsnicethat.com, B | done | 6 (0) | 32 / 63 | 5.3 min | 418 (7.0) | 1.32 | 0 | 115 / 0.2 | 13.9 |
| itsnicethat.com, C | done | 6 (0) | 32 / 63 | 5.2 min | 447 (7.5) | 1.31 | 0 | 102 / 0.2 | 14.0 |
| itsnicethat.com, D | done | 4 (2) | 19 / 32 | 6.1 min | 519 (8.6) | 1.50 | 2 | 60 / 0.2 | 19.9 |
| apple.com/airpods-pro, B | done | 6 (0) | 67 / 66 | 8.0 min | 672 (11.2) | 1.42 | 0 | 283 / 1.7 | 13.9 |
| apple.com/airpods-pro, C | done | 5 (1) | 41 / 42 | 5.4 min | 476 (7.9) | 1.48 | 2 | 174 / 1.0 | 9.6 |
| apple.com/airpods-pro, D | capture_failed | 0 | — | 0.2 min | 16 (0.3) | 1.48 | 2 | 15 / 0.1 | 0 |

### Fixture sites (single page)

| Fixture | Wall 2× / 1× | CPU s 2× / 1× | Held peak GiB 2× / 1× | Artifacts MB 2× / 1× | Video s |
|---|---|---|---|---|---|
| `native-tall` | 29 / 22 s | 32 / 23 | 0.82 / 0.63 | 0.36 / 0.23 | 5.8 |
| `lenis` | 29 / 22 s | 33 / 23 | 0.81 / 0.62 | 0.28 / 0.16 | 6.0 |
| `scrollsmoother` | 21 / 15 s | 17 / 8 | 0.60 / 0.33 | 0.22 / 0.08 | — |
| `locomotive` | 21 / 15 s | 18 / 8 | 0.62 / 0.33 | 0.19 / 0.06 | — |
| `webgl-hero` | 43 / 38 s | 64 / 57 | 0.78 / 0.65 | 0.20 / 0.11 | 8.4 |
| `pinned` | 23 / 19 s | 24 / 17 | 0.73 / 0.59 | 0.16 / 0.09 | 4.8 |

Every fixture was captured with every band. The two virtual-scroll fixtures produce no video: the screencast sends a frame only when the screen changes, and those pages do not scroll under wheel events.

### Where the time goes

Per page, configuration B, run 1, in seconds, from capture v2's own `timings`:

| Site | Page | Height px | load | consent | hero | video | probe | bands | encode | total |
|---|---|---|---|---|---|---|---|---|---|---|
| linear.app | home (primary) | 9,960 | 8 | 3 | 25 | 27 | 7 | 60 | 0 | 131 |
| | product | 6,310 | 8 | 3 | 1 | — | 5 | 7 | 2 | 26 |
| | pricing | 6,316 | 4 | 3 | 2 | — | 6 | 12 | 2 | 29 |
| | features | 11,059 | 7 | 3 | 2 | — | 8 | 24 | 1 | 46 |
| | blog | 6,763 | 10 | 3 | 1 | — | 5 | 10 | 3 | 32 |
| | about | 7,265 | 4 | 3 | 5 | — | 5 | 14 | 2 | 34 |
| itsnicethat.com | home (primary) | 14,498 | 2 | 8 | 8 | 26 | 5 | 54 | 7 | 116 |
| | work | 14,470 | 5 | 7 | 2 | — | 7 | 16 | 7 | 46 |
| | product | 4,364 | 2 | 7 | 1 | — | 3 | 3 | 2 | 18 |
| | features | 6,853 | 5 | 7 | 11 | — | 6 | 32 | 13 | 74 |
| | about | 4,042 | 8 | 7 | 4 | — | 3 | 3 | 2 | 27 |
| | blog | 10,760 | 3 | 7 | 2 | — | 6 | 13 | 6 | 37 |
| apple.com/airpods-pro | landing (primary) | 28,983 | 5 | 3 | 13 | 26 | 3 | 100 | 19 | 169 |
| | product | 19,064 | 11 | 3 | 3 | — | 9 | 21 | 6 | 52 |
| | shop | 12,569 | 5 | 3 | 2 | — | 7 | 13 | 3 | 39 |
| | features | 30,000 (cap) | 2 | 3 | 2 | — | 15 | 32 | 5 | 58 |
| | about | 30,000 (cap) | 3 | 3 | 1 | — | 14 | 53 | 10 | 85 |
| | services | 8,233 | 2 | 3 | 5 | — | 11 | 44 | 11 | 76 |

- **The primary page is 35–45 % of a site.** Its stable hero (screenshots until two frames match), the 26 s scroll video and its bands take 116–169 s.
- **Inner pages take 18–85 s each.** The full-page band screenshots dominate.
- **The CPU quota is the bottleneck.** Captures sat at the 1.5-CPU quota from start to end, and their threads spent 500–1,190 s throttled per site. SwiftShader rasterizes on the CPU what a GPU would draw.

### Memory

- **The peak comes from the primary page,** not from the inner pages: the scroll video, its H.264 encode running beside the full-page bands, and up to 3 ffmpeg processes in capture v2's encode pool. On `linear.app` at 1×, memory held reached 1.40 GiB 77 s in, during the primary page. The inner pages, one at a time, then ran at 0.6–0.95 GiB.
- **2× renders four times the pixels.** With 3 inner pages in parallel at 2×, every heavy site filled the 1.5 GiB limit. The kernel then killed renderers ("Target crashed"). Three times it killed the browser itself ("Target page, context or browser has been closed"), and every later page was skipped: in A, in C and in D. At 2× with 1 page (D), `linear.app` and `itsnicethat.com` still hit the limit, and `apple.com` crashed on its primary page in 11–12 s in both A and D.
- **Two pages at 1× (C)** finished `stripe.com` (9.1 min) where one page timed out. It saved 33 % on `apple.com`, but lost a page to 2 OOM kills. Elsewhere it saved 2–4 %, with memory held changing by −0.01 to +0.06 GiB. The CPU is already saturated.

### Network and output

| | In (from the internet) | Out | Artifacts |
|---|---|---|---|
| Real sites at 1×, completed | 93–283 MB | 0.2–1.7 MB | 4.4–14.5 MB |
| Fixtures | 0 (served by the router) | 0 | 0.06–0.36 MB |
| Blocked site (`aesop.com`) | 1.0 MB | 0.1 MB | 0 |

- **Download versus kept:** sites download far more than they keep. `apple.com` pulled 283 MB of video and imagery for 14 MB of artifacts.
- **Traffic:** at 2 captures per user per week that is about 2.5 GB per user per month, well inside the CX33's 20 TB.
- **The proxy:** it relayed 2.45 GB over 459 connections for all 35 runs. Its cgroup counted under 18 CPU-s in total, SPIKE-4's last probe run included, and a 22 MiB peak.

### Quality

These do not change the cost, but P4 needs them:

- **Scroll video.** The screencast is meant to run at up to 40 frames/s. On real sites it delivered 0.8–2.5 frames/s (11–51 frames over 7–27 s); on fixtures, 5.5–10. The MP4 is a slideshow, and `stripe.com` at 2× recorded 0 frames.
- **Full-page bands.** `linear.app`'s home page (9,960 px) produced no band in any configuration: each screenshot timed out after 60 s. At 2×, `stripe.com` and `itsnicethat.com` lost bands too, and some pages crashed.
- **WebGL (`lusion.co`).** It failed in every configuration, on the first screenshot (`page.screenshot: Timeout 45000ms exceeded`) after its 25 s preloader and capture v2's retry. In the desktop app the same capture runs on the GPU.
- **Anti-bot (`aesop.com`).** Cloudflare challenged the datacenter IP, as it challenged the desktop in the audit. Capture v2 detected it in 20 s and ended with `capture_blocked`.
- **Geolocation.** `stripe.com` redirected to `/de`. The VPS is in Germany, so localized sites will show their German variant unless the capture asks otherwise.

### Hermes

- **Status:** all 76 checks gave running, 0 restarts, start time `2026-10-02T10:11:58Z`, 2 GiB / 2 CPU, node online.
- **During captures:** Hermes's cgroup CPU pressure read `some avg10=0.00 avg60=0.00 avg300=0.00` while a capture held its quota (73 % pressure inside the capture container). The API's was 0.00 too.
- **Over the last 7 runs (41 min of captures),** Hermes's tasks waited 2.2 s in total for CPU (0.09 % of the time).
- **Over Hermes's 15.8 h uptime,** which includes this spike's 2.3 h of captures and the SPIKE-9 lane, its tasks waited 14.3 s in total.
- **Conclusion:** `cpu_shares` 1024 against the capture's 256, plus the capture's quota, protect Hermes as plan §3.1 intends.

### Load

- **Other work:** 0.16–0.43 cores during the runs (median about 0.2): osn's monitoring, Docker, Hermes at idle, SPIKE-9's containers when they ran, and this spike's sampler.
- **Headroom:** with the capture capped at 1.5 of 4 vCPUs, about 2 cores stayed idle throughout, so no run was starved by other work and none was repeated for load.
- **Load average:** it reached 11. That counts the capture's own throttled threads; the CPU figures come from the cgroups.

## Decisions

1. **Run with 1 inner page at a time, and capture at 1×.**
   - **What:** set `pagesParallel = 1`. Capture bands, sections and the hero at 1× on the server.
   - **Why:** at 1× every capture v2 step fits in the container. 2× does not fit in 1.5 GiB even one page at a time. Parallel pages save little, because the CPU is the bottleneck, and they cost OOM kills.
   - **Replaces:** §2.18's "2 pages in parallel (1 when WebGL-heavy)".
   - **The desktop keeps 2× (audit A6),** where a GPU exists. A server 2× hero needs P4 to measure it on its own (hero only at 2×) inside the limit below.
2. **§6.2, website capture.**
   - **Time:** p50 ≤ 6 min, p95 ≤ 12 min.
   - **Memory held:** ≤ 1.45 GiB.
   - **Site budget:** 12 min (was 10). A capture that runs out of budget keeps the pages already captured and ends as `partial`, not `timeout`.
   - **Page budget:** 180 s for the primary page, 150 s for the others.
3. **§6.4, CPU for capture.**
   - **Per site:** ~10 core-min, so ~8–9 sites/h at 1.5 CPU, about 200 a day.
   - **Users:** that is still far above the ~12 users the disk allows, at ≤ 2 sites per user per day. The "CPU, capture" row stays "not binding" until about 100 users, where it was already flagged.
4. **Memory limit: raise the capture container to 1.75 GiB, a lead decision on §3.1.**
   - **Headroom:** at 1× the peak was 1.43 GiB, 0.07 GiB under the 1.5 GiB limit. A renderer OOM kill costs a page, not the site, so 1.5 GiB is workable.
   - **Cost:** 1.75 GiB adds 0.25 GiB to Shelfy's limits (about 2.6 GiB in total), inside the ~5.8 GiB the host has free. `oom_score_adj: 600` still makes capture the first to die under host pressure.
   - **CPU:** keep the 1.5 CPU quota. This spike showed that `cpu_shares` already protects Hermes, so a burstable quota (`cpus: 3` with `cpu_shares: 256`) is possible later. That belongs to P5's capacity run.
5. **A degraded path for WebGL-heavy sites.**
   - **When the first screenshot times out:** end with the og:image fallback that §2.18 plans for blocked sites, instead of retrying. The retry doubled the cost on `lusion.co` and never succeeded.
   - **Scroll-jacked sites** get the same treatment if their filmstrip times out.

## Inputs for P4

Mapped onto the tasks of [phases/P4.md](../phases/P4.md):

1. **P4-03, the seams.** Its plan (an `electron` shim, injectable paths, launch options and page concurrency) matches what the harness needed. `src/electron-shim.ts`, `src/chromium.ts` and `src/session.ts` are working starting points.
   - **Also configurable:** the device scale and the encode-pool size.
   - **Budgets that change:** 1 page at a time (not 2, or 1 for WebGL), a 180 s budget for the primary page and 12 min for the site.
   - **Removed:** the Electron fallback driver.
2. **P4-03, code to move.** Move the v1 helpers the service still needs (`discoverPages`, `fetchImageToWebp`, `screenshotPathForUrl`, `resolveFfmpeg`) out of `electron/webcapture.ts` into `webcap/`, so the service stops bundling the v1 engine. `assemble.ts` should own `pickIcon`.
3. **P4-03 and P4-14, partial results.** `captureSite()` must return the pages it has when the site budget runs out. The service has already streamed them; the API ingests them as a partial capture, with a `partial` outcome next to `timeout`.
4. **The scroll video on this box.** At 1–2.5 frames/s it is not the smooth preview the feature promises. The options:
   - record at a lower resolution (`maxWidth` 720), with longer pauses between wheel steps;
   - make it opt-in, with the hover preview built from the bands;
   - accept a slideshow.

   This is a product decision for the lead and the owner. Until it is made, the service can run with `video: false`, which also removes the primary page's memory peak.
5. **Full-page bands.** Capture v2 takes each band as a `fullPage` screenshot with a clip, so Chromium renders the whole page height at once. On tall, animated pages (`linear.app`, 9,960 px) that times out without a GPU. Taking viewport screenshots while scrolling, as the filmstrip path already does, should be cheaper. Measure both before choosing.
6. **Encode pool.** Capture v2 runs up to 3 ffmpeg processes beside Chromium. At 1.5 CPU, with P4-03's `-threads 1`, 1 or 2 is likely enough and lowers the memory peak. Check it with the harness.
7. **P4-13, the image.** The Playwright image used here is 3.43 GB as Docker reports it, because it ships Firefox, WebKit and full Chromium. The §2.18 image is P4-13's:
   - check that Debian's ffmpeg has `libwebp` and `libx264`;
   - keep `fonts-noto-color-emoji` and `fonts-liberation`, which the Playwright image also had.
8. **P4-13, `shm_size`.** Capture v2 launches Chromium with `--disable-dev-shm-usage`, so its shared memory goes to `/tmp`, not `/dev/shm`. Together with the scratch frames, `/tmp` reached 150–380 MiB at 1×, and up to 570 MiB at 2×.
   - **Keep:** the 1 GiB `/tmp` tmpfs.
   - **`shm_size: 512m`:** harmless but unused; 64m is enough unless the flag goes.
   - **Memory:** both count toward the container's memory, as "held".
9. **P4-31, VPS measurements.** Use this note's revised budgets.
   - **Tool:** `capture-vps.sh cost` records, for each capture, CPU, memory held, bytes, OOM kills and Hermes's CPU wait. `admin capture-eval` should report at least these.
   - **Comparison:** the desktop baseline (P4-27) has a GPU, so expect it to succeed where this box cannot (WebGL heroes). The plan's rule applies: list such sites and record an accepted difference.

## Limits

- **Two runs at most per site and configuration.** Repeats agreed within 3 %, but real sites change; the ranges are indicative, not percentiles.
- **One machine:** the CX33's 4 shared vCPUs, with osn's monitoring and SPIKE-9 running alongside (the "other host CPU" column).
- **Not the production image:** the Playwright image and a static ffmpeg 6.0 stood in for the §2.18 image and Debian's ffmpeg. Encoding cost may differ a little; rendering should not, since it is the same Chromium build.
- **The audit sites are the hard cases** (WebGL, heavy media, anti-bot). Typical portfolio and landing sites should cost less, closer to the fixtures plus their page loads.

## Cleanup

`capture-vps.sh purge` removed everything at 02:00 UTC:
- the `shelfy-spike4-*` and `shelfy-spike11-*` containers;
- the `shelfy-spike-capture` and `shelfy-spike-egress` networks;
- the two pulled images (Playwright 3.43 GB, distroless 6 MB);
- `/tmp/shelfy-spike4-11` and `/var/tmp/shelfy-spike11-work`.

The checks afterwards:
- `docker ps -a` lists only the 8 osn containers;
- `docker network ls` lists only the networks that existed before;
- `docker images` has no Playwright or distroless image;
- `/tmp` holds only its 4 system entries;
- the root disk has 29 GB free, against 30 GB at the start (no spike files are left; other lanes and osn kept running).

## Reproduce

```sh
scripts/spikes/capture-vps.sh stage vpsfant
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh pull
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh up
# one site; arguments after the URL go to run-site.cjs (configuration B shown)
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh cost linear https://linear.app/ --page-concurrency 1 --dpr 1
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh cost fx-lenis https://fixtures.shelfy.test/lenis.html \
  --single-page --fixtures /harness/fixtures
ssh vpsfant sudo bash /tmp/shelfy-spike4-11/capture-vps.sh purge
```
