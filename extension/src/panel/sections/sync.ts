// "Sync now" (P2-13): walks the saved listing of the active tab with the sync controller, with
// the folder chooser of IMP-10 for IG folders and Pinterest boards (the listing's collection,
// found by its id or created under the page heading, or none), the live progress, a stop
// button, and the result of the listing's last sync.

import { deslugify } from '../../../../src/lib/browserUrls';
import { platformForUrl } from '../../shared/hosts';
import type { Translate } from '../../shared/i18n';
import { passiveScope, syncTarget, type Listing } from '../../shared/listing';
import {
  MSG,
  type BridgePong,
  type SyncCollectionChoice,
  type SyncStartAnswer,
} from '../../shared/protocol';
import type { PanelState } from '../../sw/state';
import type { RunReport } from '../../shared/run-report';
import type { SyncView } from '../../sw/sync/history';
import { button, el, setText } from '../dom';
import type { PanelContext, PanelSection } from '../section';
import { listingText } from '../tab-status';

interface ActiveTab {
  id: number | null;
  url: string | undefined;
  pong: BridgePong | null;
}

async function activeTab(): Promise<ActiveTab> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || typeof tab.id !== 'number') return { id: null, url: undefined, pong: null };
  const pong = await chrome.tabs
    .sendMessage<BridgePong>(tab.id, { kind: MSG.bridgePing }, { frameId: 0 })
    .then((answer) => (answer?.ok ? answer : null))
    .catch(() => null);
  return { id: tab.id, url: tab.url, pong };
}

export type SyncAvailability =
  | { ok: true; listing: Listing }
  | { ok: false; reason: string; listing: Listing | null };

/** Whether the active tab can be synced now, and why not (the panel's own reading of it). */
export function syncAvailability(tab: ActiveTab, state: PanelState): SyncAvailability {
  const platform = tab.url ? platformForUrl(tab.url) : null;
  const listing = platform && tab.url ? syncTarget(platform, tab.url) : null;
  const refuse = (reason: string): SyncAvailability => ({ ok: false, reason, listing });
  if (!state.paired) return refuse('not_paired');
  if (state.outdated) return refuse('outdated');
  if (!platform || !listing || !tab.url) return refuse('not_a_listing');
  if (!tab.pong) return refuse('reload_tab');
  if (platform === 'pinterest') {
    const scope = passiveScope(platform, tab.url, tab.pong.viewer);
    if (!scope.ok) return refuse(scope.reason);
  }
  const modes = state.serverModes[platform];
  if (!modes.replay && !modes.scroll) return refuse('disabled');
  const elsewhere = state.syncs.some(
    (view) => view.state === 'open' && view.platform === platform && view.tabId !== tab.id,
  );
  if (elsewhere) return refuse('busy');
  return { ok: true, listing };
}

/** "Text" of a start refusal: the section's own codes, else the shared error lines. */
function refusalText(ctx: PanelContext, code: string): string {
  const key = `sync.refused.${code}`;
  return ctx.t(key) === key ? ctx.errorText(code) : ctx.t(key);
}

/** "Syncing · replay, page 3 · 42 scanned, 5 new, 37 already saved". */
export function liveText(t: Translate, view: SyncView): string {
  const phase =
    view.state === 'ended'
      ? t('sync.sending', { count: view.queued })
      : view.phase === 'replay'
        ? t('sync.phase.replayPage', { page: view.replayPages })
        : t(`sync.phase.${view.phase ?? 'starting'}`);
  return `${phase} · ${t('sync.counts', { scanned: view.scanned, inserted: view.inserted, known: view.known })}`;
}

/** "Done: end of the feed · 120 scanned, 12 new, 108 already saved". */
export function resultText(t: Translate, view: SyncView): string {
  const reason = t(`sync.reason.${view.stopReason ?? 'stalled'}`);
  return t('sync.result', {
    reason,
    counts: t('sync.counts', { scanned: view.scanned, inserted: view.inserted, known: view.known }),
  });
}

