import { describe, it, expect, vi, beforeEach } from 'vitest';
import { createRequire } from 'module';

// ─── Require cache patching ────────────────────────────────────────────────────
// capture-engine.js is CJS, so vi.mock hoisting won't intercept its synchronous
// requires — patch the require cache directly (same approach as downloader.test.js).

const req = createRequire(import.meta.url);

function inject(id, exports) {
  const resolved = req.resolve(id);
  req.cache[resolved] = {
    id: resolved,
    filename: resolved,
    loaded: true,
    exports,
    children: [],
    paths: [],
  };
  return exports;
}

const osrMock = inject('../../electron/webcapture.js', {
  capturePage: vi.fn(async () => ({ engine: 'osr' })),
  discoverPages: vi.fn(),
});
const pwMock = inject('../../electron/webcapture-playwright.js', {
  capturePage: vi.fn(async () => ({ engine: 'playwright' })),
  closeBrowser: vi.fn(),
});

const captureEngine = req('../../electron/capture-engine.js');

beforeEach(() => {
  osrMock.capturePage.mockClear();
  pwMock.capturePage.mockClear();
});

// Error-classification: only genuine browser-availability failures may flip the
// session to OSR; per-URL/pipeline errors must propagate without latching.

describe('capture-engine fallback classification', () => {
  it('propagates a generic ENOENT pipeline error without falling back to OSR', async () => {
    pwMock.capturePage.mockRejectedValueOnce(
      new Error("ENOENT: no such file or directory, open '/tmp/shots/home.png'"),
    );
    await expect(captureEngine.capturePage('https://a.example')).rejects.toThrow(/ENOENT/);
    expect(osrMock.capturePage).not.toHaveBeenCalled();
    expect(captureEngine.activeEngine()).toBe('playwright');
  });

  it('propagates a navigation timeout without falling back to OSR', async () => {
    pwMock.capturePage.mockRejectedValueOnce(
      new Error('page.goto: Timeout 30000ms exceeded navigating to https://a.example'),
    );
    await expect(captureEngine.capturePage('https://a.example')).rejects.toThrow(/Timeout/);
    expect(osrMock.capturePage).not.toHaveBeenCalled();
    expect(captureEngine.activeEngine()).toBe('playwright');
  });

  it('propagates errors that merely mention Chromium or a download', async () => {
    pwMock.capturePage.mockRejectedValueOnce(
      new Error('page crashed while rendering Chromium download page'),
    );
    await expect(captureEngine.capturePage('https://a.example')).rejects.toThrow(/crashed/);
    expect(osrMock.capturePage).not.toHaveBeenCalled();
    expect(captureEngine.activeEngine()).toBe('playwright');
  });

  it('falls back to OSR (and latches) on a genuine launch failure', async () => {
    pwMock.capturePage.mockRejectedValueOnce(
      new Error("browserType.launch: Executable doesn't exist at /x/chrome-headless-shell"),
    );
    const out = await captureEngine.capturePage('https://a.example');
    expect(out).toEqual({ engine: 'osr' });
    expect(osrMock.capturePage).toHaveBeenCalledTimes(1);
    expect(captureEngine.activeEngine()).toBe('osr');

    // Latched: later captures go straight to OSR without retrying Playwright.
    await captureEngine.capturePage('https://b.example');
    expect(pwMock.capturePage).toHaveBeenCalledTimes(1);
    expect(osrMock.capturePage).toHaveBeenCalledTimes(2);
  });
});
