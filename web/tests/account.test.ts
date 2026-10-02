// The web client's account (web/src/api/account.ts) on `/api/v1/me/*`.
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { createAccountApi } from '../src/api/account';
import type { EventStream, ServerEventData, ServerEventName } from '../src/api/events';
import type { Http } from '../src/api/http';
import { webCapabilities, createHttpClient, WEB_CAPABILITIES } from '../src/api/httpClient';
import { OWNER } from './authFakes';

vi.mock('../src/auth/passkeys', () => ({
  createPasskey: vi.fn(async () => ({ id: 'new', rawId: 'new', type: 'public-key', response: {} })),
}));

function json(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { 'Content-Type': 'application/json' },
  });
}

function fakeHttp(answers: Record<string, unknown> = {}) {
  const sent: { method: string; path: string; body: unknown }[] = [];
  const sessionEnded = vi.fn();
  const http: Http = {
    get: vi.fn(async (path: string) => answers[`GET ${path}`]) as Http['get'],
    send: vi.fn(async (method: string, path: string, body?: unknown) => {
      sent.push({ method, path, body });
      const answer = answers[`${method} ${path}`];
      return answer === undefined ? new Response(null, { status: 204 }) : json(answer);
    }) as Http['send'],
    onUnauthorized: () => () => {},
    sessionEnded,
    onReauthRequired: () => () => {},
  };
  return { http, sent, sessionEnded };
}

function fakeEvents() {
  const listeners = new Map<string, ((data: unknown) => void)[]>();
  return {
    on<N extends ServerEventName>(name: N, listener: (data: ServerEventData<N>) => void) {
      const list = listeners.get(name) ?? [];
      list.push(listener as (data: unknown) => void);
      listeners.set(name, list);
      return () =>
        listeners.set(
          name,
          (listeners.get(name) ?? []).filter((l) => l !== listener),
        );
    },
    emit(name: string, data: unknown) {
      for (const listener of listeners.get(name) ?? []) listener(data);
    },
  };
}

const PASSKEY_START = {
  ceremonyId: 'c1',
  publicKey: {
    rp: { id: 'localhost', name: 'Shelfy' },
    user: { id: 'AQID', name: 'o@x.test', displayName: 'o@x.test' },
    challenge: 'AAEC',
    pubKeyCredParams: [{ type: 'public-key', alg: -7 }],
  },
};

let now = 0;
beforeEach(() => {
  now = 1_000;
});

