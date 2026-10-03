// Settings → Connections (plan §2.19 Settings; P2-12): the browser extension
// (detection by `shelfy.ping`, pairing, paired browsers), the iOS Shortcut's
// token and setup steps, and the bookmarklet and Android share. A code or a
// token needs a sign-in from the last 5 minutes: the client asks the user to
// confirm who they are, then carries on.
import TokenExpirySelect from './TokenExpirySelect';
import { DEFAULT_TOKEN_TTL_DAYS } from '../../api/account';
import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Check, Copy, Globe, MonitorSmartphone, Plus, Puzzle, Smartphone } from 'lucide-react';
import type {
  AccountApi,
  AccountToken,
  ExtensionProbe,
  ExtensionStatus,
  NewToken,
} from '../../api/account';
import { useFailureText } from '../../hooks/useFailureText';
import { useLang, useT, type Translate } from '../../i18n';
import { formatDateTime } from './format';
import {
  BUTTON,
  Card,
  CardHeader,
  ConfirmAction,
  INPUT,
  InlineNote,
  Loading,
  PRIMARY_BUTTON,
} from './ui';

const PAIR_ERRORS = new Set([
  'invalid_pairing_code',
  'network',
  'access_redirect',
  'bad_response',
  'bad_request',
  'rate_limited',
  'extension_outdated',
  'unreachable',
]);

function pairErrorText(code: string, t: Translate): string {
  return PAIR_ERRORS.has(code) ? t(`pairError_${code}`) : t('pairError_other', { code });
}

