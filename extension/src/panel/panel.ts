// The side panel shell: mounts the registered sections (sections.ts), pulls the worker's state
// snapshot (MSG.stateGet) and hands it to every section. It pulls again when the worker says
// something changed, when the active tab changes, and every few seconds for the clocks.

import { createTranslate, hasMessage, pickLang } from '../shared/i18n';
import { MSG, isRecord } from '../shared/protocol';
import type { PanelState } from '../sw/state';
import { byId, el, setText } from './dom';
import type { PanelContext, SectionView } from './section';
import { SECTIONS } from './sections';

const REFRESH_EVERY_MS = 5000;

const lang = pickLang(navigator.languages);
const t = createTranslate(lang);
const locale = lang === 'it' ? 'it-IT' : 'en-US';
document.documentElement.lang = lang;

let refreshTimer: ReturnType<typeof setTimeout> | null = null;

const ctx: PanelContext = {
  t,
  lang,
  send: <T>(message: unknown) => chrome.runtime.sendMessage<T>(message),
  refresh: () => scheduleRefresh(0),
  time: (ms) => new Date(ms).toLocaleTimeString(locale),
  date: (ms) => new Date(ms).toLocaleDateString(locale),
  errorText: (code) =>
    hasMessage(`error.${code}`) ? t(`error.${code}`) : t('error.unknown', { code }),
};

const container = byId('sections');
const mounted: Array<{
  root: HTMLElement;
  view: SectionView;
  visible: (s: PanelState) => boolean;
}> = SECTIONS.map((section) => {
  const root = el('section', { attrs: { 'data-section': section.id } });
  // A section that depends on the state stays hidden until the first state arrives.
  root.hidden = section.visible !== undefined;
  container.append(root);
  return {
    root,
    view: section.mount(root, ctx),
    visible: section.visible ?? (() => true),
  };
});

async function refresh(): Promise<void> {
  const state = await chrome.runtime.sendMessage<PanelState>({ kind: MSG.stateGet });
  if (!isRecord(state) || typeof state.version !== 'string') return;
  setText(
    byId('subtitle'),
    [t('panel.version', { version: state.version }), state.debug ? t('panel.debugBuild') : null]
      .filter(Boolean)
      .join(' · '),
  );
  for (const section of mounted) {
    const visible = section.visible(state);
    section.root.hidden = !visible;
    if (visible) section.view.update(state);
  }
}

function scheduleRefresh(delay: number): void {
  if (refreshTimer) clearTimeout(refreshTimer);
  refreshTimer = setTimeout(() => {
    refreshTimer = null;
    refresh()
      .catch(() => undefined)
      .finally(() => {
        if (!refreshTimer) scheduleRefresh(REFRESH_EVERY_MS);
      });
  }, delay);
}

// The worker announces changes; the panel pulls. Never answer: other listeners (the worker) own
// every other message.
chrome.runtime.onMessage.addListener((message) => {
  if (isRecord(message) && message.kind === MSG.stateChanged) scheduleRefresh(50);
  return false;
});
chrome.tabs.onActivated.addListener(() => scheduleRefresh(0));
chrome.tabs.onUpdated.addListener((_tabId, change) => {
  if (change.url || change.status === 'complete') scheduleRefresh(100);
});

scheduleRefresh(0);
