// Capture-engine selector. The web-reference pipeline used to call
// webcapture.capturePage() (Electron offscreen rendering) directly; that path
// cannot composite WebGL, so WebGL/canvas-heavy reference sites (lusion.co, most
// awwwards entries) captured blank. The Playwright engine renders WebGL correctly.
//
// This module is the single seam the orchestrator imports:
//   • capturePage()  → Playwright by default, transparently falling back to the
//     OSR engine if Chromium isn't available (e.g. a dev box that never ran
//     `playwright install`, or a build that didn't bundle the browser).
//   • discoverPages() → unchanged (network-only, engine-agnostic) passthrough.
//
// Override with SHELFY_CAPTURE_ENGINE = 'playwright' | 'osr'. Default: 'playwright'.

import * as webcapture from './webcapture';

// Options accepted by either engine's capturePage(). All optional; each engine
// fills its own defaults. Kept loose where the underlying knobs are engine- or
// caller-specific (the OSR and Playwright engines share most of these).
interface CapturePageOptions {
  partition?: string;
  maxHeightPx?: number;
  format?: string;
  quality?: number;
  signal?: AbortSignal;
  onStep?: (label: string, delta: number, tot: number) => void;
  settleBeforeShotMs?: number;
  readyMaxMs?: number;
  captureStamp?: number;
}

// The live pageCtx contract returned by both engines. `evaluate`/`dispose` stay
// usable until dispose() is called; the metadata fields describe the captured shot.
interface PageCtx {
  screenshotPath: string;
  width: number;
  height: number;
  finalUrl: string;
  title: string;
  capped: boolean;
  html: string;
  headers: Record<string, string>;
  webglHeavy: boolean;
  evaluate: (code: string) => Promise<unknown>;
  dispose: () => Promise<void>;
}

// Options for discoverPages() (network-only, engine-agnostic).
interface DiscoverPagesOptions {
  maxPages?: number;
  signal?: AbortSignal;
}

// One discovered page (a selectRepresentative candidate): a normalized URL plus
// its ranking metadata. `isHome` marks the authoritative home (always pages[0]).
interface DiscoverPage {
  url: string;
  score: number;
  templateHint: string;
  isHome?: boolean;
}

// Result of discoverPages(): the resolved entry plus the representative pages.
interface DiscoverResult {
  finalUrl: string;
  domain: string;
  origin: string;
  source: string;
  pages: DiscoverPage[];
}

// Engine identifier exposed via activeEngine() and used internally by preferred().
type Engine = 'playwright' | 'osr';

// Shape of the optional Playwright capture engine (./webcapture-playwright). Kept
// as a local interface because the module is loaded lazily via require() (it
// pulls in playwright-core, a heavy native dep that may be absent).
interface PlaywrightCapture {
  capturePage: (url: string, opts?: CapturePageOptions) => Promise<PageCtx>;
  closeBrowser?: () => Promise<void>;
}

let pwcapture: PlaywrightCapture | null = null;
try {
  // Lazy/conditional load of a heavy native dep (playwright-core): keep as a
  // synchronous require() so a missing browser/module degrades to OSR only.
  pwcapture = require('./webcapture-playwright') as PlaywrightCapture;
} catch {
  pwcapture = null; // module load failure (missing playwright-core) → OSR only
}

// Remembers a hard Playwright-unavailable verdict so we don't repeatedly pay the
// launch-timeout cost on a machine without the browser.
let pwUnavailable = false;

function preferred(): Engine {
  const env = String(process.env.SHELFY_CAPTURE_ENGINE || '').toLowerCase();
  if (env === 'osr') return 'osr';
  if (env === 'playwright') return 'playwright';
  return 'playwright'; // default
}

// Per-URL navigation/timeout errors are never an availability problem: that page
// genuinely failed and must surface as such.
function isPerUrlError(m: string): boolean {
  return /Timeout.*exceeded|net::ERR_|Navigation failed/i.test(m);
}

// The browser BINARY is missing and the self-heal install also failed (wiped
// cache and offline, no bundle). Durable for the session → worth latching, so we
// don't pay a ~10-min install attempt on every subsequent capture.
function isBrowserMissing(err: unknown): boolean {
  const m = String((err as { message?: string } | null | undefined)?.message || err || '');
  if (isPerUrlError(m)) return false;
  // ENOENT only counts when it points at the browser itself: a generic fs
  // ENOENT from the capture pipeline (e.g. screenshot dir missing) must not
  // disable Playwright for the whole session.
  return (
    /Executable doesn't exist|playwright install|headless.shell/i.test(m) ||
    (/ENOENT/i.test(m) && /playwright|chrom|headless/i.test(m))
  );
}

// Transient launch trouble (the browser failed to spawn THIS time): fall back to
// OSR for this capture only — getBrowser() relaunches on the next one. A shared-
// browser CRASH mid-session must NOT latch pwUnavailable: crash messages used to
// match the old catch-all regex ("Chromium", "download") and silently degraded
// every later capture to the blank-WebGL OSR path for the rest of the session.
function isLaunchFailure(err: unknown): boolean {
  const m = String((err as { message?: string } | null | undefined)?.message || err || '');
  if (isPerUrlError(m)) return false;
  return /Failed to launch|browserType\.launch/i.test(m);
}

/**
 * Capture one page, returning the live pageCtx contract shared by both engines.
 * Falls back from Playwright to OSR only on a browser-availability error —
 * durably (latched) when the binary is missing, per-call on a launch failure.
 */
async function capturePage(url: string, opts: CapturePageOptions = {}): Promise<PageCtx> {
  const want = preferred();
  if (want === 'playwright' && pwcapture && !pwUnavailable) {
    try {
      return await pwcapture.capturePage(url, opts);
    } catch (err) {
      if (isBrowserMissing(err)) {
        pwUnavailable = true; // binary absent + self-heal failed → stop retrying
        console.warn(
          '[capture-engine] Playwright browser missing, falling back to OSR:',
          (err as { message?: string } | null | undefined)?.message || err,
        );
        return webcapture.capturePage(url, opts);
      }
      if (isLaunchFailure(err)) {
        console.warn(
          '[capture-engine] Playwright launch failed (transient), OSR for this page:',
          (err as { message?: string } | null | undefined)?.message || err,
        );
        return webcapture.capturePage(url, opts);
      }
      throw err; // genuine per-URL failure (incl. a mid-capture crash) → propagate
    }
  }
  return webcapture.capturePage(url, opts);
}

// Close the shared Playwright browser (called on app teardown). No-op for OSR.
async function closeBrowser(): Promise<void> {
  if (pwcapture && typeof pwcapture.closeBrowser === 'function') {
    try {
      await pwcapture.closeBrowser();
    } catch {
      /* ignore */
    }
  }
}

function discoverPages(url: string, opts?: DiscoverPagesOptions): Promise<DiscoverResult> {
  return webcapture.discoverPages(url, opts);
}

export { capturePage, discoverPages, closeBrowser };
export const activeEngine = (): Engine =>
  preferred() === 'playwright' && pwcapture && !pwUnavailable ? 'playwright' : 'osr';
