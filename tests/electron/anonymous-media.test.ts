import { describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ fromPartition: vi.fn(), fetch: vi.fn() }));
vi.mock('electron', () => ({
  session: {
    fromPartition: mocks.fromPartition.mockImplementation(() => ({ fetch: mocks.fetch })),
  },
}));

const { ANONYMOUS_MEDIA_PARTITION, fetchAnonymousMedia } =
  await import('../../electron/anonymous-media');

describe('anonymous media requests', () => {
  it('uses an ephemeral session and strips account credentials', async () => {
    mocks.fetch.mockResolvedValue(new Response('ok'));
    await fetchAnonymousMedia('https://www.instagram.com/p/example/', {
      credentials: 'include',
      headers: {
        Cookie: 'session=secret',
        Authorization: 'Bearer secret',
        'Proxy-Authorization': 'Basic secret',
        Accept: 'text/html',
      },
    });

    expect(ANONYMOUS_MEDIA_PARTITION).not.toMatch(/^persist:/);
    expect(mocks.fromPartition).toHaveBeenCalledWith(ANONYMOUS_MEDIA_PARTITION);
    const init = mocks.fetch.mock.calls[0][1] as RequestInit;
    expect(init.credentials).toBe('omit');
    const headers = new Headers(init.headers);
    expect(headers.get('cookie')).toBeNull();
    expect(headers.get('authorization')).toBeNull();
    expect(headers.get('proxy-authorization')).toBeNull();
    expect(headers.get('accept')).toBe('text/html');
  });
});
