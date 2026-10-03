// The side panel's pure parts: the active-tab status, the connection-check lines (P2-22), the
// "not captured" summary and the debug census rows.

import { describe, expect, it } from 'vitest';
import { censusRows } from '../src/panel/sections/debug';
import { connectionLines } from '../src/panel/sections/connection';
import { discardSummary } from '../src/panel/sections/queue';
import type { PanelContext } from '../src/panel/section';
import { tabStatus, tabStatusText } from '../src/panel/tab-status';
import { createTranslate } from '../src/shared/i18n';
import { emptyCounters } from '../src/sw/queue/types';
import type { ConnectionCheck } from '../src/sw/settings';

const t = createTranslate('en');
const ctx = { t, errorText: (code: string) => code } as unknown as PanelContext;
const on = {
  paired: true,
  outdated: false,
  passive: { instagram: true, twitter: true, pinterest: true },
  serverPassive: { instagram: true, twitter: true, pinterest: true },
};
const pong = (viewer: string | null = null) => ({ ok: true as const, docId: 'd', viewer });

describe('tabStatus', () => {
  it('capturing on a saved listing with a live bridge', () => {
    const status = tabStatus(
      'https://www.instagram.com/someone/saved/recipes/17890000000000001/',
      pong(),
      on,
    );
    expect(status.kind).toBe('capturing');
    expect(tabStatusText(t, status)).toBe('This tab: Instagram · folder recipes — capturing');
  });

  it('asks for a reload when the bridge does not answer', () => {
    const status = tabStatus('https://x.com/i/bookmarks', null, on);
    expect(status.kind).toBe('reload');
    expect(tabStatusText(t, status)).toBe(
      'This tab: X · bookmarks — reload the tab to start capturing',
    );
  });

  it('reports off, out of scope, other boards and unsupported sites', () => {
    expect(tabStatus('https://x.com/i/bookmarks', pong(), { ...on, paired: false }).kind).toBe(
      'off',
    );
    expect(
      tabStatus('https://x.com/i/bookmarks', pong(), {
        ...on,
        serverPassive: { ...on.serverPassive, twitter: false },
      }).kind,
    ).toBe('off');
    expect(tabStatus('https://www.instagram.com/explore/', pong(), on).kind).toBe('out_of_scope');
    expect(tabStatus('https://www.pinterest.com/other/board/', pong('someone'), on).kind).toBe(
      'not_own_board',
    );
    expect(tabStatus('https://www.pinterest.com/someone/board/', pong(null), on).kind).toBe(
      'viewer_unknown',
    );
    expect(tabStatus('https://www.pinterest.com/someone/board/', pong('someone'), on).kind).toBe(
      'capturing',
    );
    expect(tabStatus('https://example.com/', pong(), on).kind).toBe('none');
    expect(tabStatus(undefined, null, on).kind).toBe('none');
    expect(tabStatusText(t, tabStatus(undefined, null, on))).toBe('This tab: no supported site');
  });
});

describe('connection check lines (P2-22)', () => {
  const check: ConnectionCheck = {
    at: 1,
    origin: 'https://refs.niccolofanton.dev',
    cookie: { outcome: 'access_redirect', status: null, detail: null, version: null },
    headers: { outcome: 'ok', status: 200, detail: null, version: '0.2.0' },
    token: { outcome: 'ok', detail: null },
  };

  it('says which mode passes Access', () => {
    expect(connectionLines(ctx, check)).toEqual([
      'Browser cookie: redirected to the Cloudflare Access sign-in',
      'Service-token headers: Shelfy answered',
      'Extension token: accepted',
      'Server version: 0.2.0',
    ]);
    expect(
      connectionLines(ctx, {
        ...check,
        cookie: { outcome: 'ok', status: 200, detail: null, version: '0.2.0' },
        headers: null,
        token: { outcome: 'unpaired', detail: null },
      }),
    ).toEqual([
      'Browser cookie: Shelfy answered',
      'Service-token headers: not set',
      'Extension token: none, the extension is not paired',
      'Server version: 0.2.0',
    ]);
    expect(
      connectionLines(ctx, {
        ...check,
        cookie: { outcome: 'failed', status: 502, detail: 'HTTP 502', version: null },
        headers: null,
        token: { outcome: 'failed', detail: 'not_found' },
      }),
    ).toEqual([
      'Browser cookie: failed (HTTP 502)',
      'Service-token headers: not set',
      'Extension token: failed (not_found)',
    ]);
  });
});

describe('queue section', () => {
  it('summarizes what was not captured, and nothing when all was', () => {
    const counters = emptyCounters();
    expect(discardSummary(t, counters)).toBeNull();
    counters.discarded.out_of_scope = 12;
    counters.discarded.not_own_board = 2;
    expect(discardSummary(t, counters)).toBe(
      "Not captured: outside saved listings 12, another user's board 2",
    );
  });

  it('every discard reason and panel code has a string', () => {
    const counters = emptyCounters();
    for (const reason of Object.keys(counters.discarded))
      expect(t(`queue.reason.${reason}`)).not.toBe(`queue.reason.${reason}`);
    for (const code of [
      'network',
      'access_redirect',
      'unauthorized',
      'outdated',
      'rate_limited',
      'unavailable',
      'server',
      'forbidden',
      'source_disabled',
      'invalid_pairing_code',
      'validation_failed',
    ])
      expect(t(`error.${code}`)).not.toBe(`error.${code}`);
  });
});

describe('debug section', () => {
  it('lists census counts by platform and endpoint', () => {
    expect(
      censusRows({ 'twitter|graphql Bookmarks': 2, 'instagram|graphql X': 5, 'web|nope': 1 }),
    ).toEqual([
      ['instagram · graphql X', 5],
      ['twitter · graphql Bookmarks', 2],
    ]);
    expect(censusRows(null)).toEqual([]);
  });
});
