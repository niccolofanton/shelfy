// The realtime stream client (web/src/api/events.ts) against a fake
// EventSource: one connection per tab, reconnects with backoff and jitter,
// resumes after the last event id, and hands `resync` to its subscribers.
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import {
  createEventStream,
  reconnectDelay,
  type EventSourceLike,
  type EventStreamOptions,
} from '../src/api/events';

// An EventSource the test drives. Like the real one, it keeps a last event
// id buffer: an event without an `id:` reports the previous id of this
// connection, and a new connection starts empty.
class FakeEventSource implements EventSourceLike {
  static all: FakeEventSource[] = [];
  onopen: ((event: Event) => void) | null = null;
  onerror: ((event: Event) => void) | null = null;
  closed = false;
  private idBuffer = '';
  private readonly listeners = new Map<string, ((event: MessageEvent) => void)[]>();

  constructor(readonly url: string) {
    FakeEventSource.all.push(this);
  }

  static get last(): FakeEventSource {
    return FakeEventSource.all[FakeEventSource.all.length - 1];
  }

  addEventListener(type: string, listener: (event: MessageEvent) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  close(): void {
    this.closed = true;
  }

  open(): void {
    this.onopen?.(new Event('open'));
  }

  fail(): void {
    this.onerror?.(new Event('error'));
  }

  emit(type: string, data: unknown, id?: string): void {
    if (id !== undefined) this.idBuffer = id;
    const event = new MessageEvent(type, {
      data: typeof data === 'string' ? data : JSON.stringify(data),
      lastEventId: this.idBuffer,
    });
    for (const listener of this.listeners.get(type) ?? []) listener(event);
  }
}

function stream(options: EventStreamOptions = {}) {
  return createEventStream({
    url: '/api/v1/events',
    EventSource: FakeEventSource,
    // No jitter: every wait is exactly half the exponential delay.
    random: () => 0,
    target: null,
    ...options,
  });
}

beforeEach(() => {
  FakeEventSource.all = [];
  vi.useFakeTimers();
});
afterEach(() => {
  vi.useRealTimers();
});

describe('reconnectDelay', () => {
  it('doubles from 1 s up to 30 s, jittered between half and all of it', () => {
    expect(reconnectDelay(0, () => 0)).toBe(500);
    expect(reconnectDelay(0, () => 1)).toBe(1000);
    expect(reconnectDelay(3, () => 0.5)).toBe(6000);
    expect(reconnectDelay(10, () => 0)).toBe(15_000);
    expect(reconnectDelay(10, () => 1)).toBe(30_000);
    expect(reconnectDelay(1_000, () => 1)).toBe(30_000);
  });
});

describe('event stream', () => {
  it('opens one connection for every subscriber and closes it after the last', () => {
    const events = stream();
    expect(events.state).toBe('closed');
    const offA = events.on('posts.changed', () => {});
    const offB = events.on('stats.changed', () => {});
    expect(FakeEventSource.all).toHaveLength(1);
    expect(FakeEventSource.last.url).toBe('/api/v1/events');
    expect(events.state).toBe('connecting');
    FakeEventSource.last.open();
    expect(events.state).toBe('open');
    offA();
    offA();
    expect(FakeEventSource.last.closed).toBe(false);
    offB();
    expect(FakeEventSource.last.closed).toBe(true);
    expect(events.state).toBe('closed');
  });

  it('hands each subscriber the typed payload of its event', () => {
    const events = stream();
    const changed = vi.fn();
    const stats = vi.fn();
    events.on('posts.changed', changed);
    events.on('stats.changed', stats);
    const es = FakeEventSource.last;
    es.open();
    es.emit('hello', { version: '0.1.0', heartbeatMs: 20_000, lastEventId: 'e-0' }, 'e-0');
    es.emit('posts.changed', { keys: ['ig_1'], reason: 'edit' }, 'e-1');
    es.emit('stats.changed', {}, 'e-2');
    expect(changed).toHaveBeenCalledWith({ keys: ['ig_1'], reason: 'edit' });
    expect(stats).toHaveBeenCalledWith({});
    expect(events.lastEventId).toBe('e-2');
  });

  it('reconnects with backoff and resumes after the last event id', () => {
    const events = stream();
    events.on('posts.changed', () => {});
    const first = FakeEventSource.last;
    first.open();
    first.emit('hello', { version: 'v', heartbeatMs: 20_000, lastEventId: 'e-4' }, 'e-4');
    first.emit('posts.changed', { keys: null, reason: 'ingest' }, 'e-5');

    first.fail();
    expect(first.closed).toBe(true);
    expect(events.state).toBe('waiting');
    vi.advanceTimersByTime(499);
    expect(FakeEventSource.all).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.all).toHaveLength(2);
    expect(FakeEventSource.last.url).toBe('/api/v1/events?lastEventId=e-5');
  });

  it('keeps the position when a resumed hello carries no id', () => {
    const events = stream();
    events.on('posts.changed', () => {});
    FakeEventSource.last.emit('hello', { version: 'v', heartbeatMs: 1, lastEventId: 'e-9' }, 'e-9');
    FakeEventSource.last.fail();
    vi.advanceTimersByTime(500);
    const resumed = FakeEventSource.last;
    // A resuming stream's hello has no `id:`; its payload names the head.
    resumed.emit('hello', { version: 'v', heartbeatMs: 1, lastEventId: 'e-12' });
    expect(events.lastEventId).toBe('e-9');
    resumed.emit('posts.changed', { keys: ['x_1'], reason: 'ai' }, 'e-10');
    expect(events.lastEventId).toBe('e-10');
  });

  it('waits longer after each failure, up to 30 s, and starts over after hello', () => {
    const events = stream();
    events.on('stats.changed', () => {});
    const waits: number[] = [];
    for (let i = 0; i < 8; i++) {
      const before = FakeEventSource.all.length;
      FakeEventSource.last.fail();
      let waited = 0;
      while (FakeEventSource.all.length === before) {
        vi.advanceTimersByTime(100);
        waited += 100;
      }
      waits.push(waited);
    }
    expect(waits).toEqual([500, 1000, 2000, 4000, 8000, 15_000, 15_000, 15_000]);

    FakeEventSource.last.open();
    FakeEventSource.last.emit('hello', { version: 'v', heartbeatMs: 1, lastEventId: 'e-1' }, 'e-1');
    FakeEventSource.last.fail();
    vi.advanceTimersByTime(500);
    expect(FakeEventSource.last.url).toBe('/api/v1/events?lastEventId=e-1');
    expect(FakeEventSource.all).toHaveLength(10);
  });

  it('jitters each wait between half and all of the delay', () => {
    const events = stream({ random: () => 0.5 });
    events.on('stats.changed', () => {});
    FakeEventSource.last.fail();
    vi.advanceTimersByTime(749);
    expect(FakeEventSource.all).toHaveLength(1);
    vi.advanceTimersByTime(1);
    expect(FakeEventSource.all).toHaveLength(2);
  });

  it('hands resync to its subscribers and resumes from its id', () => {
    const events = stream();
    const resync = vi.fn();
    events.on('resync', resync);
    FakeEventSource.last.emit('resync', { reason: 'expired' }, 'e-40');
    expect(resync).toHaveBeenCalledWith({ reason: 'expired' });
    FakeEventSource.last.fail();
    vi.advanceTimersByTime(500);
    expect(FakeEventSource.last.url).toBe('/api/v1/events?lastEventId=e-40');
  });

  it('checks the session only when a connection fails before it opened', () => {
    const onConnectFailed = vi.fn();
    const events = stream({ onConnectFailed });
    events.on('posts.changed', () => {});
    FakeEventSource.last.fail();
    expect(onConnectFailed).toHaveBeenCalledTimes(1);
    vi.advanceTimersByTime(500);
    FakeEventSource.last.open();
    FakeEventSource.last.fail();
    expect(onConnectFailed).toHaveBeenCalledTimes(1);
  });

  it('stops retrying once the last subscriber leaves', () => {
    const events = stream();
    const off = events.on('posts.changed', () => {});
    FakeEventSource.last.fail();
    off();
    vi.advanceTimersByTime(60_000);
    expect(FakeEventSource.all).toHaveLength(1);
    expect(events.state).toBe('closed');
  });

  it('reconnects at once when the browser is back online', () => {
    const target = new EventTarget();
    const events = stream({ target });
    events.on('posts.changed', () => {});
    for (let i = 0; i < 5; i++) {
      FakeEventSource.last.fail();
      vi.runOnlyPendingTimers();
    }
    FakeEventSource.last.fail();
    const before = FakeEventSource.all.length;
    target.dispatchEvent(new Event('online'));
    expect(FakeEventSource.all).toHaveLength(before + 1);
    expect(events.state).toBe('connecting');
  });

  it('ignores malformed payloads and survives a failing listener', () => {
    const errors = vi.spyOn(console, 'error').mockImplementation(() => {});
    const events = stream();
    const good = vi.fn();
    events.on('posts.changed', () => {
      throw new Error('listener bug');
    });
    events.on('posts.changed', good);
    FakeEventSource.last.emit('posts.changed', 'not json', 'e-1');
    expect(good).not.toHaveBeenCalled();
    FakeEventSource.last.emit('posts.changed', { keys: null, reason: 'edit' }, 'e-2');
    expect(good).toHaveBeenCalledTimes(1);
    expect(FakeEventSource.last.closed).toBe(false);
    errors.mockRestore();
  });

  it('never connects without an EventSource', () => {
    const events = createEventStream({ EventSource: null, target: null });
    const off = events.on('posts.changed', () => {});
    expect(events.state).toBe('closed');
    off();
  });
});