// The token's list for one kind, kept in step by `reload`.
function useTokens(account: AccountApi, kind: AccountToken['kind']) {
  const [items, setItems] = useState<AccountToken[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const reload = useCallback(async () => {
    try {
      const all = await account.listTokens();
      setItems(all.filter((token) => token.kind === kind));
      setError(null);
    } catch (err) {
      setError(err);
    }
  }, [account, kind]);
  useEffect(() => {
    void reload();
  }, [reload]);
  return { items, error, reload };
}

function TokenRows({
  items,
  account,
  reload,
  onError,
  testId,
  empty,
}: {
  items: AccountToken[];
  account: AccountApi;
  reload: () => Promise<void>;
  onError: (err: unknown) => void;
  testId: string;
  empty: string;
}): React.JSX.Element {
  const t = useT('connections');
  const ts = useT('settings');
  const { lang } = useLang();
  if (items.length === 0) {
    return (
      <p data-testid={`${testId}-empty`} className="text-xs text-gray-500">
        {empty}
      </p>
    );
  }
  return (
    <ul className="divide-y divide-[#242424]" data-testid={testId}>
      {items.map((token) => (
        <li key={token.id} data-testid={`${testId}-row`} className="flex items-center gap-3 py-2.5">
          <div className="flex-1 min-w-0">
            <p className="text-sm text-gray-200 truncate">{token.label || token.kind}</p>
            <p className="text-[11px] text-gray-500 mt-0.5">
              {token.lastUsedAt
                ? t('lastUsed', { date: formatDateTime(token.lastUsedAt, lang) })
                : t('neverUsed')}
              {token.expiresAt != null &&
                ` · ${ts('tokenExpires', { date: formatDateTime(token.expiresAt, lang) })}`}
            </p>
          </div>
          <ConfirmAction
            testId={`${testId}-revoke`}
            label={t('revoke')}
            onConfirm={async () => {
              try {
                await account.revokeToken(token.id);
                await reload();
              } catch (err) {
                onError(err);
              }
            }}
          />
        </li>
      ))}
    </ul>
  );
}

function ExtensionCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('connections');
  const { lang } = useLang();
  const failure = useFailureText();
  const bridge = account.extension;
  const [probe, setProbe] = useState<ExtensionProbe | null>(null);
  const [checking, setChecking] = useState(false);
  const [status, setStatus] = useState<ExtensionStatus | null>(null);
  const [pairing, setPairing] = useState(false);
  const [paired, setPaired] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const tokens = useTokens(account, 'extension');
  // Events that arrive before the first answer win over it.
  const sawEvent = useRef(false);

  const check = useCallback(async () => {
    if (!bridge) {
      setProbe({ state: 'unsupported' });
      return;
    }
    setChecking(true);
    try {
      setProbe(await bridge.probe());
    } finally {
      setChecking(false);
    }
  }, [bridge]);
  useEffect(() => {
    void check();
  }, [check]);

  useEffect(() => {
    const off = account.onExtensionStatus((next) => {
      sawEvent.current = true;
      setStatus(next);
    });
    account
      .extensionStatus()
      .then((first) => {
        if (!sawEvent.current) setStatus(first);
      })
      .catch(() => {});
    return off;
  }, [account]);

  const pair = async (): Promise<void> => {
    if (!bridge) return;
    setPairing(true);
    setPaired(false);
    setError(null);
    try {
      const { code } = await account.createPairingCode();
      const result = await bridge.pair(code);
      if (result.ok) {
        setPaired(true);
        await Promise.all([check(), tokens.reload()]);
      } else {
        setError(pairErrorText(result.code, t));
      }
    } catch (err) {
      setError(failure(err));
    } finally {
      setPairing(false);
    }
  };

  return (
    <Card testId="connections-extension">
      <CardHeader icon={Puzzle} title={t('extTitle')} description={t('extDesc')} />
      <p className="mb-3 text-xs text-secondary">{t('syncDesktopHelp')}</p>
      <div className="mt-4 space-y-3">
        {probe === null && <Loading label={t('extChecking')} />}

        {probe?.state === 'unsupported' && (
          <InlineNote tone="info" testId="ext-unsupported">
            {t('extUnsupported')}
          </InlineNote>
        )}

        {probe?.state === 'missing' && (
          <div data-testid="ext-missing" className="space-y-3">
            <p className="text-sm text-gray-200">{t('extMissingTitle')}</p>
            <p className="text-xs text-gray-400 leading-relaxed">{t('extMissing')}</p>
            <div>
              <p className="text-[11px] font-medium text-gray-400 mb-1">{t('extStepsTitle')}</p>
              <ol className="list-decimal pl-5 space-y-1 text-xs text-gray-400 leading-relaxed">
                {[1, 2, 3, 4].map((n) => (
                  <li key={n}>{t(`extStep${n}`)}</li>
                ))}
              </ol>
            </div>
            <button
              type="button"
              data-testid="ext-check"
              className={BUTTON}
              disabled={checking}
              onClick={() => void check()}
            >
              {checking ? t('extChecking') : t('extCheckAgain')}
            </button>
          </div>
        )}

        {probe?.state === 'ready' && (
          <div data-testid="ext-ready" className="space-y-3">
            <div className="flex flex-wrap items-center gap-2 text-xs">
              {probe.paired ? (
                <span
                  data-testid="ext-state"
                  data-connected={status?.connected ? 'true' : 'false'}
                  className={`rounded-full px-2 py-0.5 ${
                    status?.connected
                      ? 'bg-emerald-950/60 text-emerald-300'
                      : 'bg-[#2a2a2a] text-gray-300'
                  }`}
                >
                  {status?.connected ? t('extConnected') : t('extDisconnected')}
                </span>
              ) : (
                <span data-testid="ext-state" data-connected="false" className="text-gray-400">
                  {t('extUnpaired')}
                </span>
              )}
              <span className="text-gray-500">{t('extVersion', { version: probe.version })}</span>
              {probe.paired && status && (
                <span className="text-gray-500">
                  {status.lastSeenAt
                    ? t('extLastSeen', { date: formatDateTime(status.lastSeenAt, lang) })
                    : t('extNeverSeen')}
                </span>
              )}
            </div>
            {probe.outdated && (
              <InlineNote tone="error" testId="ext-outdated">
                {t('extOutdated')}
              </InlineNote>
            )}
            <button
              type="button"
              data-testid="ext-pair"
              className={probe.paired ? BUTTON : PRIMARY_BUTTON}
              disabled={pairing}
              onClick={() => void pair()}
            >
              {pairing ? t('extPairing') : probe.paired ? t('extPairAgain') : t('extPair')}
            </button>
          </div>
        )}

        {paired && (
          <InlineNote tone="ok" testId="ext-paired">
            {t('extPaired')}
          </InlineNote>
        )}
        {error && (
          <InlineNote tone="error" testId="ext-error">
            {error}
          </InlineNote>
        )}
      </div>

      <div className="mt-5 border-t border-[#242424] pt-4">
        <p className="text-[11px] font-medium text-gray-400 mb-1">{t('browsersTitle')}</p>
        {tokens.items === null ? (
          <Loading />
        ) : (
          <TokenRows
            items={tokens.items}
            account={account}
            reload={tokens.reload}
            onError={(err) => setError(failure(err))}
            testId="ext-tokens"
            empty={t('browsersEmpty')}
          />
        )}
        {!!tokens.error && (
          <InlineNote tone="error" testId="ext-tokens-error">
            {failure(tokens.error)}
          </InlineNote>
        )}
      </div>
    </Card>
  );
}

function ShortcutCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('connections');
  const failure = useFailureText();
  const tokens = useTokens(account, 'shortcut');
  const [form, setForm] = useState<{ label: string; ttlDays: number } | null>(null);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<NewToken | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const endpoint = `${window.location.origin}/api/v1/links`;

  const create = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!form) return;
    setBusy(true);
    setError(null);
    try {
      const token = await account.createToken('shortcut', form.label, { ttlDays: form.ttlDays });
      setForm(null);
      setCopied(false);
      setCreated(token);
      await tokens.reload();
    } catch (err) {
      setError(failure(err));
    } finally {
      setBusy(false);
    }
  };

  const copy = async (): Promise<void> => {
    if (!created) return;
    try {
      await navigator.clipboard.writeText(created.value);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <Card testId="connections-shortcut">
      <CardHeader icon={Smartphone} title={t('scTitle')} description={t('scDesc')} />
      <div className="mt-4 space-y-3">
        {created && (
          <div
            data-testid="sc-created"
            className="rounded-lg border border-amber-900/50 bg-amber-950/20 p-3"
          >
            <p className="text-sm font-medium text-amber-200">{t('scValueTitle')}</p>
            <p className="mt-1 text-xs leading-relaxed text-amber-100/80">{t('scValueWarning')}</p>
            <div className="mt-2 flex items-center gap-2">
              <input
                readOnly
                data-testid="sc-value"
                aria-label={t('scValueTitle')}
                value={created.value}
                onFocus={(e) => e.currentTarget.select()}
                className={`${INPUT} font-mono text-xs`}
              />
              <button type="button" onClick={copy} className={BUTTON} data-testid="sc-copy">
                {copied ? <Check size={13} /> : <Copy size={13} />}
                {copied ? t('scCopied') : t('scCopy')}
              </button>
              <button
                type="button"
                data-testid="sc-done"
                className={BUTTON}
                onClick={() => setCreated(null)}
              >
                {t('scDone')}
              </button>
            </div>
          </div>
        )}

        {form ? (
          <form onSubmit={create} data-testid="sc-form" className="flex flex-wrap items-end gap-2">
            <label className="flex-1 space-y-1">
              <span className="text-[11px] font-medium text-gray-400">{t('scLabel')}</span>
              <input
                data-testid="sc-label"
                className={INPUT}
                maxLength={64}
                placeholder={t('scLabelPlaceholder')}
                value={form.label}
                onChange={(e) => setForm({ ...form, label: e.target.value })}
              />
            </label>
            <TokenExpirySelect
              testId="sc-expiry"
              value={form.ttlDays}
              onChange={(ttlDays) => setForm({ ...form, ttlDays })}
              disabled={busy}
            />
            <button
              type="submit"
              disabled={busy}
              className={PRIMARY_BUTTON}
              data-testid="sc-submit"
            >
              {t('scCreate')}
            </button>
            <button type="button" className={BUTTON} onClick={() => setForm(null)}>
              {t('scCancel')}
            </button>
          </form>
        ) : (
          <button
            type="button"
            data-testid="sc-new"
            className={PRIMARY_BUTTON}
            onClick={() => {
              setCreated(null);
              setForm({ label: '', ttlDays: DEFAULT_TOKEN_TTL_DAYS });
            }}
          >
            <Plus size={13} />
            {t('scNew')}
          </button>
        )}
        {error && (
          <InlineNote tone="error" testId="sc-error">
            {error}
          </InlineNote>
        )}

        <div>
          <p className="text-[11px] font-medium text-gray-400 mb-1">{t('scStepsTitle')}</p>
          <ol
            data-testid="sc-steps"
            className="list-decimal pl-5 space-y-1 text-xs text-gray-400 leading-relaxed"
          >
            <li>{t('scStep1')}</li>
            <li>
              {t('scStep2')} <code className="text-gray-200 break-all">{endpoint}</code>
            </li>
            <li>{t('scStep3')}</li>
            <li>{t('scStep4')}</li>
            <li>{t('scStep5')}</li>
            <li>{t('scStep6')}</li>
          </ol>
        </div>
      </div>

      <div className="mt-5 border-t border-[#242424] pt-4">
        {tokens.items === null ? (
          <Loading />
        ) : (
          <TokenRows
            items={tokens.items}
            account={account}
            reload={tokens.reload}
            onError={(err) => setError(failure(err))}
            testId="sc-tokens"
            empty={t('scEmpty')}
          />
        )}
        {!!tokens.error && (
          <InlineNote tone="error" testId="sc-tokens-error">
            {failure(tokens.error)}
          </InlineNote>
        )}
      </div>
    </Card>
  );
}

