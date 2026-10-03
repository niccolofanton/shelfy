// Pairing (C2, C9) and the extension config (C3) against the fake Shelfy API.

import { describe, expect, it } from 'vitest';
import { DEFAULT_CONFIG, parseConfig } from '../src/sw/contracts';
import { installLabel, pair } from '../src/sw/pairing';
import { KEYS } from '../src/sw/settings';
import { compareVersions, isBelowMinVersion } from '../src/shared/version';
import { T0, harness, type Harness } from './helpers';

const deps = (h: Harness, paired: () => void = () => undefined) => ({
  api: h.client,
  store: h.store,
  version: '0.2.0',
  label: 'Chrome on macOS',
  now: () => h.clock.now,
  paired,
});

describe('pair', () => {
  it('exchanges a code for a token, stored only in the worker storage', async () => {
    const h = harness();
    const code = h.api.issuePairingCode();
    let pairedCalls = 0;
    expect(
      await pair(
        code,
        deps(h, () => (pairedCalls += 1)),
      ),
    ).toEqual({ ok: true });
    expect(pairedCalls).toBe(1);
    const pairing = await h.store.pairing();
    expect(pairing).toMatchObject({
      tokenId: 'tok-1',
      accountId: 'account-synthetic',
      pairedAt: T0,
    });
    expect(pairing?.token).toMatch(/^shx_[A-Za-z0-9_-]{43}$/);
    expect(pairing?.scopes).toEqual(['ingest', 'tasks', 'uploads', 'lookup']);
    expect([...h.storage.data.keys()].sort()).toEqual(
      [KEYS.install, KEYS.pairing, KEYS.status].sort(),
    );

    const [request] = h.api.log;
    expect(request).toMatchObject({
      method: 'POST',
      path: '/api/v1/extension/pair',
      authorization: null,
      extension: '0.2.0',
    });
  });

  it('sends a random, persistent install id, so re-pairing replaces the old token', async () => {
    const h = harness();
    await pair(h.api.issuePairingCode(), deps(h));
    const first = (await h.store.pairing())!.token;
    const installId = await h.store.installId();
    expect(installId).toMatch(/^[0-9a-f]{32}$/);
    await pair(h.api.issuePairingCode(), deps(h));
    expect(await h.store.installId()).toBe(installId);
    const second = (await h.store.pairing())!.token;
    expect(second).not.toBe(first);
    // The fake revokes the previous token of the same installation (P2-G16).
    await h.store.setPairing({ token: first, tokenId: 'old', scopes: [], pairedAt: T0 });
    expect((await h.client.get('/api/v1/extension/config', { auth: 'token' })).ok).toBe(false);
  });

  it('answers the server code for a used or unknown code, and keeps any earlier pairing', async () => {
    const h = harness();
    const code = h.api.issuePairingCode();
    expect(await pair(code, deps(h))).toEqual({ ok: true });
    const kept = await h.store.pairing();
    expect(await pair(code, deps(h))).toEqual({ ok: false, code: 'invalid_pairing_code' });
    expect(await h.store.pairing()).toEqual(kept);
  });

  it('answers network and access_redirect when the server cannot be reached', async () => {
    const h = harness();
    h.network.down = true;
    expect(await pair(h.api.issuePairingCode(), deps(h))).toEqual({ ok: false, code: 'network' });
    h.network.down = false;
    h.api.requireAccess = { clientId: 'id', clientSecret: 'secret' };
    expect(await pair(h.api.issuePairingCode(), deps(h))).toEqual({
      ok: false,
      code: 'access_redirect',
    });
  });

  it('labels the browser for the token list', () => {
    const mac =
      'Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36';
    expect(installLabel(mac, 'MacIntel')).toBe('Chrome on macOS');
    expect(
      installLabel('Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/153 Edg/153', 'Win32'),
    ).toBe('Edge on Windows');
    expect(installLabel('Mozilla/5.0 (X11; Linux x86_64) Chrome/153', 'Linux x86_64')).toBe(
      'Chrome on Linux',
    );
    expect(installLabel('Mozilla/5.0 (X11; CrOS x86_64 14541.0.0) Chrome/153')).toBe(
      'Chrome on ChromeOS',
    );
    expect(installLabel('Mozilla/5.0')).toBe('Chrome');
  });
});

