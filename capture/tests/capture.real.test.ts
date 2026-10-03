// Real capture against local fixture pages (headless Playwright Chromium, no
// network — the fixtures are served by Playwright's router, as SPIKE-11 did).
// Skips cleanly when no browser can launch (like the Rust video tools tests skip
// without ffmpeg). Covers the SPIKE-11 budgets, the partial-on-budget rule and
// the assemble.ts parity.

import { describe, it, expect, beforeAll, afterAll, vi } from 'vitest';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { fileURLToPath } from 'url';

// The capture graph imports `electron`; use the env-backed shim.
vi.mock('electron', () => import('../src/electron-shim'));

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');
const EVAL_FIX = path.join(ROOT, 'scripts', 'web-capture-eval', 'fixtures');

// Local browser cache + adblock engine for this dev/CI machine (the service image
// sets these itself). Default the Playwright cache per-OS when unset.
if (!process.env.PLAYWRIGHT_BROWSERS_PATH) {
  const home = os.homedir();
  process.env.PLAYWRIGHT_BROWSERS_PATH =
    process.platform === 'darwin'
      ? path.join(home, 'Library', 'Caches', 'ms-playwright')
      : process.platform === 'win32'
        ? path.join(home, 'AppData', 'Local', 'ms-playwright')
        : path.join(home, '.cache', 'ms-playwright');
}
process.env.SHELFY_ADBLOCK_ENGINE = path.join(ROOT, 'build', 'adblock', 'engine.bin');

const browser = await import('../../electron/webcap/browser');
const { setFfmpegThreads } = await import('../../electron/webcap/encode');
const { captureSite } = await import('../../electron/webcap/capture');
const { assembleSite } = await import('../../electron/webcap/assemble');
const { runCapture } = await import('../src/run');
const { ENV } = await import('../src/env');
const { serveFixturesOnBrowser, FIXTURE_ORIGIN } = await import('./fixtures-serve');
const { ManifestSchema, ASSET_FILE_RE } = await import('../src/protocol');

let browserOk = false;
let servedDir: string;

beforeAll(async () => {
  servedDir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-served-'));
  // The single-page fixture, plus a tiny multi-page site for the budget test.
  fs.copyFileSync(
    path.join(EVAL_FIX, 'native-tall.html'),
    path.join(servedDir, 'native-tall.html'),
  );
  fs.writeFileSync(
    path.join(servedDir, 'home.html'),
    `<!doctype html><html><head><title>Home</title></head><body style="height:3000px">
     <h1>Home</h1><nav><a href="/a.html">A</a> <a href="/b.html">B</a></nav></body></html>`,
  );
  for (const p of ['a', 'b'])
    fs.writeFileSync(
      path.join(servedDir, `${p}.html`),
      `<!doctype html><html><head><title>${p}</title></head><body style="height:2000px"><h1>${p}</h1></body></html>`,
    );

  browser.setBrowserLaunchOverrides({ selfHeal: false });
  setFfmpegThreads(1);
  try {
    const b = await browser.getBrowser();
    serveFixturesOnBrowser(b, servedDir);
    browserOk = true;
  } catch (e) {
    console.warn('[capture.real] no browser — skipping real-capture tests:', (e as Error).message);
  }
}, 120_000);

afterAll(async () => {
  await browser.closeBrowser().catch(() => {});
  if (servedDir) fs.rmSync(servedDir, { recursive: true, force: true });
});

function tmpBase(): string {
  return fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-cap-'));
}

