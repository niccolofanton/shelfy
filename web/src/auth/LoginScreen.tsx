import React, { useEffect, useState } from 'react';
import { Fingerprint, Loader2, Mail } from 'lucide-react';
import { useFailureText } from '@ui/hooks/useFailureText';
import { useT } from '@ui/i18n';
import type { AuthApi, AuthMethods } from '../api/auth';
import { isApiError } from '../api/http';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './AuthLayout';
import { passkeysSupported } from './passkeys';

const SECONDARY_BUTTON =
  'u-press flex h-10 w-full items-center justify-center gap-2 rounded-lg border border-[#2e2e2e] bg-[#1c1c1c] px-4 text-sm font-medium text-gray-200 transition-colors hover:bg-[#242424] disabled:cursor-not-allowed disabled:opacity-60';

// The i18n key of a failed request's message.
function errorKey(err: unknown): string {
  if (isApiError(err, 'validation_failed')) return 'invalidEmail';
  if (isApiError(err, 'rate_limited')) return 'rateLimited';
  if (isApiError(err, 'network') || isApiError(err, 'unavailable')) return 'unreachable';
  return 'genericError';
}

// The sign-in page (plan §2.11 Login): a passkey of this device, username-less,
// when the server and the browser have passkeys; "email me a link" when the
// server sends email; otherwise (E4: SMTP is optional) the operator mints a
// link with `shelfy-server admin login-link`. `error` is the `?error=` a failed
// link redirect carries. `onSignedIn` runs once a passkey signed in: the app
// checks the session again and goes on to `?next=`.
export default function LoginScreen({
  auth,
  error,
  onSignedIn,
}: {
  auth: AuthApi;
  error?: string | null;
  onSignedIn?: () => void;
}): React.JSX.Element {
  const t = useT('auth');
  const failure = useFailureText();
  const [methods, setMethods] = useState<AuthMethods | null>(null);
  const [methodsError, setMethodsError] = useState<string | null>(null);
  const [email, setEmail] = useState('');
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const [passkeyBusy, setPasskeyBusy] = useState(false);
  const [passkeyError, setPasskeyError] = useState<string | null>(null);

  useEffect(() => {
    let alive = true;
    auth
      .methods()
      .then((m) => alive && setMethods(m))
      .catch((err: unknown) => alive && setMethodsError(errorKey(err)));
    return () => {
      alive = false;
    };
  }, [auth]);

  const submit = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    const address = email.trim();
    if (!address) return;
    setSending(true);
    setSendError(null);
    try {
      await auth.requestLink(address);
      setSent(true);
    } catch (err) {
      setSendError(errorKey(err));
    } finally {
      setSending(false);
    }
  };

  const signInWithPasskey = async (): Promise<void> => {
    setPasskeyBusy(true);
    setPasskeyError(null);
    try {
      await auth.signInWithPasskey();
      onSignedIn?.();
    } catch (err) {
      setPasskeyBusy(false);
      // A passkey the server does not know: removed from the account, or made
      // for another server.
      if (isApiError(err, 'passkey_invalid')) setPasskeyError(t('passkeyNotRegistered'));
      else setPasskeyError(failure(err));
    }
  };

  const withPasskey = !!methods?.passkeys && passkeysSupported();

  return (
    <AuthLayout title={t('title')}>
      <div className="space-y-4">
        {error === 'invalid_link' && !sent && (
          <Notice tone="error" testId="login-invalid-link">
            {t('invalidLink')}
          </Notice>
        )}

        {methodsError && (
          <Notice tone="error" testId="login-methods-error">
            {t(methodsError)}
          </Notice>
        )}

        {!methods && !methodsError && (
          <p className="flex items-center gap-2 text-sm text-gray-500">
            <Loader2 size={14} className="animate-spin" /> {t('loading')}
          </p>
        )}

        {withPasskey && (
          <div className="space-y-2">
            <button
              type="button"
              data-testid="login-passkey"
              onClick={signInWithPasskey}
              disabled={passkeyBusy}
              className={PRIMARY_BUTTON}
            >
              {passkeyBusy ? (
                <Loader2 size={15} className="animate-spin" />
              ) : (
                <Fingerprint size={15} />
              )}
              {passkeyBusy ? t('passkeySigningIn') : t('passkeySignIn')}
            </button>
            {passkeyError && (
              <Notice tone="error" testId="login-passkey-error">
                {passkeyError}
              </Notice>
            )}
            <p className="text-[12px] leading-relaxed text-gray-500">{t('passkeyFirstTime')}</p>
          </div>
        )}

        {withPasskey && methods && (
          <div className="flex items-center gap-3 text-[11px] uppercase tracking-wider text-gray-600">
            <span className="h-px flex-1 bg-[#2a2a2a]" />
            {t('or')}
            <span className="h-px flex-1 bg-[#2a2a2a]" />
          </div>
        )}

        {methods && !methods.emailLink && (
          <Notice tone="info" testId="login-ask-operator">
            {t('askOperator')}
          </Notice>
        )}

        {methods?.emailLink && sent && (
          <div className="space-y-3">
            <Notice tone="info" testId="login-link-sent">
              {t('linkSent')}
            </Notice>
            <button
              type="button"
              onClick={() => setSent(false)}
              className="u-press text-[13px] text-gray-400 hover:text-white"
            >
              {t('sendAnother')}
            </button>
          </div>
        )}

        {methods?.emailLink && !sent && (
          <form data-testid="login-form" onSubmit={submit} className="space-y-3" noValidate>
            <label className="block space-y-1.5">
              <span className="text-[12px] font-medium text-gray-400">{t('emailLabel')}</span>
              <input
                type="email"
                name="email"
                autoComplete="email"
                inputMode="email"
                required
                value={email}
                onChange={(e) => setEmail(e.target.value)}
                placeholder={t('emailPlaceholder')}
                className="h-10 w-full rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] px-3 text-sm text-gray-100 placeholder-gray-600 outline-none transition-colors focus:border-[#7B5CFF]/70"
              />
            </label>
            {sendError && (
              <Notice tone="error" testId="login-send-error">
                {t(sendError)}
              </Notice>
            )}
            <button
              type="submit"
              disabled={sending || !email.trim()}
              className={withPasskey ? SECONDARY_BUTTON : PRIMARY_BUTTON}
            >
              {sending ? <Loader2 size={15} className="animate-spin" /> : <Mail size={15} />}
              {sending ? t('sending') : t('sendLink')}
            </button>
          </form>
        )}
      </div>
    </AuthLayout>
  );
}