describe('live AI stream subscription', () => {
  it('opts in on the shared connection only while requested and preserves replay cursor', () => {
    const s = stream();
    const stopPosts = s.on('posts.changed', () => {});
    const original = FakeEventSource.last;
    expect(original.url).not.toContain('topics=');
    original.emit('posts.changed', { keys: null, reason: 'ai' }, '42');
    const receive = vi.fn();
    const stopAi = s.on('ai.stream', receive);
    expect(original.closed).toBe(true);
    const opted = FakeEventSource.last;
    expect(opted.url).toContain('topics=ai.stream');
    expect(opted.url).toContain('lastEventId=42');
    const connections = FakeEventSource.all.length;
    const stopAi2 = s.on('ai.stream', () => {});
    expect(FakeEventSource.all).toHaveLength(connections);
    opted.emit('ai.stream', { postKey: 'x_1', text: 'Preview' });
    expect(receive).toHaveBeenCalledWith({ postKey: 'x_1', text: 'Preview' });
    stopAi();
    expect(opted.closed).toBe(false);
    stopAi2();
    expect(opted.closed).toBe(true);
    expect(FakeEventSource.last.url).not.toContain('topics=');
    expect(FakeEventSource.last.url).toContain('lastEventId=42');
    stopPosts();
    expect(FakeEventSource.last.closed).toBe(true);
  });
});
