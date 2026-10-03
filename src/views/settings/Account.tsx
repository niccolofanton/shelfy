// Settings → Account on a server (plan §2.19 Settings; P1-20): the profile
// with its read-only email (owner decision E4), the passkeys, the signed-in
// sessions and the API tokens. Adding or removing a passkey and creating a
// token need a sign-in from the last 5 minutes: the client asks the user to
// confirm who they are, then carries on.
import TokenExpirySelect from './TokenExpirySelect';
import { DEFAULT_TOKEN_TTL_DAYS } from '../../api/account';
import React, { useCallback, useEffect, useState } from 'react';
import {
  Check,
  Copy,
  Fingerprint,
  KeyRound,
  KeySquare,
  LogOut,
  Monitor,
  Plus,
  Trash2,
  UserRound,
} from 'lucide-react';
import type {
  AccountApi,
  AccountPasskey,
  AccountSession,
  AccountToken,
  NewToken,
  PasskeyDraft,
  TokenKind,
} from '../../api/account';
import { useFailureText } from '../../hooks/useFailureText';
import { useLang, useT, type Translate } from '../../i18n';
import { describeUserAgent, formatDate, formatDateTime } from './format';
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

// "Chrome on macOS", "Safari", "Unknown device"…
export function deviceLabel(userAgent: string | null | undefined, t: Translate): string {
  const device = describeUserAgent(userAgent);
  if (!device) return t('unknownDevice');
  if (device.browser && device.os) return t('deviceOn', { browser: device.browser, os: device.os });
  return device.browser ?? device.os ?? t('unknownDevice');
}

// Whether this browser can run passkey ceremonies.
function browserHasPasskeys(): boolean {
  return (
    typeof window !== 'undefined' &&
    typeof window.PublicKeyCredential === 'function' &&
    typeof navigator !== 'undefined' &&
    !!navigator.credentials
  );
}

// A list the card loads, with its failure.
function useList<T>(load: () => Promise<T[]>): {
  items: T[] | null;
  error: unknown;
  reload: () => Promise<void>;
} {
  const [items, setItems] = useState<T[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const reload = useCallback(async () => {
    try {
      const next = await load();
      setItems(next);
      setError(null);
    } catch (err) {
      setError(err);
    }
  }, [load]);
  useEffect(() => {
    void reload();
  }, [reload]);
  return { items, error, reload };
}

function ProfileCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const failure = useFailureText();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { email, role } = account.profile;

  const signOut = async (): Promise<void> => {
    setBusy(true);
    setError(null);
    try {
      await account.signOut();
    } catch (err) {
      setError(failure(err));
      setBusy(false);
    }
  };

  return (
    <Card testId="account-profile">
      <CardHeader
        icon={UserRound}
        title={t('profileTitle')}
        description={t('emailManaged')}
        aside={
          <button
            type="button"
            data-testid="account-sign-out"
            onClick={signOut}
            disabled={busy}
            className={BUTTON}
          >
            <LogOut size={13} className="shrink-0" />
            {busy ? t('signingOut') : t('signOut')}
          </button>
        }
      />
      <dl className="mt-4 grid grid-cols-[auto_1fr] gap-x-4 gap-y-2 text-sm">
        <dt className="text-gray-500 text-xs pt-0.5">{t('emailLabel')}</dt>
        <dd className="min-w-0 flex flex-wrap items-center gap-2">
          <span data-testid="account-email" className="text-gray-100 break-all">
            {email}
          </span>
          <span className="rounded-full border border-[#333] px-2 py-px text-[10px] uppercase tracking-wide text-gray-400">
            {t(role === 'owner' ? 'roleOwner' : 'roleMember')}
          </span>
        </dd>
      </dl>
      {error && (
        <InlineNote tone="error" testId="account-sign-out-error">
          {error}
        </InlineNote>
      )}
    </Card>
  );
}

function PasskeysCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const tc = useT('common');
  const { lang } = useLang();
  const failure = useFailureText();
  const load = useCallback(() => account.listPasskeys(), [account]);
  const { items, error: loadError, reload } = useList<AccountPasskey>(load);
  const [draft, setDraft] = useState<PasskeyDraft | null>(null);
  const [label, setLabel] = useState('');
  const [step, setStep] = useState<'idle' | 'preparing' | 'creating'>('idle');
  const [error, setError] = useState<string | null>(null);
  const [added, setAdded] = useState(false);
  const enabled = account.signIn.passkeys;
  const supported = browserHasPasskeys();

  const prepare = async (): Promise<void> => {
    setStep('preparing');
    setError(null);
    setAdded(false);
    try {
      const next = await account.preparePasskey();
      setDraft(next);
      setLabel(deviceLabel(navigator.userAgent, t));
    } catch (err) {
      setError(failure(err));
    } finally {
      setStep('idle');
    }
  };

  const create = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!draft) return;
    setStep('creating');
    setError(null);
    try {
      await draft.create(label);
      setDraft(null);
      setAdded(true);
      await reload();
    } catch (err) {
      setError(failure(err));
    } finally {
      setStep('idle');
    }
  };

  const remove = async (passkey: AccountPasskey): Promise<void> => {
    setError(null);
    setAdded(false);
    try {
      await account.removePasskey(passkey.id);
      await reload();
    } catch (err) {
      setError(failure(err));
    }
  };

  return (
    <Card testId="account-passkeys">
      <CardHeader icon={Fingerprint} title={t('passkeysTitle')} description={t('passkeysDesc')} />
      <div className="mt-4">
        {!items && !loadError && <Loading />}
        {loadError != null && (
          <InlineNote tone="error" testId="passkeys-load-error">
            {failure(loadError)}
          </InlineNote>
        )}
        {items && items.length === 0 && (
          <p data-testid="passkeys-empty" className="text-xs text-gray-500">
            {t('passkeysEmpty')}
          </p>
        )}
        {items && items.length > 0 && (
          <ul className="divide-y divide-[#242424]" data-testid="passkey-list">
            {items.map((passkey) => (
              <li
                key={passkey.id}
                data-testid="passkey-row"
                className="flex items-center justify-between gap-3 py-2.5"
              >
                <div className="flex min-w-0 items-start gap-2.5">
                  <KeyRound size={15} className="mt-0.5 shrink-0 text-[#a59bff]" />
                  <div className="min-w-0">
                    <p className="truncate text-sm text-gray-100" data-testid="passkey-label">
                      {passkey.label || t('passkeyUnnamed')}
                    </p>
                    <p className="text-[11px] text-gray-500">
                      {t('passkeyAdded', { date: formatDate(passkey.createdAt, lang) })}
                      {' · '}
                      {passkey.lastUsedAt
                        ? t('passkeyLastUsed', { date: formatDateTime(passkey.lastUsedAt, lang) })
                        : t('passkeyNeverUsed')}
                    </p>
                  </div>
                </div>
                <ConfirmAction
                  testId="passkey-remove"
                  icon={Trash2}
                  label={tc('remove')}
                  onConfirm={() => remove(passkey)}
                />
              </li>
            ))}
          </ul>
        )}
      </div>

      {!enabled && (
        <InlineNote tone="info" testId="passkeys-off">
          {t('passkeysOff')}
        </InlineNote>
      )}
      {enabled && !supported && (
        <InlineNote tone="info" testId="passkeys-unsupported">
          {t('passkeysUnsupported')}
        </InlineNote>
      )}

      {enabled && supported && !draft && (
        <button
          type="button"
          data-testid="passkey-add"
          onClick={prepare}
          disabled={step !== 'idle'}
          className={`${BUTTON} mt-4`}
        >
          <Plus size={13} className="shrink-0" />
          {step === 'preparing' ? t('passkeyPreparing') : t('passkeyAdd')}
        </button>
      )}

      {draft && (
        <form
          data-testid="passkey-form"
          onSubmit={create}
          className="u-fade-in mt-4 flex flex-col gap-2 sm:flex-row sm:items-end"
        >
          <label className="flex-1 space-y-1">
            <span className="text-[11px] font-medium text-gray-400">{t('passkeyLabel')}</span>
            <input
              data-testid="passkey-label-input"
              value={label}
              maxLength={64}
              onChange={(e) => setLabel(e.target.value)}
              className={INPUT}
            />
          </label>
          <div className="flex gap-2">
            <button
              type="submit"
              data-testid="passkey-create"
              disabled={step !== 'idle'}
              className={PRIMARY_BUTTON}
            >
              <Fingerprint size={13} className="shrink-0" />
              {step === 'creating' ? t('passkeyCreating') : t('passkeyCreate')}
            </button>
            <button
              type="button"
              onClick={() => setDraft(null)}
              disabled={step !== 'idle'}
              className={BUTTON}
            >
              {tc('cancel')}
            </button>
          </div>
        </form>
      )}

      {added && (
        <InlineNote tone="ok" testId="passkey-added">
          {t('passkeyCreated')}
        </InlineNote>
      )}
      {error && (
        <InlineNote tone="error" testId="passkeys-error">
          {error}
        </InlineNote>
      )}
    </Card>
  );
}

function SessionsCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const { lang } = useLang();
  const failure = useFailureText();
  const load = useCallback(() => account.listSessions(), [account]);
  const { items, error: loadError, reload } = useList<AccountSession>(load);
  const [error, setError] = useState<string | null>(null);
  const others = (items ?? []).filter((s) => !s.current);

  const end = async (session: AccountSession): Promise<void> => {
    setError(null);
    try {
      await account.endSession(session.id);
      if (!session.current) await reload();
    } catch (err) {
      setError(failure(err));
    }
  };

  const endOthers = async (): Promise<void> => {
    setError(null);
    try {
      await account.endOtherSessions();
      await reload();
    } catch (err) {
      setError(failure(err));
    }
  };

  return (
    <Card testId="account-sessions">
      <CardHeader icon={Monitor} title={t('sessionsTitle')} description={t('sessionsDesc')} />
      <div className="mt-4">
        {!items && !loadError && <Loading />}
        {loadError != null && <InlineNote tone="error">{failure(loadError)}</InlineNote>}
        {items && (
          <ul className="divide-y divide-[#242424]" data-testid="session-list">
            {items.map((session) => (
              <li
                key={session.id}
                data-testid="session-row"
                data-session-id={session.id}
                data-current={session.current ? 'true' : undefined}
                className="flex items-center justify-between gap-3 py-2.5"
              >
                <div className="min-w-0">
                  <p className="flex flex-wrap items-center gap-2 text-sm text-gray-100">
                    <span className="truncate" title={session.userAgent ?? undefined}>
                      {deviceLabel(session.userAgent, t)}
                    </span>
                    {session.current && (
                      <span className="rounded-full bg-emerald-500/10 px-2 py-px text-[10px] font-medium text-emerald-400">
                        {t('sessionThisBrowser')}
                      </span>
                    )}
                  </p>
                  <p className="text-[11px] text-gray-500">
                    {t('sessionSignedIn', { date: formatDateTime(session.createdAt, lang) })}
                    {' · '}
                    {t('sessionLastSeen', { date: formatDateTime(session.lastSeenAt, lang) })}
                  </p>
                </div>
                <ConfirmAction
                  testId={session.current ? 'session-end-current' : 'session-end'}
                  icon={LogOut}
                  label={t('sessionEnd')}
                  onConfirm={() => end(session)}
                />
              </li>
            ))}
          </ul>
        )}
      </div>
      {others.length > 0 && (
        <div className="mt-3">
          <ConfirmAction
            testId="sessions-end-others"
            icon={LogOut}
            label={t('sessionsEndOthers')}
            onConfirm={endOthers}
          />
        </div>
      )}
      {error && (
        <InlineNote tone="error" testId="sessions-error">
          {error}
        </InlineNote>
      )}
    </Card>
  );
}

const CREATABLE_KINDS: Exclude<TokenKind, 'migrate'>[] = ['shortcut', 'extension', 'library'];

