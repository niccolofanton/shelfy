import { describe, it, expect, vi } from 'vitest';
import { createAiSearchApi, readChatEvents, chatHistory } from '../../src/api/ai/search';
import { createHttp, ApiError } from '../../src/api/http';
import { apiPost } from '../fixtures';
const terminal = {
  tags: { general: ['design'], specific: ['lamp'] },
  keywords: ['brass'],
  remove: ['glass'],
  modelUsed: true,
};
const frame = (name: string, data: unknown) =>
  `event: ${name}\r\ndata: ${JSON.stringify(data)}\r\n\r\n`;
const answer = (result = terminal) =>
  frame('run', { runId: 'run-one' }) +
  ': heartbeat\r\n\r\n' +
  frame('token', { text: 'Café 🎧' }) +
  frame('result', result);
function sse(text: string, bytes = 5) {
  const encoded = new TextEncoder().encode(text);
  return new Response(
    new ReadableStream({
      start(c) {
        for (let i = 0; i < encoded.length; i += bytes) c.enqueue(encoded.slice(i, i + bytes));
        c.close();
      },
    }),
    { headers: { 'Content-Type': 'text/event-stream' } },
  );
}
const events = { on: vi.fn(() => () => {}) };
it('reads fragmented UTF-8, CRLF, comments and multiple JSON data lines', async () => {
  const seen: unknown[] = [];
  await readChatEvents(
    sse(': comment\r\n\r\nevent: token\r\ndata: {\r\ndata: "text": "Café 🎧"}\r\n\r\n', 1),
    (name, value) => seen.push({ name, value }),
  );
  expect(seen).toEqual([{ name: 'token', value: { text: 'Café 🎧' } }]);
});
it('maps tokens and allowlisted groups, sends scope/CSRF/cookie and selected provider without mutating preferences', async () => {
  const fetch = vi.fn(async (url: string) =>
    url.endsWith('/providers')
      ? Response.json([
          { id: 'custom', label: 'My provider', configured: true, models: { text: 'm' } },
        ])
      : url.endsWith('/settings')
        ? Response.json({ aiRouting: {} })
        : sse(answer()),
  );
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  await api.selectProvider('custom');
  const tokens: unknown[] = [];
  api.onToken((t) => tokens.push(t));
  const result = await api.chat([{ role: 'user', content: 'lamp' }], ['glass'], { source: 'web' });
  expect(result).toMatchObject({
    reply: 'Café 🎧',
    tagsToAdd: ['design', 'lamp'],
    tagsToRemove: ['glass'],
    tagGroups: { broad: ['design'], specific: ['lamp'], keywords: ['brass'] },
    modelUsed: true,
  });
  expect(tokens).toEqual([
    { start: true, runId: 'run-one' },
    { runId: 'run-one', token: 'Café 🎧' },
  ]);
  const [, options] = fetch.mock.calls.at(-1)! as unknown as [string, RequestInit];
  expect(options.credentials).toBe('same-origin');
  expect(options.headers).toMatchObject({ 'X-Shelfy-Client': 'web', Accept: 'text/event-stream' });
  expect(JSON.parse(options.body as string)).toMatchObject({
    scope: 'sites',
    providerId: 'custom',
    activeTags: ['glass'],
  });
  expect(fetch.mock.calls.filter((c) => c[0].endsWith('/chat'))).toHaveLength(1);
});
it('sends the displayed configured BYOK on an unset route without granting consent', async () => {
  const fetch = vi.fn(async (url: string) =>
    url.endsWith('/providers')
      ? Response.json([{ id: 'byok', label: 'My BYOK', configured: true, models: { text: 'm' } }])
      : url.endsWith('/settings')
        ? Response.json({ aiRouting: {} })
        : sse(
            answer({ ...terminal, modelUsed: false, replyCode: 'suggestions' } as typeof terminal),
          ),
  );
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  expect(await api.getProviders()).toEqual([{ id: 'byok', name: 'My BYOK', selected: true }]);
  expect((await api.chat([{ role: 'user', content: 'lamp' }])).modelUsed).toBe(false);
  const [, options] = fetch.mock.calls.at(-1)! as unknown as [string, RequestInit];
  expect(JSON.parse(options.body as string).providerId).toBe('byok');
  expect(
    fetch.mock.calls.filter(([url]) => !url.endsWith('/providers') && !url.endsWith('/settings')),
  ).toHaveLength(1);
});
it('prefers the supported operator when routing is unset and sends that displayed route', async () => {
  const fetch = vi.fn(async (url: string) =>
    url.endsWith('/providers')
      ? Response.json([
          { id: 'byok', label: 'My BYOK', configured: true, models: { text: 'm' } },
          { id: 'operator', label: 'Node', configured: true, models: { text: 'm' } },
        ])
      : url.endsWith('/settings')
        ? Response.json({ aiRouting: {} })
        : sse(answer()),
  );
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  expect((await api.getProviders()).find((p) => p.selected)?.id).toBe('operator');
  await api.chat([{ role: 'user', content: 'lamp' }]);
  const [, options] = fetch.mock.calls.at(-1)! as unknown as [string, RequestInit];
  expect(JSON.parse(options.body as string).providerId).toBe('operator');
});
it('falls back by replyCode and discards partial model prose', async () => {
  const fetch = vi.fn(async () =>
    sse(answer({ ...terminal, modelUsed: false, replyCode: 'suggestions' } as typeof terminal)),
  );
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  expect(await api.chat([{ role: 'user', content: 'lamp' }])).toMatchObject({
    reply: '',
    replyCode: 'suggestions',
    modelUsed: false,
    tagsToAdd: ['design', 'lamp'],
  });
});
describe('never repeats an interrupted or rejected POST', () => {
  for (const text of [
    frame('run', { runId: 'a' }),
    frame('run', { runId: 'a' }) + 'event: result\ndata: broken\n\n',
    frame('token', { text: 'unannounced' }),
    frame('run', { runId: 'a' }) + frame('result', {}),
  ]) {
    it(`rejects incomplete protocol ${text.length}`, async () => {
      const fetch = vi.fn(async () => sse(text));
      const api = createAiSearchApi(
        createHttp({ fetch: fetch as typeof globalThis.fetch }),
        events,
      );
      await expect(api.chat([{ role: 'user', content: 'lamp' }])).rejects.toBeInstanceOf(ApiError);
      expect(fetch).toHaveBeenCalledTimes(1);
    });
  }
  it('does not resubmit after a reauthentication refusal', async () => {
    const fetch = vi.fn(async () => Response.json({ code: 'reauth_required' }, { status: 403 }));
    const http = createHttp({ fetch: fetch as typeof globalThis.fetch });
    const reauth = vi.fn(async () => true);
    http.onReauthRequired(reauth);
    await expect(
      createAiSearchApi(http, events).chat([{ role: 'user', content: 'lamp' }]),
    ).rejects.toMatchObject({ code: 'reauth_required' });
    expect(fetch).toHaveBeenCalledTimes(1);
    expect(reauth).not.toHaveBeenCalled();
  });
});
it('aborts the stream on stop, requests account-scoped cancel and ignores an already-ended run', async () => {
  let controller: ReadableStreamDefaultController<Uint8Array>;
  const fetch = vi.fn(async (url: string, options: RequestInit) => {
    if (url.endsWith('/cancel')) return Response.json({ code: 'not_found' }, { status: 404 });
    const stream = new ReadableStream<Uint8Array>({
      start(c) {
        controller = c;
        c.enqueue(new TextEncoder().encode(frame('run', { runId: 'run-one' })));
      },
    });
    options.signal?.addEventListener('abort', () =>
      controller.error(new DOMException('Cancelled', 'AbortError')),
    );
    return new Response(stream, { headers: { 'Content-Type': 'text/event-stream' } });
  });
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  const started = new Promise<void>((resolve) => api.onToken(() => resolve()));
  const result = api.chat([{ role: 'user', content: 'lamp' }]);
  const rejected = expect(result).rejects.toMatchObject({ name: 'AbortError' });
  await started;
  await api.cancelChat();
  await rejected;
  expect(fetch.mock.calls.at(-1)?.[0]).toBe('/api/v1/search/chat/run-one/cancel');
  expect(await api.cancelChat()).toEqual({ ok: true });
});
it('maps ranked search with repeated tags, mode/scope/text, and cursor offset', async () => {
  const fetch = vi.fn(async (_url: string) =>
    Response.json({
      items: [apiPost({ key: 'p1' }), apiPost({ key: 'p2' })],
      total: 3,
      nextCursor: 'next',
    }),
  );
  fetch.mockResolvedValueOnce(
    Response.json({ items: [apiPost({ key: 'p1' })], total: 3, nextCursor: 'next' }),
  );
  const api = createAiSearchApi(createHttp({ fetch: fetch as typeof globalThis.fetch }), events);
  const result = await api.hybrid(['lamp', 'design'], 'brass', 'and', 1, 1, 'social');
  expect(result.total).toBe(3);
  expect(result.posts[0].id).toBe('p1');
  const first = new URL(fetch.mock.calls[0][0] as string, 'https://shelfy.test');
  expect(first.searchParams.getAll('tags')).toEqual(['lamp', 'design']);
  expect(first.searchParams.get('scope')).toBe('social');
  expect(first.searchParams.get('tagMode')).toBe('and');
  expect(first.searchParams.get('q')).toBe('brass');
  expect(String(fetch.mock.calls[1][0])).toContain('cursor=next');
});

it('bounds history to eight turns and 24k UTF-16 units without a broken surrogate', () => {
  const turns = Array.from({ length: 12 }, (_, i) => ({
    role: 'user' as const,
    content: String(i),
  }));
  expect(chatHistory(turns).map((t) => t.content)).toEqual([
    '4',
    '5',
    '6',
    '7',
    '8',
    '9',
    '10',
    '11',
  ]);
  expect(
    chatHistory([
      { role: 'assistant', content: 'old'.repeat(8000) },
      { role: 'user', content: 'latest' },
    ]),
  ).toEqual([{ role: 'user', content: 'latest' }]);
  const content = '🎧'.repeat(12001) + 'x';
  const result = chatHistory([{ role: 'user', content }])[0].content;
  expect(result.length).toBeLessThanOrEqual(24000);
  expect(result.endsWith('x')).toBe(true);
  expect(result.charCodeAt(0)).toBe(0xd83c);
});
