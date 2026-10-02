// Confirming who you are before a sensitive action (plan §2.11; P1-13): the
// server answers 403 `reauth_required` when the session's last sign-in is
// older than 5 minutes, Http asks ReauthHost, and this dialog offers the ways
// to confirm:
//
//   - a passkey of the account (`POST /auth/reauth/{start,finish}`);
//   - an emailed link, when the server sends email;
//   - a link from the operator (`shelfy-server admin login-link --purpose
//     reauth`), which E4 makes the owner's own fallback.
//
// A link opens `/login/reauth#<token>` (ReauthLinkScreen) in this browser,
// usually in another tab; that page announces the confirmation on a
// BroadcastChannel and this dialog carries on by itself. "I opened the link"
// does the same where BroadcastChannel is missing. Http then sends the refused
// request again; if it is still refused, the dialog comes back with a note.
import React, { useCallback, useEffect, useRef, useState } from 'react';
import { Fingerprint, Loader2, Mail, ShieldCheck, Terminal } from 'lucide-react';
import type { SignInMethods } from '@ui/api/account';
import { useFailureText } from '@ui/hooks/useFailureText';
import { useT } from '@ui/i18n';
import type { AuthApi } from '../api/auth';
import { isApiError, type Http } from '../api/http';
import { Notice, PRIMARY_BUTTON } from './AuthLayout';
import { passkeysSupported } from './passkeys';

// Where a confirmed re-authentication link is announced to the other tabs.
export const AUTH_CHANNEL = 'shelfy:auth';
export const REAUTH_MESSAGE = 'reauth';

// The command that mints a re-authentication link on the server.
export const REAUTH_COMMAND = 'shelfy-server admin login-link --purpose reauth';

const SECONDARY_BUTTON =
  'u-press flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#1c1c1c] px-4 text-sm font-medium text-gray-200 transition-colors hover:bg-[#242424] disabled:cursor-not-allowed disabled:opacity-60';

