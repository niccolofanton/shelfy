// What the extension's real-browser smoke tests share (smoke.ts for P2-06's capture and queue,
// sync-smoke.ts for P2-13's sync controller): the check log, Playwright's Chromium with the
// unpacked build, the fake Shelfy server (scripts/fake-api.ts over HTTP), and helpers on the
// browser side. Nothing here reaches a real platform or server.

import { existsSync, readdirSync } from 'node:fs';
import { createServer, type IncomingMessage, type Server, type ServerResponse } from 'node:http';
import { homedir, platform as osPlatform } from 'node:os';
import { join } from 'node:path';
import { chromium, type BrowserContext, type Page, type Worker } from 'playwright-core';
import { EXTENSION_ID } from '../src/id';
import { MSG } from '../src/shared/protocol';
import type { PanelState } from '../src/sw/state';
import { FakeShelfyApi } from './fake-api';

// ── The check log ───────────────────────────────────────────────────────────

let failures = 0;

export function check(condition: boolean, what: string, detail = ''): void {
  if (!condition) failures += 1;
  console.log(`${condition ? 'ok  ' : 'FAIL'} - ${what}${detail ? ` (${detail})` : ''}`);
}

export async function waitFor(
  condition: () => boolean | Promise<boolean>,
  what: string,
  timeoutMs = 20_000,
): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (await condition()) return true;
    await new Promise((resolve) => setTimeout(resolve, 200));
  }
  console.log(`     (timed out after ${timeoutMs} ms waiting for ${what})`);
  return false;
}

export const sleep = (ms: number) => new Promise((resolve) => setTimeout(resolve, ms));

/** Runs a smoke test and sets the exit code from its checks. */
export function runSmoke(main: () => Promise<void>): void {
  main()
    .catch((err: unknown) => {
      failures += 1;
      console.error(`FAIL - ${err instanceof Error ? (err.stack ?? err.message) : String(err)}`);
    })
    .finally(() => {
      console.log(failures ? `\n${failures} check(s) failed` : '\nall checks passed');
      process.exitCode = failures ? 1 : 0;
    });
}

// ── Chromium ─────────────────────────────────────────────────────────────────

const CHROME_BINARIES = [
  'chrome-mac-arm64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
  'chrome-mac-x64/Google Chrome for Testing.app/Contents/MacOS/Google Chrome for Testing',
  'chrome-mac/Chromium.app/Contents/MacOS/Chromium',
  'chrome-linux64/chrome',
  'chrome-linux/chrome',
  'chrome-win64/chrome.exe',
  'chrome-win/chrome.exe',
];

/** `chrome`, else Playwright's own Chromium, else the newest full Chromium in its cache. */
export function findChromium(chrome?: string): string | undefined {
  if (chrome) return chrome;
  const bundled = chromium.executablePath();
  if (existsSync(bundled)) return bundled;
  const cache =
    process.env.PLAYWRIGHT_BROWSERS_PATH ??
    (osPlatform() === 'darwin'
      ? join(homedir(), 'Library', 'Caches', 'ms-playwright')
      : osPlatform() === 'win32'
        ? join(homedir(), 'AppData', 'Local', 'ms-playwright')
        : join(homedir(), '.cache', 'ms-playwright'));
  if (!existsSync(cache)) return undefined;
  const builds = readdirSync(cache)
    .filter((name) => /^chromium-\d+$/.test(name))
    .sort((a, b) => Number(b.slice(9)) - Number(a.slice(9)));
  for (const build of builds)
    for (const binary of CHROME_BINARIES)
      if (existsSync(join(cache, build, binary))) return join(cache, build, binary);
  return undefined;
}

export interface LaunchOptions {
  headed?: boolean;
  chrome?: string;
}

export async function launch(
  profile: string,
  dist: string,
  options: LaunchOptions = {},
): Promise<BrowserContext> {
  return chromium.launchPersistentContext(profile, {
    headless: !options.headed,
    executablePath: findChromium(options.chrome),
    args: [
      `--disable-extensions-except=${dist}`,
      `--load-extension=${dist}`,
      '--disable-background-networking',
      '--disable-component-update',
      '--no-first-run',
    ],
  });
}

