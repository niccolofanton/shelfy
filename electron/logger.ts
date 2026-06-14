// File logger for diagnosing packaged builds (no DevTools available there).
// Mirrors console.* to <userData>/logs/main.log and records renderer crashes,
// load failures and the renderer's own console output.

import { app } from 'electron';
import type { WebContents, BrowserWindow } from 'electron';
import path from 'path';
import fs from 'fs';

const MAX_LOG_BYTES = 5 * 1024 * 1024; // rotate main.log once it exceeds 5 MB

let stream: fs.WriteStream | null = null;
let logPath: string | null = null;

// Keep a single rotated copy (main.log → main.log.1) when the live log grows
// past MAX_LOG_BYTES, so the file can't grow without bound across launches.
function rotateIfNeeded(file: string): void {
  try {
    if (fs.statSync(file).size <= MAX_LOG_BYTES) return;
  } catch {
    return;
  } // no file yet → nothing to rotate
  try {
    fs.renameSync(file, `${file}.1`);
  } catch {
    try {
      fs.truncateSync(file, 0);
    } catch {}
  }
}

function ts(): string {
  return new Date().toISOString();
}

function fmt(a: unknown): string {
  if (typeof a === 'string') return a;
  if (a instanceof Error) return a.stack || a.message;
  try {
    return JSON.stringify(a);
  } catch {
    return String(a);
  }
}

const IS_DEV = process.env.ELECTRON_DEV === 'true';

// In dev, also mirror renderer console output to the terminal running `npm run
// dev`, so the whole codebase (main + renderer) is readable in one place.
// Main-process console.* already prints to that terminal on its own.
function echoToTerminal(source: string, level: string, message: string): void {
  if (!IS_DEV) return;
  const text = `[${source}:${level}] ${message}\n`;
  try {
    (level === 'error' ? process.stderr : process.stdout).write(text);
  } catch {}
}

// Electron passes console-message level as 0=verbose 1=info 2=warning 3=error.
function levelName(level: number): string {
  return ['log', 'info', 'warn', 'error'][level] || 'log';
}

// Map webContents type to a friendly source tag. The main window reports as
// 'window'; we call it 'renderer' to match how the rest of the app refers to it.
function sourceTag(wc: WebContents): string {
  try {
    const t = wc.getType();
    return t === 'window' ? 'renderer' : t; // 'webview' | 'offscreen' | 'browserView' | ...
  } catch {
    return 'renderer';
  }
}

// Webviews host remote, logged-in pages (the persist:social session): their
// console output can echo URLs carrying tokens/session params, which must never
// land in plaintext in main.log. Skip the mirror for those surfaces entirely.
function isUntrustedRemoteSurface(wc: WebContents): boolean {
  try {
    if (wc.getType() === 'webview') return true;
  } catch {}
  try {
    const sp = wc.session?.storagePath || '';
    if (/[\\/]Partitions[\\/]social([\\/]|$)/.test(sp)) return true;
  } catch {}
  return false;
}

// Attach the console mirror to any webContents, once. Used both for the main
// window and (via app.on('web-contents-created')) for webviews and the
// offscreen capture page, so every renderer surface lands in main.log + terminal.
const consoleAttached = new WeakSet<WebContents>();
function attachConsole(wc: WebContents): void {
  if (!wc || consoleAttached.has(wc)) return;
  if (isUntrustedRemoteSurface(wc)) return; // never mirror remote-page consoles
  consoleAttached.add(wc);
  const source = sourceTag(wc);
  wc.on('console-message', (_e, level, message, line, sourceId) => {
    const name = levelName(level);
    write(source, [`${sourceId}:${line} ${message}`]);
    echoToTerminal(source, name, message);
  });
}

function write(level: string, args: unknown[]): void {
  const line = `[${ts()}] [${level}] ${args.map(fmt).join(' ')}\n`;
  try {
    if (stream) stream.write(line);
  } catch {}
}

function init(): string | null {
  if (stream) return logPath;
  const dir = path.join(app.getPath('userData'), 'logs');
  fs.mkdirSync(dir, { recursive: true });
  logPath = path.join(dir, 'main.log');
  rotateIfNeeded(logPath);
  stream = fs.createWriteStream(logPath, { flags: 'a' });

  write('info', [`=== SHELFY launch ===`]);
  write('info', [
    `version=${app.getVersion()} electron=${process.versions.electron} node=${process.versions.node} platform=${process.platform} arch=${process.arch}`,
  ]);
  write('info', [`resourcesPath=${process.resourcesPath}`]);
  write('info', [`userData=${app.getPath('userData')}`]);

  for (const level of ['log', 'info', 'warn', 'error'] as const) {
    const orig = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      write(level, args);
      orig(...args);
    };
  }

  process.on('uncaughtException', (err) => write('error', ['uncaughtException', err]));
  process.on('unhandledRejection', (reason) => write('error', ['unhandledRejection', reason]));

  // Catch every renderer surface created from now on (webviews, offscreen
  // capture page, child windows). The main window is attached explicitly in
  // attachWindow(); the WeakSet guard makes a double-attach a no-op.
  app.on('web-contents-created', (_e, wc) => attachConsole(wc));

  return logPath;
}

// Strip query string and fragment before logging a URL: query params can carry
// tokens/session ids that must not end up in plaintext in main.log.
function safeUrlForLog(url: string): string {
  try {
    const u = new URL(url);
    return `${u.origin}${u.pathname}`;
  } catch {
    return String(url).split(/[?#]/)[0].slice(0, 200);
  }
}

function attachWindow(win: BrowserWindow): void {
  const wc = win.webContents;
  wc.on('did-fail-load', (_e, code, desc, url, isMainFrame) =>
    write('error', [
      `did-fail-load code=${code} desc="${desc}" url=${safeUrlForLog(url)} mainFrame=${isMainFrame}`,
    ]),
  );
  wc.on('render-process-gone', (_e, details) => write('error', ['render-process-gone', details]));
  wc.on('preload-error', (_e, p, err) => write('error', [`preload-error ${p}`, err]));
  attachConsole(wc);
  wc.on('did-finish-load', () => write('info', ['renderer did-finish-load']));
}

export { init, attachWindow };
export const getLogPath = (): string | null => logPath;
