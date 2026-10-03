import { MSG, type BridgePong } from '../../shared/protocol';
import { platformForUrl } from '../../shared/hosts';
import { passiveScope, syncTarget } from '../../shared/listing';
import type { SelectAnswer } from '../../content/select/service';
import { button, el, setText } from '../dom';
import type { PanelSection } from '../section';

export const selectSection: PanelSection = {
  id: 'select',
  mount(root, ctx) {
    const { t } = ctx;
    let tabId: number | null = null,
      enabled = false,
      count = 0,
      pending = false,
      retryPending = false;
    let listingKey: string | null = null;
    const status = el('p', { attrs: { role: 'status', 'data-testid': 'select-status' } });
    const countLine = el('p', { attrs: { 'data-testid': 'select-count' } });
    const chooser = el('fieldset', {
      className: 'chooser',
      attrs: { 'data-testid': 'select-chooser' },
    });
    chooser.append(el('legend', { text: t('sync.chooser.title') }));
    const radio = (value: string, text: string) => {
      const input = el('input', {
        attrs: {
          type: 'radio',
          name: 'select-collection',
          value,
          'data-testid': `select-into-${value}`,
        },
      });
      const label = el('label', { className: 'toggle' });
      label.append(input, el('span', { text }));
      chooser.append(label);
      return input;
    };
    const auto = radio('auto', t('sync.chooser.auto'));
    auto.checked = true;
    const name = el('input', {
      attrs: { type: 'text', maxlength: '120', 'data-testid': 'select-name' },
    });
    const nameLabel = el('label', { className: 'field' });
    nameLabel.append(el('span', { text: t('sync.chooser.name') }), name);
    chooser.append(nameLabel);
    const none = radio('none', t('sync.chooser.none'));
    for (const input of [auto, none])
      input.addEventListener('change', () => {
        name.disabled = !auto.checked;
      });
    const error = (code: string) => {
      const key = `select.error.${code}`;
      if (t(key) !== key) return t(key);
      const syncKey = `sync.refused.${code}`;
      return t(syncKey) !== syncKey ? t(syncKey) : ctx.errorText(code);
    };
    const send = (action: string): Promise<SelectAnswer> =>
      ctx.send({
        kind: MSG.selectCommand,
        tabId,
        action,
        lang: ctx.lang,
        collection: !chooser.hidden && none.checked ? 'none' : 'auto',
        name: !chooser.hidden && auto.checked ? name.value : null,
      });
    const render = () => {
      start.hidden = enabled;
      stop.hidden = !enabled;
      start.disabled = tabId === null || pending;
      stop.disabled = pending;
      submit.disabled = tabId === null || !enabled || (count === 0 && !retryPending) || pending;
      setText(submit, t(retryPending ? 'select.retry' : 'select.import'));
      setText(countLine, t('select.count', { count }));
    };
    const act = (action: string) => {
      if (tabId === null || pending) return;
      pending = true;
      render();
      setText(status, action === 'import' ? t('select.importing') : '');
      void send(action)
        .then((answer) => {
          if (answer.ok) {
            enabled = answer.enabled;
            count = answer.count;
            setText(
              status,
              answer.imported !== undefined ? t('select.imported', { count: answer.imported }) : '',
            );
          } else setText(status, error(answer.code));
        })
        .catch(() => setText(status, ctx.errorText('internal')))
        .finally(() => {
          pending = false;
          render();
          ctx.refresh();
        });
    };
    const start = button(t('select.start'), () => act('enable'), { testId: 'select-start' });
    const stop = button(t('select.stop'), () => act('disable'), { testId: 'select-stop' });
    const submit = button(t('select.import'), () => act('import'), { testId: 'select-import' });
    const actions = el('div', { className: 'actions' });
    actions.append(start, stop, submit);
    root.append(
      el('h2', { text: t('select.title') }),
      el('p', { className: 'muted', text: t('select.hint') }),
      chooser,
      countLine,
      actions,
      status,
    );
    chooser.hidden = true;
    render();
    let request = 0;
    return {
      update(state) {
        if (pending) return;
        const mine = ++request;
        void (async () => {
          const [tab] = await chrome.tabs.query({ active: true, currentWindow: true });
          const platform = platformForUrl(tab?.url ?? '');
          const listing = platform && tab?.url ? syncTarget(platform, tab.url) : null;
          const pong =
            tab?.id !== undefined && listing
              ? await chrome.tabs
                  .sendMessage<BridgePong>(tab.id, { kind: MSG.bridgePing }, { frameId: 0 })
                  .catch(() => null)
              : null;
          if (mine !== request || pending) return;
          const refusal = !state.paired
            ? 'not_paired'
            : state.outdated
              ? 'outdated'
              : !listing
                ? 'not_a_listing'
                : !pong?.ok
                  ? 'reload_tab'
                  : pong.syncing
                    ? 'busy'
                    : platform === 'pinterest' &&
                        tab?.url &&
                        !passiveScope(platform, tab.url, pong.viewer).ok
                      ? 'not_own_board'
                      : null;
          if (listingKey !== listing?.key || tabId !== tab?.id) {
            listingKey = listing?.key ?? null;
            enabled = false;
            count = 0;
            retryPending = false;
            name.value = pong?.heading ?? listing?.name ?? '';
            setText(status, '');
          }
          tabId = refusal ? null : (tab?.id ?? null);
          chooser.hidden = !listing || listing.externalId === null || !!refusal;
          if (refusal) setText(status, error(refusal));
          if (tabId !== null) {
            const answer = await send('status');
            if (mine !== request || pending) return;
            if (answer.ok) {
              enabled = answer.enabled;
              count = answer.count;
              retryPending = answer.pending === true;
            } else {
              tabId = null;
              setText(status, error(answer.code));
            }
          }
          root.dataset.activeUrl = tab?.url ?? '';
          render();
        })().catch(() => undefined);
      },
    };
  },
};
