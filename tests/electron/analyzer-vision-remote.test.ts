import { afterAll, describe, expect, it, vi } from 'vitest';

const remote = vi.hoisted(() => ({ reachable: true }));
vi.mock('electron', () => ({ app: { getPath: () => '/tmp' } }));
vi.mock('../../electron/db', () => ({}));
vi.mock('../../electron/ai-providers', () => ({
  acquireRemoteRequest: async () => () => {},
  aiRoute: () => ({ mode: 'local' }),
  onRemoteStatusChange: () => () => {},
  probeRemote: async () => ({ reachable: remote.reachable }),
  chatEndpoint: (provider: { model: string }) => ({
    url: 'http://100.64.0.1:8080/v1/chat/completions',
    headers: { 'Content-Type': 'application/json', Authorization: 'Bearer test-secret' },
    model: provider.model,
  }),
}));

const { runInference, analyzeFrames } = await import('../../electron/analyzer');
const originalFetch = globalThis.fetch;

afterAll(() => {
  globalThis.fetch = originalFetch;
});

describe('remote vision inference', () => {
  it('sends the exact model ID and image to the authenticated OpenAI endpoint', async () => {
    const fetchMock = vi.fn(async () =>
      Response.json({ choices: [{ message: { content: '{"description":"test"}' } }] }),
    );
    globalThis.fetch = fetchMock as typeof fetch;
    const result = await runInference(
      ['data:image/jpeg;base64,AAAA'],
      'caption',
      [],
      undefined,
      undefined,
      'social',
      {
        id: 'custom:ornith-vision',
        name: 'Ornith Vision',
        baseUrl: 'http://100.64.0.1:8080',
        model: 'qwen3.8-27b',
        vision: true,
      },
    );
    expect(result.description).toBe('test');
    const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
    expect(url).toBe('http://100.64.0.1:8080/v1/chat/completions');
    expect(init.redirect).toBe('error');
    expect(init.headers).toEqual({
      'Content-Type': 'application/json',
      Authorization: 'Bearer test-secret',
    });
    const body = JSON.parse(init.body as string);
    expect(body.model).toBe('qwen3.8-27b');
    expect(body.chat_template_kwargs).toEqual({ enable_thinking: false });
    expect(body.messages[1].content[1]).toEqual({
      type: 'image_url',
      image_url: { url: 'data:image/jpeg;base64,AAAA' },
    });
  });

  it('retries an empty remote stream without streaming on the same model', async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(new Response('data: [DONE]\n\n', { status: 200 }))
      .mockResolvedValueOnce(
        Response.json({ choices: [{ message: { content: '{"description":"recovered"}' } }] }),
      );
    globalThis.fetch = fetchMock as typeof fetch;

    const result = await analyzeFrames(
      ['data:image/jpeg;base64,AAAA'],
      '',
      [],
      undefined,
      undefined,
      () => {},
      'social',
      {
        id: 'custom:ornith-vision',
        name: 'Ornith Vision',
        baseUrl: 'http://100.64.0.1:8080',
        model: 'qwen3.8-27b',
        vision: true,
      },
    );

    expect(result.description).toBe('recovered');
    expect(result.modelUsed).toBe('Ornith Vision');
    expect(fetchMock).toHaveBeenCalledTimes(2);
    const firstBody = JSON.parse(fetchMock.mock.calls[0][1].body);
    const retryBody = JSON.parse(fetchMock.mock.calls[1][1].body);
    expect(firstBody.stream).toBe(true);
    expect(retryBody.stream).toBeUndefined();
    expect(retryBody.model).toBe('qwen3.8-27b');
  });

  it('parks the job when the node stops answering instead of using the local model', async () => {
    const fetchMock = vi.fn(async () => {
      throw new TypeError('fetch failed');
    });
    globalThis.fetch = fetchMock as typeof fetch;
    remote.reachable = false;
    try {
      await expect(
        analyzeFrames(
          ['data:image/jpeg;base64,AAAA'],
          '',
          [],
          undefined,
          undefined,
          undefined,
          'web',
          {
            id: 'custom:ornith-vision',
            name: 'Ornith Vision',
            baseUrl: 'http://100.64.0.1:8080',
            model: 'qwen3.8-27b',
            vision: true,
          },
        ),
      ).rejects.toThrow('REMOTE_UNAVAILABLE');
      // Only the remote attempt: no local llama-server fallback was tried.
      expect(fetchMock).toHaveBeenCalledTimes(1);
    } finally {
      remote.reachable = true;
    }
  });
});
