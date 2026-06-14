'use strict';

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

const webcapture = require('./webcapture');

let pwcapture = null;
try {
  pwcapture = require('./webcapture-playwright');
} catch {
  pwcapture = null; // module load failure (missing playwright-core) → OSR only
}

// Remembers a hard Playwright-unavailable verdict so we don't repeatedly pay the
// launch-timeout cost on a machine without the browser.
let pwUnavailable = false;

function preferred() {
  const env = String(process.env.SHELFY_CAPTURE_ENGINE || '').toLowerCase();
  if (env === 'osr') return 'osr';
  if (env === 'playwright') return 'playwright';
  return 'playwright'; // default
}

// Per-URL navigation/timeout errors are never an availability problem: that page
// genuinely failed and must surface as such.
function isPerUrlError(m) {
  return /Timeout.*exceeded|net::ERR_|Navigation failed/i.test(m);
}

// The browser BINARY is missing and the self-heal install also failed (wiped
// cache and offline, no bundle). Durable for the session → worth latching, so we
// don't pay a ~10-min install attempt on every subsequent capture.
function isBrowserMissing(err) {
  const m = String(err?.message || err || '');
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
function isLaunchFailure(err) {
  const m = String(err?.message || err || '');
  if (isPerUrlError(m)) return false;
  return /Failed to launch|browserType\.launch/i.test(m);
}

/**
 * Capture one page, returning the live pageCtx contract shared by both engines.
 * Falls back from Playwright to OSR only on a browser-availability error —
 * durably (latched) when the binary is missing, per-call on a launch failure.
 */
async function capturePage(url, opts = {}) {
  const want = preferred();
  if (want === 'playwright' && pwcapture && !pwUnavailable) {
    try {
      return await pwcapture.capturePage(url, opts);
    } catch (err) {
      if (isBrowserMissing(err)) {
        pwUnavailable = true; // binary absent + self-heal failed → stop retrying
        console.warn(
          '[capture-engine] Playwright browser missing, falling back to OSR:',
          err?.message || err,
        );
        return webcapture.capturePage(url, opts);
      }
      if (isLaunchFailure(err)) {
        console.warn(
          '[capture-engine] Playwright launch failed (transient), OSR for this page:',
          err?.message || err,
        );
        return webcapture.capturePage(url, opts);
      }
      throw err; // genuine per-URL failure (incl. a mid-capture crash) → propagate
    }
  }
  return webcapture.capturePage(url, opts);
}

// Close the shared Playwright browser (called on app teardown). No-op for OSR.
async function closeBrowser() {
  if (pwcapture && typeof pwcapture.closeBrowser === 'function') {
    try {
      await pwcapture.closeBrowser();
    } catch {
      /* ignore */
    }
  }
}

function discoverPages(url, opts) {
  return webcapture.discoverPages(url, opts);
}

module.exports = {
  capturePage,
  discoverPages,
  closeBrowser,
  activeEngine: () =>
    preferred() === 'playwright' && pwcapture && !pwUnavailable ? 'playwright' : 'osr',
};
