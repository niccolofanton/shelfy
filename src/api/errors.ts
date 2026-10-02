import { messages } from '../i18n';

// Failed ShelfyClient calls, in the UI's terms. The web client's errors carry
// the API's stable problem `code` (plan §2.9: the server never sends UI prose),
// or `network` when no answer came back; the desktop's IPC errors carry none.
// Each code's message lives in the `errors` i18n namespace as `code.<code>`
// (src/i18n/messages/errors.ts), which covers every code the API defines.

// The problem code of a failed call, or null when it has none.
export function errorCodeOf(err: unknown): string | null {
  if (!err || typeof err !== 'object' || !('code' in err)) return null;
  const { code } = err as { code: unknown };
  return typeof code === 'string' && code ? code : null;
}

// The key, in the `errors` namespace, of the message for a failed call
// (`useT('errors')(key)`), or null when its code has no message: no code, or
// one this build does not know yet.
export function errorMessageKey(err: unknown): string | null {
  const code = errorCodeOf(err);
  if (!code) return null;
  const key = `code.${code}`;
  return `errors.${key}` in messages.en ? key : null;
}
