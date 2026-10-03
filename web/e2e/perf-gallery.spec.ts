/* eslint-disable */
//
// Web gallery scroll-performance harness (P1-08; plan §2.19, §6.2).
// ────────────────────────────────────────────────────────────────
// Measures the same thing the desktop profiler does (e2e/perf-gallery.spec.ts)
// against the web build: how fast new rows settle while scrolling, and
// whether the frame budget holds. SCROLL = real wheel events
// (`page.mouse.wheel()`, injected via CDP into Chromium's input pipeline), not
// `el.scrollTop =` — a synchronous main-thread mutation that never reproduces
// the compositor-path desync (banding, jank) a trackpad/wheel does. This is
// the trap the desktop harness's own header comment calls out, and it applies
// identically here: the grid component is shared (src/components/*).
//
// Opt-in, like the desktop harness — not part of the mocked-API web e2e suite
// (web/e2e/api.ts), and not run by a bare `playwright test`:
//
//   PERF=1 pnpm run perf:gallery:web
//
// This spec does NOT seed data. Prepare a synthetic library first, with the
// server stopped (P1-05's `admin synth`; carry-over note to P1-08):
//
//   CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 \
//     cargo build --release -p shelfy-server
//   ./target/release/shelfy-server admin create-owner \
//     --data-dir <dir> --email owner@example.test
//   ./target/release/shelfy-server admin synth \
//     --data-dir <dir> --email owner@example.test --posts 6000   # or 20000
//
// then point this spec at it (PERF_LIBRARY is cosmetic, for the report):
//
//   PERF=1 PERF_DATA_DIR=<dir> PERF_LIBRARY=6k pnpm run perf:gallery:web
//
// It builds web/dist once, then owns both process lifecycles itself: the API
// (`shelfy-server serve`) and the production SPA (`vite preview`, proxying
// /api and /media to the API — the same proxy web/vite.config.ts's dev server
// uses), on the lane's assigned ports (18189 / 18188; override with
// SHELFY_API_PERF_PORT / SHELFY_WEB_PERF_PORT). Sign-in uses a one-time
// `admin login-link` (E4: no SMTP), redeemed through the real /login/magic
// page — the server enforces CSRF/Origin against --public-url, so this is the
// same path a signed-in browser takes, not a bypass.
//
// Budgets (plan §6.2, this card's acceptance):
//   - no sampled frame > 50ms
//   - median fps >= 55
// Reported, not gated (no hard budget here, measured only): time to first
// thumbnails, JS heap (Chromium-only, best-effort).

import { test, expect, type Page } from '@playwright/test';
import { spawn, execFileSync, type ChildProcessByStdio } from 'node:child_process';
import { setTimeout as sleep } from 'node:timers/promises';
import path from 'node:path';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';
import type { Readable } from 'node:stream';

// Both children are spawned with stdio: ['ignore', 'pipe', 'pipe'] — stdout
// and stderr are readable streams, stdin doesn't exist.
type PipedChild = ChildProcessByStdio<null, Readable, Readable>;

const __filename = fileURLToPath(import.meta.url);
const __dirname = path.dirname(__filename);
const ROOT = path.join(__dirname, '..', '..'); // repo root (web/e2e/../..)

const SERVER_BIN = path.join(ROOT, 'target', 'release', 'shelfy-server');
const VITE_BIN = path.join(ROOT, 'node_modules', 'vite', 'bin', 'vite.js');
// P1 lane rule 1 (P1-n => port 18180+n): P1-08 => 18188/18189. This spec's
// own brief assigns them web/local-server, in that order.
const WEB_PORT = Number(process.env.SHELFY_WEB_PERF_PORT || 18188);
const API_PORT = Number(process.env.SHELFY_API_PERF_PORT || 18189);
const METRICS_PORT = Number(process.env.SHELFY_METRICS_PERF_PORT || 19464);
const DATA_DIR =
  process.env.PERF_DATA_DIR || '/Users/fant/work/experiments/shelfy-web-local/data/p1-8';
const EMAIL = process.env.PERF_EMAIL || 'perf-owner@example.test';
const LIBRARY_LABEL = process.env.PERF_LIBRARY || path.basename(DATA_DIR);
const WEB_ORIGIN = `http://127.0.0.1:${WEB_PORT}`;
const API_ORIGIN = `http://127.0.0.1:${API_PORT}`;

