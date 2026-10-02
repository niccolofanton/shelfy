import { afterAll, describe, expect, it, vi } from 'vitest';

vi.mock('electron', () => ({ app: { getPath: () => '/tmp' } }));
vi.mock('../../electron/db', () => ({ getTagStats: () => [] }));
vi.mock('../../electron/ai-providers', () => ({
  acquireRemoteRequest: async () => () => {},
  aiRoute: () => ({ mode: 'remote', provider: { id: 'pi:bonsai2' } }),
  onRemoteStatusChange: () => () => {},
  probeRemote: async () => ({ reachable: true }),
  chatEndpoint: () => ({
    url: 'http://127.0.0.1:8787/v1/chat/completions',
    headers: { 'Content-Type': 'application/json', Authorization: 'Bearer test-secret' },
    model: 'remote-model.gguf',
  }),
}));

const { chatSearch } = await import('../../electron/analyzer');
const originalFetch = globalThis.fetch;

afterAll(() => {
  globalThis.fetch = originalFetch;
});

describe('remote image search chat', () => {
  it('uses the remote provider and consumes its streamed search filters', async () => {
    const content = 'Cerco immagini. [[KEYWORDS]]architettura[[/KEYWORDS]]';
    const event = `data: ${JSON.stringify({ choices: [{ delta: { content } }] })}\n\ndata: [DONE]\n\n`;
    const fetchMock = vi.fn(
      async () =>
        new Response(event, {
          headers: { 'content-type': 'text/event-stream' },
        }),
    );
    globalThis.fetch = fetchMock as typeof fetch;
    const tokens: string[] = [];

    const result = await chatSearch(
      [{ role: 'user', content: 'foto di architettura' }],
      [],
      (token) => tokens.push(token),
    );

    expect(result.modelUsed).toBe(true);
    expect(result.keywordsToAdd).toEqual(['architettura']);
    expect(tokens.join('')).toBe('Cerco immagini. ');
    const [url, request] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('http://127.0.0.1:8787/v1/chat/completions');
    expect(request.redirect).toBe('error');
    expect(request.headers).toEqual({
      'Content-Type': 'application/json',
      Authorization: 'Bearer test-secret',
    });
    expect(JSON.parse(request.body as string).model).toBe('remote-model.gguf');
  });
});
