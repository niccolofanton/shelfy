import React, { useState } from 'react';
import { Link } from 'wouter';
import { AlertTriangle, CheckCircle2, Loader2, MonitorSmartphone } from 'lucide-react';
import { useFailureText } from '@ui/hooks/useFailureText';
import { useT } from '@ui/i18n';
import type { AuthApi } from '../api/auth';
import { isApiError } from '../api/http';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './AuthLayout';

// A user code as the CLI shows it: 8 letters, `BCDF-GHJK`. Case, dashes and
// spaces do not matter to the server; anything else is dropped as it is typed.
const CODE_LETTERS = 8;

export function formatUserCode(input: string): string {
  const letters = input
    .toUpperCase()
    .replace(/[^A-Z]/g, '')
    .slice(0, CODE_LETTERS);
  return letters.length > 4 ? `${letters.slice(0, 4)}-${letters.slice(4)}` : letters;
}

function isComplete(code: string): boolean {
  return code.replace(/-/g, '').length === CODE_LETTERS;
}

type Status = 'ready' | 'busy' | 'approved';

// `/device` (plan §2.11 device tokens; P1-17): approves the sign-in code of a
// device, today the migration CLI (`shelfy-migrate login`), which then gets a
// `migrate` token for this account, valid 7 days. Approving needs a sign-in
// from the last 5 minutes: the re-authentication dialog asks first when it is
// older. The code comes from the address (`/device#<code>`, the CLI's
// complete link) or the keyboard; it is never approved without a click.
export default function DevicePage({
  auth,
  initialCode,
}: {
  auth: AuthApi;
  initialCode?: string | null;
}): React.JSX.Element {
  const t = useT('auth');
  const te = useT('errors');
  const failure = useFailureText();
  const [code, setCode] = useState(() => formatUserCode(initialCode ?? ''));
  const [status, setStatus] = useState<Status>('ready');
  const [error, setError] = useState<string | null>(null);

  const approve = async (e: React.FormEvent): Promise<void> => {
    e.preventDefault();
    if (!isComplete(code) || status === 'busy') return;
    setStatus('busy');
    setError(null);
    try {
      await auth.approveDevice(code);
      setStatus('approved');
    } catch (err) {
      setStatus('ready');
      if (isApiError(err, 'invalid_device_code')) setError(t('deviceInvalid'));
      else if (isApiError(err, 'reauth_required')) setError(t('deviceReauth'));
      else if (isApiError(err, 'rate_limited')) setError(t('deviceRateLimited'));
      else setError(failure(err));
    }
  };

  if (status === 'approved') {
    return (
      <AuthLayout title={t('deviceTitle')}>
        <div className="space-y-4">
          <p
            data-testid="device-approved"
            role="status"
            className="flex items-start gap-2 text-sm leading-relaxed text-gray-300"
          >
            <CheckCircle2 size={16} className="mt-0.5 shrink-0 text-emerald-400" />
            {t('deviceApproved')}
          </p>
          <Link href="/" className={PRIMARY_BUTTON} data-testid="device-back">
            {te('backToLibrary')}
          </Link>
        </div>
      </AuthLayout>
    );
  }

  const busy = status === 'busy';
  return (
    <AuthLayout title={t('deviceTitle')}>
      <form data-testid="device-form" onSubmit={approve} className="space-y-4" noValidate>
        <p className="text-sm leading-relaxed text-gray-400">{t('deviceIntro')}</p>
        <p
          data-testid="device-warning"
          className="flex items-start gap-2 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-[13px] leading-relaxed text-amber-100"
        >
          <AlertTriangle size={15} className="mt-0.5 shrink-0 text-amber-400" />
          {t('deviceWarning')}
        </p>
        <label className="block space-y-1.5">
          <span className="text-[12px] font-medium text-gray-400">{t('deviceCodeLabel')}</span>
          <input
            data-testid="device-code"
            name="code"
            value={code}
            onChange={(e) => setCode(formatUserCode(e.target.value))}
            placeholder="ABCD-EFGH"
            autoComplete="one-time-code"
            autoCapitalize="characters"
            autoCorrect="off"
            spellCheck={false}
            inputMode="text"
            className="h-12 w-full rounded-lg border border-[#2e2e2e] bg-[#1a1a1a] px-3 text-center font-mono text-xl tracking-[0.25em] text-gray-100 placeholder-gray-700 outline-none transition-colors focus:border-[#7B5CFF]/70"
          />
        </label>
        {error && (
          <Notice tone="error" testId="device-error">
            {error}
          </Notice>
        )}
        <button
          type="submit"
          data-testid="device-approve"
          disabled={busy || !isComplete(code)}
          className={PRIMARY_BUTTON}
        >
          {busy ? <Loader2 size={15} className="animate-spin" /> : <MonitorSmartphone size={15} />}
          {busy ? t('deviceApproving') : t('deviceApprove')}
        </button>
        <Link
          href="/"
          className="u-press block text-center text-[13px] text-gray-500 hover:text-white"
          data-testid="device-cancel"
        >
          {te('backToLibrary')}
        </Link>
      </form>
    </AuthLayout>
  );
}
