import { describe, expect, it, vi } from 'vitest';
import { type SelectDeps, SelectionService } from '../src/content/select/service';
import { MSG } from '../src/shared/protocol';
import { hasMessage, translate } from '../src/shared/i18n';
import { exportRunReport } from '../src/sw/sync/report';
import { harness, igItem } from './helpers';

function selectionHarness(count = 0) {
  const h = harness();
  const marked: Array<{ key: string; id: string }> = [];
  const opened: string[] = [];
  const actions: string[] = [];
  const hooks: { mark?: () => void } = {};
  const selected = Array.from({ length: count }, (_, n) => ({
    key: `code${n + 1}`,
    item: igItem(n + 1),
  }));
  const deps: SelectDeps = {
    api: h.client,
    store: h.store,
    storage: h.storage,
    tabs: {
      get: async () => ({
        id: 7,
        url: 'https://www.instagram.com/me/saved/recipes/17899999999/',
        windowId: 1,
        active: true,
      }),
      sendMessage: async <T>(_: number, message: unknown) => {
        expect(message).toEqual({ kind: MSG.bridgePing });
        return { ok: true, syncing: null, viewer: null, heading: 'Recipes', docId: 'doc' } as T;
      },
      create: async (options) => {
        opened.push(options.url);
        return { windowId: 1, active: true };
      },
    },
    execute: async (injection) => {
      const [action, value] = injection.args ?? [];
      if (typeof action === 'string') actions.push(action);
      if (action === 'mark') hooks.mark?.();
      if (action === 'mark')
        for (const pair of value as Array<{ key: string; id: string }>) {
          if (!marked.some((entry) => entry.key === pair.key)) marked.push(pair);
        }
      return [
        {
          frameId: 0,
          documentId: 'chrome-doc',
          result:
            action === 'collect'
              ? {
                  json: JSON.stringify(selected),
                  href: 'https://www.instagram.com/me/saved/recipes/17899999999/',
                }
              : { enabled: true, count: selected.length - marked.length },
        },
      ];
    },
    client: { ext: '0.2.0', parser: 'fixture' },
    changed: () => undefined,
    accepted: (record) => h.queue.recordAcceptedKeys(record),
  };
  const service = new SelectionService(deps);
  const restart = () => new SelectionService(deps);
  return { ...h, service, restart, selected, marked, opened, actions, hooks };
}
const sender = {
  frameId: 0,
  documentId: 'chrome-doc',
  url: 'https://www.instagram.com/me/saved/all-posts/',
  tab: { id: 7 },
};

