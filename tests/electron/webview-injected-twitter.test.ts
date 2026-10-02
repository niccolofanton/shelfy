// @vitest-environment jsdom
import { describe, expect, it, vi } from 'vitest';

describe('X Bookmarks DOM fallback', () => {
  it('captures rendered bookmarks once and ignores the Likes tab', async () => {
    const send = vi.fn();
    vi.stubGlobal('location', { pathname: '/i/history', origin: 'https://x.com' });
    window.__socialSavedBridge = { send };
    window.__socialSavedInjected = false;
    document.body.innerHTML = `
      <div role="tab" aria-selected="true">Bookmarks</div>
      <div role="tab" aria-selected="false">Likes</div>
      <article data-testid="tweet">
        <div data-testid="User-Name">Alice @alice</div>
        <a href="/alice/status/123"><time datetime="2026-09-28T10:00:00Z">today</time></a>
        <div data-testid="tweetText">Saved photo</div>
        <img src="https://pbs.twimg.com/media/photo.jpg" />
      </article>`;

    await import('../../electron/webview-injected');
    window.__ssScanTwitterBookmarks?.();
    expect(send).toHaveBeenCalledTimes(1);
    const [items, hasNextPage, platform] = send.mock.calls[0];
    expect(platform).toBe('twitter');
    expect(hasNextPage).toBeNull();
    expect(items[0]).toMatchObject({
      id: '123',
      postUrl: 'https://x.com/alice/status/123',
      text: 'Saved photo',
      mediaType: 'image',
      timestamp: '2026-09-28T10:00:00.000Z',
      media: [{ type: 'image', url: 'https://pbs.twimg.com/media/photo.jpg' }],
    });

    window.__ssScanTwitterBookmarks?.();
    expect(send).toHaveBeenCalledTimes(1);
    document.querySelectorAll('[role="tab"]')[0].setAttribute('aria-selected', 'false');
    document.querySelectorAll('[role="tab"]')[1].setAttribute('aria-selected', 'true');
    document.querySelector('article a')?.setAttribute('href', '/alice/status/456');
    window.__ssScanTwitterBookmarks?.();
    expect(send).toHaveBeenCalledTimes(1);
    vi.unstubAllGlobals();
  });
});
