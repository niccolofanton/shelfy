import { app, BrowserWindow, dialog, Menu, net, protocol, session, shell } from 'electron';
import type { MenuItemConstructorOptions, WebContents, WebPreferences } from 'electron';
import path from 'path';
import fs from 'fs';
import { Readable } from 'stream';
import { fileURLToPath, pathToFileURL } from 'url';
import * as logger from './logger';
import * as netSafety from './net-safety';
import {
  THUMB_EXTS,
  thumbnailFor,
  thumbETag,
  prewarmThumbCache,
  backfillThumbBlurs,
} from './thumbs';

const isDev = process.env.ELECTRON_DEV === 'true';

// Custom marker properties the hardening logic stamps on auth-popup / capture
// windows. Kept as an augmentation so the tagged-window checks below stay typed
// (BrowserWindow has no index signature).
type TaggedWindow = BrowserWindow & {
  __shelfyAuthPopup?: boolean;
  __shelfyCapture?: boolean;
};

// Shapes of the app's own modules loaded lazily via require() below. They are
// required (not imported) to preserve the exact on-demand load ordering — these
// run only after app.whenReady / createWindow, never at module top level — and
// because some are still being converted in parallel; local interfaces keep
// main.ts decoupled from their in-flux export declarations.
interface InterceptorModule {
  setupInterceptor: (mainWindow: BrowserWindow) => void;
}
interface IpcModule {
  registerIpcHandlers: (mainWindow: BrowserWindow) => void;
}
interface DbModule {
  initialize: () => void;
  close: () => void;
}
interface RecoverableModule {
  recover: () => void;
}
interface UpdaterModule {
  initUpdater: (win: BrowserWindow) => void;
  setWindow: (win: BrowserWindow) => void;
}
type EnsureResult = unknown;
interface BinariesModule {
  ensureBinaries: (
    onProgress?: (phase: string, fraction: number) => void,
    opts?: { force?: boolean },
  ) => Promise<EnsureResult>;
}
interface AnalyzerModule {
  recover: () => void;
  forceShutdown: () => void;
  setVariantFallbackHandler: (fn: (failedVariant: string) => void) => void;
}
interface ShutdownModule {
  forceShutdown: () => void;
}
interface WebOrchestratorModule {
  recover: () => void;
  cancelAll: () => void;
}
interface CaptureEngineModule {
  closeBrowser: () => void;
}

// The current top-level app window. Recreated on macOS re-activate (all windows
// closed, then dock-reopened). Closures that emit progress events to the
// renderer must read THIS ref — not a window captured at first launch — so the
// events reach the live window after a recreation. Mirrors ipc.js's _window.
let currentWindow: BrowserWindow | null = null;

// Send an IPC event to the current window, guarding against a destroyed window.
function sendToCurrentWindow(channel: string, payload: unknown): void {
  if (currentWindow && !currentWindow.isDestroyed()) {
    currentWindow.webContents.send(channel, payload);
  }
}

// Custom protocol to serve locally-downloaded assets to the renderer.
// In dev the renderer runs on http://localhost:5173 and Chromium refuses to
// load file:// resources from an http origin, so downloaded images appear
// broken. A privileged scheme works from any origin in both dev and prod.
const ASSET_MIME: Record<string, string> = {
  jpg: 'image/jpeg',
  jpeg: 'image/jpeg',
  png: 'image/png',
  webp: 'image/webp',
  gif: 'image/gif',
  avif: 'image/avif',
  bmp: 'image/bmp',
  svg: 'image/svg+xml',
  mp4: 'video/mp4',
  // Additional originals a manual bookmark can carry (served inline for the
  // modal's image zoom / video playback).
  webm: 'video/webm',
  mov: 'video/quicktime',
  m4v: 'video/x-m4v',
  mkv: 'video/x-matroska',
  avi: 'video/x-msvideo',
  pdf: 'application/pdf',
};