const TARGET_ROWS = 40; // rows of continuous scroll per measured phase
const VELOCITY = 3000; // px/s, a brisk but not flung continuous scroll
const JANK_MS = 50; // plan §6.2: no sampled frame over this
const FPS_BUDGET = 55; // plan §6.2: median fps at least this

function log(...args: unknown[]): void {
  console.log('[perf-gallery:web]', ...args);
}

async function waitForHealth(url: string, timeoutMs: number): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      const res = await fetch(url);
      if (res.ok) {
        const body = (await res.json()) as { status?: string };
        if (body.status === 'ok') return;
      }
    } catch {
      /* not listening yet */
    }
    if (Date.now() > deadline) throw new Error(`${url} did not become healthy in time`);
    await sleep(200);
  }
}

// Polls instead of watching stdout for vite's ready banner: piped (non-TTY)
// stdout is block-buffered by Node, so the banner can sit unflushed well past
// any reasonable timeout even though the server is already accepting
// connections (confirmed: curl succeeds against the port while the pipe
// stays silent).
async function waitForPort(url: string, timeoutMs: number, label: string): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  for (;;) {
    try {
      await fetch(url);
      return;
    } catch {
      /* not listening yet */
    }
    if (Date.now() > deadline) throw new Error(`${label}: ${url} did not come up in time`);
    await sleep(200);
  }
}

// Kills the whole process group, not just the spawned child — both server
// and preview are started with `detached: true` for exactly this: a negative
// pid signals the group, so a wrapper that forked its own child (vite's own
// launcher, in an earlier version of this spec) can't survive teardown as an
// orphan holding the port.
function killTree(child: PipedChild | null): void {
  if (!child || child.pid == null) return;
  try {
    process.kill(-child.pid, 'SIGKILL');
  } catch {
    try {
      child.kill('SIGKILL');
    } catch {
      /* already gone */
    }
  }
}

// ── Page-injected scroll instrumentation (mirrors e2e/perf-gallery.spec.ts,
// scoped down: one profile, no cold/cacheless modes — this is a first budget
// gate, not an iteration harness on a known-regressed area). ────────────────
function installPerf() {
  const w = window as unknown as { __perf: unknown };
  const state = {
    scroller: null as HTMLElement | null,
    mo: null as MutationObserver | null,
    rafId: 0,
    sampling: false,
    frames: [] as number[],
    firstThumbAt: null as number | null,
    phaseStart: 0,
    live: new Set<Element>(),
  };

  function findScroller(): HTMLElement | null {
    const grid = document.querySelector('[data-testid="post-grid"]');
    let el: Element | null = grid;
    while (el && el !== document.body) {
      const s = getComputedStyle(el);
      if (
        (s.overflowY === 'auto' || s.overflowY === 'scroll') &&
        (el as HTMLElement).scrollHeight > (el as HTMLElement).clientHeight + 4
      ) {
        return el as HTMLElement;
      }
      el = el.parentElement;
    }
    return grid ? (grid.parentElement as HTMLElement) : null;
  }

  function trackImage(img: HTMLImageElement) {
    if (state.live.has(img)) return;
    state.live.add(img);
    const mark = () => {
      if (state.firstThumbAt == null) state.firstThumbAt = performance.now();
    };
    if (img.complete && img.naturalWidth > 0) mark();
    else img.addEventListener('load', mark, { once: true });
  }

  function onMutations(muts: MutationRecord[]) {
    for (const m of muts) {
      m.addedNodes.forEach((n) => {
        if (!(n instanceof Element)) return;
        if (n.matches?.('[data-testid="card-image"]')) trackImage(n as HTMLImageElement);
        n.querySelectorAll?.('[data-testid="card-image"]').forEach((img) =>
          trackImage(img as HTMLImageElement),
        );
      });
    }
  }

  function sampleFrame(now: number) {
    if (!state.sampling) return;
    state.frames.push(now);
    state.rafId = requestAnimationFrame(sampleFrame);
  }

  (w as unknown as { __perf: Record<string, unknown> }).__perf = {
    info() {
      state.scroller = findScroller();
      const el = state.scroller;
      return {
        found: !!el,
        scrollHeight: el ? el.scrollHeight : 0,
        clientHeight: el ? el.clientHeight : 0,
        scrollable: el ? el.scrollHeight > el.clientHeight + 4 : false,
        rowHeight: (() => {
          const row = el?.querySelector('[data-index]');
          return row ? Math.round(row.getBoundingClientRect().height) : 0;
        })(),
        cards: document.querySelectorAll('[data-testid="post-card"]').length,
        center: (() => {
          if (!el) return { x: window.innerWidth / 2, y: window.innerHeight / 2 };
          const r = el.getBoundingClientRect();
          return { x: Math.round(r.left + r.width / 2), y: Math.round(r.top + r.height / 2) };
        })(),
      };
    },
    beginPhase() {
      if (!state.scroller) state.scroller = findScroller();
      state.frames = [];
      state.phaseStart = performance.now();
      state.mo = new MutationObserver(onMutations);
      if (state.scroller) state.mo.observe(state.scroller, { childList: true, subtree: true });
      state.sampling = true;
      state.rafId = requestAnimationFrame(sampleFrame);
    },
    scrollState() {
      const el = state.scroller || (state.scroller = findScroller());
      if (!el) return { scrollTop: 0, maxTop: 0 };
      return { scrollTop: el.scrollTop, maxTop: Math.max(0, el.scrollHeight - el.clientHeight) };
    },
    markScrollEnd() {
      state.sampling = false;
      if (state.rafId) cancelAnimationFrame(state.rafId);
    },
    endPhase() {
      state.mo?.disconnect();
      const heap = (performance as unknown as { memory?: { usedJSHeapSize: number } }).memory
        ?.usedJSHeapSize;
      return {
        frames: state.frames.slice(),
        firstThumbMs:
          state.firstThumbAt != null ? Math.round(state.firstThumbAt - state.phaseStart) : null,
        cardsMounted: document.querySelectorAll('[data-testid="post-card"]').length,
        heapBytes: typeof heap === 'number' ? heap : null,
      };
    },
  };
}

