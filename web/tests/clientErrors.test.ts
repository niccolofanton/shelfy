// Crash reports to `POST /api/v1/client-errors` (web/src/api/clientErrors.ts):
// technical fields only, within the server's limits, throttled.
import { describe, it, expect, vi } from 'vitest';
import type { Http } from '../src/api/http';
import { createErrorReporter, toClientErrorReport } from '../src/api/clientErrors';

function fakeHttp() {
  const send = vi.fn().mockResolvedValue(new Response(null, { status: 204 }));
  const http: Http = {
    get: vi.fn(),
    send,
    onUnauthorized: () => () => {},
    sessionEnded: () => {},
    onReauthRequired: () => () => {},
  };
  return { http, send };
}

const crash = (message = 'Cannot read properties of undefined (reading "map")') =>
  Object.assign(new TypeError(message), { stack: `TypeError: ${message}\n    at Gallery` });

describe('toClientErrorReport', () => {
  it('keeps only the fields the server takes', () => {
    const body = toClientErrorReport(
      { view: 'gallery', error: crash(), componentStack: '\n    at Gallery\n    at App' },
      {
        occurredAt: 1_700_000_000_000.7,
        route: '/c/:collectionId',
        clientVersion: '1759420800000',
      },
    );
    expect(body).toEqual({
      view: 'gallery',
      name: 'TypeError',
      message: 'Cannot read properties of undefined (reading "map")',
      stack: 'TypeError: Cannot read properties of undefined (reading "map")\n    at Gallery',
      componentStack: '\n    at Gallery\n    at App',
      route: '/c/:collectionId',
      clientVersion: '1759420800000',
      occurredAt: 1_700_000_000_000,
    });
  });

  it('clips text to the server limits without splitting a character', () => {
    const body = toClientErrorReport(
      { view: 'gallery', error: new Error(`${'a'.repeat(999)}😀tail`) },
      { occurredAt: 0 },
    );
    expect(body.message).toBe('a'.repeat(999));
    const long = toClientErrorReport(
      { view: 'gallery', error: crash('x'.repeat(9000)), componentStack: 'c'.repeat(9000) },
      { occurredAt: 0 },
    );
    expect(long.message).toHaveLength(1000);
    expect(long.stack).toHaveLength(8000);
    expect(long.componentStack).toHaveLength(8000);
    const lone = toClientErrorReport(
      { view: 'gallery', error: new Error('broken \ud83d text') },
      { occurredAt: 0 },
    );
    expect(lone.message).toBe('broken \ufffd text');
  });

  it('cleans identifiers the server would refuse instead of losing the report', () => {
    const body = toClientErrorReport(
      { view: 'post modal/edit', error: 'boom' },
      { occurredAt: 0, route: '/p/ig_1?tab=ai', clientVersion: '1.0 beta' },
    );
    expect(body.view).toBe('post-modal-edit');
    expect(body.message).toBe('boom');
    expect(body.route).toBeUndefined();
    expect(body.clientVersion).toBeUndefined();
    expect(body.name).toBeUndefined();
    expect(toClientErrorReport({ view: '', error: { secret: 1 } }, { occurredAt: 0 })).toEqual({
      view: 'unknown',
      message: 'object',
      occurredAt: 0,
    });
  });
});

describe('createErrorReporter', () => {
  it('posts a report that survives the page closing, with the route pattern', async () => {
    const { http, send } = fakeHttp();
    const report = createErrorReporter(http, {
      route: () => '/p/:key',
      clientVersion: 'b1',
      now: () => 42,
    });
    report({ view: 'postModal', error: crash() });
    expect(send).toHaveBeenCalledWith(
      'POST',
      '/api/v1/client-errors',
      expect.objectContaining({ view: 'postModal', route: '/p/:key', occurredAt: 42 }),
      { keepalive: true },
    );
  });

  it('sends the same error once a minute and at most ten a minute', () => {
    const { http, send } = fakeHttp();
    let now = 0;
    const report = createErrorReporter(http, { now: () => now });
    report({ view: 'gallery', error: crash() });
    report({ view: 'gallery', error: crash() });
    expect(send).toHaveBeenCalledTimes(1);
    for (let i = 0; i < 20; i++) report({ view: 'gallery', error: crash(`error ${i}`) });
    expect(send).toHaveBeenCalledTimes(10);
    now = 60_000;
    report({ view: 'gallery', error: crash() });
    expect(send).toHaveBeenCalledTimes(11);
  });

  it('drops a report the server does not take', async () => {
    const { http, send } = fakeHttp();
    send.mockRejectedValue(new Error('offline'));
    const report = createErrorReporter(http);
    expect(() => report({ view: 'gallery', error: crash() })).not.toThrow();
    await Promise.resolve();
  });
});
