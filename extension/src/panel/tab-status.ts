// What passive capture does in the active tab, for the panel's Capture section: the same rules
// as the worker (shared/listing.ts passiveScope), plus whether this extension's content scripts
// are live in the tab (they are injected on page load only, so a tab opened before an install or
// a reload of the extension shows "reload the tab").

import { platformForUrl } from '../shared/hosts';
import type { Translate } from '../shared/i18n';
import { classifyListing, passiveScope, type Listing } from '../shared/listing';
import type { BridgePong } from '../shared/protocol';
import type { PanelState } from '../sw/state';

export type TabStatusKind =
  | 'none'
  | 'reload'
  | 'out_of_scope'
  | 'not_own_board'
  | 'viewer_unknown'
  | 'off'
  | 'capturing';

export interface TabStatus {
  kind: TabStatusKind;
  listing: Listing | null;
}

export function tabStatus(
  url: string | undefined,
  pong: BridgePong | null,
  state: Pick<PanelState, 'paired' | 'outdated' | 'passive' | 'serverPassive'>,
): TabStatus {
  const platform = url ? platformForUrl(url) : null;
  if (!url || !platform) return { kind: 'none', listing: null };
  const listing = classifyListing(platform, url);
  if (!pong) return { kind: 'reload', listing };
  const scope = passiveScope(platform, url, pong.viewer);
  if (!scope.ok) return { kind: scope.reason, listing };
  const on =
    state.paired && !state.outdated && state.passive[platform] && state.serverPassive[platform];
  return { kind: on ? 'capturing' : 'off', listing: scope.listing };
}

const STATUS_KEY: Record<TabStatusKind, string> = {
  none: 'capture.tab.none',
  reload: 'capture.tab.reload',
  out_of_scope: 'capture.tab.outOfScope',
  not_own_board: 'capture.tab.notOwnBoard',
  viewer_unknown: 'capture.tab.viewerUnknown',
  off: 'capture.tab.off',
  capturing: 'capture.tab.capturing',
};

export function listingText(t: Translate, listing: Listing): string {
  return t(`capture.listing.${listing.kind}`, {
    name: listing.name ?? listing.externalId ?? '?',
  });
}

/** "This tab: Instagram · folder recipes — capturing". */
export function tabStatusText(t: Translate, status: TabStatus): string {
  const what = t(STATUS_KEY[status.kind]);
  return t('capture.tab', {
    listing: status.listing ? `${listingText(t, status.listing)} — ${what}` : what,
  });
}
