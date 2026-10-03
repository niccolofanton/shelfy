// Upload queue: items waiting and batches ready, what the server answered, items dropped or
// refused, what was not captured and why, the next retry and "Retry now".

import { MSG } from '../../shared/protocol';
import type { Translate } from '../../shared/i18n';
import { DISCARD_REASONS, type Counters } from '../../sw/queue/types';
import type { PanelState } from '../../sw/state';
import { button, el, setText } from '../dom';
import type { PanelContext, PanelSection } from '../section';

/** "outside saved listings 12, not paired 3", or null when nothing was discarded. */
export function discardSummary(t: Translate, counters: Counters): string | null {
  const parts = DISCARD_REASONS.filter((reason) => counters.discarded[reason] > 0).map(
    (reason) => `${t(`queue.reason.${reason}`)} ${counters.discarded[reason]}`,
  );
  return parts.length ? t('queue.notCaptured', { list: parts.join(', ') }) : null;
}

export const queueSection: PanelSection = {
  id: 'queue',
  mount(root: HTMLElement, ctx: PanelContext) {
    const { t } = ctx;
    const waiting = el('p', { attrs: { 'data-testid': 'queue-waiting' } });
    const batches = el('p', { className: 'muted' });
    const sent = el('p', { attrs: { 'data-testid': 'queue-sent' } });
    const results = el('p', { className: 'muted' });
    const dropped = el('p', { className: 'warn', attrs: { 'data-testid': 'queue-dropped' } });
    const refused = el('p', { className: 'warn' });
    const notCaptured = el('p', {
      className: 'muted',
      attrs: { 'data-testid': 'queue-discarded' },
    });
    const retryAt = el('p', { className: 'muted', attrs: { 'data-testid': 'queue-retry-at' } });
    const retry = button(
      t('queue.retry'),
      () => {
        retry.disabled = true;
        void ctx.send({ kind: MSG.queueFlush }).finally(() => {
          retry.disabled = false;
          ctx.refresh();
        });
      },
      { testId: 'queue-retry' },
    );
    for (const node of [batches, results, dropped, refused, notCaptured, retryAt])
      node.hidden = true;
    const actions = el('div', { className: 'actions' });
    actions.append(retry);
    root.append(
      el('h2', { text: t('queue.title') }),
      waiting,
      batches,
      sent,
      results,
      dropped,
      refused,
      notCaptured,
      retryAt,
      actions,
    );

    return {
      update(state: PanelState): void {
        const q = state.queue;
        setText(
          waiting,
          q.queuedItems ? t('queue.waiting', { count: q.queuedItems }) : t('queue.empty'),
        );
        batches.hidden = !q.batches;
        setText(batches, t('queue.batches', { count: q.batches }));
        setText(sent, t('queue.sent', { count: q.sentItems }));
        results.hidden = !q.sentBatches;
        setText(
          results,
          t('queue.results', {
            inserted: q.inserted,
            updated: q.updated,
            known: q.known,
            rejected: q.rejected,
          }),
        );
        dropped.hidden = !q.droppedItems;
        setText(dropped, t('queue.dropped', { count: q.droppedItems }));
        refused.hidden = !q.refusedItems;
        setText(refused, t('queue.refused', { count: q.refusedItems }));
        const summary = discardSummary(t, q);
        notCaptured.hidden = !summary;
        setText(notCaptured, summary ?? '');
        const blocked = state.blockedUntil > Date.now() && q.queuedItems > 0;
        retryAt.hidden = !blocked;
        setText(retryAt, t('queue.retryAt', { time: ctx.time(state.blockedUntil) }));
        retry.disabled = !q.queuedItems || !state.paired || state.outdated;
      },
    };
  },
};
