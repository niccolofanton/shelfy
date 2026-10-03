import { afterEach, describe, expect, it, vi } from 'vitest';
import fs from 'fs';
import os from 'os';
import path from 'path';
import { ManifestSchema } from '../src/protocol';
vi.mock('../../electron/webcap/sitefetch', () => ({
  encodeImage: vi.fn(async (_src: string, out: string) => {
    const fs = await import('fs');
    fs.copyFileSync(path.resolve('capture/fixtures/recorded/basic/p0-hero.webp'), out);
  }),
}));
import { FETCH_CAP, blockedFallback, fetchComplete, ogUrl } from '../src/fetched';
afterEach(() => {
  vi.unstubAllGlobals();
});
const signal = new AbortController().signal;
describe('strict capture metadata fetch', () => {
  it('returns only a complete bounded body', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => new Response('hello', { headers: { 'content-length': '5' } })),
    );
    expect((await fetchComplete('https://example.test/', signal, 'text/html')).toString()).toBe(
      'hello',
    );
  });
  it('rejects truncation and both declared and streamed oversize', async () => {
    for (const response of [
      new Response('abc', { headers: { 'content-length': '9' } }),
      new Response('abc', { headers: { 'content-length': String(FETCH_CAP + 1) } }),
      new Response(new Uint8Array(FETCH_CAP + 1)),
    ]) {
      vi.stubGlobal(
        'fetch',
        vi.fn(async () => response),
      );
      await expect(fetchComplete('https://example.test/', signal, 'image/*')).rejects.toThrow();
    }
  });
  it('validates every redirect before requesting it', async () => {
    const fetcher = vi.fn(
      async () =>
        new Response(null, { status: 302, headers: { location: 'http://127.0.0.1/secret' } }),
    );
    vi.stubGlobal('fetch', fetcher);
    await expect(fetchComplete('https://example.test/', signal, 'text/html')).rejects.toThrow();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
  it('resolves OG attributes in either order without allowing private origins', () => {
    expect(
      ogUrl(`<meta content="/cover.jpg?x=1&amp;y=2" property="og:image">`, 'https://example.test/'),
    ).toBe('https://example.test/cover.jpg?x=1&y=2');
    expect(
      ogUrl(`<meta property='og:image' content='http://127.0.0.1/a'>`, 'https://example.test/'),
    ).toBeNull();
  });
});

describe('blocked OG fallback', () => {
  it('writes a schema-valid partial manifest only after a complete image', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-blocked-test-'));
    const req = {
      captureId: '01J0000000000000000000000X',
      url: 'https://example.test/',
      maxPages: 6,
      singlePage: false,
      video: false,
      workDir: '/work/01J0000000000000000000000X',
    };
    try {
      vi.stubGlobal(
        'fetch',
        vi.fn(
          async (url: string) =>
            new Response(
              url.endsWith('cover.webp')
                ? fs.readFileSync('capture/fixtures/recorded/basic/p0-hero.webp')
                : `<meta property="og:image" content="/cover.webp">`,
            ),
        ),
      );
      expect(await blockedFallback(req, dir, signal, Date.now(), 1024)).toBe(true);
      const manifest = JSON.parse(fs.readFileSync(path.join(dir, 'manifest.json'), 'utf8'));
      expect(ManifestSchema.safeParse(manifest).success).toBe(true);
      expect(manifest.cover.role).toBe('og');
      expect(manifest.pages).toEqual([]);
      expect(manifest.partial).toBe(true);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
  it('does not encode or write a fallback from a truncated image', async () => {
    const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'shelfy-blocked-test-'));
    const req = {
      captureId: '01J0000000000000000000000X',
      url: 'https://example.test/',
      maxPages: 6,
      singlePage: false,
      video: false,
      workDir: '/work/01J0000000000000000000000X',
    };
    try {
      vi.stubGlobal(
        'fetch',
        vi.fn(async (url: string) =>
          url.endsWith('cover.webp')
            ? new Response('partial', { headers: { 'content-length': '100' } })
            : new Response(`<meta property="og:image" content="/cover.webp">`),
        ),
      );
      expect(await blockedFallback(req, dir, signal, Date.now(), 1024)).toBe(false);
      expect(fs.readdirSync(dir)).toEqual([]);
    } finally {
      fs.rmSync(dir, { recursive: true, force: true });
    }
  });
});
