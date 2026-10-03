// Sender checks of the worker's router (sw/router.ts) and of the capture path
// (sw/capture.ts checkCaptureSender): who may send what, inside the extension and from the SPA.

import { describe, expect, it, vi } from 'vitest';
import { EXTERNAL, MSG } from '../src/shared/protocol';
import { checkCaptureSender } from '../src/sw/capture';
import {
  Router,
  isContentSender,
  isPageSender,
  isShelfySender,
  type Sender,
} from '../src/sw/router';

const ID = 'ckdhhkeliaagkhdgogjacajofoidkbem';
const ORIGIN = 'https://refs.niccolofanton.dev';

const content: Sender = {
  id: ID,
  url: 'https://www.instagram.com/someone/saved/all-posts/',
  origin: 'https://www.instagram.com',
  frameId: 0,
  tab: { id: 5, url: 'https://www.instagram.com/someone/saved/all-posts/' },
};
const panel: Sender = {
  id: ID,
  url: `chrome-extension://${ID}/panel.html`,
  origin: `chrome-extension://${ID}`,
};
const spa: Sender = { url: `${ORIGIN}/settings/connections`, origin: ORIGIN, tab: { id: 9 } };

describe('sender roles', () => {
  it('content scripts: this extension, in a tab', () => {
    expect(isContentSender(content, ID)).toBe(true);
    expect(isContentSender({ ...content, id: 'another-extension' }, ID)).toBe(false);
    expect(isContentSender(panel, ID)).toBe(false);
  });

  it('extension pages: this extension, at a chrome-extension:// URL of this extension', () => {
    expect(isPageSender(panel, ID)).toBe(true);
    // The panel opened in a tab is still an extension page, not a content script.
    const panelInTab = { ...panel, tab: { id: 12, url: panel.url } };
    expect(isPageSender(panelInTab, ID)).toBe(true);
    expect(isContentSender(panelInTab, ID)).toBe(false);
    expect(isPageSender(content, ID)).toBe(false);
    expect(
      isPageSender({ ...panel, url: 'chrome-extension://otherextensionid/panel.html' }, ID),
    ).toBe(false);
    expect(isPageSender({ ...panel, id: undefined }, ID)).toBe(false);
  });

  it('the SPA: exactly the Shelfy origin of the build', () => {
    expect(isShelfySender(spa, ORIGIN)).toBe(true);
    expect(isShelfySender({ origin: ORIGIN }, ORIGIN)).toBe(true);
    for (const sender of [
      { ...spa, origin: 'https://evil.example' },
      { ...spa, origin: 'https://refs.niccolofanton.dev.evil.example' },
      { ...spa, origin: 'http://refs.niccolofanton.dev' },
      { ...spa, url: 'https://evil.example/' },
      { id: 'another-extension', origin: 'chrome-extension://another-extension' },
      {},
    ])
      expect(isShelfySender(sender, ORIGIN), JSON.stringify(sender)).toBe(false);
  });
});

describe('checkCaptureSender', () => {
  const sender = { tabId: 5, frameId: 0, url: content.url, tabUrl: content.tab?.url };

  it('takes the top frame of a tab whose host matches the declared platform and the page URL', () => {
    expect(checkCaptureSender(sender, 'instagram', content.url!)).toEqual({ ok: true });
  });

  it('refuses subframes, non-tabs, other hosts and mismatched platforms', () => {
    expect(checkCaptureSender({ ...sender, frameId: 2 }, 'instagram', content.url!)).toEqual({
      ok: false,
      reason: 'not_a_top_frame',
    });
    expect(checkCaptureSender({ ...sender, tabId: undefined }, 'instagram', content.url!)).toEqual({
      ok: false,
      reason: 'not_a_top_frame',
    });
    expect(
      checkCaptureSender(
        { ...sender, url: 'https://evil.example/', tabUrl: 'https://evil.example/' },
        'instagram',
        content.url!,
      ),
    ).toEqual({ ok: false, reason: 'unsupported_host' });
    expect(checkCaptureSender(sender, 'twitter', content.url!)).toEqual({
      ok: false,
      reason: 'platform_host_mismatch',
    });
    expect(checkCaptureSender(sender, 'instagram', 'https://x.com/i/bookmarks')).toEqual({
      ok: false,
      reason: 'platform_host_mismatch',
    });
  });
});