describe('config', () => {
  it('fetches once, then revalidates with the ETag (304)', async () => {
    const h = harness();
    await h.pairNow();
    expect(await h.config.refresh()).toEqual({ outcome: 'updated' });
    expect(await h.config.refresh()).toEqual({ outcome: 'fresh' });
    h.clock.now += 5 * 60_000;
    expect(await h.config.refresh()).toEqual({ outcome: 'unchanged' });
    const requests = h.api.log.filter((r) => r.path === '/api/v1/extension/config');
    expect(requests.map((r) => r.status)).toEqual([200, 304]);
    expect((await h.store.config())?.fetchedAt).toBe(h.clock.now);
    h.api.bumpConfig();
    expect(await h.config.refresh(true)).toEqual({ outcome: 'updated' });
  });

  it('marks the extension outdated below minVersion, and clears it when the server lowers it', async () => {
    const h = harness();
    await h.pairNow();
    h.api.minVersion = '0.3.0';
    await h.config.refresh(true);
    expect(await h.store.status()).toMatchObject({ outdated: true, minVersion: '0.3.0' });
    h.api.minVersion = '0.2.0';
    h.api.bumpConfig();
    await h.config.refresh(true);
    expect((await h.store.status()).outdated).toBe(false);
  });

  it('uses the defaults while unpaired', async () => {
    const h = harness();
    expect(await h.config.refresh(true)).toEqual({ outcome: 'unpaired' });
    expect(await h.config.current()).toEqual(DEFAULT_CONFIG);
  });

  it('parses C3, keeps the defaults for anything missing, and never goes faster than the plan', () => {
    expect(parseConfig(null)).toEqual(DEFAULT_CONFIG);
    const parsed = parseConfig({
      minVersion: '0.2.0',
      platforms: {
        instagram: { passive: false, replayGapMs: 100, replayMaxPages: 500, stopAfterKnown: 15 },
        twitter: { scrollSettleMs: 300, scroll: false },
        pinterest: 'nope',
      },
      maxSteps: 99_999,
      maxRunMs: 600_000,
      taskPollMinutes: 10,
    });
    expect(parsed.minVersion).toBe('0.2.0');
    expect(parsed.platforms.instagram).toMatchObject({
      passive: false,
      replayGapMs: 700,
      replayMaxPages: 100,
      stopAfterKnown: 15,
    });
    expect(parsed.platforms.twitter).toMatchObject({
      scrollSettleMs: 750,
      scroll: false,
      passive: true,
    });
    expect(parsed.platforms.pinterest).toEqual(DEFAULT_CONFIG.platforms.pinterest);
    expect(parsed).toMatchObject({ maxSteps: 16_000, maxRunMs: 600_000, taskPollMinutes: 10 });
  });

  it('compares versions on major.minor.patch, pre-releases first', () => {
    expect(compareVersions('0.2.0', '0.2.0')).toBe(0);
    expect(compareVersions('0.2.0', '0.10.0')).toBeLessThan(0);
    expect(compareVersions('1.0.0', '0.9.9')).toBeGreaterThan(0);
    expect(compareVersions('0.2.0-beta.1', '0.2.0')).toBeLessThan(0);
    expect(compareVersions('0.2', '0.2.0')).toBe(0);
    expect(compareVersions('x', '0.2.0')).toBeNull();
    expect(isBelowMinVersion('0.2.0', '0.2.1')).toBe(true);
    expect(isBelowMinVersion('0.2.0', null)).toBe(false);
    expect(isBelowMinVersion('0.2.0', 'garbage')).toBe(false);
  });
});

it('rejects a new C2 response without stable account identity', async () => {
  const { parsePairResponse } = await import('../src/sw/contracts');
  const value = { token: `shx_${'A'.repeat(43)}`, tokenId: 'installation', scopes: ['ingest'] };
  expect(parsePairResponse(value)).toBeNull();
  expect(parsePairResponse({ ...value, accountId: '' })).toBeNull();
  expect(parsePairResponse({ ...value, accountId: 'account-A' })).toMatchObject({
    accountId: 'account-A',
  });
});
