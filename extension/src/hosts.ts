// Hosts the extension runs on.
//
// Manifest match patterns cannot express the `pinterest\.[a-z]{2,3}(\.[a-z]{2})?` regex the
// desktop uses (ALLOWED_HOSTS in src/lib/browserUrls.ts), so Pinterest gets an explicit ccTLD
// list here (plan §2.16). A unit test checks every entry against the desktop's isAllowedUrl(),
// so the two lists cannot drift apart. Locale subdomains (it.pinterest.com, …) are covered by
// the `*.` wildcard of each pattern.

import { isAllowedUrl } from '../../src/lib/browserUrls';
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

/** Platform of an https URL on a supported host, using the desktop's own allowlist. */
export function platformForUrl(url: string): Platform | null {
  for (const platform of PLATFORMS) if (isAllowedUrl(platform, url)) return platform;
  return null;
}