describe('Router', () => {
  function router() {
    const onError = vi.fn();
    const r = new Router(ID, ORIGIN, onError)
      .internal(MSG.capture, 'content', async () => ({ ok: true, from: 'capture' }))
      .internal(MSG.stateGet, 'page', async () => ({ ok: true, from: 'state' }))
      .internal(MSG.queueFlush, 'page', async () => {
        throw new Error('broken');
      })
      .external(EXTERNAL.ping, async () => ({ ok: true, version: '0.2.0' }));
    return { r, onError };
  }

  async function internal(r: Router, message: unknown, sender: Sender) {
    const sendResponse = vi.fn();
    const async = r.onMessage(message, sender, sendResponse);
    await vi.waitFor(() => (async ? expect(sendResponse).toHaveBeenCalled() : undefined));
    return { async, response: sendResponse.mock.calls[0]?.[0] };
  }

  async function external(r: Router, message: unknown, sender: Sender) {
    const sendResponse = vi.fn();
    const async = r.onMessageExternal(message, sender, sendResponse);
    if (async) await vi.waitFor(() => expect(sendResponse).toHaveBeenCalled());
    return {
      async,
      response: sendResponse.mock.calls[0]?.[0],
      calls: sendResponse.mock.calls.length,
    };
  }

  it('routes internal messages only from the role their route names', async () => {
    const { r } = router();
    expect(await internal(r, { kind: MSG.capture }, content)).toEqual({
      async: true,
      response: { ok: true, from: 'capture' },
    });
    expect(await internal(r, { kind: MSG.stateGet }, panel)).toEqual({
      async: true,
      response: { ok: true, from: 'state' },
    });
    // A content script cannot read the panel state; the panel cannot inject captures.
    expect((await internal(r, { kind: MSG.stateGet }, content)).async).toBe(false);
    expect((await internal(r, { kind: MSG.capture }, panel)).async).toBe(false);
    expect((await internal(r, { kind: 'shelfy/unknown' }, panel)).async).toBe(false);
    expect((await internal(r, 'not a message', panel)).async).toBe(false);
  });

  it('answers a failing handler with a code and reports the error', async () => {
    const { r, onError } = router();
    expect(await internal(r, { kind: MSG.queueFlush }, panel)).toEqual({
      async: true,
      response: { ok: false, code: 'internal' },
    });
    expect(onError).toHaveBeenCalledWith(MSG.queueFlush, expect.any(Error));
  });

  it('answers the SPA of the Shelfy origin, and nobody else (C9)', async () => {
    const { r } = router();
    expect(await external(r, { type: 'shelfy.ping' }, spa)).toMatchObject({
      async: true,
      response: { ok: true, version: '0.2.0' },
    });
    expect(
      await external(r, { type: 'shelfy.ping' }, { ...spa, origin: 'https://evil.example' }),
    ).toEqual({
      async: false,
      response: undefined,
      calls: 0,
    });
    expect(await external(r, { type: 'shelfy.pair', code: 'x' }, spa)).toMatchObject({
      response: { ok: false, code: 'bad_request' },
    });
    expect(
      await external(
        r,
        {
          type: 'shelfy.sync.start',
          target: { platform: 'instagram' },
          expectedAccountId: 'A',
          expectedTokenId: 'install-A',
        },
        spa,
      ),
    ).toMatchObject({
      response: { ok: false, code: 'unsupported' },
    });
  });

  it('refuses to register a route twice', () => {
    const { r } = router();
    expect(() => r.internal(MSG.capture, 'content', async () => null)).toThrow(
      /already registered/,
    );
    expect(() => r.external(EXTERNAL.ping, async () => null)).toThrow(/already registered/);
  });
});
