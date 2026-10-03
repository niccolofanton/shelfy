// Passive capture: one toggle per platform (on by default; the server's kill switch shows next
// to it), folder mapping, items captured, and what capture does in the active tab.

import { MSG, PLATFORMS, type BridgePong, type Platform } from '../../shared/protocol';
import type { PanelState } from '../../sw/state';
import { el, setText } from '../dom';
import type { PanelContext, PanelSection } from '../section';
import { tabStatus, tabStatusText } from '../tab-status';

async function activeTab(): Promise<{ url: string | undefined; pong: BridgePong | null }> {
  const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
  if (!tab || typeof tab.id !== 'number') return { url: undefined, pong: null };
  const pong = await chrome.tabs
    .sendMessage<BridgePong>(tab.id, { kind: MSG.bridgePing })
    .then((answer) => (answer?.ok ? answer : null))
    .catch(() => null);
  return { url: tab.url, pong };
}

export const captureSection: PanelSection = {
  id: 'capture',
  mount(root: HTMLElement, ctx: PanelContext) {
    const { t } = ctx;
    const needsPairing = el('p', { className: 'notice', text: t('capture.needsPairing') });
    needsPairing.hidden = true;
    const rows = {} as Record<
      Platform,
      { input: HTMLInputElement; captured: HTMLSpanElement; killed: HTMLSpanElement }
    >;
    const list = el('div');
    for (const platform of PLATFORMS) {
      const input = el('input', {
        attrs: { type: 'checkbox', 'data-testid': `passive-${platform}` },
      });
      input.addEventListener('change', () => {
        void ctx
          .send({ kind: MSG.settingsSet, patch: { passive: { [platform]: input.checked } } })
          .finally(ctx.refresh);
      });
      const captured = el('span', { className: 'muted' });
      const killed = el('span', { className: 'warn', text: t('capture.killed') });
      killed.hidden = true;
      const label = el('label', { className: 'toggle' });
      label.append(input, el('span', { text: t(`platform.${platform}`) }), captured, killed);
      list.append(label);
      rows[platform] = { input, captured, killed };
    }
    const folders = el('input', { attrs: { type: 'checkbox', 'data-testid': 'passive-folders' } });
    folders.addEventListener('change', () => {
      void ctx
        .send({ kind: MSG.settingsSet, patch: { passiveFolders: folders.checked } })
        .finally(ctx.refresh);
    });
    const foldersLabel = el('label', { className: 'toggle' });
    foldersLabel.append(folders, el('span', { text: t('capture.folders') }));
    const tabLine = el('p', { className: 'card', attrs: { 'data-testid': 'tab-status' } });

    root.append(
      el('h2', { text: t('capture.title') }),
      el('p', { className: 'muted', text: t('capture.hint') }),
      needsPairing,
      list,
      foldersLabel,
      tabLine,
    );

    let tabRequest = 0;
    return {
      update(state: PanelState): void {
        needsPairing.hidden = state.paired;
        for (const platform of PLATFORMS) {
          const row = rows[platform];
          row.input.checked = state.passive[platform];
          row.killed.hidden = state.serverPassive[platform];
          setText(row.captured, t('capture.captured', { count: state.queue.captured[platform] }));
        }
        folders.checked = state.passiveFolders;
        const request = ++tabRequest;
        activeTab()
          .then(({ url, pong }) => {
            if (request === tabRequest)
              setText(tabLine, tabStatusText(t, tabStatus(url, pong, state)));
          })
          .catch(() => undefined);
      },
    };
  },
};
