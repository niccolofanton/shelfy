// Connection: pairing state, an outdated build, the last error, "Check connection" (the P2-22
// Access probe: cookie mode, header mode, the token) and the Access service-token headers.
// Pairing itself starts in Shelfy (Settings → Connections), which hands the code over (C9).

import { MSG } from '../../shared/protocol';
import type { ConnectionCheck, Probe } from '../../sw/settings';
import type { PanelState } from '../../sw/state';
import { button, confirmButton, el, setText } from '../dom';
import type { PanelContext, PanelSection } from '../section';

const ACCESS_VALUE = /^[\x21-\x7e]{1,512}$/;

function probeLine(ctx: PanelContext, mode: 'cookie' | 'headers', probe: Probe): string {
  const detail = probe.detail ?? (probe.status !== null ? `HTTP ${probe.status}` : '?');
  return ctx.t(`connection.${mode}.${probe.outcome}`, { detail });
}

export function connectionLines(ctx: PanelContext, check: ConnectionCheck): string[] {
  const lines = [probeLine(ctx, 'cookie', check.cookie)];
  lines.push(
    check.headers ? probeLine(ctx, 'headers', check.headers) : ctx.t('connection.headers.unset'),
  );
  lines.push(
    ctx.t(`connection.token.${check.token.outcome}`, { detail: check.token.detail ?? '?' }),
  );
  const version = check.cookie.version ?? check.headers?.version ?? null;
  if (version) lines.push(ctx.t('connection.serverVersion', { version }));
  return lines;
}

export const connectionSection: PanelSection = {
  id: 'connection',
  mount(root: HTMLElement, ctx: PanelContext) {
    const { t } = ctx;
    const server = el('p', { className: 'muted' });
    const pairing = el('p', { attrs: { 'data-testid': 'pairing-state' } });
    const hint = el('p', { className: 'muted', text: t('connection.unpairedHint') });
    const outdated = el('p', { className: 'notice', attrs: { 'data-testid': 'outdated' } });
    const lastError = el('p', { className: 'warn', attrs: { 'data-testid': 'last-error' } });

    const checkStatus = el('p', { className: 'status', attrs: { role: 'status' } });
    const results = el('ul', {
      className: 'plain',
      attrs: { 'data-testid': 'connection-results' },
    });
    const renderCheck = (check: ConnectionCheck | null): void => {
      results.replaceChildren(
        ...(check ? connectionLines(ctx, check) : []).map((line) => el('li', { text: line })),
      );
      setText(checkStatus, check ? t('connection.checkedAt', { time: ctx.time(check.at) }) : '');
    };
    const checkButton = button(
      t('connection.check'),
      () => {
        checkButton.disabled = true;
        setText(checkStatus, t('connection.checking'));
        ctx
          .send<{ ok: boolean; check?: ConnectionCheck }>({ kind: MSG.connectionCheck })
          .then((answer) => renderCheck(answer.check ?? null))
          .catch(() => setText(checkStatus, t('error.unknown', { code: 'check' })))
          .finally(() => {
            checkButton.disabled = false;
            ctx.refresh();
          });
      },
      { testId: 'check-connection' },
    );

    const forget = confirmButton(
      t('connection.forget'),
      t('connection.forgetConfirm'),
      () => void ctx.send({ kind: MSG.pairingForget }).finally(ctx.refresh),
      { className: 'danger', testId: 'forget-pairing' },
    );

    const accessState = el('p', { className: 'muted', attrs: { 'data-testid': 'access-state' } });
    const accessStatus = el('p', { className: 'status', attrs: { role: 'status' } });
    const clientId = el('input', {
      attrs: { type: 'text', autocomplete: 'off', spellcheck: 'false', 'data-testid': 'access-id' },
    });
    const clientSecret = el('input', {
      attrs: { type: 'password', autocomplete: 'off', 'data-testid': 'access-secret' },
    });
    const field = (label: string, input: HTMLInputElement): HTMLLabelElement => {
      const node = el('label', { className: 'field', text: label });
      node.append(input);
      return node;
    };
    const saveAccess = (access: { clientId: string; clientSecret: string } | null): void => {
      ctx
        .send<{ ok: boolean }>({ kind: MSG.settingsSet, patch: { access } })
        .then((answer) => {
          if (!answer.ok) setText(accessStatus, t('connection.accessInvalid'));
          else {
            clientId.value = '';
            clientSecret.value = '';
            setText(accessStatus, '');
          }
        })
        .finally(ctx.refresh);
    };
    const save = button(
      t('connection.accessSave'),
      () => {
        const id = clientId.value.trim();
        const secret = clientSecret.value.trim();
        if (!ACCESS_VALUE.test(id) || !ACCESS_VALUE.test(secret)) {
          setText(accessStatus, t('connection.accessInvalid'));
          return;
        }
        saveAccess({ clientId: id, clientSecret: secret });
      },
      { testId: 'access-save' },
    );
    const clear = button(t('connection.accessClear'), () => saveAccess(null), {
      className: 'danger',
      testId: 'access-clear',
    });

    const checkActions = el('div', { className: 'actions' });
    checkActions.append(checkButton, forget);
    const accessActions = el('div', { className: 'actions' });
    accessActions.append(save, clear);
    root.append(
      el('h2', { text: t('connection.title') }),
      server,
      pairing,
      hint,
      outdated,
      lastError,
      checkActions,
      checkStatus,
      results,
      el('h3', { text: t('connection.accessTitle') }),
      el('p', { className: 'muted', text: t('connection.accessHint') }),
      accessState,
      field(t('connection.accessClientId'), clientId),
      field(t('connection.accessClientSecret'), clientSecret),
      accessActions,
      accessStatus,
    );

    // Shown by update() when they apply.
    for (const node of [hint, outdated, lastError, forget]) node.hidden = true;

    let renderedCheckAt: number | null = null;
    return {
      update(state: PanelState): void {
        setText(server, t('connection.server', { host: new URL(state.origin).host }));
        if (state.paired && state.pairedAt)
          setText(pairing, t('connection.paired', { date: ctx.date(state.pairedAt) }), 'ok');
        else setText(pairing, t('connection.unpaired'), 'warn');
        hint.hidden = state.paired;
        forget.hidden = !state.paired;
        outdated.hidden = !state.outdated;
        setText(outdated, t('connection.outdated', { min: state.minVersion ?? '?' }));
        lastError.hidden = !state.lastError;
        if (state.lastError)
          setText(
            lastError,
            t('connection.lastError', {
              time: ctx.time(state.lastError.at),
              message: ctx.errorText(state.lastError.code),
            }),
          );
        setText(
          accessState,
          t(state.accessHeadersSet ? 'connection.accessSet' : 'connection.accessUnset'),
        );
        clear.disabled = !state.accessHeadersSet;
        if (state.connection && state.connection.at !== renderedCheckAt) {
          renderedCheckAt = state.connection.at;
          renderCheck(state.connection);
        }
      },
    };
  },
};
