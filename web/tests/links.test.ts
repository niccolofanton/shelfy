// The web client's links (web/src/api/links.ts) on `POST /api/v1/links`
// (contract C7; P2-07). The route itself lands with P2-11 (EXECUTION.md
// assumption); this client is coded against the contract with a fake server,
// the same way P2-06 codes its extension against C1–C5.
import { describe, it, expect, vi } from 'vitest';
import { createLinksApi } from '../src/api/links';
import type { Http } from '../src/api/http';
import { ApiError } from '../src/api/http';

function json(body: unknown, status = 201): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function fakeHttp(answer: Response | Error) {
  const sent: { method: string; path: string; body: unknown }[] = [];
  const http: Http = {
    get: vi.fn() as Http['get'],
    send: vi.fn(async (method: string, path: string, body?: unknown) => {
      sent.push({ method, path, body });
      if (answer instanceof Error) throw answer;
      return answer;
    }) as Http['send'],
    onUnauthorized: () => () => {},
    sessionEnded: () => {},
    onReauthRequired: () => () => {},
    reauthenticate: () => Promise.resolve(false),
  };
  return { http, sent };
}

describe('createLinksApi', () => {
  it('posts the URL, with note and tags defaulted to null', async () => {
    const { http, sent } = fakeHttp(json({ key: 'ig_1', platform: 'instagram', created: true }));
    const result = await createLinksApi(http).create('https://www.instagram.com/p/C0ffee/');
    expect(sent).toEqual([
      {
        method: 'POST',
        path: '/api/v1/links',
        body: { url: 'https://www.instagram.com/p/C0ffee/', note: null, tags: null },
      },
    ]);
    expect(result).toEqual({ key: 'ig_1', platform: 'instagram', created: true });
  });

  it('passes a note and tags through', async () => {
    const { http, sent } = fakeHttp(json({ key: 'web_2', platform: 'web', created: true }));
    await createLinksApi(http).create('https://example.test/a', {
      note: 'from the share sheet',
      tags: ['inspo'],
    });
    expect(sent[0].body).toEqual({
      url: 'https://example.test/a',
      note: 'from the share sheet',
      tags: ['inspo'],
    });
  });

  it('reports created: false for an already-saved key, without throwing', async () => {
    const { http } = fakeHttp(json({ key: 'x_2', platform: 'twitter', created: false }, 200));
    const result = await createLinksApi(http).create('https://x.com/studio/status/2');
    expect(result.created).toBe(false);
  });

  it('propagates a failed request as-is (the /share page maps it to a message)', async () => {
    const { http } = fakeHttp(new ApiError(404, 'not_found'));
    await expect(createLinksApi(http).create('https://example.test/a')).rejects.toThrow(ApiError);
  });
});