export const syncSection: PanelSection = {
  id: 'sync',
  mount(root: HTMLElement, ctx: PanelContext) {
    const { t } = ctx;
    const tabLine = el('p', { className: 'card', attrs: { 'data-testid': 'sync-tab' } });
    const reason = el('p', { className: 'muted', attrs: { 'data-testid': 'sync-reason' } });

    const chooser = el('fieldset', {
      className: 'chooser',
      attrs: { 'data-testid': 'sync-chooser' },
    });
    const radio = (value: SyncCollectionChoice, text: string): HTMLInputElement => {
      const input = el('input', {
        attrs: {
          type: 'radio',
          name: 'sync-collection',
          value,
          'data-testid': `sync-into-${value}`,
        },
      });
      const label = el('label', { className: 'toggle' });
      label.append(input, el('span', { text }));
      chooser.append(label);
      return input;
    };
    chooser.append(el('legend', { text: t('sync.chooser.title') }));
    const intoAuto = radio('auto', t('sync.chooser.auto'));
    const nameLabel = el('label', { className: 'field' });
    const name = el('input', {
      attrs: { type: 'text', maxlength: '120', spellcheck: 'false', 'data-testid': 'sync-name' },
    });
    nameLabel.append(el('span', { className: 'muted', text: t('sync.chooser.name') }), name);
    chooser.append(nameLabel);
    const intoNone = radio('none', t('sync.chooser.none'));
    intoAuto.checked = true;
    const toggleName = (): void => void (name.disabled = !intoAuto.checked);
    intoAuto.addEventListener('change', toggleName);
    intoNone.addEventListener('change', toggleName);

    const status = el('p', {
      className: 'status',
      attrs: { role: 'status', 'data-testid': 'sync-status' },
    });
    const live = el('p', { attrs: { 'data-testid': 'sync-live' } });
    const skipped = el('p', { className: 'muted', attrs: { 'data-testid': 'sync-skipped' } });
    const last = el('p', { className: 'muted', attrs: { 'data-testid': 'sync-last' } });
    for (const node of [chooser, live, skipped, last, reason]) node.hidden = true;

    let tab: ActiveTab = { id: null, url: undefined, pong: null };
    let namedFor: string | null = null;

    const start = button(
      t('sync.start'),
      () => {
        if (tab.id === null) return;
        start.disabled = true;
        setText(status, t('sync.starting'));
        ctx
          .send<SyncStartAnswer>({
            kind: MSG.syncStart,
            tabId: tab.id,
            collection: !chooser.hidden && intoNone.checked ? 'none' : 'auto',
            name: !chooser.hidden && intoAuto.checked ? name.value : null,
          })
          .then((answer) => setText(status, answer.ok ? '' : refusalText(ctx, answer.code)))
          .catch(() => setText(status, t('error.unknown', { code: 'sync' })))
          .finally(ctx.refresh);
      },
      { testId: 'sync-start' },
    );
    const stop = button(
      t('sync.stop'),
      () => {
        if (tab.id === null) return;
        stop.disabled = true;
        void ctx.send({ kind: MSG.syncStop, tabId: tab.id }).finally(ctx.refresh);
      },
      { className: 'danger', testId: 'sync-stop' },
    );
    stop.hidden = true;
    const actions = el('div', { className: 'actions' });
    const exportReport = button(
      t('sync.exportReport'),
      () => {
        exportReport.disabled = true;
        void ctx
          .send<RunReport>({ kind: MSG.syncReport })
          .then((report) => {
            if (!report.runs?.length) {
              setText(status, t('sync.reportEmpty'));
              return;
            }
            const url = URL.createObjectURL(
              new Blob([JSON.stringify(report, null, 2)], { type: 'application/json' }),
            );
            const link = document.createElement('a');
            link.href = url;
            link.download = `shelfy-run-report-${new Date().toISOString().slice(0, 10)}.json`;
            link.click();
            setTimeout(() => URL.revokeObjectURL(url), 1000);
          })
          .catch(() => setText(status, t('sync.reportFailed')))
          .finally(() => {
            exportReport.disabled = false;
          });
      },
      { testId: 'sync-export-report' },
    );
    actions.append(start, stop, exportReport);

    root.append(
      el('h2', { text: t('sync.title') }),
      el('p', { className: 'muted', text: t('sync.hint') }),
      tabLine,
      reason,
      chooser,
      actions,
      status,
      live,
      skipped,
      last,
    );

    const render = (state: PanelState): void => {
      const availability = syncAvailability(tab, state);
      const listing = availability.listing;
      setText(
        tabLine,
        t('sync.tab', {
          listing: listing ? listingText(t, listing) : t('sync.tab.none'),
        }),
      );
      const current = state.syncs.find(
        (view) =>
          view.tabId === tab.id &&
          tab.id !== null &&
          view.listingKey === listing?.key &&
          view.state === 'open',
      );
      const sending = state.syncs.find(
        (view) =>
          view.tabId === tab.id &&
          view.listingKey === listing?.key &&
          view.state === 'ended' &&
          view.queued > 0,
      );
      const previous = state.syncs.find(
        (view) => view.listingKey === listing?.key && view.state === 'ended' && view !== sending,
      );

      reason.hidden = availability.ok || !!current;
      if (!availability.ok) setText(reason, refusalText(ctx, availability.reason));

      // The chooser: explicit syncs of IG folders and Pinterest boards (IMP-10).
      const folder = availability.ok && listing?.externalId !== null && !current;
      chooser.hidden = !folder;
      if (folder && listing && namedFor !== listing.key) {
        namedFor = listing.key;
        name.value = tab.pong?.heading ?? deslugify(listing.name, listing.name ?? '');
      }

      start.hidden = !!current;
      start.disabled = !availability.ok;
      stop.hidden = !current;
      stop.disabled = false;

      const shown = current ?? sending;
      live.hidden = !shown;
      if (shown) setText(live, liveText(t, shown));
      const done = sending ?? previous;
      last.hidden = !done || !!current || !!sending;
      if (done && !current && !sending)
        setText(
          last,
          t('sync.last', {
            time: ctx.time(done.endedAt ?? done.startedAt),
            result: resultText(t, done),
          }),
        );
      const skippedOf = shown ?? done;
      skipped.hidden = !skippedOf?.skipped.length;
      if (skippedOf?.skipped.length)
        setText(
          skipped,
          t('sync.skipped', {
            modes: skippedOf.skipped.map((mode) => t(`sync.mode.${mode}`)).join(', '),
          }),
        );
    };

    let request = 0;
    return {
      update(state: PanelState): void {
        const mine = ++request;
        render(state);
        activeTab()
          .then((next) => {
            if (mine !== request) return;
            if (next.id !== tab.id || next.url !== tab.url) setText(status, '');
            tab = next;
            render(state);
          })
          .catch(() => undefined);
      },
    };
  },
};
