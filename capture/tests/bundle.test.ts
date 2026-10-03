// The service bundle must NOT drag in the v1 Electron capture engine or the
// Electron-only unblock drivers (SPIKE-11): the capture path moved its shared
// helpers into webcap/, so the bundle is self-contained. This bundles the service
// entry the way capture/build.ts does and inspects esbuild's input list.

import { describe, it, expect } from 'vitest';
import path from 'path';
import { fileURLToPath } from 'url';
import { build } from 'esbuild';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const ROOT = path.resolve(HERE, '..', '..');

const FORBIDDEN = [
  'electron/webcapture.ts', // the 2,100-line v1 engine
  'electron/webcapture-playwright.ts', // the old Playwright engine + cookie bridge
  'electron/webcap/electron-driver.ts', // the visible-window Electron fallback
  'electron/webcap/system-chrome.ts', // the system-Chrome unblock driver
];

describe('capture service bundle', () => {
  it('bundles without the v1 engine or the Electron fallbacks', async () => {
    const result = await build({
      entryPoints: [path.join(ROOT, 'capture', 'src', 'main.ts')],
      bundle: true,
      platform: 'node',
      target: 'node24',
      format: 'cjs',
      write: false,
      metafile: true,
      logLevel: 'silent',
      alias: { electron: path.join(ROOT, 'capture', 'src', 'electron-shim.ts') },
      external: [
        'playwright-core',
        '@ghostery/adblocker',
        '@ghostery/adblocker-playwright',
        '@duckduckgo/autoconsent',
        'tldts-experimental',
        'ffmpeg-static',
        'zod',
      ],
    });
    const inputs = Object.keys(result.metafile.inputs).map((p) => p.replace(/\\/g, '/'));
    for (const f of FORBIDDEN) {
      expect(
        inputs.some((i) => i.endsWith(f)),
        `bundle must not include ${f}`,
      ).toBe(false);
    }
    // Sanity: it DOES bundle the capture entry and the v2 capture engine.
    expect(inputs.some((i) => i.endsWith('capture/src/run.ts'))).toBe(true);
    expect(inputs.some((i) => i.endsWith('electron/webcap/capture.ts'))).toBe(true);
    expect(inputs.some((i) => i.endsWith('electron/webcap/sitefetch.ts'))).toBe(true);
    expect(inputs.some((i) => i.endsWith('electron/webcap/browser.ts'))).toBe(true);
  }, 60_000);
});
