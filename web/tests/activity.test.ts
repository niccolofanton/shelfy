import { afterEach, describe, expect, it, vi } from 'vitest';
import { createActivityApi } from '../src/api/activity';
import { createHttp } from '../src/api/http';
import { createEventStream, type EventSourceLike } from '../src/api/events';

class Source implements EventSourceLike {
  static instances: Source[] = [];
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  listeners = new Map<string, (event: MessageEvent) => void>();
  closed = false;
  constructor(readonly url: string) {
    Source.instances.push(this);
  }
  addEventListener(name: string, listener: (event: MessageEvent) => void) {
    this.listeners.set(name, listener);
  }
  close() {
    this.closed = true;
  }
  emit(name: string, data: unknown, id = '') {
    this.listeners.get(name)?.(
      new MessageEvent(name, { data: JSON.stringify(data), lastEventId: id }),
    );
  }
}
afterEach(() => {
  vi.useRealTimers();
  Source.instances = [];
});
describe('Activity API and shared SSE', () => {
  it('lists by cursor and marks reads with the authenticated CSRF transport', async () => {
    const fetch = vi.fn(
      async (_url: RequestInfo | URL, _init?: RequestInit) =>
        new Response(JSON.stringify({ items: [], nextCursor: null, unreadCount: 0 })),
    );
    const api = createActivityApi(createHttp({ fetch }), createEventStream({ EventSource: null }));
    await api.list({ limit: 999, cursor: 'older+id' });
    await api.read({ upTo: 8 });
    expect(String(fetch.mock.calls[0][0])).toBe(
      '/api/v1/notifications?limit=200&cursor=older%2Bid',
    );
    const request = fetch.mock.calls[1][1];
    expect(request?.credentials).toBe('same-origin');
    expect(new Headers(request?.headers).get('X-Shelfy-Client')).toBe('web');
    expect(request?.body).toBe(JSON.stringify({ upTo: 8 }));
  });
  it('shares one stream, resumes notifications, refreshes gaps and closes the last subscription', () => {
    vi.useFakeTimers();
    const stream = createEventStream({ EventSource: Source, random: () => 1, target: null });
    const api = createActivityApi(createHttp(), stream);
    const notification = vi.fn();
    const refresh = vi.fn();
    const stopNotification = api.onNotification(notification);
    const stopRefresh = api.onRefresh(refresh);
    expect(Source.instances).toHaveLength(1);
    Source.instances[0].onopen?.(new Event('open'));
    Source.instances[0].emit('hello', {});
    Source.instances[0].emit('notification', { id: 4 }, 'n-4');
    expect(notification).toHaveBeenCalledWith({ id: 4 });
    Source.instances[0].onerror?.(new Event('error'));
    vi.advanceTimersByTime(1000);
    expect(Source.instances[1].url).toContain('lastEventId=n-4');
    Source.instances[1].emit('hello', {});
    Source.instances[1].emit('resync', {});
    expect(refresh).toHaveBeenCalledTimes(3);
    stopNotification();
    expect(Source.instances[1].closed).toBe(false);
    stopRefresh();
    expect(Source.instances[1].closed).toBe(true);
    expect(stream.state).toBe('closed');
  });
});