export function ReauthDialog({
  auth,
  methods,
  again,
  onDone,
}: {
  auth: AuthApi;
  methods: SignInMethods;
  // The last confirmation did not take: the request was refused again.
  again: boolean;
  onDone: (confirmed: boolean) => void;
}): React.JSX.Element {
  const t = useT('auth');
  const tc = useT('common');
  const failure = useFailureText();
  const [busy, setBusy] = useState<'passkey' | 'email' | null>(null);
  const [emailSent, setEmailSent] = useState(false);
  const [noPasskey, setNoPasskey] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const passkeys = methods.passkeys && passkeysSupported() && !noPasskey;
  const firstButton = useRef<HTMLButtonElement>(null);

  // A link confirmed in another tab of this browser: carry on.
  useEffect(() => {
    if (typeof BroadcastChannel === 'undefined') return undefined;
    const channel = new BroadcastChannel(AUTH_CHANNEL);
    channel.onmessage = (event: MessageEvent) => {
      if ((event.data as { type?: unknown } | null)?.type === REAUTH_MESSAGE) onDone(true);
    };
    return () => channel.close();
  }, [onDone]);

  useEffect(() => {
    firstButton.current?.focus();
    const onKey = (e: KeyboardEvent): void => {
      if (e.key === 'Escape') onDone(false);
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onDone]);

  const withPasskey = async (): Promise<void> => {
    setBusy('passkey');
    setError(null);
    try {
      await auth.reauthWithPasskey();
      onDone(true);
    } catch (err) {
      setBusy(null);
      // The account has no passkey (or passkeys went off): links remain.
      if (isApiError(err, 'not_found')) {
        setNoPasskey(true);
        setError(t('reauthNoPasskey'));
      } else {
        setError(failure(err));
      }
    }
  };

  const byEmail = async (): Promise<void> => {
    setBusy('email');
    setError(null);
    try {
      await auth.requestReauthLink();
      setEmailSent(true);
    } catch (err) {
      setError(failure(err));
    } finally {
      setBusy(null);
    }
  };

  return (
    <div
      data-testid="reauth-dialog"
      role="dialog"
      aria-modal="true"
      aria-labelledby="reauth-title"
      className="fixed inset-0 z-[120] flex items-center justify-center bg-black/80 p-4 u-backdrop-in"
    >
      <div className="u-dialog-in w-full max-w-sm overflow-hidden rounded-xl border border-[#2e2e2e] bg-[#161616] shadow-2xl">
        <div className="space-y-4 px-6 py-6">
          <div className="flex items-center gap-2.5">
            <ShieldCheck size={18} className="shrink-0 text-[#7B5CFF]" />
            <h2 id="reauth-title" className="text-[16px] font-semibold text-white">
              {t('reauthTitle')}
            </h2>
          </div>
          <p className="text-sm leading-relaxed text-gray-400">{t('reauthBody')}</p>

          {again && (
            <Notice tone="info" testId="reauth-again">
              {t('reauthAgain')}
            </Notice>
          )}

          {passkeys && (
            <button
              ref={firstButton}
              type="button"
              data-testid="reauth-passkey"
              onClick={withPasskey}
              disabled={busy !== null}
              className={PRIMARY_BUTTON}
            >
              {busy === 'passkey' ? (
                <Loader2 size={15} className="animate-spin" />
              ) : (
                <Fingerprint size={15} />
              )}
              {busy === 'passkey' ? t('reauthPasskeyBusy') : t('reauthPasskey')}
            </button>
          )}

          {methods.emailLink && !emailSent && (
            <button
              type="button"
              data-testid="reauth-email"
              onClick={byEmail}
              disabled={busy !== null}
              className={passkeys ? SECONDARY_BUTTON : PRIMARY_BUTTON}
            >
              {busy === 'email' ? (
                <Loader2 size={15} className="animate-spin" />
              ) : (
                <Mail size={15} />
              )}
              {busy === 'email' ? t('sending') : t('reauthEmail')}
            </button>
          )}

          {emailSent && (
            <Notice tone="info" testId="reauth-email-sent">
              {t('reauthEmailSent')}
            </Notice>
          )}

          <div className="space-y-2 rounded-lg border border-[#262626] bg-[#111] px-3 py-3">
            <p className="flex items-center gap-1.5 text-[12px] font-medium text-gray-300">
              <Terminal size={13} className="shrink-0" /> {t('reauthOperatorTitle')}
            </p>
            <p className="text-[12px] leading-relaxed text-gray-500">{t('reauthOperator')}</p>
            <code className="block select-all break-all rounded bg-[#1c1c1c] px-2 py-1.5 font-mono text-[11px] text-gray-300">
              {REAUTH_COMMAND}
            </code>
          </div>

          {error && (
            <Notice tone="error" testId="reauth-error">
              {error}
            </Notice>
          )}

          <div className="flex gap-2">
            <button
              type="button"
              data-testid="reauth-cancel"
              onClick={() => onDone(false)}
              className="u-press h-9 flex-1 rounded-lg bg-[#222] px-3 text-sm text-gray-300 hover:bg-[#2a2a2a]"
            >
              {tc('cancel')}
            </button>
            <button
              type="button"
              data-testid="reauth-continue"
              onClick={() => onDone(true)}
              disabled={busy !== null}
              className="u-press h-9 flex-1 rounded-lg bg-[#2a2a2a] px-3 text-sm font-medium text-gray-100 hover:bg-[#333] disabled:opacity-60"
            >
              {t('reauthContinue')}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

interface Prompt {
  again: boolean;
}

// Answers Http's re-authentication requests with the dialog, one at a time:
// requests refused together wait for the same answer. Mounted while signed in;
// leaving cancels a dialog that is open.
export function ReauthHost({
  http,
  auth,
  methods,
}: {
  http: Http;
  auth: AuthApi;
  methods: SignInMethods;
}): React.JSX.Element | null {
  const [prompt, setPrompt] = useState<Prompt | null>(null);
  const pending = useRef<{ promise: Promise<boolean>; resolve: (ok: boolean) => void } | null>(
    null,
  );

  const finish = useCallback((confirmed: boolean) => {
    const current = pending.current;
    pending.current = null;
    setPrompt(null);
    current?.resolve(confirmed);
  }, []);

  useEffect(() => {
    const off = http.onReauthRequired(({ again }) => {
      if (pending.current) return pending.current.promise;
      let resolve: (ok: boolean) => void = () => {};
      const promise = new Promise<boolean>((r) => {
        resolve = r;
      });
      pending.current = { promise, resolve };
      setPrompt({ again });
      return promise;
    });
    return () => {
      off();
      const current = pending.current;
      pending.current = null;
      current?.resolve(false);
    };
  }, [http]);

  if (!prompt) return null;
  return <ReauthDialog auth={auth} methods={methods} again={prompt.again} onDone={finish} />;
}
