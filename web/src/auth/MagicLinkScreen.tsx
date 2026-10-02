import React, { useState } from 'react';
import { Loader2, LogIn } from 'lucide-react';
import { useT } from '@ui/i18n';
import type { AuthApi } from '../api/auth';
import { isApiError } from '../api/http';
import { LOGIN_PATH } from '../route';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './AuthLayout';

type Status = 'ready' | 'busy' | 'invalid' | 'rateLimited' | 'unreachable' | 'error';

// The page a sign-in link opens (`/login/magic#<token>`). The link is spent
// only when the user presses the button, never on load: mail scanners and
// link previews that fetch the page cannot use it up.
export default function MagicLinkScreen({
  token,
  auth,
  onSignedIn,
}: {
  token: string | null;
  auth: AuthApi;
  onSignedIn: () => void;
}): React.JSX.Element {
  const t = useT('auth');
  const [status, setStatus] = useState<Status>(token ? 'ready' : 'invalid');

  const signIn = async (): Promise<void> => {
    if (!token) return;
    setStatus('busy');
    try {
      await auth.redeem(token);
      onSignedIn();
    } catch (err) {
      if (isApiError(err, 'invalid_link')) setStatus('invalid');
      else if (isApiError(err, 'rate_limited')) setStatus('rateLimited');
      else if (isApiError(err, 'network') || isApiError(err, 'unavailable'))
        setStatus('unreachable');
      else setStatus('error');
    }
  };

  if (status === 'invalid') {
    return (
      <AuthLayout title={t('magicTitle')}>
        <div className="space-y-4">
          <Notice tone="error" testId="magic-invalid">
            {t('invalidLink')}
          </Notice>
          <a href={LOGIN_PATH} className={PRIMARY_BUTTON} data-testid="magic-back">
            {t('backToSignIn')}
          </a>
        </div>
      </AuthLayout>
    );
  }

  const busy = status === 'busy';
  const errorKey =
    status === 'rateLimited'
      ? 'rateLimited'
      : status === 'unreachable'
        ? 'unreachable'
        : status === 'error'
          ? 'genericError'
          : null;
  return (
    <AuthLayout title={t('magicTitle')}>
      <div className="space-y-4">
        <p className="text-sm leading-relaxed text-gray-400">{t('magicHint')}</p>
        {errorKey && (
          <Notice tone="error" testId="magic-error">
            {t(errorKey)}
          </Notice>
        )}
        <button
          type="button"
          data-testid="magic-sign-in"
          onClick={signIn}
          disabled={busy}
          className={PRIMARY_BUTTON}
        >
          {busy ? <Loader2 size={15} className="animate-spin" /> : <LogIn size={15} />}
          {busy ? t('signingIn') : t('signIn')}
        </button>
      </div>
    </AuthLayout>
  );
}
