// Shelfy's service worker (plan §2.17 PWA, §2.19; P2-07 acceptance 1). Built
// with vite-plugin-pwa's `injectManifest` strategy, which only compiles this
// file and injects the precache list at `self.__WB_MANIFEST` — every runtime
// rule below is plain Workbox, not `generateSW`'s declarative config, because
// the navigation rule needs an exact policy `generateSW` cannot express: try
// the network first, and fall back to the cached shell only when the network
// is unreachable outright, never on an Access redirect or an error answer
// (web/src/Root.tsx's sign-in flow must still see those).
/// <reference lib="webworker" />
import { clientsClaim } from 'workbox-core';
import { cleanupOutdatedCaches, matchPrecache, precacheAndRoute } from 'workbox-precaching';
import { registerRoute, setCatchHandler, setDefaultHandler } from 'workbox-routing';
import { CacheFirst, NetworkOnly } from 'workbox-strategies';
import { CacheableResponsePlugin } from 'workbox-cacheable-response';
import { ExpirationPlugin } from 'workbox-expiration';

declare let self: ServiceWorkerGlobalScope;

// "Prompt for update" (acceptance 1, "a new version prompts a reload"):
// `registerType: 'prompt'` (web/vite.config.ts) means a waiting worker never
// activates on its own. `web/src/main.tsx`'s `updateSW(true)` posts this
// message once the user agrees; only then does the new version take over.
self.addEventListener('message', (event) => {
  if (event.data && (event.data as { type?: string }).type === 'SKIP_WAITING') self.skipWaiting();
});
// Once activated (by the message above, or on first install — nothing is
// "waiting" yet then), control every open tab at once rather than after their
// next reload.
clientsClaim();
// Drop any precached entry from an older build that the current manifest no
// longer lists.
cleanupOutdatedCaches();

// The app shell: this build's own JS, CSS, the HTML document, the manifest
// and the icons — precached at install, so the app opens offline.
precacheAndRoute(self.__WB_MANIFEST);

// `/api/*`: never served from a cache. Every call carries the session and
// must reach the server — or fail honestly — every time; a cached answer
// here could show stale data or silently swallow a session that ended.
registerRoute(({ url }) => url.pathname.startsWith('/api/'), new NetworkOnly());

// Grid thumbnails only (`/media/*.g480.webp`): cache-first, so a scrolled
// gallery stays smooth offline or on a flaky connection. `statuses: [200]`
// and the `Content-Type` check keep out anything but a real rendition —
// never a redirect, an Access challenge page or any other HTML. `maxEntries`
// caps the cache at roughly the plan's 150 MB budget: this rendition is a few
// tens of KB each, so 3,000 of them land well under it.
const G480_PATTERN = /\/media\/[^/]+\.g480\.webp$/;
registerRoute(
  ({ request, url }) => request.method === 'GET' && G480_PATTERN.test(url.pathname),
  new CacheFirst({
    cacheName: 'shelfy-media-g480',
    plugins: [
      new CacheableResponsePlugin({ statuses: [200], headers: { 'Content-Type': 'image/webp' } }),
      new ExpirationPlugin({ maxEntries: 3000, purgeOnQuotaError: true }),
    ],
  }),
);

// Everything else — above all, navigations (the library, a post, `/share`,
// Settings…) — goes straight to the network, with no timeout racing a cache:
// an expired Access session must still reach its sign-in page, and a slow
// but working connection must never be pre-empted by a stale shell.
setDefaultHandler(new NetworkOnly());

// Only when the network is unreachable at all (truly offline — not a 4xx,
// not a redirect, those are still answers) does a navigation fall back to
// the precached shell, for any address: the server's SPA fallback (P1-02,
// P1-09) already serves the same document for every client route, so the
// one precached entry covers every page the app has.
setCatchHandler(({ event }) => {
  const request = (event as FetchEvent).request;
  if (request.mode !== 'navigate') return Promise.resolve(Response.error());
  return matchPrecache('/index.html').then((cached) => cached ?? Response.error());
});
