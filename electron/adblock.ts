// Request-level ad/tracker blocking for the Playwright capture engine.
//
// WHY (in a headless capture tool, not a browser): blocking ad/analytics/tracker
// requests makes captures FASTER (fewer requests to wait on), CLEANER (no ad
// banners / tracking overlays in the screenshot) and quieter (fewer consent
// prompts triggered). We do it at the network layer (Playwright route → abort) with
// a curated host blocklist rather than pulling EasyList: zero dependency, works
// fully OFFLINE and in a packaged build, and can't go stale-and-break a capture.
//
// Conservative by design: we only block well-known ad/tracker NETWORKS by host, so
// a site's own first-party assets, fonts and CDNs are never touched. Easy to extend.

import type { BrowserContext } from 'playwright-core';

// Host suffixes of ad networks, analytics, trackers, session-replay, CMP/consent
// widgets and tag managers. Matched as a domain suffix (so "doubleclick.net" also
// covers "stats.g.doubleclick.net").
const BLOCKED_HOST_SUFFIXES = new Set<string>([
  // Google ads / analytics / tag manager
  'doubleclick.net',
  'googlesyndication.com',
  'googleadservices.com',
  'google-analytics.com',
  'googletagmanager.com',
  'googletagservices.com',
  'adservice.google.com',
  'pagead2.googlesyndication.com',
  'analytics.google.com',
  // Meta / Facebook (the JS pixel loader; the /tr beacon path is in
  // BLOCKED_PATH_FRAGMENTS so we don't blanket-block all of facebook.com)
  'connect.facebook.net',
  // X / TikTok / LinkedIn / Bing / Snap / Pinterest ads
  'ads-twitter.com',
  'analytics.twitter.com',
  'static.ads-twitter.com',
  'analytics.tiktok.com',
  'ads.tiktok.com',
  'px.ads.linkedin.com',
  'bat.bing.com',
  'sc-static.net',
  'tr.snapchat.com',
  'ct.pinterest.com',
  // Programmatic / RTB / ad exchanges
  'adnxs.com',
  'amazon-adsystem.com',
  'criteo.com',
  'criteo.net',
  'taboola.com',
  'outbrain.com',
  'adroll.com',
  'pubmatic.com',
  'rubiconproject.com',
  'openx.net',
  'casalemedia.com',
  '33across.com',
  'bidswitch.net',
  'smartadserver.com',
  'yieldmo.com',
  'indexww.com',
  'moatads.com',
  'adsafeprotected.com',
  'serving-sys.com',
  'teads.tv',
  'sharethrough.com',
  'gumgum.com',
  'media.net',
  'contextweb.com',
  'sonobi.com',
  'districtm.io',
  // Analytics / session replay / heatmaps / product analytics
  'hotjar.com',
  'hotjar.io',
  'mouseflow.com',
  'fullstory.com',
  'clarity.ms',
  'mixpanel.com',
  'segment.com',
  'segment.io',
  'amplitude.com',
  'heap.io',
  'heapanalytics.com',
  'crazyegg.com',
  'luckyorange.com',
  'inspectlet.com',
  'logrocket.com',
  'logrocket.io',
  'sentry.io', // error telemetry (not needed for a screenshot)
  'newrelic.com',
  'nr-data.net',
  'scorecardresearch.com',
  'quantserve.com',
  'quantcount.com',
  'chartbeat.com',
  'parsely.com',
  'cdn.parsely.com',
  'matomo.cloud',
  'plausible.io',
  'cloudflareinsights.com',
  'static.cloudflareinsights.com',
  // Consent-management platforms (we already dismiss banners; blocking the widget
  // avoids the overlay entirely on most sites)
  'cookielaw.org',
  'onetrust.com',
  'cookiebot.com',
  'consensu.org',
  'usercentrics.eu',
  'trustarc.com',
  'iubenda.com',
  'termly.io',
  'cookieyes.com',
  // Tag / customer messaging widgets that add chat bubbles / popups
  'intercom.io',
  'intercomcdn.com',
  'drift.com',
  'driftt.com',
  'tawk.to',
  'zdassets.com', // zendesk widget
  'hs-scripts.com', // hubspot
  'hs-analytics.net',
  'hsforms.com',
]);

// A few path fragments that signal an ad/tracker regardless of host (cheap, after
// the host check). Kept tiny to avoid false positives on first-party assets.
const BLOCKED_PATH_FRAGMENTS = ['/gtag/js', '/gtm.js', '/collect?', '/pixel?', '/ga.js'];

// The Meta pixel beacon (facebook.com/tr, with or without a trailing slash and
// query). Matched HOST-SCOPED, not as a global path fragment: a bare '/tr' path is
// a legitimate first-party route on many sites (e.g. Turkey country paths), so a
// host-agnostic '/tr?' substring would abort real first-party requests.
function isMetaPixel(u: URL): boolean {
  const h = u.hostname.toLowerCase();
  const fb = h === 'facebook.com' || h.endsWith('.facebook.com');
  return fb && /^\/tr\/?$/.test(u.pathname);
}

function hostBlocked(hostname: string): boolean {
  const h = hostname.toLowerCase();
  for (const hostSuffix of BLOCKED_HOST_SUFFIXES) {
    // Entries are bare host suffixes only; path-specific blocking (e.g. the Meta
    // /tr pixel beacon) lives in isMetaPixel / BLOCKED_PATH_FRAGMENTS, not here.
    if (h === hostSuffix || h.endsWith('.' + hostSuffix)) return true;
  }
  return false;
}

// Decide whether a request URL is an ad/tracker that should be aborted.
function shouldBlock(urlStr: string): boolean {
  let u: URL;
  try {
    u = new URL(urlStr);
  } catch {
    return false;
  }
  if (u.protocol !== 'http:' && u.protocol !== 'https:') return false;
  if (hostBlocked(u.hostname)) return true;
  if (isMetaPixel(u)) return true;
  const pathAndQuery = (u.pathname + u.search).toLowerCase();
  for (const frag of BLOCKED_PATH_FRAGMENTS) {
    if (pathAndQuery.includes(frag)) return true;
  }
  return false;
}

interface AdblockHandle {
  blockedCount: () => number;
}

// Attach the blocker to a Playwright context: aborts ad/tracker requests, lets
// everything else through. NEVER blocks the main document (navigation), so a page
// can always load even if its host somehow matched. Best-effort: a routing error
// must not break the capture.
async function attachAdblock(context: BrowserContext): Promise<AdblockHandle> {
  let blocked = 0;
  try {
    await context.route('**/*', (route) => {
      try {
        const req = route.request();
        if (req.resourceType() !== 'document' && shouldBlock(req.url())) {
          blocked++;
          return route.abort();
        }
      } catch {
        /* fall through to continue */
      }
      return route.continue();
    });
  } catch {
    /* routing unavailable → capture proceeds without adblock */
  }
  return { blockedCount: () => blocked };
}

export { attachAdblock, shouldBlock, BLOCKED_HOST_SUFFIXES };
