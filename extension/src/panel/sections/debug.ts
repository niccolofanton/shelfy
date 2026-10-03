// Debug builds only (`build.ts --debug`): the MAIN-world request census — requests whose
// responses the hook tries to parse, by endpoint. Many requests but no captured items means the
// page returned saved items in a shape the parsers do not read (SPIKE-3).

import { PLATFORMS, type CensusCounts } from '../../shared/protocol';
import type { PanelState } from '../../sw/state';
import { el } from '../dom';
import type { PanelContext, PanelSection } from '../section';

export function censusRows(census: CensusCounts | null): Array<[string, number]> {
  if (!census) return [];
  return Object.entries(census)
    .filter(([key]) => PLATFORMS.some((platform) => key.startsWith(`${platform}|`)))
    .map(([key, count]): [string, number] => [key.replace('|', ' · '), count])
    .sort((a, b) => a[0].localeCompare(b[0]));
}

export const debugSection: PanelSection = {
  id: 'debug',
  visible: (state) => state.debug,
  mount(root: HTMLElement, ctx: PanelContext) {
    const body = el('div');
    root.append(el('h2', { text: ctx.t('debug.title') }), body);
    return {
      update(state: PanelState): void {
        const rows = censusRows(state.census);
        if (!rows.length) {
          body.replaceChildren(el('p', { className: 'muted', text: ctx.t('debug.empty') }));
          return;
        }
        const table = el('table');
        const tbody = table.createTBody();
        for (const [label, count] of rows) {
          const tr = tbody.insertRow();
          tr.append(el('td', { text: label }), el('td', { text: count }));
        }
        body.replaceChildren(table);
      },
    };
  },
};
