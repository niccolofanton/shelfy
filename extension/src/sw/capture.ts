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
//
// While the sync controller walks a listing (P2-13), the captures of its document go to that
// explicit run instead, tagged `replay` or `scroll` (C5): the passive toggle does not apply (the
// user asked for the sync), the mode's kill switch does, and so does the listing (a capture
// from another page of the tab is discarded).

import { platformForUrl } from '../shared/hosts';
import { classifyListing, passiveScope } from '../shared/listing';
import type { CaptureMessage, Platform, WireSource } from '../shared/protocol';
import type { ConfigService } from './config';
import { passiveAllowed } from './config';
import type { CollectionMode } from './contracts';
import { prefilterBatch } from './prefilter';
import type { Queue } from './queue/queue';
import type { DiscardReason, Run } from './queue/types';
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
  // Refresh ingest is owned by the leased task worker, never by page-declared scopes.
  if (message.capture === 'refresh') return discard('out_of_scope');
  const sync = await syncRunOf(deps, sender.tabId, message.docId);
  if (sync) {
    const outcome = await captureForSync(message, sync, deps, discard);
    if (outcome) return outcome;
  }
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
      accountTokenId: pairing.tokenId,
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

// ── Explicit syncs (P2-13) ──────────────────────────────────────────────────

/** Triggers whose runs the sync controller walks (P2-13, P2-15). */
export const CONTROLLER_TRIGGERS: readonly Run['trigger'][] = ['manual', 'web', 'scheduled'];

export function isControllerRun(run: Run): boolean {
  return CONTROLLER_TRIGGERS.includes(run.trigger);
}

/** The open sync of this tab and document, if the controller walks one. */
async function syncRunOf(
  deps: CaptureDeps,
  tabId: number | undefined,
  docId: string,
): Promise<Run | null> {
  if (typeof tabId !== 'number') return null;
  return (
    (await deps.queue.runs()).find(
      (run) =>
        run.state === 'open' && isControllerRun(run) && run.tabId === tabId && run.docId === docId,
    ) ?? null
  );
}

/** Queues a capture of a syncing document; null when its run ended meanwhile (passive rules). */
async function captureForSync(
  message: CaptureMessage,
  run: Run,
  deps: CaptureDeps,
  discard: (reason: DiscardReason) => Promise<CaptureOutcome>,
): Promise<CaptureOutcome | null> {
  const [pairing, status, config] = await Promise.all([
    deps.store.pairing(),
    deps.store.status(),
    deps.config.current(),
  ]);
  if (!pairing) return discard('unpaired');
  if (status.outdated) return discard('outdated');
  if (run.accountTokenId && run.accountTokenId !== pairing.tokenId) return discard('unpaired');
  if (classifyListing(message.platform, message.pageUrl)?.key !== run.listingKey)
    return discard('out_of_scope');
  const source: WireSource = message.capture === 'replay' ? 'replay' : 'scroll';
  if (!config.platforms[message.platform][source === 'replay' ? 'replay' : 'scroll'])
    return discard('killed');

  const { items, rejected } = prefilterBatch(message.items, message.platform);
  if (rejected) await deps.queue.countDiscard('invalid', rejected);
  if (!items.length) return { ok: true, queued: 0, discarded: 'invalid', items: rejected };
  const result = await deps.queue.captureToRun(run.id, {
    source,
    hasNextPage: message.hasNextPage,
    items,
    at: deps.now(),
    messageId: `${message.docId}:${message.seq}`,
  });
  if (!result.run && !result.duplicate) return null;
  // An explicit sync's group is sealed at once (eager): flush now, so the page's outcome comes
  // back before the controller's next page boundary.
  return {
    ok: true,
    queued: result.accepted,
    runId: result.run?.id ?? null,
    full: !result.duplicate,
    duplicate: result.duplicate,
  };
}