interface PerfResult {
  fps: { median: number; avg: number };
  frameMaxMs: number;
  framesOver50: number;
  frameCount: number;
  firstThumbMs: number | null;
  cardsMounted: number;
  heapBytes: number | null;
  scrollTraveledPx: number;
}

function median(nums: number[]): number {
  const s = [...nums].sort((a, b) => a - b);
  const m = Math.floor(s.length / 2);
  return s.length === 0 ? 0 : s.length % 2 ? s[m] : (s[m - 1] + s[m]) / 2;
}

async function measureScroll(page: Page, rowHeight: number, center: { x: number; y: number }) {
  const target = Math.max(1, rowHeight) * TARGET_ROWS;
  await page.evaluate(() => (window as any).__perf.beginPhase());

  await page.mouse.move(center.x, center.y);
  const t0 = Date.now();
  let sent = 0;
  const guardMs = Math.min(60_000, (target / VELOCITY) * 1000 * 2 + 4000);
  for (;;) {
    const elapsedS = (Date.now() - t0) / 1000;
    const delta = Math.min(260, Math.max(0, Math.round(VELOCITY * elapsedS - sent)));
    if (delta > 0) {
      await page.mouse.wheel(0, delta);
      sent += delta;
    } else {
      await page.waitForTimeout(2);
    }
    const s: { scrollTop: number; maxTop: number } = await page.evaluate(() =>
      (window as any).__perf.scrollState(),
    );
    if (s.scrollTop >= target || s.maxTop - s.scrollTop < 2) break;
    if (Date.now() - t0 > guardMs) break;
  }
  const finalScroll: { scrollTop: number } = await page.evaluate(() =>
    (window as any).__perf.scrollState(),
  );
  await page.evaluate(() => (window as any).__perf.markScrollEnd());
  // Let any in-flight thumbnail settle before reading firstThumbMs/cards.
  await page.waitForTimeout(300);
  const data = await page.evaluate(() => (window as any).__perf.endPhase());

  const frames: number[] = data.frames;
  const deltas: number[] = [];
  for (let i = 1; i < frames.length; i++) deltas.push(frames[i] - frames[i - 1]);
  const span = frames.length > 1 ? frames[frames.length - 1] - frames[0] : 0;
  const avgFps = span > 0 ? ((frames.length - 1) / span) * 1000 : 0;
  const medFps = (() => {
    const m = median(deltas);
    return m > 0 ? 1000 / m : 0;
  })();

  const result: PerfResult = {
    fps: { median: Math.round(medFps * 10) / 10, avg: Math.round(avgFps * 10) / 10 },
    frameMaxMs: deltas.length ? Math.round(Math.max(...deltas)) : 0,
    framesOver50: deltas.filter((d) => d > JANK_MS).length,
    frameCount: deltas.length,
    firstThumbMs: data.firstThumbMs,
    cardsMounted: data.cardsMounted,
    heapBytes: data.heapBytes,
    scrollTraveledPx: Math.round(finalScroll.scrollTop),
  };
  return result;
}

