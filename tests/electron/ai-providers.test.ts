import { afterAll, beforeAll, describe, expect, it, vi } from 'vitest';
import { mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'fs';
import { tmpdir } from 'os';
import { join } from 'path';

const root = mkdtempSync(join(tmpdir(), 'shelfy-ai-provider-'));
const data = join(root, 'shelfy');
const pi = join(root, 'pi');
const keychain = vi.hoisted(() => ({ secret: null as string | null }));
vi.mock('electron', () => ({ app: { getPath: () => data } }));
vi.mock('child_process', () => ({
  execFileSync: vi.fn((command: string, args?: string[]) => {
    if (command === '/usr/bin/security') {
      if (args?.[0] === 'add-generic-password') {
        keychain.secret = args[args.indexOf('-w') + 1];
        return '';
      }
      if (!keychain.secret) throw new Error('No Keychain item');
      return `${keychain.secret}\n`;
    }
    return JSON.stringify({
      BackendState: 'Running',
      Peer: { bonsai: { Online: true, TailscaleIPs: ['100.64.0.1'] } },
    });
  }),
}));

const providers = await import('../../electron/ai-providers');

beforeAll(() => {
  mkdirSync(data);
  mkdirSync(pi);
  process.env.SHELFY_PI_AGENT_DIR = pi;
  writeFileSync(
    join(pi, 'settings.json'),
    JSON.stringify({
      defaultProvider: 'bonsai2',
      defaultModel: 'remote-model.gguf',
      llamaSettings: {
        servers: [{ id: 'bonsai2', name: 'Bonsai', url: 'http://100.64.0.1:8080' }],
      },
    }),
  );
  writeFileSync(
    join(pi, 'auth.json'),
    JSON.stringify({ bonsai2: { type: 'api_key', key: 'test-secret' } }),
  );
});

afterAll(() => {
  delete process.env.SHELFY_PI_AGENT_DIR;
  delete process.env.ORNITH_API_KEY;
  keychain.secret = null;
  rmSync(root, { recursive: true, force: true });
});

describe('AI search providers', () => {
  it('discovers Pi without exposing credentials to the renderer', () => {
    const list = providers.listSearchProviders();
    expect(list).toEqual([
      { id: 'local', name: 'Locale', selected: true },
      { id: 'pi:bonsai2', name: 'Bonsai', selected: false, vision: false },
    ]);
    expect(JSON.stringify(list)).not.toContain('test-secret');
  });

  it('persists the toggle and builds an authenticated standard chat endpoint', () => {
    providers.selectSearchProvider('pi:bonsai2');
    expect(providers.listSearchProviders()[1].selected).toBe(true);
    expect(readFileSync(join(data, 'ai-providers.json'), 'utf8')).not.toContain('test-secret');
    const selected = providers.selectedRemoteProvider();
    expect(selected).not.toBeNull();
    expect(providers.chatEndpoint(selected!)).toEqual({
      url: 'http://100.64.0.1:8080/v1/chat/completions',
      model: 'remote-model.gguf',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer test-secret' },
    });
    providers.selectSearchProvider('local');
    expect(providers.selectedRemoteProvider()).toBeNull();
  });

  it('falls back to local when the saved Pi provider disappears', () => {
    providers.selectSearchProvider('pi:bonsai2');
    const settingsFile = join(pi, 'settings.json');
    const settings = readFileSync(settingsFile, 'utf8');
    try {
      writeFileSync(settingsFile, JSON.stringify({ llamaSettings: { servers: [] } }));
      expect(providers.listSearchProviders()).toEqual([
        { id: 'local', name: 'Locale', selected: true },
      ]);
      expect(providers.selectedRemoteProvider()).toBeNull();
    } finally {
      writeFileSync(settingsFile, settings);
      providers.selectSearchProvider('local');
    }
  });

  it('uses a Pi key reference for an OpenAI vision model without copying its secret', () => {
    writeFileSync(
      join(data, 'ai-providers.json'),
      JSON.stringify({
        searchProvider: 'custom:ornith-vision',
        providers: [
          {
            id: 'ornith-vision',
            name: 'Ornith Vision',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'qwen3.8-27b',
            apiKeyPiProvider: 'bonsai2',
            vision: true,
          },
        ],
      }),
    );
    expect(providers.listSearchProviders()).toContainEqual({
      id: 'custom:ornith-vision',
      name: 'Ornith Vision',
      selected: true,
      vision: true,
    });
    expect(providers.selectedVisionProvider()?.model).toBe('qwen3.8-27b');
    expect(providers.chatEndpoint(providers.selectedVisionProvider()!)).toEqual({
      url: 'http://100.64.0.1:8080/v1/chat/completions',
      model: 'qwen3.8-27b',
      headers: { 'Content-Type': 'application/json', Authorization: 'Bearer test-secret' },
    });
    expect(readFileSync(join(data, 'ai-providers.json'), 'utf8')).not.toContain('test-secret');
  });

  it('offers both Ornith model IDs via one environment secret and defaults to Qwen vision', () => {
    process.env.ORNITH_API_KEY = 'test-ornith-secret';
    writeFileSync(
      join(data, 'ai-providers.json'),
      JSON.stringify({
        searchProvider: 'custom:ornith-qwen',
        visionProvider: 'custom:ornith-qwen',
        providers: [
          {
            id: 'ornith-qwen',
            name: 'Qwen Vision',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'qwen3.8-27b',
            apiKeyEnv: 'ORNITH_API_KEY',
            vision: true,
          },
          {
            id: 'ornith-text',
            name: 'Ornith Text',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'ornith-1.5-35b-a3b',
            apiKeyEnv: 'ORNITH_API_KEY',
            vision: false,
          },
        ],
      }),
    );
    expect(providers.listSearchProviders()).toEqual([
      { id: 'local', name: 'Locale', selected: false },
      { id: 'custom:ornith-qwen', name: 'Qwen Vision', selected: true, vision: true },
      { id: 'custom:ornith-text', name: 'Ornith Text', selected: false, vision: false },
    ]);
    expect(providers.chatEndpoint(providers.selectedVisionProvider()!).model).toBe('qwen3.8-27b');
    expect(providers.chatEndpoint(providers.selectedVisionProvider()!).headers.Authorization).toBe(
      'Bearer test-ornith-secret',
    );
    providers.selectSearchProvider('custom:ornith-text');
    expect(providers.selectedVisionProvider()?.model).toBe('qwen3.8-27b');
    expect(providers.chatEndpoint(providers.selectedRemoteProvider()!).model).toBe(
      'ornith-1.5-35b-a3b',
    );
    providers.selectSearchProvider('local');
    expect(providers.selectedVisionProvider()).toBeNull();
    delete process.env.ORNITH_API_KEY;
    expect(providers.listSearchProviders()).toEqual([
      { id: 'local', name: 'Locale', selected: true },
    ]);
  });

  it.skipIf(process.platform !== 'darwin')(
    'reads the named macOS Keychain secret when the environment is absent',
    () => {
      keychain.secret = 'keychain-ornith-secret';
      try {
        expect(providers.listSearchProviders().map((provider) => provider.id)).toEqual([
          'local',
          'custom:ornith-qwen',
          'custom:ornith-text',
        ]);
        providers.selectSearchProvider('custom:ornith-qwen');
        expect(
          providers.chatEndpoint(providers.selectedVisionProvider()!).headers.Authorization,
        ).toBe('Bearer keychain-ornith-secret');
      } finally {
        keychain.secret = null;
      }
    },
  );

  it.skipIf(process.platform !== 'darwin')(
    'saves editable providers and a named Keychain secret without exposing it',
    () => {
      const result = providers.saveProviderSettings({
        searchProvider: 'custom:ornith-text',
        visionProvider: 'custom:ornith-qwen',
        providers: [
          {
            id: 'ornith-qwen',
            name: 'Qwen Vision',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'qwen3.8-27b',
            apiKeyEnv: 'ORNITH_API_KEY',
            vision: true,
          },
          {
            id: 'ornith-text',
            name: 'Ornith Text',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'ornith-1.5-35b-a3b',
            apiKeyEnv: 'ORNITH_API_KEY',
            vision: false,
          },
        ],
        secrets: { ORNITH_API_KEY: 'saved-only-in-keychain' },
      });
      expect(result.searchProvider).toBe('custom:ornith-text');
      expect(result.visionProvider).toBe('custom:ornith-qwen');
      expect(result.providers).toHaveLength(2);
      expect(result.providers.every((item) => item.secretConfigured)).toBe(true);
      expect(JSON.stringify(result)).not.toContain('saved-only-in-keychain');
      expect(readFileSync(join(data, 'ai-providers.json'), 'utf8')).not.toContain(
        'saved-only-in-keychain',
      );
      expect(providers.selectedRemoteProvider()?.model).toBe('ornith-1.5-35b-a3b');
      expect(providers.selectedVisionProvider()?.model).toBe('qwen3.8-27b');
      process.env.ORNITH_API_KEY = 'stale-environment-key';
      expect(providers.selectedRemoteProvider()?.apiKey).toBe('saved-only-in-keychain');
      delete process.env.ORNITH_API_KEY;
      keychain.secret = null;
    },
  );

  it('rejects invalid provider edits before changing the saved configuration', () => {
    const before = readFileSync(join(data, 'ai-providers.json'), 'utf8');
    expect(() =>
      providers.saveProviderSettings({
        searchProvider: 'custom:bad',
        visionProvider: '',
        providers: [
          {
            id: 'bad',
            name: 'Bad',
            baseUrl: 'http://user:password@example.com/v1',
            model: 'model',
            vision: false,
          },
        ],
      }),
    ).toThrow('URL del provider non valida');
    expect(readFileSync(join(data, 'ai-providers.json'), 'utf8')).toBe(before);
  });

  it('serializes remote requests across model switches', async () => {
    const releaseFirst = await providers.acquireRemoteRequest();
    let acquiredSecond = false;
    const second = providers.acquireRemoteRequest().then((release) => {
      acquiredSecond = true;
      release();
    });
    await Promise.resolve();
    expect(acquiredSecond).toBe(false);
    releaseFirst();
    await second;
    expect(acquiredSecond).toBe(true);
  });

  it('cancels a queued remote request without letting the next model overlap', async () => {
    const releaseFirst = await providers.acquireRemoteRequest();
    const controller = new AbortController();
    const cancelled = providers.acquireRemoteRequest(controller.signal);
    controller.abort();
    await expect(cancelled).rejects.toMatchObject({ name: 'AbortError' });
    let acquiredNext = false;
    const next = providers.acquireRemoteRequest().then((release) => {
      acquiredNext = true;
      release();
    });
    await Promise.resolve();
    expect(acquiredNext).toBe(false);
    releaseFirst();
    await next;
    expect(acquiredNext).toBe(true);
  });

  it('rejects HTTP endpoints without a verified Tailscale peer', () => {
    writeFileSync(
      join(data, 'ai-providers.json'),
      JSON.stringify({
        searchProvider: 'custom:unsafe',
        providers: [{ id: 'unsafe', name: 'Unsafe', baseUrl: 'http://example.com', model: 'm' }],
      }),
    );
    expect(providers.listSearchProviders()).toEqual([
      { id: 'local', name: 'Locale', selected: true },
      { id: 'pi:bonsai2', name: 'Bonsai', selected: false, vision: false },
    ]);
    expect(providers.selectedRemoteProvider()).toBeNull();
  });
  it('routes to a configured node only after it answers, never silently to local', async () => {
    process.env.ORNITH_API_KEY = 'test-ornith-secret';
    writeFileSync(
      join(data, 'ai-providers.json'),
      JSON.stringify({
        searchProvider: 'custom:ornith-qwen',
        providers: [
          {
            id: 'ornith-qwen',
            name: 'Qwen Vision',
            baseUrl: 'http://100.64.0.1:8080/v1',
            model: 'qwen3.8-27b',
            apiKeyEnv: 'ORNITH_API_KEY',
            vision: true,
          },
        ],
      }),
    );
    const originalFetch = globalThis.fetch;
    try {
      // Not probed yet: work waits instead of falling back to the local model.
      expect(providers.aiRoute('vision')).toEqual({ mode: 'blocked', name: 'Qwen Vision' });
      expect(providers.getRemoteStatus()).toMatchObject({ configured: true, reachable: null });

      const fetchMock = vi.fn(async () => new Response('{"data":[]}', { status: 200 }));
      globalThis.fetch = fetchMock as typeof fetch;
      expect(await providers.probeRemote()).toMatchObject({ configured: true, reachable: true });
      const [url, init] = fetchMock.mock.calls[0] as unknown as [string, RequestInit];
      expect(url).toBe('http://100.64.0.1:8080/v1/models');
      expect(init.headers).toEqual({ Authorization: 'Bearer test-ornith-secret' });
      const route = providers.aiRoute('vision');
      expect(route.mode === 'remote' && route.provider.model).toBe('qwen3.8-27b');

      globalThis.fetch = vi.fn(async () => {
        throw new TypeError('fetch failed');
      }) as typeof fetch;
      expect(await providers.probeRemote()).toMatchObject({ reachable: false });
      expect(providers.aiRoute('search').mode).toBe('blocked');

      // The user opts into local models for the session…
      expect(providers.useLocalModelsForSession().localOverride).toBe(true);
      expect(providers.aiRoute('vision')).toEqual({ mode: 'local' });
      // …until they explicitly pick the remote node again.
      providers.selectSearchProvider('custom:ornith-qwen');
      expect(providers.getRemoteStatus().localOverride).toBe(false);
      expect(providers.aiRoute('vision').mode).toBe('blocked');

      providers.selectSearchProvider('local');
      expect(providers.aiRoute('vision')).toEqual({ mode: 'local' });
      expect(providers.getRemoteStatus().configured).toBe(false);
    } finally {
      globalThis.fetch = originalFetch;
      delete process.env.ORNITH_API_KEY;
    }
  });

  it('does not treat a plain-HTTP non-tailnet host as a configured node', () => {
    writeFileSync(
      join(data, 'ai-providers.json'),
      JSON.stringify({
        searchProvider: 'custom:unsafe',
        providers: [{ id: 'unsafe', name: 'Unsafe', baseUrl: 'http://example.com', model: 'm' }],
      }),
    );
    expect(providers.getRemoteStatus().configured).toBe(false);
    expect(providers.aiRoute('search')).toEqual({ mode: 'local' });
  });
});