export async function extensionWorker(context: BrowserContext): Promise<Worker> {
  return (
    context.serviceWorkers().find((w) => w.url().startsWith('chrome-extension://')) ??
    (await context.waitForEvent('serviceworker', {
      predicate: (w) => w.url().startsWith('chrome-extension://'),
    }))
  );
}

// ── The fake Shelfy server ──────────────────────────────────────────────────

const SPA_PAGE =
  '<!doctype html><html><head><title>Shelfy (fake)</title></head><body>fake SPA</body></html>';

export class FakeServer {
  readonly api = new FakeShelfyApi();
  private server: Server | null = null;

  constructor(private readonly port: number) {}

  async start(): Promise<void> {
    this.server = createServer((req, res) => void this.serve(req, res));
    await new Promise<void>((resolve, reject) => {
      this.server?.once('error', reject);
      this.server?.listen(this.port, '127.0.0.1', () => resolve());
    });
  }

  async stop(): Promise<void> {
    const server = this.server;
    this.server = null;
    if (!server) return;
    server.closeAllConnections();
    await new Promise<void>((resolve) => server.close(() => resolve()));
  }

  private async serve(req: IncomingMessage, res: ServerResponse): Promise<void> {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    const path = req.url ?? '/';
    if (req.method === 'GET' && (path === '/' || path.startsWith('/settings'))) {
      res.writeHead(200, { 'content-type': 'text/html' }).end(SPA_PAGE);
      return;
    }
    const headers: Record<string, string> = {};
    for (const [name, value] of Object.entries(req.headers))
      if (typeof value === 'string') headers[name.toLowerCase()] = value;
    const result = this.api.handle({
      method: req.method ?? 'GET',
      path,
      headers,
      body: chunks.length ? Buffer.concat(chunks).toString('utf8') : null,
      bytes: chunks.length ? Buffer.concat(chunks) : undefined,
    });
    res.writeHead(result.status, result.headers).end(result.body);
  }
}

// ── Synthetic responses ─────────────────────────────────────────────────────

export const htmlPage = (body: string, head = '') => ({
  status: 200,
  contentType: 'text/html',
  body: `<!doctype html><html><head>${head}</head><body>${body}</body></html>`,
});

export const jsonBody = (body: string) => ({ status: 200, contentType: 'application/json', body });

// ── Helpers on the browser side ─────────────────────────────────────────────

export async function openPanel(context: BrowserContext, errors: string[]): Promise<Page> {
  const panel = await context.newPage();
  panel.on('console', (m) => {
    if (m.type() === 'error') errors.push(`panel: ${m.text()}`);
  });
  panel.on('pageerror', (e) => errors.push(`panel: ${e.message}`));
  await panel.goto(`chrome-extension://${EXTENSION_ID}/panel.html`);
  await panel.waitForSelector('[data-testid=pairing-state]');
  return panel;
}

export const stateOf = (panel: Page): Promise<PanelState> =>
  panel.evaluate((kind) => chrome.runtime.sendMessage<PanelState>({ kind }), MSG.stateGet);

/** chrome.runtime.sendMessage(EXTENSION_ID, …) from a page, as the Shelfy SPA sends it (C9). */
export function externalMessage(spa: Page, message: unknown): Promise<unknown> {
  return spa.evaluate(
    ([id, body]) =>
      new Promise((resolve) => {
        const runtime = (globalThis as { chrome?: { runtime?: { sendMessage?: unknown } } }).chrome
          ?.runtime as
          | { sendMessage(id: string, message: unknown, callback: (answer: unknown) => void): void }
          | undefined;
        if (!runtime?.sendMessage) resolve('no chrome.runtime');
        else runtime.sendMessage(id as string, body, resolve);
      }),
    [EXTENSION_ID, message] as const,
  );
}
