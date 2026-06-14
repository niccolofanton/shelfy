import { describe, it, expect, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import useBrowserSync, { type UseBrowserSyncOptions } from '../../src/hooks/useBrowserSync';
import type { SanitizedItem } from '../../src/lib/browserSanitize';

// The minimal <webview> surface the sync machinery touches in these tests.
interface FakeWebview {
  executeJavaScript: (code: string) => Promise<unknown>;
}

// A fake <webview>: bookkeeping snippets (the __syncStop flag flips) resolve
// immediately, while the long-running scroll scripts stay pending until the
// test settles them — that pending window is where the generation races live.
function makeWebview() {
  const scripts: Array<(value?: unknown) => void> = [];
  const wv: FakeWebview = {
    executeJavaScript: vi.fn((code: string) => {
      if (code === 'window.__syncStop = false' || code === 'window.__syncStop = true') {
        return Promise.resolve();
      }
      let resolve: (value?: unknown) => void;
      const promise = new Promise<unknown>((r) => {
        resolve = r;
      });
      scripts.push((value?: unknown) => resolve(value));
      return promise;
    }),
  };
  return { wv, scripts };
}

// A macrotask hop drains every pending microtask chain (catch → then → finishSync).
const flush = () =>
  act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });

function setup() {
  const { wv, scripts } = makeWebview();
  const webviewRefs = {
    instagram: { current: null },
    twitter: { current: wv },
    pinterest: { current: null },
  } as unknown as UseBrowserSyncOptions['webviewRefs'];
  const { result } = renderHook(() =>
    useBrowserSync({
      activeTab: 'twitter',
      webviewRefs,
      injectedScriptRef: { current: 'capture-hook' },
    }),
  );
  return { result, wv, scripts };
}

describe('useBrowserSync — generation token', () => {
  it('a previous sync settling late does not tear down the sync that replaced it', async () => {
    const { result, scripts } = setup();

    act(() => {
      expect(result.current.startSync('twitter')).toBe(true);
    });
    expect(result.current.syncing.twitter).toBe(true);

    // Take over while the first sync's scroll script is still in flight.
    act(() => {
      result.current.stopSync('twitter');
    });
    expect(result.current.syncing.twitter).toBe(false);
    act(() => {
      expect(result.current.startSync('twitter')).toBe(true);
    });
    expect(result.current.syncing.twitter).toBe(true);

    // The FIRST sync's script settles now: its completion callback carries a
    // stale generation and must not stop the second sync.
    scripts[0]();
    await flush();
    expect(result.current.syncing.twitter).toBe(true);

    // The SECOND sync's script settling tears it down normally.
    scripts[1]();
    await flush();
    expect(result.current.syncing.twitter).toBe(false);
  });

  it('exposes the running scripts promise per tab so a takeover can await it', async () => {
    const { result, scripts } = setup();
    act(() => {
      result.current.startSync('twitter');
    });
    const pending = result.current.syncScriptPromiseRefs.twitter.current;
    expect(pending).toBeInstanceOf(Promise);
    let settled = false;
    pending!.then(() => {
      settled = true;
    });
    await flush();
    expect(settled).toBe(false);
    scripts[0]();
    await flush();
    expect(settled).toBe(true);
  });

  it('mirrors step counters into syncCountsRefs synchronously with the save', async () => {
    vi.mocked(window.electronAPI.saveInterceptedPosts).mockResolvedValue({
      inserted: 2,
      skipped: 1,
    });
    const { result } = setup();
    act(() => {
      result.current.startSync('twitter');
    });
    await act(async () => {
      await result.current.ingestBatch(
        'twitter',
        [{ id: '1' }, { id: '2' }, { id: '3' }] as unknown as SanitizedItem[],
        'twitter',
        null,
      );
    });
    expect(result.current.syncCountsRefs.twitter.current).toEqual({ scanned: 3, fresh: 2 });

    // The next sync on the tab starts from a clean tally.
    act(() => {
      result.current.stopSync('twitter');
      result.current.startSync('twitter');
    });
    expect(result.current.syncCountsRefs.twitter.current).toEqual({ scanned: 0, fresh: 0 });
  });
});
