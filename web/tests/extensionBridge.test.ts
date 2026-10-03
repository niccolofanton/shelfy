// The SPA's line to the extension (P2-12) against a fake `chrome.runtime`.
import { describe, it, expect, vi } from 'vitest';
import { EXTENSION_ID } from '../../extension/src/id';
import { createExtensionBridge, type ChromeLike } from '../src/extension/bridge';

function chromeAnswering(response: unknown, lastError?: string): ChromeLike & { sent: unknown[] } {
  const sent: unknown[] = [];
  const runtime: NonNullable<ChromeLike['runtime']> = {
    lastError: lastError ? { message: lastError } : null,
    sendMessage: (id, message, callback) => {
      expect(id).toBe(EXTENSION_ID);
      sent.push(message);
      callback(response);
    },
  };
  return { runtime, sent };
}

describe('extension bridge', () => {
  it('is unsupported without a chrome global', async () => {
    expect(await createExtensionBridge({ chrome: null }).probe()).toEqual({ state: 'unsupported' });
  });

  it('is missing when there is no runtime, or nobody answers', async () => {
    expect(await createExtensionBridge({ chrome: {} }).probe()).toEqual({ state: 'missing' });
    const gone = chromeAnswering(undefined, 'Could not establish connection.');
    expect(await createExtensionBridge({ chrome: gone }).probe()).toEqual({ state: 'missing' });
  });

  it('is missing when the call times out', async () => {
    vi.useFakeTimers();
    try {
      const silent: ChromeLike = { runtime: { sendMessage: () => {} } };
      const probe = createExtensionBridge({ chrome: silent, timeoutMs: 50 }).probe();
      await vi.advanceTimersByTimeAsync(60);
      expect(await probe).toEqual({ state: 'missing' });
    } finally {
      vi.useRealTimers();
    }
  });

  it('reads the ping', async () => {
    const chrome = chromeAnswering({
      ok: true,
      version: '1.2.3',
      paired: true,
      outdated: false,
      syncing: {},
    });
    expect(await createExtensionBridge({ chrome }).probe()).toEqual({
      state: 'ready',
      version: '1.2.3',
      paired: true,
      outdated: false,
    });
    expect(chrome.sent).toEqual([{ type: 'shelfy.ping' }]);
  });

  it('pairs with a code and passes the extension’s failure code on', async () => {
    const ok = chromeAnswering({ ok: true });
    expect(await createExtensionBridge({ chrome: ok }).pair('abc')).toEqual({ ok: true });
    expect(ok.sent).toEqual([{ type: 'shelfy.pair', code: 'abc' }]);
    const bad = chromeAnswering({ ok: false, code: 'invalid_pairing_code' });
    expect(await createExtensionBridge({ chrome: bad }).pair('abc')).toEqual({
      ok: false,
      code: 'invalid_pairing_code',
    });
    const none = chromeAnswering(undefined);
    expect(await createExtensionBridge({ chrome: none }).pair('abc')).toEqual({
      ok: false,
      code: 'unreachable',
    });
  });
});
