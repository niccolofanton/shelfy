// The SPA's line to the browser extension (contract C9): `shelfy.ping` to see
// whether it is there, `shelfy.pair` to hand it a pairing code. The page
// addresses it by its fixed ID (extension/src/id.ts, P2-G6) with
// `chrome.runtime.sendMessage(EXTENSION_ID, …)`, which Chromium allows only to
// the Shelfy origin the manifest's `externally_connectable` names; the
// extension answers only that origin as well.
//
// A page cannot tell "not installed" from "not Chromium" by asking: without a
// matching extension `chrome.runtime` is simply missing. So `window.chrome`
// (every Chromium browser has it) stands for "a browser the extension could
// run in".
import type { ExtensionBridge, ExtensionPairResult, ExtensionProbe } from '@ui/api/account';
import { EXTENSION_ID } from '../../../extension/src/id';
import { EXTERNAL, type PingAnswer } from '../../../extension/src/shared/protocol';

// The part of `chrome` the bridge uses; tests pass a fake.
export interface ChromeLike {
  runtime?: {
    lastError?: { message?: string } | null;
    sendMessage?: (
      extensionId: string,
      message: unknown,
      callback: (response: unknown) => void,
    ) => void;
  };
}

export interface BridgeOptions {
  // The page's `chrome` global; default: `window.chrome`.
  chrome?: ChromeLike | null;
  // How long to wait for an answer, ms. Default 3000.
  timeoutMs?: number;
}

const DEFAULT_TIMEOUT_MS = 3000;

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === 'object' && !Array.isArray(value);
}

// One message and its answer; null when nobody answers (no extension, a
// disabled one, a timeout).
function send(chrome: ChromeLike, message: unknown, timeoutMs: number): Promise<unknown> {
  const sendMessage = chrome.runtime?.sendMessage;
  if (!sendMessage) return Promise.resolve(null);
  return new Promise((resolve) => {
    const timer = setTimeout(() => resolve(null), timeoutMs);
    const done = (value: unknown): void => {
      clearTimeout(timer);
      resolve(value);
    };
    try {
      sendMessage.call(chrome.runtime, EXTENSION_ID, message, (response) => {
        // Reading lastError marks the failure as handled.
        done(chrome.runtime?.lastError ? null : (response ?? null));
      });
    } catch {
      done(null);
    }
  });
}

export function createExtensionBridge(options: BridgeOptions = {}): ExtensionBridge {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const chrome = (): ChromeLike | null =>
    options.chrome !== undefined
      ? options.chrome
      : typeof window === 'undefined'
        ? null
        : ((window as unknown as { chrome?: ChromeLike }).chrome ?? null);

  return {
    async probe(): Promise<ExtensionProbe> {
      const c = chrome();
      if (!c) return { state: 'unsupported' };
      const answer = await send(c, { type: EXTERNAL.ping }, timeoutMs);
      if (!isRecord(answer) || answer.ok !== true || typeof answer.version !== 'string') {
        return { state: 'missing' };
      }
      const ping = answer as unknown as PingAnswer;
      return {
        state: 'ready',
        version: ping.version,
        paired: ping.paired === true,
        outdated: ping.outdated === true,
      };
    },
    async pair(code): Promise<ExtensionPairResult> {
      const c = chrome();
      const answer = c ? await send(c, { type: EXTERNAL.pair, code }, timeoutMs * 4) : null;
      if (!isRecord(answer)) return { ok: false, code: 'unreachable' };
      if (answer.ok === true) return { ok: true };
      return { ok: false, code: typeof answer.code === 'string' ? answer.code : 'bad_response' };
    },
  };
}

/** C9 sync controls share the fixed-id/origin checks of pairing. */
export function createSyncExtension(
  options: BridgeOptions = {},
): import('@ui/api/sync').SyncExtension {
  const timeoutMs = options.timeoutMs ?? DEFAULT_TIMEOUT_MS;
  const chrome = () =>
    options.chrome !== undefined
      ? options.chrome
      : typeof window === 'undefined'
        ? null
        : ((window as unknown as { chrome?: ChromeLike }).chrome ?? null);
  const request = async (message: unknown, timeout = timeoutMs) => {
    const c = chrome();
    return c ? send(c, message, timeout) : null;
  };
  const answer = async (message: unknown) => {
    const value = await request(message, Math.max(timeoutMs * 4, 30_000));
    if (!isRecord(value)) return { ok: false as const, code: 'unreachable' };
    return value.ok === true
      ? { ok: true as const }
      : { ok: false as const, code: typeof value.code === 'string' ? value.code : 'bad_response' };
  };
  return {
    async connection() {
      if (!chrome()) return { extension: { state: 'unsupported' }, syncing: {} };
      const value = await request({ type: EXTERNAL.ping });
      if (!isRecord(value) || value.ok !== true || typeof value.version !== 'string')
        return { extension: { state: 'missing' }, syncing: {} };
      const syncing: Partial<Record<import('@ui/api/sync').SyncPlatform, boolean>> = {};
      if (isRecord(value.syncing))
        for (const platform of ['instagram', 'twitter', 'pinterest'] as const)
          if (typeof value.syncing[platform] === 'boolean')
            syncing[platform] = value.syncing[platform];
      return {
        extension: {
          state: 'ready',
          version: value.version,
          paired: value.paired === true,
          outdated: value.outdated === true,
        },
        syncing,
      };
    },
    start: (target) => answer({ type: EXTERNAL.syncStart, target }),
    stop: (platform) => answer({ type: EXTERNAL.syncStop, platform }),
  };
}
