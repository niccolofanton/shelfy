// Shared headless-Chromium lifecycle for the web-reference capture (v2).
//
// Moved out of electron/webcapture-playwright.ts (SPIKE-11) so the capture path
// (electron/webcap/*) and the P4 capture service can launch a browser without
// bundling the v1 Electron engine or its cookie bridge. webcapture-playwright.ts
// re-exports getBrowser/closeBrowser/getChromium from here, so the desktop keeps
// one shared browser process and is otherwise unchanged.
//
// The capture service configures launch through setBrowserLaunchOverrides()
// (Playwright's `proxy` option per SPIKE-4, the egress flags, sandbox on, no
// runtime Chromium self-install) and arms the 120 s idle close (plan §2.18).

import path from 'path';
import fs from 'fs';
import os from 'os';
import { execFile } from 'child_process';
import { app } from 'electron';
import type { Browser, BrowserType } from 'playwright-core';

// ─── Injectable launch options ───────────────────────────────────────────────
//
// The desktop launches with defaults; the capture service injects the egress
// proxy, extra flags and a fixed sandbox/headless/executable, and turns the
// runtime self-install OFF (the image ships chromium-headless-shell).

export interface BrowserLaunchOverrides {
  proxy?: { server: string; bypass?: string };
  extraArgs?: string[];
  chromiumSandbox?: boolean;
  headless?: boolean;
  executablePath?: string;
  channel?: string;
  selfHeal?: boolean; // default true (desktop); the service sets false
}

let _overrides: BrowserLaunchOverrides = {};
export function setBrowserLaunchOverrides(o: BrowserLaunchOverrides): void {
  _overrides = { ..._overrides, ...o };
}
export function getBrowserLaunchOverrides(): Readonly<BrowserLaunchOverrides> {
  return _overrides;
}

// ─── Chromium resolution (dev vs packaged) — ALWAYS available ────────────────

function userDataBrowsersPath(): string {
  try {
    return path.join(app.getPath('userData'), 'ms-playwright');
  } catch {
    return path.join(os.tmpdir(), 'shelfy-ms-playwright');
  }
}

// Decide WHERE playwright-core should look for browsers, set before it's required.
// Prefer an explicit env override, then the intact bundled copy (packaged), then a
// writable per-user dir we can populate.
function ensureBrowsersPath(): string | null {
  if (process.env.PLAYWRIGHT_BROWSERS_PATH) return process.env.PLAYWRIGHT_BROWSERS_PATH;
  try {
    if (app && app.isPackaged) {
      const bundled = path.join(process.resourcesPath || '', 'ms-playwright');
      if (dirHasHeadlessShell(bundled)) {
        process.env.PLAYWRIGHT_BROWSERS_PATH = bundled;
        return bundled;
      }
      const ud = userDataBrowsersPath();
      process.env.PLAYWRIGHT_BROWSERS_PATH = ud;
      return ud;
    }
  } catch {
    /* fall through to the default cache */
  }
  return null;
}

// True when a browsers dir contains a FULLY installed chromium headless shell
// (the INSTALLATION_COMPLETE marker), so an interrupted download is rejected.
function dirHasHeadlessShell(dir: string): boolean {
  try {
    return (
      fs.existsSync(dir) &&
      fs
        .readdirSync(dir)
        .some(
          (d) =>
            /^chromium_headless_shell-/.test(d) &&
            fs.existsSync(path.join(dir, d, 'INSTALLATION_COMPLETE')),
        )
    );
  } catch {
    return false;
  }
}

let _playwright: typeof import('playwright-core') | null = null;
export function getChromium(): BrowserType {
  if (!_playwright) {
    ensureBrowsersPath();
    _playwright = require('playwright-core') as typeof import('playwright-core');
  }
  return _playwright.chromium;
}

function resolveCliPath(): string {
  let p: string;
  try {
    p = require.resolve('playwright-core/cli.js');
  } catch {
    p = path.join(__dirname, '..', 'node_modules', 'playwright-core', 'cli.js');
  }
  if (p.includes('app.asar') && !p.includes('app.asar.unpacked')) {
    const unpacked = p.replace('app.asar', 'app.asar.unpacked');
    if (fs.existsSync(unpacked)) return unpacked;
  }
  return p;
}

function isMissingExecutableError(err: unknown): boolean {
  return /Executable doesn't exist|chrome-headless-shell|playwright install|browserType\.launch.*ENOENT/i.test(
    String((err as { message?: string } | null | undefined)?.message || err || ''),
  );
}

