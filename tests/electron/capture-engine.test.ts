import { describe, it, expect, vi, beforeEach, type Mock } from 'vitest';
import { createRequire } from 'module';
import { fileURLToPath } from 'url';
import path from 'path';

// ─── Dependency mocking ──────────────────────────────────────────────────────────
// capture-engine has two seams with different module systems:
//   • the OSR engine is pulled in as `import * as webcapture from './webcapture'`,
//     so Vitest's vi.mock intercepts it cleanly (ESM).
//   • the Playwright engine is lazy-loaded with a synchronous, untransformed
//     `require('./webcapture-playwright')`. Vite leaves that `require` as the native
//     Node one, which neither honours vi.mock nor can resolve a .ts file — so we
//     prime Node's require cache directly (the same idea as the old CJS test, but
//     keyed on the .ts path after teaching native require to resolve .ts).

interface OsrMock {
  capturePage: Mock;
  discoverPages: Mock;
}
interface PwMock {
  capturePage: Mock;
  closeBrowser: Mock;
}

const osrMock = vi.hoisted(
  (): OsrMock => ({
    capturePage: vi.fn(async () => ({ engine: 'osr' })),
    discoverPages: vi.fn(),
  }),
);
vi.mock('../../electron/webcapture', () => osrMock);

// Minimal shape of Node's internal Module needed to register a .ts resolver.
interface ModuleInternals {
  _extensions: Record<string, (m: NodeModule, filename: string) => void>;
}

const nodeReq = createRequire(import.meta.url);
const pwAbs = path.resolve(
  fileURLToPath(import.meta.url),
  '../../../electron/webcapture-playwright.ts',
);

const pwMock: PwMock = {
  capturePage: vi.fn(async () => ({ engine: 'playwright' })),
  closeBrowser: vi.fn(),
};

// Make native require able to RESOLVE the .ts target (so the cache key below is hit),
// then seed the cache with the mock as that module's exports.
const ModuleInternal = nodeReq('module') as unknown as ModuleInternals;
if (!ModuleInternal._extensions['.ts']) {
  ModuleInternal._extensions['.ts'] = (m: NodeModule): void => {
    m.exports = {};
  };
}
nodeReq.cache[pwAbs] = {
  id: pwAbs,
  filename: pwAbs,
  loaded: true,
  exports: pwMock,
  children: [],
  paths: [],
} as unknown as NodeModule;

interface CaptureEngine {
  capturePage: (url: string) => Promise<{ engine: string }>;
  activeEngine: () => string;
}

const captureEngine = (await import('../../electron/capture-engine')) as unknown as CaptureEngine;

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