// What Chromium fires when the app can be installed.
interface InstallPromptEvent extends Event {
  prompt(): Promise<void>;
}

function ShareCard(): React.JSX.Element {
  const t = useT('connections');
  const [copied, setCopied] = useState(false);
  const [install, setInstall] = useState<InstallPromptEvent | null>(null);
  const standalone =
    typeof window.matchMedia === 'function' &&
    window.matchMedia('(display-mode: standalone)').matches;
  const bookmarklet = `javascript:(function(){window.open(${JSON.stringify(
    `${window.location.origin}/share?url=`,
  )}+encodeURIComponent(location.href),'_blank');})();`;

  useEffect(() => {
    const onPrompt = (e: Event): void => {
      e.preventDefault();
      setInstall(e as InstallPromptEvent);
    };
    window.addEventListener('beforeinstallprompt', onPrompt);
    return () => window.removeEventListener('beforeinstallprompt', onPrompt);
  }, []);

  const copy = async (): Promise<void> => {
    try {
      await navigator.clipboard.writeText(bookmarklet);
      setCopied(true);
    } catch {
      setCopied(false);
    }
  };

  return (
    <Card testId="connections-share">
      <CardHeader icon={Globe} title={t('shareTitle')} description={t('shareDesc')} />
      <div className="mt-4 space-y-4">
        <div>
          <p className="text-[11px] font-medium text-gray-400">{t('bookmarklet')}</p>
          <p className="text-xs text-gray-500 mt-0.5 mb-2">{t('bookmarkletHelp')}</p>
          <div className="flex items-center gap-2">
            <input
              readOnly
              data-testid="bookmarklet"
              aria-label={t('bookmarklet')}
              value={bookmarklet}
              onFocus={(e) => e.currentTarget.select()}
              className={`${INPUT} font-mono text-xs`}
            />
            <button type="button" className={BUTTON} data-testid="bookmarklet-copy" onClick={copy}>
              {copied ? <Check size={13} /> : <Copy size={13} />}
              {copied ? t('scCopied') : t('scCopy')}
            </button>
          </div>
        </div>
        <div>
          <p className="flex items-center gap-1.5 text-[11px] font-medium text-gray-400">
            <MonitorSmartphone size={13} />
            {t('androidTitle')}
          </p>
          <p className="text-xs text-gray-500 mt-0.5">{t('androidHelp')}</p>
          {install && (
            <button
              type="button"
              data-testid="install-app"
              className={`${BUTTON} mt-2`}
              onClick={() => {
                void install.prompt();
                setInstall(null);
              }}
            >
              {t('installApp')}
            </button>
          )}
          {standalone && (
            <p data-testid="app-installed" className="text-xs text-gray-400 mt-2">
              {t('installed')}
            </p>
          )}
        </div>
      </div>
    </Card>
  );
}

export default function ConnectionsSection({
  account,
}: {
  account: AccountApi;
}): React.JSX.Element {
  return (
    <div className="space-y-4">
      <ExtensionCard account={account} />
      <ShortcutCard account={account} />
      <ShareCard />
    </div>
  );
}