// Runtime self-heal: download the chromium HEADLESS SHELL into the resolved
// writable browsers path. Disabled by the capture service (the image ships it).
let _installPromise: Promise<boolean> | null = null;
function installChromium(): Promise<boolean> {
  let packaged = false;
  try {
    packaged = !!(app && app.isPackaged);
  } catch {
    try {
      packaged = !!(
        process.resourcesPath && fs.existsSync(path.join(process.resourcesPath, 'app.asar'))
      );
    } catch {
      packaged = false;
    }
  }
  if (packaged) {
    console.warn(
      '[webcap/browser] Chromium headless shell missing in the packaged build — runtime self-heal is disabled; web captures fall back to the OSR engine',
    );
    return Promise.resolve(false);
  }
  if (_installPromise) return _installPromise;
  _installPromise = (async () => {
    const browsersPath =
      process.env.PLAYWRIGHT_BROWSERS_PATH ||
      (app && app.isPackaged ? userDataBrowsersPath() : null);
    try {
      if (browsersPath) fs.mkdirSync(browsersPath, { recursive: true });
    } catch {}
    console.warn('[webcap/browser] Chromium missing — downloading headless shell (one-time)…');
    const cli = resolveCliPath();
    await new Promise<void>((resolve, reject) => {
      execFile(
        process.execPath,
        [cli, 'install', 'chromium-headless-shell'],
        {
          env: {
            ...process.env,
            ELECTRON_RUN_AS_NODE: '1',
            ...(browsersPath ? { PLAYWRIGHT_BROWSERS_PATH: browsersPath } : {}),
          },
          timeout: 10 * 60_000,
          maxBuffer: 1024 * 1024 * 32,
        },
        (err) => (err ? reject(err) : resolve()),
      );
    });
    console.warn('[webcap/browser] Chromium headless shell installed');
    return true;
  })().catch((err) => {
    _installPromise = null;
    throw err;
  });
  return _installPromise;
}

// GL backend per platform. Metal/D3D11 where a GPU exists; Linux headless stays on
// SwiftShader. `--enable-unsafe-swiftshader` is the universal software fallback.
function glArgs(): string[] {
  if (process.platform === 'darwin') return ['--use-gl=angle', '--use-angle=metal'];
  if (process.platform === 'win32') return ['--use-gl=angle', '--use-angle=d3d11'];
  return ['--use-gl=angle', '--use-angle=swiftshader'];
}
const LAUNCH_ARGS = [
  '--ignore-gpu-blocklist',
  '--enable-webgl',
  '--enable-unsafe-swiftshader',
  ...glArgs(),
  '--disable-dev-shm-usage',
  '--hide-scrollbars',
  '--mute-audio',
];

// One shared browser process for the whole app (capture concurrency is 1 in the
// orchestrator, but a shared instance also amortizes launch cost across captures).
let _browser: Browser | null = null;
let _browserPromise: Promise<Browser> | null = null;

export async function getBrowser(): Promise<Browser> {
  if (_browser && _browser.isConnected()) return _browser;
  if (_browserPromise) return _browserPromise;
  _browserPromise = (async () => {
    const chromium = getChromium();
    // Chromium's sandbox stays ON: this browser renders arbitrary remote content.
    // Playwright adds --no-sandbox unless chromiumSandbox is explicitly true, so
    // the flag is mandatory. The capture service may inject a proxy, extra flags,
    // a fixed executable/channel and the sandbox setting.
    const o = _overrides;
    const launch = (): Promise<Browser> =>
      chromium.launch({
        headless: o.headless ?? true,
        args: [...LAUNCH_ARGS, ...(o.extraArgs || [])],
        chromiumSandbox: o.chromiumSandbox ?? process.env.SHELFY_DISABLE_SANDBOX !== '1',
        ...(o.proxy ? { proxy: o.proxy } : {}),
        ...(o.executablePath ? { executablePath: o.executablePath } : {}),
        ...(o.channel ? { channel: o.channel } : {}),
      });
    let b: Browser;
    try {
      b = await launch();
    } catch (err) {
      // Binary not installed → self-heal (desktop only). The capture service sets
      // selfHeal:false, so a missing executable fails the capture instead.
      if (!isMissingExecutableError(err) || o.selfHeal === false) throw err;
      const installed = await installChromium();
      if (installed === false) throw err;
      b = await launch();
    }
    _browser = b;
    b.on('disconnected', () => {
      if (_browser === b) _browser = null;
    });
    return b;
  })()
    .then((b) => {
      _browserPromise = null;
      return b;
    })
    .catch((err) => {
      _browserPromise = null;
      throw err;
    });
  return _browserPromise;
}

// Whether a shared browser is currently launched and connected (for /health).
export function isBrowserConnected(): boolean {
  return !!_browser && _browser.isConnected();
}

export async function closeBrowser(): Promise<void> {
  cancelBrowserIdle();
  const b = _browser;
  _browser = null;
  _browserPromise = null;
  if (b) {
    try {
      await b.close();
    } catch {
      /* already gone */
    }
  }
}

// ─── Idle auto-close (plan §2.18: close the browser after 120 s idle) ──────────

let _idleTimer: ReturnType<typeof setTimeout> | null = null;
let _idleMs = 0;

// Configure the idle window (0 = never). The capture service sets 120_000.
export function setBrowserIdleTimeout(ms: number): void {
  _idleMs = ms > 0 ? ms : 0;
}

// Cancel any pending idle close (call when a capture starts).
export function cancelBrowserIdle(): void {
  if (_idleTimer) {
    clearTimeout(_idleTimer);
    _idleTimer = null;
  }
}

// (Re)arm the idle close (call when a capture finishes and the browser is idle).
export function armBrowserIdleClose(): void {
  cancelBrowserIdle();
  if (_idleMs > 0) {
    _idleTimer = setTimeout(() => {
      _idleTimer = null;
      void closeBrowser();
    }, _idleMs);
    _idleTimer.unref?.();
  }
}
