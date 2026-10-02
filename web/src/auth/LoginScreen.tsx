import React, { useEffect, useState } from 'react';
import { Loader2, Mail } from 'lucide-react';
import { useT } from '@ui/i18n';
import type { AuthApi, AuthMethods } from '../api/auth';
import { isApiError } from '../api/http';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './AuthLayout';

// The i18n key of a failed request's message.
function errorKey(err: unknown): string {
  if (isApiError(err, 'validation_failed')) return 'invalidEmail';
  if (isApiError(err, 'rate_limited')) return 'rateLimited';
  if (isApiError(err, 'network') || isApiError(err, 'unavailable')) return 'unreachable';
  return 'genericError';
}

// The sign-in page. With email sign-in on, the user asks for a link; without
// it (E4: SMTP is optional), the operator mints one with
// `shelfy-server admin login-link`. `error` is the `?error=` a failed link
// redirect carries.
export default function LoginScreen({
  auth,
  error,
}: {
  auth: AuthApi;
  error?: string | null;
}): React.JSX.Element {
  const t = useT('auth');
  const [methods, setMethods] = useState<AuthMethods | null>(null);
  const [methodsError, setMethodsError] = useState<string | null>(null);
  const [email, setEmail] = useState('');
  const [sending, setSending] = useState(false);
  const [sent, setSent] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);

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
            <button type="submit" disabled={sending || !email.trim()} className={PRIMARY_BUTTON}>
              {sending ? <Loader2 size={15} className="animate-spin" /> : <Mail size={15} />}
              {sending ? t('sending') : t('sendLink')}
            </button>
          </form>
        )}
      </div>
    </AuthLayout>
  );
}