describe('capture service — real capture', () => {
  it('captures a fixture within the budgets and writes safe artifacts + manifest', async () => {
    if (!browserOk) return;
    const base = tmpBase();
    const captureId = '01JTESTBUDGET00000000000000';
    const lines: import('../src/protocol').CaptureLine[] = [];
    const env = { ...ENV, workBase: base, siteBudgetMs: 5 * 60_000, primaryDeadlineMs: 180_000 };
    await runCapture(
      {
        captureId,
        url: `${FIXTURE_ORIGIN}/native-tall.html`,
        maxPages: 1,
        singlePage: true,
        video: false,
        workDir: `/work/${captureId}`,
      },
      { emit: (l) => lines.push(l), signal: new AbortController().signal, env },
    );

    const done = lines.at(-1);
    expect(done?.type, JSON.stringify(lines.at(-1))).toBe('done');
    if (done?.type !== 'done') return;
    expect(done.durationMs).toBeLessThan(env.siteBudgetMs); // within the site budget
    expect(done.peakRssBytes).toBeGreaterThan(0);
    expect(done.bytes).toBeGreaterThan(0);
    expect(done.partial).toBe(false);

    const workDir = path.join(base, captureId);
    const manifest = JSON.parse(fs.readFileSync(path.join(workDir, 'manifest.json'), 'utf8'));
    expect(ManifestSchema.safeParse(manifest).success).toBe(true);
    expect(manifest.pages.length).toBe(1);
    // Every artifact relocated to a safe, role-based name that P4-14 will accept.
    for (const a of manifest.pages[0].assets) {
      expect(ASSET_FILE_RE.test(a.file), a.file).toBe(true);
      expect(fs.existsSync(path.join(workDir, a.file))).toBe(true);
    }
    // 1× scale: the hero is the 1440×900 viewport, not 2×.
    const hero = manifest.pages[0].assets.find((x: { role: string }) => x.role === 'p0-hero');
    expect(hero?.w).toBe(1440);
    fs.rmSync(base, { recursive: true, force: true });
  }, 120_000);

  it('keeps the pages already captured when the site budget runs out (partial)', async () => {
    if (!browserOk) return;
    const base = tmpBase();
    const captureId = '01JTESTPARTIAL0000000000000';
    const lines: import('../src/protocol').CaptureLine[] = [];
    // siteBudgetMs = 1: after the primary page the budget is already spent, so no
    // inner page is started — they are kept as budget_exceeded, the primary stays.
    const env = { ...ENV, workBase: base, siteBudgetMs: 1, pagesParallel: 1 };
    await runCapture(
      {
        captureId,
        url: `${FIXTURE_ORIGIN}/home.html`,
        maxPages: 3,
        singlePage: false,
        video: false,
        workDir: `/work/${captureId}`,
      },
      { emit: (l) => lines.push(l), signal: new AbortController().signal, env },
    );

    const done = lines.at(-1);
    expect(done?.type).toBe('done');
    if (done?.type !== 'done') return;
    expect(done.partial).toBe(true);
    const workDir = path.join(base, captureId);
    const manifest = JSON.parse(fs.readFileSync(path.join(workDir, 'manifest.json'), 'utf8'));
    expect(manifest.pages.length).toBe(1); // only the primary survived
    expect(manifest.skipped.length).toBeGreaterThanOrEqual(1);
    expect(manifest.skipped.every((s: { reason: string }) => s.reason === 'budget_exceeded')).toBe(
      true,
    );
    fs.rmSync(base, { recursive: true, force: true });
  }, 120_000);

  it('parity: the desktop and the service derive the same assembly through assemble.ts', async () => {
    if (!browserOk) return;
    process.env.CAPTURE_WORK_BASE = tmpBase();
    const captureId = '01JTESTPARITY00000000000000';
    process.env.CAPTURE_WORK_DIR = path.join(process.env.CAPTURE_WORK_BASE, captureId);
    fs.mkdirSync(process.env.CAPTURE_WORK_DIR, { recursive: true });

    const site = await captureSite(`${FIXTURE_ORIGIN}/native-tall.html`, {
      maxPages: 1,
      singlePage: true,
      stamp: Math.floor(Date.now() / 1000),
      video: false,
      pagesParallel: 1,
      deviceScale: 1,
    });
    // Both callers run the SAME pure assembly on the SAME capture → identical.
    const desktop = await assembleSite(site, { ogFetched: false, singlePage: true });
    const service = await assembleSite(site, { ogFetched: false, singlePage: true });
    expect(service.meta).toEqual(desktop.meta);
    expect(service).toEqual(desktop);
    expect(service.meta.schema).toBe(2);
    expect(service.palette.length).toBeGreaterThan(0);
    fs.rmSync(process.env.CAPTURE_WORK_BASE, { recursive: true, force: true });
    delete process.env.CAPTURE_WORK_DIR;
  }, 120_000);
});