function TokensCard({ account }: { account: AccountApi }): React.JSX.Element {
  const t = useT('settings');
  const tc = useT('common');
  const { lang } = useLang();
  const failure = useFailureText();
  const load = useCallback(() => account.listTokens(), [account]);
  const { items, error: loadError, reload } = useList<AccountToken>(load);
  const [form, setForm] = useState<{
    kind: Exclude<TokenKind, 'migrate'>;
    label: string;
    ttlDays: number;
    libraryWrite: boolean;
  } | null>(null);
  const [busy, setBusy] = useState(false);
  const [created, setCreated] = useState<NewToken | null>(null);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const create = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!form) return;
    setBusy(true);
    setError(null);
    try {
      const token = await account.createToken(form.kind, form.label, {
        ttlDays: form.ttlDays,
        ...(form.kind === 'library' ? { libraryWrite: form.libraryWrite } : {}),
      });
      setForm(null);
      setCopied(false);
      setCreated(token);
      await reload();
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

  const revoke = async (token: AccountToken): Promise<void> => {
    setError(null);
    try {
      await account.revokeToken(token.id);
      await reload();
    } catch (err) {
      setError(failure(err));
    }
  };

  return (
    <Card testId="account-tokens">
      <CardHeader icon={KeySquare} title={t('tokensTitle')} description={t('tokensDesc')} />
      <div className="mt-4">
        {!items && !loadError && <Loading />}
        {loadError != null && <InlineNote tone="error">{failure(loadError)}</InlineNote>}
        {items && items.length === 0 && (
          <p data-testid="tokens-empty" className="text-xs text-gray-500">
            {t('tokensEmpty')}
          </p>
        )}
        {items && items.length > 0 && (
          <ul className="divide-y divide-[#242424]" data-testid="token-list">
            {items.map((token) => (
              <li
                key={token.id}
                data-testid="token-row"
                data-kind={token.kind}
                className="flex items-center justify-between gap-3 py-2.5"
              >
                <div className="min-w-0">
                  <p className="truncate text-sm text-gray-100">
                    {t(`tokenKind_${token.kind}`)}
                    {token.label && <span className="text-gray-400"> · {token.label}</span>}
                  </p>
                  <p className="text-[11px] text-gray-500">
                    {t('tokenCreated', { date: formatDate(token.createdAt, lang) })}
                    {' · '}
                    {token.lastUsedAt
                      ? t('tokenLastUsed', { date: formatDateTime(token.lastUsedAt, lang) })
                      : t('tokenNeverUsed')}
                    {token.expiresAt != null &&
                      ` · ${t('tokenExpires', { date: formatDateTime(token.expiresAt, lang) })}`}
                  </p>
                </div>
                <ConfirmAction
                  testId="token-revoke"
                  icon={Trash2}
                  label={t('tokenRevoke')}
                  onConfirm={() => revoke(token)}
                />
              </li>
            ))}
          </ul>
        )}
      </div>

      {created && (
        <div
          data-testid="token-created"
          className="u-fade-in mt-4 rounded-lg border border-amber-500/30 bg-amber-500/10 p-3"
        >
          <p className="text-sm font-medium text-amber-200">{t('tokenValueTitle')}</p>
          <p className="mt-1 text-xs leading-relaxed text-amber-100/80">{t('tokenValueWarning')}</p>
          <div className="mt-2 flex gap-2">
            <input
              readOnly
              data-testid="token-value"
              aria-label={t('tokenValueTitle')}
              value={created.value}
              onFocus={(e) => e.currentTarget.select()}
              className={`${INPUT} font-mono text-xs`}
            />
            <button type="button" onClick={copy} className={BUTTON} data-testid="token-copy">
              {copied ? <Check size={13} /> : <Copy size={13} />}
              {copied ? t('tokenCopied') : t('tokenCopy')}
            </button>
          </div>
          <button
            type="button"
            data-testid="token-done"
            onClick={() => setCreated(null)}
            className={`${PRIMARY_BUTTON} mt-3`}
          >
            {tc('done')}
          </button>
        </div>
      )}

      {!created && !form && (
        <button
          type="button"
          data-testid="token-new"
          onClick={() => {
            setError(null);
            setForm({
              kind: 'shortcut',
              label: '',
              ttlDays: DEFAULT_TOKEN_TTL_DAYS,
              libraryWrite: false,
            });
          }}
          className={`${BUTTON} mt-4`}
        >
          <Plus size={13} className="shrink-0" />
          {t('tokenNew')}
        </button>
      )}

      {form && (
        <form
          data-testid="token-form"
          onSubmit={create}
          className="u-fade-in mt-4 flex flex-col gap-2 sm:flex-row sm:items-end"
        >
          <label className="space-y-1">
            <span className="text-[11px] font-medium text-gray-400">{t('tokenKindLabel')}</span>
            <select
              data-testid="token-kind"
              value={form.kind}
              onChange={(e) =>
                setForm({
                  ...form,
                  kind: e.target.value as Exclude<TokenKind, 'migrate'>,
                  libraryWrite: false,
                })
              }
              className={`${INPUT} sm:w-48`}
            >
              {CREATABLE_KINDS.map((kind) => (
                <option key={kind} value={kind}>
                  {t(`tokenKind_${kind}`)}
                </option>
              ))}
            </select>
          </label>
          {form.kind === 'library' && (
            <label className="flex items-center gap-2 text-xs text-gray-300">
              <input
                type="checkbox"
                data-testid="token-library-write"
                checked={form.libraryWrite}
                onChange={(e) => setForm({ ...form, libraryWrite: e.target.checked })}
                disabled={busy}
              />
              {t('tokenLibraryWrite')}
            </label>
          )}
          <label className="flex-1 space-y-1">
            <span className="text-[11px] font-medium text-gray-400">{t('tokenLabel')}</span>
            <input
              data-testid="token-label"
              value={form.label}
              maxLength={64}
              onChange={(e) => setForm({ ...form, label: e.target.value })}
              className={INPUT}
            />
          </label>
          <TokenExpirySelect
            testId="token-expiry"
            value={form.ttlDays}
            onChange={(ttlDays) => setForm({ ...form, ttlDays })}
            disabled={busy}
          />
          <div className="flex gap-2">
            <button
              type="submit"
              data-testid="token-create"
              disabled={busy}
              className={PRIMARY_BUTTON}
            >
              {busy ? tc('inProgress') : t('tokenCreate')}
            </button>
            <button type="button" onClick={() => setForm(null)} disabled={busy} className={BUTTON}>
              {tc('cancel')}
            </button>
          </div>
        </form>
      )}

      {error && (
        <InlineNote tone="error" testId="tokens-error">
          {error}
        </InlineNote>
      )}
    </Card>
  );
}

export default function AccountSection({ account }: { account: AccountApi }): React.JSX.Element {
  return (
    <div className="flex flex-col gap-4" data-testid="settings-account">
      <ProfileCard account={account} />
      <PasskeysCard account={account} />
      <SessionsCard account={account} />
      <TokensCard account={account} />
    </div>
  );
}
