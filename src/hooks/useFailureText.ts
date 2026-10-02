import { useCallback } from 'react';
import { isPasskeyFailure, type PasskeyFailureReason } from '../api/account';
import { errorMessageKey } from '../api/errors';
import { useT } from '../i18n';

// The `auth` message of each way a passkey ceremony fails in the browser.
const PASSKEY_KEYS: Record<PasskeyFailureReason, string> = {
  cancelled: 'passkeyCancelled',
  exists: 'passkeyExists',
  unsupported: 'passkeyUnsupported',
  origin: 'passkeyOrigin',
  failed: 'passkeyFailed',
};

// The message of a failed account or sign-in call, in the active language:
// the browser's reason for a passkey ceremony, the API's problem code
// (src/api/errors.ts), or a generic failure.
export function useFailureText(): (err: unknown) => string {
  const ta = useT('auth');
  const te = useT('errors');
  const tc = useT('common');
  return useCallback(
    (err: unknown): string => {
      if (isPasskeyFailure(err)) return ta(PASSKEY_KEYS[err.reason] ?? 'passkeyFailed');
      const key = errorMessageKey(err);
      return key ? te(key) : tc('genericError');
    },
    [ta, te, tc],
  );
}