describe('extension selection', () => {
  it('exports server-accepted selection keys without captured content', async () => {
    const h = selectionHarness(1);
    await h.pairNow();
    const answer = await h.service.command(7, 'import', 'en', 'none', null);
    expect(answer.ok).toBe(true);
    const report = await exportRunReport(h.queue, h.store, Date.now());
    expect(report.runs[0]).toMatchObject({
      trigger: 'selection',
      listingKey: 'instagram:ig_collection:17899999999',
      keys: ['ig_3400000000000000001'],
    });
    expect(JSON.stringify(report)).not.toContain('caption');
  });
  it('looks up bounded chunks with the extension token, matches DOM aliases, opens the saved permalink', async () => {
    const h = selectionHarness();
    await h.pairNow();
    h.api.posts.set('ig_3400000000000000001', 1);
    const keys = [
      '3400000000000000001',
      ...Array.from({ length: 1000 }, (_, n) => String(4000 + n)),
    ];
    expect(await h.service.lookup(keys, sender)).toEqual({ ok: true });
    expect(h.api.lookups.map((lookup) => lookup.keys.length)).toEqual([1000, 1]);
    expect(h.marked).toEqual([{ key: keys[0], id: 'ig_3400000000000000001' }]);
    expect(
      h.api.log
        .filter((request) => request.path.includes('/lookup'))
        .every((request) => request.authorization?.startsWith('Bearer shx_')),
    ).toBe(true);
    expect(await h.service.open(h.marked[0].id, sender)).toEqual({ ok: true });
    expect(h.opened).toEqual([`${h.client.origin}/p/ig_3400000000000000001`]);
    expect(await h.service.lookup(['a'], { ...sender, frameId: 1 })).toEqual({ ok: false });
    expect(await h.service.open('../settings', sender)).toEqual({ ok: false });
  });

  it('imports 601 selections as 500 + 101, creates a selection run and clears accepted selections', async () => {
    const h = selectionHarness(601);
    await h.pairNow();
    const answer = await h.service.command(7, 'import', 'en', 'auto', 'Chosen recipes');
    expect(answer).toEqual({ ok: true, enabled: true, count: 0, imported: 601 });
    expect(h.api.ingests.map((batch) => [batch.count, batch.source])).toEqual([
      [500, 'selection'],
      [101, 'selection'],
    ]);
    expect([...h.api.runs.values()]).toMatchObject([
      {
        trigger: 'selection',
        state: 'done',
        listing: { name: 'Chosen recipes' },
        collection: { mode: 'auto' },
      },
    ]);
    expect(h.marked).toHaveLength(601);
    expect(h.actions).toContain('clear');
    expect(h.storage.data.has('shelfy.selection.7')).toBe(false);
  });

  it('persists a lost response and replays the same batch after the worker restarts', async () => {
    const h = selectionHarness(2);
    await h.pairNow();
    h.api.loseNextIngestResponse = true;
    const failed = await h.service.command(7, 'import', 'it', 'none', null);
    expect(failed.ok).toBe(false);
    expect(h.marked).toHaveLength(0);
    expect(h.storage.data.has('shelfy.selection.7')).toBe(true);
    expect(await h.restart().command(7, 'import', 'it', 'none', null)).toMatchObject({
      ok: true,
      count: 0,
    });
    expect(h.api.runs.size).toBe(1);
    expect(h.api.ingests.map((batch) => batch.replayed)).toEqual([false, true]);
    expect([...h.api.posts.values()]).toEqual([1, 1]);
  });

  it('offers a pending retry after the closing PATCH fails, even with no remaining selection', async () => {
    const h = selectionHarness(2);
    await h.pairNow();
    h.hooks.mark = () => {
      h.api.failWith = { status: 503, code: 'internal' };
    };
    expect((await h.service.command(7, 'import', 'en', 'none', null)).ok).toBe(false);
    h.hooks.mark = undefined;
    h.api.failWith = null;
    expect(await h.restart().command(7, 'status', 'en', 'none', null)).toMatchObject({
      ok: true,
      count: 0,
      pending: true,
    });
    expect(await h.restart().command(7, 'import', 'en', 'none', null)).toMatchObject({
      ok: true,
      count: 0,
    });
    expect(h.api.runs.size).toBe(1);
    expect(h.api.ingests).toHaveLength(1);
  });

  it('refuses a request when credentials change after the selection captured its pairing', async () => {
    const h = selectionHarness();
    const token = await h.pairNow();
    const pairing = (await h.store.pairing())!;
    await h.store.setPairing({ ...pairing, tokenId: 'tok-2', token: 'shx_other-account' });
    const before = h.api.log.length;
    expect(
      await h.client.post(
        '/api/v1/posts/lookup',
        { platform: 'instagram', keys: [] },
        { auth: 'token', expectedToken: token },
      ),
    ).toEqual({ ok: false, failure: { kind: 'unpaired' } });
    expect(h.api.log).toHaveLength(before);
  });

  it('discards a pending journal after pairing changes and never replays another account run', async () => {
    const h = selectionHarness(2);
    await h.pairNow();
    h.api.loseNextIngestResponse = true;
    await h.service.command(7, 'import', 'en', 'none', null);
    const old = h.storage.data.get('shelfy.selection.7') as { tokenId: string; runId: string };
    expect(old.tokenId).toBe('tok-1');
    const pairing = (await h.store.pairing())!;
    await h.store.setPairing({ ...pairing, tokenId: 'tok-2' });
    expect(await h.restart().command(7, 'status', 'en', 'none', null)).toMatchObject({
      pending: false,
    });
    expect(h.storage.data.has('shelfy.selection.7')).toBe(false);
    expect(await h.restart().command(7, 'import', 'en', 'none', null)).toMatchObject({ ok: true });
    expect(h.api.runs.size).toBe(2);
    expect(h.api.ingests.map((batch) => batch.replayed)).toEqual([false, false]);
  });

  it('discards an in-flight import response after re-pairing before persisting or marking it', async () => {
    const h = selectionHarness(601);
    await h.pairNow();
    const original = h.client.post.bind(h.client);
    vi.spyOn(h.client, 'post').mockImplementation(async (path, body, options) => {
      const response = await original(path, body, options);
      if (path.includes('/ingest/batches')) {
        const pairing = (await h.store.pairing())!;
        await h.store.setPairing({ ...pairing, tokenId: 'tok-2' });
      }
      return response;
    });
    expect(await h.service.command(7, 'import', 'en', 'none', null)).toEqual({
      ok: false,
      code: 'not_paired',
    });
    expect(h.api.ingests).toHaveLength(1);
    expect(h.marked).toHaveLength(0);
    expect(h.storage.data.has('shelfy.selection.7')).toBe(false);
    expect([...h.api.runs.values()][0].state).not.toBe('done');
  });

  it('does not apply an in-flight lookup to the overlay after unpairing', async () => {
    const h = selectionHarness();
    await h.pairNow();
    h.api.posts.set('ig_3400000000000000001', 1);
    const original = h.client.post.bind(h.client);
    vi.spyOn(h.client, 'post').mockImplementation(async (path, body, options) => {
      const response = await original(path, body, options);
      await h.store.setPairing(null);
      return response;
    });
    expect(await h.service.lookup(['3400000000000000001'], sender)).toEqual({ ok: false });
    expect(h.marked).toHaveLength(0);
    expect(h.actions).toContain('bind');
  });

  it('serializes imports before asynchronous tab checks', async () => {
    const h = selectionHarness(2);
    await h.pairNow();
    const first = h.service.command(7, 'import', 'en', 'none', null);
    expect(await h.service.command(7, 'import', 'en', 'none', null)).toEqual({
      ok: false,
      code: 'busy',
    });
    expect(await first).toMatchObject({ ok: true, count: 0 });
    expect(h.api.runs.size).toBe(1);
  });

  it('retains server-rejected selections and uses the no-collection chooser', async () => {
    const h = selectionHarness(2);
    await h.pairNow();
    h.selected[1].item.id = '!invalid';
    expect(await h.service.command(7, 'import', 'en', 'none', null)).toMatchObject({
      ok: true,
      count: 1,
      imported: 1,
    });
    expect(h.actions).not.toContain('clear');
    expect([...h.api.runs.values()][0].collection).toEqual({ mode: 'none' });
  });

  it('has translated selection labels, with Import in both languages', () => {
    for (const key of [
      'title',
      'start',
      'stop',
      'hint',
      'import',
      'importing',
      'imported',
      'count',
      'saved',
      'open',
      'disabled',
    ]) {
      expect(hasMessage(`select.${key}`)).toBe(true);
    }
    expect(translate('en', 'select.import')).toBe('Import selected');
    expect(translate('it', 'select.import')).toBe('Importa selezionati');
  });
});
