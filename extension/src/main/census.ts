// MAIN-world request census: counts the page requests whose responses the hook tries to parse,
// labelled by endpoint (IG GraphQL by friendly name). Comparing these counts with the batches
// that reach the side panel shows when the page fetched saved items in a shape the parsers do
// not recognise. Reads URLs and the IG friendly-name header/param only: no bodies are read and
// no request is changed or added.

import { CENSUS_MESSAGE, type Platform } from '../protocol';

const IG_PATHS = ['/graphql/query', '/api/v1/feed/saved/', '/api/v1/feed/collection/'];
const PIN_RESOURCE =
  /\/resource\/((?:BoardFeed|BoardSectionPins|UserPins|UserActivityPins|UserActivityFeed)Resource)\/get\//;
const FRIENDLY_NAME = /^[A-Za-z0-9_]{1,80}$/;

export interface CensusHit {
  platform: Platform;
  label: string;
}

/**
 * Same URL tests as matchPlatform() in electron/webview-injected.ts (not exported there), in
 * the same order, so the census counts exactly the responses the hook parses.
 */
export function censusLabel(
  rawUrl: string,
  friendlyName: () => string | null,
  baseUrl: string,
): CensusHit | null {
  if (!rawUrl) return null;
  let path = '';
  try {
    path = new URL(rawUrl, baseUrl).pathname;
  } catch {
    return null;
  }
  if (IG_PATHS.some((p) => rawUrl.includes(p))) {
    if (rawUrl.includes('/graphql/query'))
      return { platform: 'instagram', label: `graphql ${friendlyName() ?? '(unnamed)'}` };
    return { platform: 'instagram', label: `rest ${path.replace(/\/\d+(?=\/)/g, '/:id')}` };
  }
  if (rawUrl.includes('/i/api/graphql/') && rawUrl.toLowerCase().includes('bookmark')) {
    const operation = /\/i\/api\/graphql\/[^/]+\/([^/?#]+)/.exec(path)?.[1] ?? '(unknown)';
    return { platform: 'twitter', label: `graphql ${operation}` };
  }
  const resource = PIN_RESOURCE.exec(rawUrl);
  if (resource) return { platform: 'pinterest', label: `resource ${resource[1]}` };
  return null;
}

function readHeader(headers: unknown, name: string): string | null {
  if (!headers) return null;
  if (typeof Headers !== 'undefined' && headers instanceof Headers) return headers.get(name);
  if (Array.isArray(headers)) {
    for (const pair of headers)
      if (Array.isArray(pair) && String(pair[0]).toLowerCase() === name) return String(pair[1]);
    return null;
  }
  if (typeof headers === 'object') {
    for (const [key, value] of Object.entries(headers as Record<string, unknown>))
      if (key.toLowerCase() === name) return String(value);
  }
  return null;
}

function readBodyParam(body: unknown, name: string): string | null {
  try {
    if (typeof body === 'string') return new URLSearchParams(body).get(name);
    if (typeof URLSearchParams !== 'undefined' && body instanceof URLSearchParams)
      return body.get(name);
    if (typeof FormData !== 'undefined' && body instanceof FormData) {
      const value = body.get(name);
      return typeof value === 'string' ? value : null;
    }
  } catch {
    /* unreadable body: no name */
  }
  return null;
}

/** IG Relay names each GraphQL query in a header and in the form body. */
export function friendlyNameFrom(headers: unknown, body: unknown): string | null {
  const name =
    readHeader(headers, 'x-fb-friendly-name') ?? readBodyParam(body, 'fb_api_req_friendly_name');
  return name && FRIENDLY_NAME.test(name) ? name : null;
}

function requestUrl(input: unknown): string {
  if (typeof input === 'string') return input;
  if (input instanceof URL) return input.href;
  if (input && typeof input === 'object' && typeof (input as Request).url === 'string')
    return (input as Request).url;
  return '';
}

interface XhrState {
  url: string;
  friendlyName: string | null;
}

/**
 * Wraps fetch and XMLHttpRequest (outermost, after the hook installed its own patches) and
 * posts the counts every `flushMs`. Every wrapper is transparent: it calls through unchanged.
 */
export function installCensus(win: Window & typeof globalThis, flushMs = 2000): void {
  const pending = new Map<string, number>();
  const record = (url: string, friendlyName: () => string | null): void => {
    try {
      const hit = censusLabel(url, friendlyName, win.location.href);
      if (!hit) return;
      const key = `${hit.platform}|${hit.label}`;
      pending.set(key, (pending.get(key) ?? 0) + 1);
    } catch {
      /* never let the census break a page request */
    }
  };

  const innerFetch = win.fetch;
  if (typeof innerFetch === 'function') {
    win.fetch = function (this: unknown, ...args: Parameters<typeof fetch>): Promise<Response> {
      const [input, init] = args;
      record(requestUrl(input), () =>
        friendlyNameFrom(
          init?.headers ?? (input instanceof Request ? input.headers : null),
          init?.body,
        ),
      );
      return innerFetch.apply(this, args);
    } as typeof fetch;
  }

  const proto = win.XMLHttpRequest?.prototype;
  if (proto) {
    const state = new WeakMap<XMLHttpRequest, XhrState>();
    const innerOpen = proto.open as (this: XMLHttpRequest, ...args: unknown[]) => void;
    const innerSetHeader = proto.setRequestHeader;
    const innerSend = proto.send;
    proto.open = function (this: XMLHttpRequest, ...args: unknown[]): void {
      state.set(this, { url: requestUrl(args[1]), friendlyName: null });
      return innerOpen.apply(this, args);
    } as typeof proto.open;
    proto.setRequestHeader = function (this: XMLHttpRequest, name: string, value: string): void {
      const s = state.get(this);
      if (s && name.toLowerCase() === 'x-fb-friendly-name') s.friendlyName = value;
      return innerSetHeader.call(this, name, value);
    };
    proto.send = function (this: XMLHttpRequest, body?: Document | XMLHttpRequestBodyInit | null) {
      const s = state.get(this);
      if (s) record(s.url, () => s.friendlyName ?? friendlyNameFrom(null, body));
      return innerSend.call(this, body);
    };
  }

  win.setInterval(() => {
    if (!pending.size) return;
    const counts = Object.fromEntries(pending);
    pending.clear();
    win.postMessage({ type: CENSUS_MESSAGE, counts }, win.location.origin);
  }, flushMs);
}