protocol.registerSchemesAsPrivileged([
  {
    scheme: 'asset',
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      stream: true,
      bypassCSP: true,
    },
  },
]);

// Resolve symlinks on an absolute path. If the path doesn't exist yet (so it
// can't be a symlink to an escape target), fall back to the input unchanged so
// the caller's prefix check still runs and a later read can 404 normally.
async function safeRealpath(p: string): Promise<string> {
  try {
    return await fs.promises.realpath(p);
  } catch {
    return p;
  }
}

// HTTP caching for asset responses: `no-cache` means "store, but revalidate" —
// Chromium can keep the bytes in its HTTP cache and re-issue the request with
// If-None-Match, which we answer with a 304 after a stat (no read, no thumbnail
// generation, no body transfer). Assets are keyed by mtime+size so an
// overwritten file (web re-capture, re-download) invalidates immediately —
// a fixed max-age could serve a stale tile for its whole lifetime.
// SHELFY_THUMB_NO_CACHE (set only by the gallery perf harness' "cacheless" mode)
// forces every asset response to be uncacheable: `no-store` keeps Chromium from
// reusing bytes, and the handler also skips its 304 short-circuits, so every
// request re-runs the full pipeline (thumbnailFor regenerates — see thumbs.js).
// Inert in normal runs.
const NO_HTTP_CACHE = process.env.SHELFY_THUMB_NO_CACHE === '1';

function assetHeaders(mime: string, etag: string): Record<string, string> {
  const h: Record<string, string> = NO_HTTP_CACHE
    ? { 'Cache-Control': 'no-store' }
    : { ETag: etag, 'Cache-Control': 'public, no-cache' };
  if (mime) h['Content-Type'] = mime;
  return h;
}

