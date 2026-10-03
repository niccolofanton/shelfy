// Passive capture in the worker (plan §2.16): hook → bridge → here → the desktop sanitizer as a
// pre-filter → the offline queue. A relayed batch is queued only when every check passes, in
// this order (the first failing one is counted as the discard reason):
//
// 1. sender: the top frame of a tab whose host, the declared platform and the relayed page URL
//    agree (the browser vouches for the tab; the page vouches for nothing);
// 2. paired; 3. not outdated; 4. the platform's passive toggle (panel); 5. its kill switch
//    (server config);
// 6. scope: IG saved and folders, X bookmarks, and the signed-in user's Pinterest boards;
// 7. at least one item survives the pre-filter.
//
// Each listing visit (tab × document × listing) opens one `passive` sync run (C4); the run ends
// when the tab shows another listing, reloads, closes, or stays idle (sw/index.ts).

import { platformForUrl } from '../shared/hosts';
import { passiveScope } from '../shared/listing';
import type { CaptureMessage, Platform } from '../shared/protocol';
import type { ConfigService } from './config';
import { passiveAllowed } from './config';
import type { CollectionMode } from './contracts';
import { prefilterBatch } from './prefilter';
import type { Queue } from './queue/queue';
import type { DiscardReason } from './queue/types';
import type { SettingsStore } from './settings';

export interface CaptureSender {
  tabId: number | undefined;
  frameId: number | undefined;
  /** The sender frame's URL, as the browser reports it. */
  url: string | undefined;
  tabUrl: string | undefined;
}

export type SenderCheck = { ok: true } | { ok: false; reason: string };

/**
 * A batch must come from the top frame of a tab on a supported host, and the platform the page
 * declared must match both the sender's host and the relayed page URL.
 */
export function checkCaptureSender(
  sender: CaptureSender,
  declared: Platform,
  pageUrl: string,
): SenderCheck {
  if (typeof sender.tabId !== 'number' || sender.frameId !== 0)
    return { ok: false, reason: 'not_a_top_frame' };
  const senderPlatform = platformForUrl(sender.url ?? sender.tabUrl ?? '');
  if (!senderPlatform) return { ok: false, reason: 'unsupported_host' };
  if (senderPlatform !== declared || platformForUrl(pageUrl) !== declared)
    return { ok: false, reason: 'platform_host_mismatch' };
  return { ok: true };
}

export type CaptureOutcome =
  | { ok: true; queued: number; runId: string | null; full: boolean; duplicate: boolean }
  | { ok: true; queued: 0; discarded: DiscardReason; items: number };

export interface CaptureDeps {
  queue: Queue;
  store: SettingsStore;
  config: ConfigService;
  now(): number;
}

export async function handleCapture(
  message: CaptureMessage,
  sender: CaptureSender,
  deps: CaptureDeps,
): Promise<CaptureOutcome> {
  const received = message.items.length;
  const discard = async (reason: DiscardReason): Promise<CaptureOutcome> => {
    if (received) await deps.queue.countDiscard(reason, received);
    return { ok: true, queued: 0, discarded: reason, items: received };
  };

  if (!checkCaptureSender(sender, message.platform, message.pageUrl).ok) return discard('sender');
  const [pairing, status, settings, config] = await Promise.all([
    deps.store.pairing(),
    deps.store.status(),
    deps.store.settings(),
    deps.config.current(),
  ]);
  if (!pairing) return discard('unpaired');
  if (status.outdated) return discard('outdated');
  if (!settings.passive[message.platform]) return discard('disabled');
  if (!passiveAllowed(config, message.platform)) return discard('killed');
  const scope = passiveScope(message.platform, message.pageUrl, message.viewer);
  if (!scope.ok) return discard(scope.reason);

  const { items, rejected } = prefilterBatch(message.items, message.platform);
  if (!items.length) {
    if (rejected) await deps.queue.countDiscard('invalid', rejected);
    return { ok: true, queued: 0, discarded: 'invalid', items: rejected };
  }
  if (rejected) await deps.queue.countDiscard('invalid', rejected);

  const collection: CollectionMode =
    settings.passiveFolders && scope.wire.externalId ? { mode: 'auto' } : { mode: 'none' };
  const result = await deps.queue.capturePassive(
    {
      platform: message.platform,
      trigger: 'passive',
      listing: scope.wire,
      collection,
      tabId: sender.tabId ?? null,
      docId: message.docId,
      listingKey: scope.listing.key,
    },
    {
      source: 'passive',
      hasNextPage: message.hasNextPage,
      items,
      at: deps.now(),
      messageId: `${message.docId}:${message.seq}`,
    },
  );
  return {
    ok: true,
    queued: result.accepted,
    runId: result.run?.id ?? null,
    full: result.full,
    duplicate: result.duplicate,
  };
}
