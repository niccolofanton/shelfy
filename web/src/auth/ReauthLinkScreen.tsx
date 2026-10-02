import React, { useState } from 'react';
import { CheckCircle2, Loader2, ShieldCheck } from 'lucide-react';
import { useT } from '@ui/i18n';
import type { AuthApi } from '../api/auth';
import { isApiError } from '../api/http';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './AuthLayout';
import { AUTH_CHANNEL, REAUTH_MESSAGE } from './ReauthDialog';

type Status =
  | 'ready'
  | 'busy'
  | 'done'
  | 'invalid'
  | 'signedOut'
  | 'rateLimited'
  | 'unreachable'
  | 'error';

// Tells the other tabs of this browser that the session is confirmed: an open
// re-authentication dialog carries on (./ReauthDialog.tsx).
function announce(): void {
  if (typeof BroadcastChannel === 'undefined') return;
  try {
    const channel = new BroadcastChannel(AUTH_CHANNEL);
    channel.postMessage({ type: REAUTH_MESSAGE });
    channel.close();
  } catch {
    /* the dialog's "I opened the link" still works */
  }
}

// The page a re-authentication link opens (`/login/reauth#<token>`: an
// emailed link, or `shelfy-server admin login-link --purpose reauth`). Like a
// sign-in link it is spent only when the user presses the button, never on
// load, and only in a browser signed in to the same account: it confirms that
// session for 5 minutes.
export default function ReauthLinkScreen({
  token,
  auth,
}: {
  token: string | null;
  auth: AuthApi;
}): React.JSX.Element {
  const t = useT('auth');
  const [status, setStatus] = useState<Status>(token ? 'ready' : 'invalid');

  const confirm = async (): Promise<void> => {
    if (!token) return;
    setStatus('busy');
    try {
      await auth.reauthWithLink(token);
      announce();
      setStatus('done');
    } catch (err) {
      if (isApiError(err, 'invalid_link')) setStatus('invalid');
      else if (isApiError(err, 'unauthorized')) setStatus('signedOut');
      else if (isApiError(err, 'rate_limited')) setStatus('rateLimited');
      else if (isApiError(err, 'network') || isApiError(err, 'unavailable'))
        setStatus('unreachable');
      else setStatus('error');
    }
  };

  if (status === 'done') {
    return (
      <AuthLayout title={t('reauthTitle')}>
        <div className="space-y-4">
          <p
            data-testid="reauth-link-done"
            role="status"
            className="flex items-start gap-2 text-sm leading-relaxed text-gray-300"
          >
            <CheckCircle2 size={16} className="mt-0.5 shrink-0 text-emerald-400" />
            {t('reauthLinkDone')}
          </p>
          <a href="/" className={PRIMARY_BUTTON} data-testid="reauth-link-back">
            {t('backToShelfy')}
          </a>
        </div>
      </AuthLayout>
    );
  }

  if (status === 'invalid' || status === 'signedOut') {
    return (
      <AuthLayout title={t('reauthTitle')}>
        <div className="space-y-4">
          <Notice tone="error" testId={`reauth-link-${status}`}>
            {status === 'invalid' ? t('invalidReauthLink') : t('reauthSignedOut')}
          </Notice>
          <a href="/" className={PRIMARY_BUTTON} data-testid="reauth-link-back">
            {t('backToShelfy')}
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
    <AuthLayout title={t('reauthTitle')}>
      <div className="space-y-4">
        <p className="text-sm leading-relaxed text-gray-400">{t('reauthLinkHint')}</p>
        {errorKey && (
          <Notice tone="error" testId="reauth-link-error">
            {t(errorKey)}
          </Notice>
        )}
        <button
          type="button"
          data-testid="reauth-link-confirm"
          onClick={confirm}
          disabled={busy}
          className={PRIMARY_BUTTON}
        >
          {busy ? <Loader2 size={15} className="animate-spin" /> : <ShieldCheck size={15} />}
          {busy ? t('reauthConfirming') : t('reauthConfirm')}
        </button>
      </div>
    </AuthLayout>
  );
}