describe('account', () => {
  it('describes the signed-in user and how they can confirm who they are', () => {
    const account = createAccountApi(fakeHttp().http, OWNER, { events: fakeEvents() });
    expect(account.profile).toEqual({ id: 'u1', email: 'o@x.test', role: 'owner', createdAt: 0 });
    expect(account.signIn).toEqual({ passkeys: true, emailLink: true });
  });

  it('records consent and remembers it', async () => {
    const consent = {
      disclaimerVersion: '2026-06-07',
      disclaimerAcceptedAt: 5,
      privacyVersion: '1',
      privacyAcceptedAt: 5,
    };
    const { http, sent } = fakeHttp({ 'POST /api/v1/me/consent': consent });
    const account = createAccountApi(http, OWNER, { events: fakeEvents() });
    expect(account.consent().disclaimerVersion).toBeNull();
    await account.acceptConsent({ disclaimer: '2026-06-07', privacy: '1' });
    expect(sent[0].body).toEqual({ disclaimerVersion: '2026-06-07', privacyVersion: '1' });
    expect(account.consent()).toEqual(consent);
  });

  it('adds a passkey: options first, then the authenticator and the label', async () => {
    const passkey = { id: 7, label: 'Chrome on macOS', createdAt: 1, lastUsedAt: null };
    const { http, sent } = fakeHttp({
      'POST /api/v1/me/passkeys/start': PASSKEY_START,
      'POST /api/v1/me/passkeys': passkey,
    });
    const account = createAccountApi(http, OWNER, { events: fakeEvents(), now: () => now });
    const draft = await account.preparePasskey();
    expect(sent.map((s) => s.path)).toEqual(['/api/v1/me/passkeys/start']);
    await expect(draft.create('  Chrome on macOS ')).resolves.toEqual(passkey);
    expect(sent[1]).toMatchObject({
      method: 'POST',
      path: '/api/v1/me/passkeys',
      body: { ceremonyId: 'c1', label: 'Chrome on macOS', credential: { id: 'new' } },
    });
  });

  it('asks for new options when a draft is old or spent', async () => {
    const { http, sent } = fakeHttp({
      'POST /api/v1/me/passkeys/start': PASSKEY_START,
      'POST /api/v1/me/passkeys': { id: 1, label: null, createdAt: 1, lastUsedAt: null },
    });
    const account = createAccountApi(http, OWNER, { events: fakeEvents(), now: () => now });
    const draft = await account.preparePasskey();
    now += 5 * 60_000;
    await draft.create('');
    expect(sent.map((s) => s.path)).toEqual([
      '/api/v1/me/passkeys/start',
      '/api/v1/me/passkeys/start',
      '/api/v1/me/passkeys',
    ]);
    expect(sent[2].body).not.toHaveProperty('label');
    await draft.create();
    expect(sent.filter((s) => s.path === '/api/v1/me/passkeys/start')).toHaveLength(3);
  });

  it('signs this browser out when its own session ends, not for another one', async () => {
    const sessions = {
      items: [
        { id: 'aa', current: true, createdAt: 1, lastSeenAt: 2, expiresAt: 3, userAgent: null },
        { id: 'bb', current: false, createdAt: 1, lastSeenAt: 2, expiresAt: 3, userAgent: null },
      ],
    };
    const { http, sent, sessionEnded } = fakeHttp({ 'GET /api/v1/me/sessions': sessions });
    const account = createAccountApi(http, OWNER, { events: fakeEvents() });
    await account.listSessions();
    await account.endSession('bb');
    expect(sessionEnded).not.toHaveBeenCalled();
    await account.endSession('aa');
    expect(sessionEnded).toHaveBeenCalledTimes(1);
    expect(sent.map((s) => `${s.method} ${s.path}`)).toEqual([
      'DELETE /api/v1/me/sessions/bb',
      'DELETE /api/v1/me/sessions/aa',
    ]);
  });

  it('signs out even when the server does not answer', async () => {
    const { http, sessionEnded } = fakeHttp();
    (http.send as ReturnType<typeof vi.fn>).mockRejectedValueOnce(new Error('offline'));
    const account = createAccountApi(http, OWNER, { events: fakeEvents() });
    await expect(account.signOut()).rejects.toThrow('offline');
    expect(sessionEnded).toHaveBeenCalledTimes(1);
  });

  it('creates a token, whose value it hands over once', async () => {
    const created = {
      token: 'shx_secret',
      apiToken: {
        id: 't1',
        kind: 'shortcut',
        label: 'iPhone',
        scopes: ['links:create'],
        createdAt: 1,
        lastUsedAt: null,
        expiresAt: null,
      },
    };
    const { http, sent } = fakeHttp({ 'POST /api/v1/me/tokens': created });
    const account = createAccountApi(http, OWNER, { events: fakeEvents() });
    const token = await account.createToken('shortcut', ' iPhone ');
    expect(sent[0].body).toEqual({ kind: 'shortcut', label: 'iPhone' });
    expect(token.value).toBe('shx_secret');
    expect(token.token).toMatchObject({ id: 't1', kind: 'shortcut', scopes: ['links:create'] });
  });

  it('saves only the settings it is given', async () => {
    const settings = {
      language: 'en',
      archiveAssetTypes: { thumbnail: true, image: false, video: true },
    };
    const { http, sent } = fakeHttp({ 'PUT /api/v1/me/settings': settings });
    const account = createAccountApi(http, OWNER, { events: fakeEvents() });
    await expect(account.updateSettings({ language: 'en' })).resolves.toEqual(settings);
    expect(sent[0].body).toEqual({ language: 'en' });
  });

  it('says when the storage was counted again', () => {
    const events = fakeEvents();
    const account = createAccountApi(fakeHttp().http, OWNER, { events });
    const listener = vi.fn();
    const off = account.onUsageChanged(listener);
    const job = (kind: string, state: string) => ({
      id: 1,
      kind,
      state,
      progress: null,
      stage: null,
      postKey: null,
      errorCode: null,
    });
    events.emit('job.updated', job('usage.recompute', 'running'));
    events.emit('job.updated', job('migrate', 'succeeded'));
    expect(listener).not.toHaveBeenCalled();
    events.emit('job.updated', job('usage.recompute', 'succeeded'));
    expect(listener).toHaveBeenCalledTimes(1);
    off();
    events.emit('job.updated', job('usage.recompute', 'succeeded'));
    expect(listener).toHaveBeenCalledTimes(1);
  });
});

describe('web capabilities', () => {
  it('turn on the account and its Settings for a signed-in user, and nothing else yet', () => {
    expect(webCapabilities(null)).toBe(WEB_CAPABILITIES);
    expect(webCapabilities(OWNER)).toEqual({ ...WEB_CAPABILITIES, account: true, settings: true });
  });

  it('give a client made for a user its account', () => {
    const { http } = fakeHttp();
    const events = fakeEvents() as unknown as EventStream;
    const client = createHttpClient(http, { me: OWNER, events });
    expect(client.capabilities.account).toBe(true);
    expect(client.account?.profile.email).toBe('o@x.test');
    expect(createHttpClient(http, { events }).account).toBeUndefined();
  });
});
