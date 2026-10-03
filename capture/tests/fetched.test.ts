import { afterEach, describe, expect, it, vi } from 'vitest';
vi.mock('../../electron/webcap/sitefetch', () => ({ encodeImage: vi.fn() }));
import { FETCH_CAP, fetchComplete, ogUrl } from '../src/fetched';
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
