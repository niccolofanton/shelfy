import { describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({
  fetch: vi.fn(),
  getPost: vi.fn(),
  clearMissingLocalPaths: vi.fn(() => false),
  refreshPreviewUrl: vi.fn(() => true),
  enqueuePreviews: vi.fn(),
}));

vi.mock('../../electron/anonymous-media', () => ({ fetchAnonymousMedia: mocks.fetch }));
vi.mock('../../electron/db', () => ({
  getPost: mocks.getPost,
  clearMissingLocalPaths: mocks.clearMissingLocalPaths,
  refreshPreviewUrl: mocks.refreshPreviewUrl,
}));
vi.mock('../../electron/interceptor', () => ({ SOCIAL_UA: 'test' }));
vi.mock('../../electron/preview-cache', () => ({
  isSupportedPreviewUrl: () => true,
  enqueuePreviews: mocks.enqueuePreviews,
}));

const { metaContent, requestPreviewRepair } = await import('../../electron/preview-repair');

describe('expired CDN preview recovery', () => {
  it('extracts fresh URLs from canonical metadata and queues a local preview', async () => {
    const old = 'https://old.cdninstagram.com/expired.jpg';
    const fresh = 'https://new.cdninstagram.com/fresh.jpg?a=1&b=2';
    mocks.getPost.mockReturnValue({
      id: 'ig-test',
      platform: 'instagram',
      postUrl: 'https://www.instagram.com/p/abc123/',
      thumbnailUrl: old,
      previewPath: null,
      thumbnailPath: null,
      imagePath: null,
    });
    mocks.fetch.mockResolvedValue(
      new Response(
        `<meta property="og:url" content="https://www.instagram.com/p/abc123/">` +
          `<meta content="${fresh.replace('&', '&amp;')}" property="og:image">`,
        { headers: { 'content-type': 'text/html; charset=utf-8' } },
      ),
    );
    const onSaved = vi.fn();
    expect(requestPreviewRepair('ig-test', onSaved)).toBe(true);
    await vi.waitFor(() =>
      expect(mocks.enqueuePreviews).toHaveBeenCalledWith(
        [{ id: 'ig-test', platform: 'instagram', thumbnailUrl: fresh }],
        onSaved,
      ),
    );
    expect(mocks.refreshPreviewUrl).toHaveBeenCalledWith('ig-test', old, fresh);
    expect(mocks.clearMissingLocalPaths).toHaveBeenCalledWith('ig-test');
    expect(mocks.fetch).toHaveBeenCalledWith(
      'https://www.instagram.com/p/abc123/',
      expect.objectContaining({ redirect: 'manual' }),
    );
    expect(metaContent('<meta content="a&amp;b" property="og:image">', 'og:image')).toBe('a&b');
  });
});
