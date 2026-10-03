// Runs in the worker. MAIN-world collection is treated as untrusted input and sanitized;
// the API token never goes to the overlay, bridge or panel.
import { FILES } from '../../manifest';
import { platformForUrl } from '../../shared/hosts';
import { createTranslate, type Lang } from '../../shared/i18n';
import { passiveScope, syncTarget, toWireListing, type Listing } from '../../shared/listing';
import { MSG, isRecord, type BridgePong, type Platform } from '../../shared/protocol';
import { createUlid } from '../../shared/ulid';
import type { ApiClient } from '../../sw/api';
import {
  API,
  parseIngestResult,
  parseSyncRunCreated,
  type IngestBatchBody,
} from '../../sw/contracts';
import { failureCode } from '../../sw/errors';
import { itemBytes, prefilterBatch, type WireItem } from '../../sw/prefilter';
import type { SettingsStore, StorageArea } from '../../sw/settings';
import type { Sender } from '../../sw/router';
import { selectMain } from './main';

export interface SelectDeps {
  api: ApiClient;
  store: SettingsStore;
  storage: StorageArea;
  tabs: Pick<typeof chrome.tabs, 'get' | 'sendMessage' | 'create'>;
  execute: typeof chrome.scripting.executeScript;
  client: { ext: string; parser: string };
  changed(): void;
}
export type SelectAnswer =
  | { ok: true; enabled: boolean; count: number; imported?: number; pending?: boolean }
  | { ok: false; code: string };
interface Entry {
  key: string;
  item: WireItem;
}
interface Journal {
  tokenId: string;
  listingKey: string;
  platform: Platform;
  runId: string;
  batches: Array<{ id: string; entries: Entry[] }>;
  next: number;
  scanned: number;
  saved: Array<{ key: string; id: string }>;
  client: { ext: string; parser: string };
}
const journalKey = (tabId: number): string => `shelfy.selection.${tabId}`;
const no = (code: string): SelectAnswer => ({ ok: false, code });

export class SelectionService {
  private readonly active = new Set<number>();
  private readonly ulid = createUlid();
  constructor(private readonly deps: SelectDeps) {}

  private async context(
    tabId: number,
  ): Promise<
    | { code: string }
    | { platform: Platform; listing: Listing; pong: BridgePong; tokenId: string; token: string }
  > {
    const [tab, pairing, status] = await Promise.all([
      this.deps.tabs.get(tabId).catch(() => null),
      this.deps.store.pairing(),
      this.deps.store.status(),
    ]);
    if (!pairing) {
      await this.main(tabId, 'bind', null).catch(() => undefined);
      await this.deps.storage.remove(journalKey(tabId));
      return { code: 'not_paired' } as const;
    }
    if (status.outdated) return { code: 'outdated' } as const;
    const platform = platformForUrl(tab?.url ?? '');
    const listing = platform && tab?.url ? syncTarget(platform, tab.url) : null;
    if (!platform || !listing || !tab?.url) return { code: 'not_a_listing' } as const;
    const pong = await this.deps.tabs
      .sendMessage<BridgePong>(tabId, { kind: MSG.bridgePing }, { frameId: 0 })
      .catch(() => null);
    if (!pong?.ok) return { code: 'reload_tab' } as const;
    if (pong.syncing) return { code: 'busy' } as const;
    if (platform === 'pinterest') {
      const scope = passiveScope(platform, tab.url, pong.viewer);
      if (!scope.ok) return { code: scope.reason } as const;
    }
    const storageKey = journalKey(tabId);
    const journal = (await this.deps.storage.get(storageKey))[storageKey] as Journal | undefined;
    if (journal && journal.tokenId !== pairing.tokenId) await this.deps.storage.remove(storageKey);
    await this.main(tabId, 'bind', pairing.tokenId);
    return { platform, listing, pong, tokenId: pairing.tokenId, token: pairing.token };
  }

  private async paired(tabId: number, tokenId: string): Promise<boolean> {
    const current = await this.deps.store.pairing();
    if (current?.tokenId === tokenId) return true;
    const key = journalKey(tabId);
    const stored = (await this.deps.storage.get(key))[key] as Journal | undefined;
    if (stored?.tokenId === tokenId || stored?.tokenId === undefined)
      await this.deps.storage.remove(key);
    await this.main(tabId, 'bind', current?.tokenId ?? null).catch(() => undefined);
    return false;
  }

  private async main(tabId: number, action: string, value: unknown = null, documentId?: string) {
    const [result] = await this.deps.execute({
      target: documentId ? { tabId, documentIds: [documentId] } : { tabId, frameIds: [0] },
      world: 'MAIN',
      func: selectMain,
      args: [action, value],
    });
    return result;
  }