function fmtKB(bytes: number | null): string {
  return bytes == null ? '–' : `${Math.round(bytes / 1024)} KB`;
}

function report(label: string, r: PerfResult): string {
  return [
    `  library:        ${label}`,
    `  cards mounted:  ${r.cardsMounted}`,
    `  scroll:         ${r.scrollTraveledPx}px over ${r.frameCount} sampled frames`,
    `  fps:            median ${r.fps.median}  avg ${r.fps.avg}`,
    `  frame max:      ${r.frameMaxMs}ms  (frames > ${JANK_MS}ms: ${r.framesOver50})`,
    `  first thumb:    ${r.firstThumbMs == null ? '–' : `${r.firstThumbMs}ms`}`,
    `  JS heap:        ${fmtKB(r.heapBytes)}`,
  ].join('\n');
}

test.describe('Web gallery scroll performance', () => {
  test.skip(!process.env.PERF, 'Profiler opt-in: run with PERF=1 (pnpm run perf:gallery:web).');
  test.describe.configure({ mode: 'serial' });

  test(`scroll latency and frame budget — ${LIBRARY_LABEL}`, async ({ browser }) => {
    test.setTimeout(600_000);

    if (!fs.existsSync(SERVER_BIN)) {
      throw new Error(
        `${SERVER_BIN} not found. Build it first:\n` +
          `  CARGO_TARGET_DIR="$PWD/target" CARGO_INCREMENTAL=0 cargo build --release -p shelfy-server`,
      );
    }
    if (!fs.existsSync(DATA_DIR)) {
      throw new Error(
        `${DATA_DIR} not found. Seed it first (server stopped):\n` +
          `  ${SERVER_BIN} admin create-owner --data-dir ${DATA_DIR} --email ${EMAIL}\n` +
          `  ${SERVER_BIN} admin synth --data-dir ${DATA_DIR} --email ${EMAIL} --posts 6000`,
      );
    }

    log('building web/dist…');
    execFileSync('pnpm', ['run', 'web:build'], { cwd: ROOT, stdio: 'inherit' });

    log(`starting shelfy-server on ${API_ORIGIN} (data dir ${DATA_DIR})…`);
    const server = spawn(
      SERVER_BIN,
      [
        'serve',
        '--data-dir',
        DATA_DIR,
        '--listen',
        `127.0.0.1:${API_PORT}`,
        '--metrics-listen',
        `127.0.0.1:${METRICS_PORT}`,
        '--public-url',
        WEB_ORIGIN,
      ],
      { stdio: ['ignore', 'pipe', 'pipe'], detached: true },
    );
    server.stderr.on('data', (d) => log('[server]', d.toString().trim()));

    let preview: PipedChild | null = null;
    try {
      await waitForHealth(`${API_ORIGIN}/health`, 20_000);
      log('server healthy');

      log('minting a sign-in link (admin login-link)…');
      const linkOut = execFileSync(
        SERVER_BIN,
        [
          'admin',
          'login-link',
          '--data-dir',
          DATA_DIR,
          '--public-url',
          WEB_ORIGIN,
          '--email',
          EMAIL,
        ],
        { encoding: 'utf8' },
      );
      const magicUrl = linkOut
        .split('\n')
        .map((l) => l.trim())
        .find((l) => l.startsWith(WEB_ORIGIN));
      if (!magicUrl) throw new Error(`admin login-link printed no URL:\n${linkOut}`);

      log(`starting vite preview on ${WEB_ORIGIN}…`);
      // Spawns the resolved vite binary directly, not `pnpm exec vite` — pnpm
      // wraps it through two more process layers (its corepack shim, then its
      // own Node launcher), and killing only the top one at teardown left the
      // real vite grandchild running as an orphan (found still bound to this
      // same port after an earlier, interrupted run).
      preview = spawn(
        process.execPath,
        [
          VITE_BIN,
          'preview',
          '--config',
          'web/vite.config.ts',
          '--host',
          '127.0.0.1',
          '--port',
          String(WEB_PORT),
          '--strictPort',
        ],
        {
          cwd: ROOT,
          stdio: ['ignore', 'pipe', 'pipe'],
          env: { ...process.env, SHELFY_API_URL: API_ORIGIN },
          detached: true,
        },
      );
      preview.stderr.on('data', (d) => log('[preview]', d.toString().trim()));
      await waitForPort(WEB_ORIGIN, 30_000, 'vite preview');

      const page = await browser.newPage({ viewport: { width: 1440, height: 900 } });
      const thirdParty: string[] = [];
      page.on('request', (req) => {
        const origin = new URL(req.url()).origin;
        if (origin !== WEB_ORIGIN) thirdParty.push(req.url());
      });

      log(`signing in via ${magicUrl}`);
      await page.goto(magicUrl, { waitUntil: 'domcontentloaded' });
      await page.getByTestId('magic-sign-in').click();
      await page.waitForSelector('[data-testid="post-card"]', { timeout: 20_000 });

      // A fresh (synth) account has never recorded consent, so the first-run
      // disclaimer gate covers the whole viewport (fixed inset-0) and would
      // silently swallow every wheel event otherwise — found by a diagnostic
      // elementFromPoint() dump when the very first run scrolled 0px despite
      // "sending" wheel deltas for 9+ seconds.
      const disclaimer = page.getByTestId('disclaimer-gate');
      if (await disclaimer.isVisible().catch(() => false)) {
        // The real input is `sr-only` (visually hidden; a styled <span>
        // stands in for it inside the same <label>), so Playwright's default
        // actionability check sees the label intercepting the click at the
        // input's own (near-invisible) box — force it, as it's this
        // component's intended hit target, not a layering bug.
        await page.getByTestId('disclaimer-checkbox').click({ force: true });
        await page.getByTestId('disclaimer-accept').click();
        await expect(disclaimer).toBeHidden();
      }

      await page.evaluate(installPerf);
      const info: {
        found: boolean;
        scrollable: boolean;
        rowHeight: number;
        center: { x: number; y: number };
      } = await page.evaluate(() => (window as any).__perf.info());
      expect(info.found, 'the gallery scroll container must exist').toBe(true);
      if (!info.scrollable) {
        log(
          `WARNING: the ${LIBRARY_LABEL} library does not fill the viewport — numbers below are not meaningful.`,
        );
      }

      const result = await measureScroll(page, info.rowHeight, info.center);
      console.log('\n' + report(LIBRARY_LABEL, result) + '\n');

      const outDir = path.join(ROOT, 'perf-results');
      fs.mkdirSync(outDir, { recursive: true });
      fs.writeFileSync(
        path.join(outDir, `gallery-web-${LIBRARY_LABEL}.json`),
        JSON.stringify({ library: LIBRARY_LABEL, ...result }, null, 2),
      );

      // #10: the SPA makes zero third-party requests — a synth library (no
      // remote URLs; synth.rs's own doc comment) makes this a clean check.
      expect(thirdParty, 'third-party requests').toEqual([]);

      // plan §6.2 / this card's acceptance.
      expect(result.frameMaxMs, 'no sampled frame over the jank budget').toBeLessThanOrEqual(
        JANK_MS,
      );
      expect(result.fps.median, 'median fps at least the budget').toBeGreaterThanOrEqual(
        FPS_BUDGET,
      );

      await page.close();
    } finally {
      killTree(preview);
      killTree(server);
    }
  });
});