function registerAssetProtocol(): void {
  protocol.handle('asset', async (request): Promise<Response> => {
    try {
      const url = new URL(request.url);
      const filePath = decodeURIComponent(url.pathname.replace(/^\//, ''));
      // Confine reads to the app's userData tree (where downloaded assets live,
      // e.g. <userData>/assets/...). Resolve to defeat ../ traversal AND resolve
      // symlinks (realpath) so a symlink inside userData can't point outside the
      // root and escape the prefix check.
      const resolved = await safeRealpath(path.resolve(filePath));
      const assetRoot = await safeRealpath(path.resolve(app.getPath('userData')));
      if (resolved !== assetRoot && !resolved.startsWith(assetRoot + path.sep)) {
        return new Response('Forbidden', { status: 403 });
      }
      const ext = path.extname(resolved).slice(1).toLowerCase();
      const ifNoneMatch = request.headers.get('if-none-match');
      // ?w=N → downscaled grid thumbnail (images only). Clamped so arbitrary
      // widths can't balloon the cache; anything invalid serves the original.
      const w = Math.min(1024, Math.floor(Number(url.searchParams.get('w')) || 0));
      if (w >= 64 && THUMB_EXTS.has(ext)) {
        const tag = await thumbETag(resolved, w, ext);
        if (!NO_HTTP_CACHE && tag && ifNoneMatch === `"${tag}"`) {
          return new Response(null, { status: 304, headers: assetHeaders('', `"${tag}"`) });
        }
        const thumb = await thumbnailFor(resolved, w, ext);
        if (thumb) {
          return new Response(
            new Uint8Array(
              thumb.data.buffer,
              thumb.data.byteOffset,
              thumb.data.byteLength,
            ) as Uint8Array<ArrayBuffer>,
            {
              headers: assetHeaders(thumb.mime, `"${thumb.etag}"`),
            },
          );
        }
      }
      // Originals: streamed (net.fetch on a file URL) instead of buffered whole
      // in memory — a hover-preview video starts playing without first loading
      // the entire file, and a multi-MB original never spikes the main process.
      const stat = await fs.promises.stat(resolved);
      const tag = `"${stat.size}-${Math.round(stat.mtimeMs)}"`;
      if (!NO_HTTP_CACHE && ifNoneMatch === tag) {
        return new Response(null, { status: 304, headers: assetHeaders('', tag) });
      }
      const mime = ASSET_MIME[ext] || 'application/octet-stream';
      // Honor byte-range requests so Chromium's media element can seek/scrub a
      // video and probe its duration instead of re-downloading from byte 0.
      // `bytes=start-` and `bytes=start-end` are both supported; an open-ended
      // or missing end means "to EOF". Always advertise Accept-Ranges so the
      // element knows ranges are available even on the initial full request.
      const range = request.headers.get('range');
      const m = range && /^bytes=(\d*)-(\d*)$/.exec(range.trim());
      if (m && (m[1] !== '' || m[2] !== '')) {
        let start = m[1] === '' ? NaN : Number(m[1]);
        let end = m[2] === '' ? stat.size - 1 : Number(m[2]);
        // Suffix form `bytes=-N` → the last N bytes of the file.
        if (m[1] === '') {
          start = Math.max(0, stat.size - Number(m[2]));
          end = stat.size - 1;
        }
        if (!Number.isFinite(start) || !Number.isFinite(end) || start > end || start >= stat.size) {
          return new Response('Range Not Satisfiable', {
            status: 416,
            headers: { 'Content-Range': `bytes */${stat.size}` },
          });
        }
        end = Math.min(end, stat.size - 1);
        const stream = fs.createReadStream(resolved, { start, end });
        return new Response(Readable.toWeb(stream) as ReadableStream<Uint8Array>, {
          status: 206,
          headers: {
            ...assetHeaders(mime, tag),
            'Accept-Ranges': 'bytes',
            'Content-Range': `bytes ${start}-${end}/${stat.size}`,
            'Content-Length': String(end - start + 1),
          },
        });
      }
      const fileRes = await net.fetch(pathToFileURL(resolved).toString());
      return new Response(fileRes.body, {
        status: 200,
        headers: {
          ...assetHeaders(mime, tag),
          'Accept-Ranges': 'bytes',
          'Content-Length': String(stat.size),
        },
      });
    } catch {
      return new Response('Asset not found', { status: 404 });
    }
  });
}

// In dev, electronmon (see the `dev` npm script) watches electron/*.js and
// restarts the whole main process on change; Vite hot-reloads the renderer.

app.setName('Shelfy');

function buildMenu(mainWindow: BrowserWindow): Menu {
  const editMenu: MenuItemConstructorOptions = {
    label: 'Edit',
    submenu: [
      { role: 'undo' },
      { role: 'redo' },
      { type: 'separator' },
      { role: 'cut' },
      { role: 'copy' },
      { role: 'paste' },
      { role: 'selectAll' },
    ],
  };

  if (isDev) {
    (editMenu.submenu as MenuItemConstructorOptions[]).push(
      { type: 'separator' },
      {
        label: 'Toggle DevTools',
        accelerator: process.platform === 'darwin' ? 'Alt+Command+I' : 'Ctrl+Shift+I',
        click() {
          mainWindow.webContents.toggleDevTools();
        },
      },
    );
  }

  const template: MenuItemConstructorOptions[] = [editMenu];
  return Menu.buildFromTemplate(template);
}

// Restrictive Content-Security-Policy for the app's OWN renderer documents
// (served from localhost:5173 in dev, asset:// / file:// in prod). NOT applied
// to the 'persist:social' webview session (Instagram/X need their own policy).
// Conservative in dev so Vite HMR (inline scripts, eval, ws) keeps working.
function applyMainCsp(): void {
  const imgSrc = "img-src 'self' asset: data: blob: https:";
  const connectDev = "connect-src 'self' http://localhost:5173 ws://localhost:5173 asset: data:";
  const connectProd = "connect-src 'self' asset: data:";
  const scriptDev = "script-src 'self' 'unsafe-inline' 'unsafe-eval'";
  const scriptProd = "script-src 'self'";
  const styleSrc = "style-src 'self' 'unsafe-inline'";
  const csp = [
    "default-src 'self' asset:",
    isDev ? scriptDev : scriptProd,
    styleSrc,
    imgSrc,
    "media-src 'self' asset: data: blob: https:",
    "font-src 'self' data:",
    isDev ? connectDev : connectProd,
    "object-src 'none'",
    // The renderer uses <webview> (governed by will-attach-webview prefs and the
    // WEBVIEW_HOST_RE navigation allowlist), not <iframe> — frame-src does not
    // apply to webviews. Keep this 'self' only so an injection into the trusted
    // document can't embed an arbitrary https iframe (clickjacking / exfil).
    "frame-src 'self'",
    "base-uri 'self'",
  ].join('; ');

  session.defaultSession.webRequest.onHeadersReceived((details, callback) => {
    callback({
      responseHeaders: {
        ...details.responseHeaders,
        'Content-Security-Policy': [csp],
      },
    });
  });
}

// web-contents-created hardening: gate new-window opens and confine navigation.
// Applies to the host window and any attached <webview>.
function hardenWebContents(): void {
  const APP_ORIGINS = isDev ? ['http://localhost:5173'] : ['file://', 'asset://'];
  // Expected webview hosts: IG/X plus the federated-login / CDN domains they
  // redirect through (Meta auth for IG, Google/Apple sign-in for X). Kept
  // deliberately broad on the auth side so login flows are not broken.
  const WEBVIEW_HOST_RE =
    /(?:^|\.)(?:instagram\.com|cdninstagram\.com|x\.com|twitter\.com|twimg\.com|pinterest\.[a-z]{2,3}(?:\.[a-z]{2})?|pinimg\.com|facebook\.com|fbcdn\.net|accounts\.google\.com|appleid\.apple\.com)$/;

  const isExternalWeb = (url: string): boolean => {
    try {
      const u = new URL(url);
      return u.protocol === 'http:' || u.protocol === 'https:';
    } catch {
      return false;
    }
  };

  // True for URLs an OAuth login popup from the social webview may open: the
  // initial about:blank/about: handshake window (how Google Identity et al.
  // start), or a known auth/social host. Used to allow "Continue with Google/
  // Apple/Facebook" popups instead of silently denying them.
  const isAuthPopupUrl = (url: string): boolean => {
    if (!url || url === 'about:blank' || url.startsWith('about:')) return true;
    try {
      return WEBVIEW_HOST_RE.test(new URL(url).hostname);
    } catch {
      return false;
    }
  };

  app.on('web-contents-created', (_e, contents: WebContents) => {
    const type = contents.getType();

    // OAuth/login popups ("Continue with Google/Apple/Facebook") spawned from the
    // social webview open as a real popup window that SHARES the persist:social
    // session, so the auth cookie lands in the webview's own partition (otherwise
    // the user logs in elsewhere and the webview stays logged out). These popups
    // begin at about:blank and then navigate to the provider; will-navigate
    // confines them to the known auth hosts via the __shelfyAuthPopup tag below.
    // Everything else: open external http(s) in the OS browser, deny the rest.
    contents.setWindowOpenHandler(({ url }) => {
      if (type === 'webview' && isAuthPopupUrl(url)) {
        return {
          action: 'allow',
          overrideBrowserWindowOptions: {
            width: 600,
            height: 720,
            autoHideMenuBar: true,
            webPreferences: {
              partition: 'persist:social', // share the webview's logged-in session
              nodeIntegration: false,
              contextIsolation: true,
              sandbox: false,
            },
          },
        };
      }
      if (isExternalWeb(url)) {
        // SSRF guard before handing the URL to the OS browser: isExternalWeb
        // only checks the scheme, so loopback/private/numeric-IP literals
        // would otherwise reach shell.openExternal.
        try {
          netSafety.assertSafeUrl(url);
          shell.openExternal(url);
        } catch (err) {
          console.warn(
            '[main] blocked openExternal for unsafe URL:',
            url,
            String((err as { message?: string } | null | undefined)?.message || err),
          );
        }
      }
      return { action: 'deny' };
    });

    // Tag the windows the social webview is allowed to pop open (OAuth flows) so
    // will-navigate confines them to the auth hosts instead of the app-origin lock
    // that governs normal app windows.
    contents.on('did-create-window', (win) => {
      (win as TaggedWindow).__shelfyAuthPopup = true;
    });

    contents.on('will-navigate', (event, url) => {
      // The hidden web-capture window (webcapture.js tags it `__shelfyCapture`)
      // must be allowed to navigate to arbitrary external hosts — that IS its
      // job. Without this bypass the app-origin check below would block every
      // capture load. SSRF is already enforced upstream (assertSafeUrl before
      // the window navigates), so trusting the tagged window here is safe.
      const w = BrowserWindow.fromWebContents(contents) as TaggedWindow | null;
      if (w && w.__shelfyCapture) return;

      // OAuth popup spawned from the social webview: confine it to the same
      // auth/social hosts as the webview (not the app-origin lock for windows).
      if (w && w.__shelfyAuthPopup) {
        let host = '';
        try {
          host = new URL(url).hostname;
        } catch {
          event.preventDefault();
          return;
        }
        if (!WEBVIEW_HOST_RE.test(host)) event.preventDefault();
        return;
      }

      if (type === 'webview') {
        // Confine the social webview to the expected hosts (IG / X).
        let host = '';
        try {
          host = new URL(url).hostname;
        } catch {
          event.preventDefault();
          return;
        }
        if (!WEBVIEW_HOST_RE.test(host)) event.preventDefault();
        return;
      }
      // Host app window: only allow navigating within the app's own origin(s).
      const allowed = APP_ORIGINS.some((o) => url.startsWith(o));
      if (!allowed) event.preventDefault();
    });

    // Defense-in-depth: enforce hardened webview prefs in the main process so a
    // tampered renderer can't re-enable nodeIntegration. The legitimate capture
    // preload (webview-preload.js) is preserved — capture relies on it.
    contents.on('will-attach-webview', (_event, webPreferences, params) => {
      webPreferences.nodeIntegration = false;
      webPreferences.contextIsolation = true;
      webPreferences.nodeIntegrationInSubFrames = false;
      webPreferences.webSecurity = true;
      // Strip a `disablewebsecurity` attribute a compromised renderer could
      // mount; the legit webview in Browser.jsx never sets it. allowpopups is
      // expected (OAuth popups) and gated by setWindowOpenHandler above, so it
      // is deliberately left alone. sandbox is NOT forced: the legit webview
      // runs sandbox=false so its preload keeps working.
      // `params` is typed Record<string, string>, but Electron accepts the
      // boolean write at runtime; widen the value type to preserve the exact
      // assignment (= false) without changing behavior.
      const attachParams = params as Record<string, string | boolean | undefined>;
      if (attachParams && attachParams.disablewebsecurity !== undefined) {
        attachParams.disablewebsecurity = false;
      }
      // Pin the preload to the only legitimate one (webview-preload.js — the
      // path the renderer reads from electronAPI.webviewPreloadPath), so a
      // compromised renderer can't attach a webview with an arbitrary preload.
      const expectedPreload = path.join(__dirname, 'webview-preload.js');
      const reqPrefs = webPreferences as WebPreferences & { preloadURL?: string };
      const requested = reqPrefs.preload || reqPrefs.preloadURL;
      if (requested) {
        let requestedPath = String(requested);
        try {
          if (requestedPath.startsWith('file:')) requestedPath = fileURLToPath(requestedPath);
        } catch {}
        if (path.resolve(requestedPath) !== expectedPreload) {
          console.warn(
            '[main] will-attach-webview: unexpected preload',
            requested,
            '— forcing',
            expectedPreload,
          );
        }
        reqPrefs.preload = expectedPreload;
        delete reqPrefs.preloadURL;
      }
    });
  });
}

function createWindow(): BrowserWindow {
  const mainWindow = new BrowserWindow({
    title: 'SHELFY',
    width: 1280,
    height: 800,
    minWidth: 900,
    minHeight: 600,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      webviewTag: true,
      preload: path.join(__dirname, 'preload.js'),
    },
  });

  currentWindow = mainWindow;
  logger.attachWindow(mainWindow);

  if (isDev) {
    const startUrl = 'http://localhost:5173';
    (function loadWithRetry(retries = 10, delay = 500): void {
      mainWindow.loadURL(startUrl).catch((err: NodeJS.ErrnoException) => {
        if (retries > 0 && err.code === 'ERR_CONNECTION_REFUSED') {
          setTimeout(() => loadWithRetry(retries - 1, delay), delay);
        } else {
          console.error('[main] Failed to load URL:', startUrl, err);
        }
      });
    })();
  } else {
    // loadFile (not loadURL with a file:// string) so Windows drive paths like
    // C:\… are turned into a valid file:///C:/… URL and asar paths resolve.
    const indexHtml = path.join(__dirname, '..', 'dist', 'index.html');
    console.log('[main] loading', indexHtml, 'exists=', fs.existsSync(indexHtml));
    mainWindow
      .loadFile(indexHtml)
      .catch((err) => console.error('[main] Failed to load file:', indexHtml, err));
  }

  if (isDev && process.env.PLAYWRIGHT_E2E !== '1') {
    mainWindow.webContents.openDevTools();
  }

  Menu.setApplicationMenu(buildMenu(mainWindow));

  try {
    (require('./interceptor') as InterceptorModule).setupInterceptor(mainWindow);
  } catch (err) {
    console.error('[main] Failed to set up interceptor:', err);
  }

  try {
    (require('./ipc') as IpcModule).registerIpcHandlers(mainWindow);
  } catch (err) {
    console.error('[main] Failed to register IPC handlers:', err);
  }

  return mainWindow;
}

app
  .whenReady()
  .then(() => {
    logger.init();

    // A database that fails to open is fatal: continuing would leave the app as a
    // zombie (empty gallery, infinite spinners, failing job recovery) with no clue
    // for the user. Surface the error and quit instead.
    try {
      (require('./db') as DbModule).initialize();
    } catch (err) {
      console.error('[main] Failed to initialize database:', err);
      const dbPath = path.join(app.getPath('userData'), 'shelfy.sqlite');
      const message = (err as { message?: string } | null | undefined)?.message;
      dialog.showErrorBox(
        'Shelfy cannot start',
        'The database could not be opened, so the app cannot work.\n\n' +
          `Database: ${dbPath}\n` +
          `Error: ${err && message ? message : String(err)}\n\n` +
          'The file may be corrupted or locked by another process. ' +
          'Move or rename it to start with an empty library, then restart Shelfy.',
      );
      app.quit();
      return;
    }

    registerAssetProtocol();
    applyMainCsp();
    hardenWebContents();

    // Allow microphone access for the main renderer (used by the AI-search voice
    // dictation). interceptor.js only covers the 'persist:social' webview session,
    // so without this getUserMedia() in the app window is denied.
    // Only the app's OWN renderer (dev: http://localhost:5173, prod: file:// from
    // loadFile, plus the asset: privileged scheme) may get the mic — NOT arbitrary
    // third-party frames that would otherwise share the defaultSession's grant.
    // Allow the app's own renderer (dev localhost, prod file:///asset://) plus the
    // opaque "null"/empty origin that file:// pages report in the check handler.
    // We only need to DENY clearly-foreign http(s) origins — those always carry a
    // real origin, never null — so being lenient on null can't leak the grant to a
    // third-party frame while it keeps voice dictation working in the packaged build.
    const isAppOrigin = (url: string | undefined): boolean => {
      if (!url || url === 'null') return true;
      if (isDev && url.startsWith('http://localhost:5173')) return true;
      return url.startsWith('file://') || url.startsWith('asset://');
    };
    const allowMic = (permission: string): boolean =>
      permission === 'media' || permission === 'audioCapture';
    session.defaultSession.setPermissionRequestHandler((_wc, permission, callback, details) => {
      // `details` is a union (PermissionRequest | Filesystem… | MediaAccess… |
      // OpenExternal…); only some members carry requestingUrl, so narrow with an
      // `in` guard rather than asserting a single shape.
      const requestingUrl =
        details && 'requestingUrl' in details ? details.requestingUrl : undefined;
      callback(allowMic(permission) && isAppOrigin(requestingUrl));
    });
    session.defaultSession.setPermissionCheckHandler(
      (_wc, permission, requestingOrigin) => allowMic(permission) && isAppOrigin(requestingOrigin),
    );

    const mainWindow = createWindow();

    // Warm the grid-thumbnail cache once startup has settled, then backfill the
    // per-post blur-up placeholders (cheap once the tiles exist — see thumbs.js).
    // Errors are non-fatal: on-demand generation in the asset handler still
    // covers tile misses, and a missing placeholder just means a plain dark card.
    // PERF_NO_PREWARM disables this for the gallery perf harness: a profiler must
    // never have background cache generation / a coalesced grid reload landing
    // mid-measurement (it would silently re-warm tiles a cold/cacheless run wants
    // cold). Inert in normal runs — the env var is set only by the harness.
    if (!process.env.PERF_NO_PREWARM) {
      setTimeout(() => {
        prewarmThumbCache()
          .catch((err) => console.warn(`[thumb-cache] pre-warm failed: ${err}`))
          .then(() => backfillThumbBlurs())
          .then((written) => {
            // The gallery rendered before the backfill landed: one coalesced reload
            // (same channel as new-post inserts) lets the open grid pick up the
            // fresh placeholders without a restart. No-op when nothing was written.
            if (written > 0) sendToCurrentWindow('interceptor:newPosts', { source: 'thumb-blur' });
          })
          .catch((err) => console.warn(`[thumb-blur] backfill failed: ${err}`));
      }, 15_000);
    }

    // Resume background work left unfinished by a previous run. createWindow() has
    // already registered the IPC handlers (which wire each manager's progress
    // emitter), so the restored queues stream their state to the renderer. Each is
    // isolated so one failing recovery can't block the others.
    for (const mod of ['downloader', 'analyzer', 'weborchestrator']) {
      try {
        (require(`./${mod}`) as RecoverableModule).recover();
      } catch (err) {
        console.error(`[main] ${mod} recovery failed:`, err);
      }
    }

    try {
      (require('./updater') as UpdaterModule).initUpdater(mainWindow);
    } catch (err) {
      console.error('[main] Failed to init updater:', err);
    }

    // Provision the runtime sidecar binaries (yt-dlp/ffmpeg/llama/whisper) into
    // userData. No-op in dev (binaries come from the source tree). The renderer
    // can show progress via the 'binaries:progress' channel; the ensure() guard
    // makes a concurrent manual trigger from Settings a no-op.
    mainWindow.webContents.once('did-finish-load', () => {
      try {
        (require('./binaries') as BinariesModule)
          .ensureBinaries((phase, fraction) => {
            sendToCurrentWindow('binaries:progress', { phase, fraction });
          })
          .then((r) => {
            sendToCurrentWindow('binaries:progress', {
              phase: 'done',
              fraction: 1,
              result: r,
            });
          })
          .catch((err) => {
            console.error('[main] binaries provisioning failed:', err);
            sendToCurrentWindow('binaries:progress', {
              phase: 'error',
              error: String((err as { message?: string } | null | undefined)?.message || err),
            });
          });
      } catch (err) {
        console.error('[main] binaries init error:', err);
      }
    });

    // When a GPU llama.cpp build fails to start, the analyzer has already demoted the
    // variant to CPU (binaries.markVariantFailed). Warn the renderer and, since the
    // CPU build is a different download, re-provision it in the background so the next
    // analysis runs without the user having to click anything.
    try {
      (require('./analyzer') as AnalyzerModule).setVariantFallbackHandler((failedVariant) => {
        sendToCurrentWindow('ai:variantFallback', { failedVariant });
        const binaries = require('./binaries') as BinariesModule;
        binaries
          .ensureBinaries(
            (phase, fraction) => {
              sendToCurrentWindow('binaries:progress', { phase, fraction });
            },
            { force: true },
          )
          .then((r) => {
            sendToCurrentWindow('binaries:progress', {
              phase: 'done',
              fraction: 1,
              result: r,
            });
          })
          .catch((err) => {
            console.error('[main] CPU re-provisioning failed:', err);
            sendToCurrentWindow('binaries:progress', {
              phase: 'error',
              error: String((err as { message?: string } | null | undefined)?.message || err),
            });
          });
      });
    } catch (err) {
      console.error('[main] could not wire variant fallback:', err);
    }

    app.on('activate', () => {
      if (BrowserWindow.getAllWindows().length === 0) {
        // Recreate the window AND re-point the updater at it: the old window was
        // destroyed, so without this 'updater:state' pushes would be discarded.
        const w = createWindow();
        try {
          (require('./updater') as UpdaterModule).setWindow(w);
        } catch (err) {
          console.error('[main] Failed to re-point updater to new window:', err);
        }
      }
    });
  })
  .catch((err) => {
    // A fatal error in startup setup (protocol registration, CSP, createWindow —
    // e.g. a packaged preload path missing) would otherwise leave the app running
    // with no window, no dialog and only a console line the user never sees.
    // Mirror the db-failure path: surface the error and quit instead of lingering
    // as a zombie.
    console.error('[main] Fatal startup error:', err);
    const message = (err as { message?: string } | null | undefined)?.message;
    dialog.showErrorBox(
      'Shelfy cannot start',
      'A fatal error occurred during startup, so the app cannot work.\n\n' +
        `Error: ${err && message ? message : String(err)}`,
    );
    app.quit();
  });

app.on('before-quit', () => {
  // SIGKILL immediately: on quit the SIGTERM grace-period timer never fires (the
  // process is exiting), so a graceful-only shutdown can leave orphaned
  // llama/whisper servers holding VRAM and their port. forceShutdown guarantees
  // the children are reaped synchronously.
  try {
    (require('./analyzer') as AnalyzerModule).forceShutdown();
  } catch {}
  try {
    (require('./stt') as ShutdownModule).forceShutdown();
  } catch {}
  try {
    (require('./embeddings') as ShutdownModule).forceShutdown();
  } catch {}
  // Abort any in-flight web capture so a hidden BrowserWindow can't linger.
  try {
    (require('./weborchestrator') as WebOrchestratorModule).cancelAll();
  } catch {}
  // Close the shared Playwright browser so no headless Chromium is orphaned.
  try {
    (require('./capture-engine') as CaptureEngineModule).closeBrowser();
  } catch {}
  // Clean DB shutdown: checkpoint the WAL into the main file and close the
  // handle so the next launch's initialize() opens fresh. No-op if not yet
  // initialized.
  try {
    (require('./db') as DbModule).close();
  } catch {}
});

app.on('window-all-closed', () => {
  if (process.platform !== 'darwin') {
    app.quit();
  }
});