  async command(
    tabId: number,
    action: string,
    lang: Lang,
    collection: 'auto' | 'none',
    name: string | null,
  ): Promise<SelectAnswer> {
    if (this.active.has(tabId)) return no('busy');
    const importing = action === 'import';
    if (importing) this.active.add(tabId);
    try {
      const ctx = await this.context(tabId);
      if ('code' in ctx) return no(ctx.code);
      if (importing) return await this.importSelection(tabId, ctx, collection, name);
      if (action === 'enable') {
        await this.deps.execute({
          target: { tabId, frameIds: [0] },
          world: 'MAIN',
          files: [FILES.select],
        });
      }
      const t = createTranslate(lang);
      const result = await this.main(
        tabId,
        action,
        action === 'enable'
          ? {
              saved: t('select.saved'),
              open: t('select.open'),
              disabled: t('select.disabled'),
            }
          : null,
      );
      if (action !== 'status') this.deps.changed();
      const value = result?.result;
      const journal = (await this.deps.storage.get(journalKey(tabId)))[journalKey(tabId)] as
        | Journal
        | undefined;
      return isRecord(value) && typeof value.count === 'number'
        ? {
            ok: true,
            enabled: value.enabled === true,
            count: value.count,
            pending: journal?.tokenId === ctx.tokenId && journal?.listingKey === ctx.listing.key,
          }
        : no('reload_tab');
    } finally {
      if (importing) {
        this.active.delete(tabId);
        this.deps.changed();
      }
    }
  }

  async lookup(keys: string[], sender: Sender): Promise<{ ok: boolean }> {
    if (
      sender.frameId !== 0 ||
      sender.tab?.id === undefined ||
      !sender.url ||
      keys.length > 16_000 ||
      keys.some((key) => !/^[A-Za-z0-9_-]{1,200}$/.test(key))
    )
      return { ok: false };
    const tabId = sender.tab.id;
    const ctx = await this.context(tabId);
    if ('code' in ctx || platformForUrl(sender.url) !== ctx.platform) return { ok: false };
    const asked = [...new Set(keys)];
    for (let i = 0; i < asked.length; i += 1000) {
      if (!(await this.paired(tabId, ctx.tokenId))) return { ok: false };
      const chunk = asked.slice(i, i + 1000);
      const response = await this.deps.api.post(
        '/api/v1/posts/lookup',
        { platform: ctx.platform, keys: chunk },
        { auth: 'token', expectedToken: ctx.token },
      );
      if (!(await this.paired(tabId, ctx.tokenId))) return { ok: false };
      if (!response.ok) {
        await this.main(tabId, 'retry', asked.slice(i), sender.documentId).catch(() => undefined);
        return { ok: false };
      }
      const items =
        isRecord(response.data) && Array.isArray(response.data.items) ? response.data.items : [];
      const pairs = items
        .filter(
          (item) =>
            isRecord(item) &&
            chunk.includes(String(item.key)) &&
            typeof item.postKey === 'string' &&
            /^(ig|x|pin)_[A-Za-z0-9_-]{1,196}$/.test(item.postKey),
        )
        .map((item) => ({
          key: (item as Record<string, unknown>).key,
          id: (item as Record<string, unknown>).postKey,
        }));
      if (pairs.length) await this.main(tabId, 'mark', pairs, sender.documentId);
    }
    this.deps.changed();
    return { ok: true };
  }

  async open(key: string, sender: Sender): Promise<{ ok: boolean }> {
    if (
      sender.frameId !== 0 ||
      sender.tab?.id === undefined ||
      !/^(ig|x|pin)_[A-Za-z0-9_-]{1,196}$/.test(key)
    )
      return { ok: false };
    const ctx = await this.context(sender.tab.id);
    if ('code' in ctx) return { ok: false };
    await this.deps.tabs.create({ url: `${this.deps.api.origin}/p/${encodeURIComponent(key)}` });
    return { ok: true };
  }

