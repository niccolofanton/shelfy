// The contract between the side-panel shell (panel.ts) and its sections (sections.ts). A section
// mounts once into its own <section> element and gets every new state snapshot.

import type { Lang, Translate } from '../shared/i18n';
import type { PanelState } from '../sw/state';

export interface PanelContext {
  t: Translate;
  lang: Lang;
  /** chrome.runtime.sendMessage to the worker. */
  send<T = unknown>(message: unknown): Promise<T>;
  /** Pull the state again now. */
  refresh(): void;
  time(ms: number): string;
  date(ms: number): string;
  /** The panel text of an error code (`error.<code>`, else a generic line with the code). */
  errorText(code: string): string;
}

export interface SectionView {
  update(state: PanelState): void;
}

export interface PanelSection {
  id: string;
  /** Shown only when this returns true (default: always). */
  visible?(state: PanelState): boolean;
  mount(root: HTMLElement, ctx: PanelContext): SectionView;
}
