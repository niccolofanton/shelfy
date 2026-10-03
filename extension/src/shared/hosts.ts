// Hosts the extension runs on and talks to.
//
// Manifest match patterns cannot express the `pinterest\.[a-z]{2,3}(\.[a-z]{2})?` regex the
// desktop uses (ALLOWED_HOSTS in src/lib/browserUrls.ts), so Pinterest gets an explicit ccTLD
// list here (plan §2.16). A unit test checks every entry against the desktop's isAllowedUrl(),
// so the two lists cannot drift apart. Locale subdomains (it.pinterest.com, …) are covered by
// the `*.` wildcard of each pattern. The server's ingest allowlist mirrors PINTEREST_HOSTS (P2-02).

import { isAllowedUrl } from '../../../src/lib/browserUrls';
import { PLATFORMS, type Platform } from './protocol';

export const PINTEREST_TLDS = [
  'com',
  'it',
  'de',
  'fr',
  'es',
  'co.uk',
  'ca',
  'com.au',
  'jp',
  'com.mx',
  'at',
  'ch',
  'pt',
  'se',
  'dk',
  'nz',
  'ie',
  'ph',
  'cl',
  'co.kr',
  'ru',
] as const;

export const PINTEREST_HOSTS: readonly string[] = PINTEREST_TLDS.map((tld) => `pinterest.${tld}`);

/** Content-script matches and host permissions: the social hosts the desktop syncs from. */
export const SOCIAL_MATCHES: readonly string[] = [
  'https://www.instagram.com/*',
  'https://x.com/*',
  'https://twitter.com/*',
  ...PINTEREST_HOSTS.map((host) => `https://*.${host}/*`),
];

/**
 * The platform CDNs of plan §2.16. Host permission lets the worker fetch media bytes with
 * `credentials: "omit"` for extension uploads (P2-17); no content script runs there.
 */
export const CDN_MATCHES: readonly string[] = [
  'https://*.cdninstagram.com/*',
  'https://*.fbcdn.net/*',
  'https://pbs.twimg.com/*',
  'https://video.twimg.com/*',
  'https://*.pinimg.com/*',
];

/** Platform of an https URL on a supported host, using the desktop's own allowlist. */
export function platformForUrl(url: string): Platform | null {
  for (const platform of PLATFORMS) if (isAllowedUrl(platform, url)) return platform;
  return null;
}

/** The production Shelfy origin (E5). */
export const DEFAULT_SHELFY_ORIGIN = 'https://refs.niccolofanton.dev';

const LOCAL_HOSTS = new Set(['localhost', '127.0.0.1']);

/**
 * Validates the Shelfy origin a build targets: `https://<host>[:port]`, or
 * `http://localhost:<port>` / `http://127.0.0.1:<port>` for development and e2e. Returns the
 * normalized origin, or null when the value is not one.
 */
export function parseShelfyOrigin(value: string): string | null {
  let url: URL;
  try {
    url = new URL(value);
  } catch {
    return null;
  }
  if (url.username || url.password || url.search || url.hash) return null;
  if (url.pathname !== '/' && url.pathname !== '') return null;
  if (url.protocol === 'https:') return url.hostname.includes('.') ? url.origin : null;
  if (url.protocol === 'http:') return LOCAL_HOSTS.has(url.hostname) ? url.origin : null;
  return null;
}

/** The match pattern of a Shelfy origin, for host_permissions and externally_connectable. */
export function originMatch(origin: string): string {
  return `${origin}/*`;
}