  private async importSelection(
    tabId: number,
    ctx: Exclude<Awaited<ReturnType<SelectionService['context']>>, { code: string }>,
    collection: 'auto' | 'none',
    name: string | null,
  ): Promise<SelectAnswer> {
    if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
    const snapshot = await this.main(tabId, 'collect');
    const documentId = snapshot?.documentId;
    const result = snapshot?.result;
    if (
      !isRecord(result) ||
      typeof result.json !== 'string' ||
      result.json.length > 64 * 1024 * 1024 ||
      typeof result.href !== 'string' ||
      syncTarget(ctx.platform, result.href)?.key !== ctx.listing.key
    )
      return no('not_a_listing');
    const raw: unknown = JSON.parse(result.json);
    if (!Array.isArray(raw) || raw.length > 16_000) return no('bad_request');
    const storageKey = journalKey(tabId);
    const stored = (await this.deps.storage.get(storageKey))[storageKey] as Journal | undefined;
    let journal =
      stored?.tokenId === ctx.tokenId && stored?.listingKey === ctx.listing.key ? stored : null;
    if (stored && !journal) return no('selection_pending');
    if (!journal) {
      const entries: Entry[] = [];
      const seen = new Set<string>();
      for (const entry of raw) {
        if (
          !isRecord(entry) ||
          typeof entry.key !== 'string' ||
          !/^[A-Za-z0-9_-]{1,200}$/.test(entry.key) ||
          seen.has(entry.key)
        )
          return no('bad_request');
        const item = prefilterBatch([entry.item], ctx.platform).items[0];
        if (!item) return no('bad_request');
        seen.add(entry.key);
        entries.push({ key: entry.key, item });
      }
      if (!entries.length) return no('selection_empty');
      const wire = toWireListing(ctx.listing);
      if (!wire) return no('not_a_listing');
      if (name && wire.externalId !== null) wire.name = name;
      if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
      const opened = await this.deps.api.post(
        API.syncRuns,
        {
          platform: ctx.platform,
          trigger: 'selection',
          listing: wire,
          collection: { mode: collection === 'auto' && wire.externalId !== null ? 'auto' : 'none' },
        },
        { auth: 'token', expectedToken: ctx.token },
      );
      if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
      if (!opened.ok) return no(failureCode(opened.failure));
      const run = parseSyncRunCreated(opened.data);
      if (!run) return no('bad_response');
      const batches: Journal['batches'] = [];
      let group: Entry[] = [],
        bytes = 0;
      for (const entry of entries) {
        const size = itemBytes(entry.item);
        if (group.length && (group.length >= 500 || bytes + size > 7 * 1024 * 1024)) {
          batches.push({ id: this.ulid(), entries: group });
          group = [];
          bytes = 0;
        }
        group.push(entry);
        bytes += size;
      }
      if (group.length) batches.push({ id: this.ulid(), entries: group });
      journal = {
        tokenId: ctx.tokenId,
        listingKey: ctx.listing.key,
        platform: ctx.platform,
        runId: run.id,
        batches,
        next: 0,
        scanned: 0,
        saved: [],
        client: this.deps.client,
      };
      await this.deps.storage.set({ [storageKey]: journal });
    }
    const accepted = journal.saved;
    for (; journal.next < journal.batches.length; journal.next++) {
      const batch = journal.batches[journal.next];
      const body: IngestBatchBody = {
        syncRunId: journal.runId,
        platform: journal.platform,
        source: 'selection',
        hasNextPage: false,
        client: journal.client,
        items: batch.entries.map((entry) => entry.item),
      };
      if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
      const response = await this.deps.api.post(API.ingestBatches, body, {
        auth: 'token',
        idempotencyKey: batch.id,
        expectedToken: ctx.token,
      });
      if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
      if (!response.ok) return no(failureCode(response.failure));
      const result = parseIngestResult(response.data);
      for (const item of result.results) {
        const entry = batch.entries[item.index];
        if (entry && /^(ig|x|pin)_[A-Za-z0-9_-]{1,196}$/.test(item.key))
          accepted.push({ key: entry.key, id: item.key });
      }
      journal.scanned += batch.entries.length;
      // Persist progress before touching a possibly navigated page. A replay retains the same key.
      await this.deps.storage.set({ [storageKey]: { ...journal, next: journal.next + 1 } });
      if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
      await this.main(tabId, 'mark', accepted, documentId).catch(() => undefined);
    }
    if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
    await this.main(tabId, 'mark', accepted, documentId).catch(() => undefined);
    if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
    const closed = await this.deps.api.patch(
      API.syncRun(journal.runId),
      {
        state: 'done',
        pages: journal.batches.length,
        scanned: journal.scanned,
        stopReason: 'user',
        resumeCursor: null,
        errorCode: null,
      },
      { auth: 'token', expectedToken: ctx.token },
    );
    if (!(await this.paired(tabId, ctx.tokenId))) return no('not_paired');
    if (!closed.ok) return no(failureCode(closed.failure));
    await this.deps.storage.remove(storageKey);
    // markSaved removes only accepted posts; rejected records remain selected for correction.
    const state = await this.main(tabId, 'status', null, documentId).catch(() => null);
    const count =
      isRecord(state?.result) && typeof state.result.count === 'number' ? state.result.count : 0;
    if (!count) await this.main(tabId, 'clear', null, documentId).catch(() => undefined);
    return { ok: true, enabled: true, count, imported: accepted.length };
  }
}
