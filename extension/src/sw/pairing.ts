// Pairing (C2, C9; plan §2.11): the SPA asks the server for a 60-second code and hands it to the
// extension (`shelfy.pair`), which exchanges it at `POST /extension/pair` for its own token. The
// token goes straight into storage: it is never sent to a page, a content script or the SPA.
// Re-pairing with the same `installId` makes the server revoke this installation's previous
// token (P2-G16).

import type { ExternalAnswer } from '../shared/protocol';
import type { ApiClient } from './api';
import { API, parsePairResponse, type PairRequest } from './contracts';
import { failureCode } from './errors';
import type { SettingsStore } from './settings';

/** "Chrome on macOS": the label the user sees next to the token in Settings → Connections. */
export function installLabel(userAgent: string, platform?: string): string {
  const source = `${platform ?? ''} ${userAgent}`.toLowerCase();
  const os = source.includes('cros')
    ? 'ChromeOS'
    : source.includes('android')
      ? 'Android'
      : source.includes('win')
        ? 'Windows'
        : source.includes('mac')
          ? 'macOS'
          : source.includes('linux')
            ? 'Linux'
            : null;
  const browser = /\bedg\//.test(source) ? 'Edge' : /\bopr\//.test(source) ? 'Opera' : 'Chrome';
  return os ? `${browser} on ${os}` : browser;
}

export interface PairingDeps {
  api: ApiClient;
  store: SettingsStore;
  version: string;
  label: string;
  now(): number;
  /** Runs after a successful pairing (config refresh, queue flush). */
  paired(): void;
}

export async function pair(code: string, deps: PairingDeps): Promise<ExternalAnswer> {
  const body: PairRequest = {
    code,
    installId: await deps.store.installId(),
    label: deps.label,
    version: deps.version,
  };
  const response = await deps.api.post(API.pair, body, { auth: 'none' });
  if (!response.ok) {
    const failure = response.failure;
    // The server's own code when it gave one (`invalid_pairing_code`, `rate_limited`, …).
    const code =
      failure.kind === 'http' && failure.code ? failure.code : failureCode(response.failure);
    return { ok: false, code };
  }
  const minted = parsePairResponse(response.data);
  if (!minted) return { ok: false, code: 'bad_response' };
  await deps.store.setPairing({ ...minted, pairedAt: deps.now() });
  // A new pairing starts clean: no wait from the old token's failures, no stale error.
  await deps.store.patchStatus({ blockedUntil: 0, failures: 0, lastError: null });
  deps.paired();
  return { ok: true };
}

/** Forgets the token here; the server keeps it until it is revoked in Settings → Connections. */
export async function forgetPairing(store: SettingsStore): Promise<void> {
  await store.setPairing(null);
}
