// The web app's addresses (plan §2.19 Routes; P1 assumption G14 adds the
// last three), and the shared UI's Navigation on the address bar (wouter).
//
// | Path                   | Page                                             |
// |------------------------|--------------------------------------------------|
// | `/`                    | the library                                      |
// | `/c/:collectionId`     | the library, one folder                          |
// | `/p/:key`              | a post, over the library                         |
// | `/trash`               | the trash                                        |
// | `/settings/:section`   | a Settings section; `/settings` is the first one |
// | `/device`              | approving a device's sign-in code (the CLI's);   |
// |                        | `/device#<code>` fills the code in               |
// | `/login`               | sign-in; `?error=invalid_link`, `?next=<path>`   |
// | `/login/magic#<token>` | what a sign-in link opens                        |
// | `/login/reauth#<token>`| what a re-authentication link opens              |
// | `/share`               | Android share, the bookmarklet, the iOS          |
// |                        | Shortcut; `?url=`, `?text=`, `?title=` (P2-07)   |
//
// A link's token stays in the fragment, so it never reaches a server or its
// logs (EXECUTION.md L1); the app takes it out of the address at once.
//
// The first five are AppRoutes (src/api/navigation.tsx), which App renders. The
// rest are web-only pages that Root shows around App. Anything else is
// `notFound`. Adding a route is one PATHS entry, one row of APP_ROUTES (or a
// line in parseRoute) and its case in pathOf.
import React, { useCallback, useMemo, useRef } from 'react';
import { useLocation, useSearch } from 'wouter';
import {
  DEFAULT_SETTINGS_SECTION,
  NavigationProvider,
  type AppRoute,
  type CurrentRoute,
  type Navigation,
} from '@ui/api/navigation';

export const PATHS = {
  library: '/',
  collection: '/c/:collectionId',
  post: '/p/:key',
  trash: '/trash',
  aiTags: '/ai/tags',
  aiSearch: '/ai/search',
  jobs: '/jobs',
  aiQueue: '/ai/queue',
  settings: '/settings/:section',
  device: '/device',
  login: '/login',
  magic: '/login/magic',
  reauth: '/login/reauth',
  share: '/share',
} as const;

// Defensive cap on a `/jobs` query's repeated values — the Jobs view (the one
// place that knows which kinds and states are valid) filters further; this
// only keeps a malformed or adversarial address from building an unbounded
// array.
const MAX_JOBS_FILTER_VALUES = 20;

export const LOGIN_PATH = PATHS.login;

// The pattern reported for an address that matches no route.
const NOT_FOUND_PATTERN = '/*';

// `url`, `text` and `title` are the Web Share Target API's own field names
// (plan §2.17 Android), parsed here like `login`'s `next` so that
// `web/src/share/SharePage.tsx` never re-reads the address itself.
export type ShareRoute = {
  name: 'share';
  url: string | null;
  text: string | null;
  title: string | null;
};

export type WebRoute =
  | CurrentRoute
  | { name: 'device' }
  | { name: 'login'; error: string | null; next: string | null }
  | { name: 'magic' }
  | { name: 'reauth' }
  | ShareRoute;

type Params = Record<string, string>;

// Matches `path` against a pattern of fixed segments and `:name` segments.
function matchPattern(pattern: string, path: string): Params | null {
  const want = pattern.split('/').filter(Boolean);
  const got = path.split('/').filter(Boolean);
  if (want.length !== got.length) return null;
  const params: Params = {};
  for (let i = 0; i < want.length; i++) {
    if (want[i].startsWith(':')) {
      try {
        params[want[i].slice(1)] = decodeURIComponent(got[i]);
      } catch {
        return null;
      }
    } else if (want[i] !== got[i]) {
      return null;
    }
  }
  return params;
}

// The `kind`/`state` repeats of a `/jobs` address, capped and with blanks
// dropped; the Jobs view validates the values themselves.
function jobsFilterOf(query: URLSearchParams, key: string): string[] {
  const values = query.getAll(key).filter(Boolean);
  return values.slice(0, MAX_JOBS_FILTER_VALUES);
}

// The AppRoutes: a pattern, and the route its parameters make (null when they
// are not valid, which reads as `notFound`). `search` only matters to `/jobs`
// today; every other route ignores its second argument.
const APP_ROUTES: {
  pattern: string;
  route: (params: Params, search: string) => AppRoute | null;
}[] = [
  { pattern: PATHS.library, route: () => ({ name: 'library' }) },
  {
    pattern: PATHS.collection,
    route: ({ collectionId }) =>
      /^[1-9]\d{0,15}$/.test(collectionId)
        ? { name: 'collection', collectionId: Number(collectionId) }
        : null,
  },
  { pattern: PATHS.post, route: ({ key }) => (key ? { name: 'post', key } : null) },
  { pattern: PATHS.aiTags, route: () => ({ name: 'aiTags' }) },
  { pattern: PATHS.trash, route: () => ({ name: 'trash' }) },
  { pattern: PATHS.aiQueue, route: () => ({ name: 'aiQueue' }) },
  { pattern: PATHS.aiSearch, route: () => ({ name: 'aiSearch' }) },
  {
    pattern: PATHS.jobs,
    route: (_params, search) => {
      const query = new URLSearchParams(search);
      return {
        name: 'jobs',
        kind: jobsFilterOf(query, 'kind'),
        state: jobsFilterOf(query, 'state'),
      };
    },
  },
  { pattern: '/settings', route: () => ({ name: 'settings', section: DEFAULT_SETTINGS_SECTION }) },
  {
    pattern: PATHS.settings,
    route: ({ section }) =>
      /^[a-z0-9-]{1,32}$/.test(section) ? { name: 'settings', section } : null,
  },
];

// The route of an address the shared UI shows (anything but the web-only
// pages). `search` is only read by `/jobs` (its `kind`/`state` filters).
export function parseAppRoute(pathname: string, search = ''): CurrentRoute {
  for (const { pattern, route } of APP_ROUTES) {
    const params = matchPattern(pattern, pathname);
    const found = params && route(params, search);
    if (found) return found;
  }
  return { name: 'notFound' };
}

// The route of an address.
export function parseRoute(pathname: string, search = ''): WebRoute {
  if (matchPattern(PATHS.magic, pathname)) return { name: 'magic' };
  if (matchPattern(PATHS.reauth, pathname)) return { name: 'reauth' };
  if (matchPattern(PATHS.device, pathname)) return { name: 'device' };
  if (matchPattern(PATHS.login, pathname)) {
    const query = new URLSearchParams(search);
    return { name: 'login', error: query.get('error'), next: safeNext(query.get('next')) };
  }
  if (matchPattern(PATHS.share, pathname)) {
    const query = new URLSearchParams(search);
    return {
      name: 'share',
      url: query.get('url'),
      text: query.get('text'),
      title: query.get('title'),
    };
  }
  return parseAppRoute(pathname, search);
}

// `/jobs` with its `kind`/`state` repeated as query parameters (empty: none).
function jobsPath(kind: string[], state: string[]): string {
  const query = new URLSearchParams();
  for (const k of kind) query.append('kind', k);
  for (const s of state) query.append('state', s);
  const qs = query.toString();
  return qs ? `${PATHS.jobs}?${qs}` : PATHS.jobs;
}

// The address of a route. Takes every WebRoute that `safeNext` allows through
// as a sign-in destination (AppRoutes, `device`, `share`) — not `login`,
// `magic` or `reauth`, which only ever ends there, so `navigate()` never
// targets them and callers keep using the narrower AppRoute type.
export function pathOf(route: AppRoute | { name: 'device' } | ShareRoute): string {
  switch (route.name) {
    case 'library':
      return '/';
    case 'collection':
      return `/c/${route.collectionId}`;
    case 'post':
      return `/p/${encodeURIComponent(route.key)}`;
    case 'aiTags':
      return PATHS.aiTags;
    case 'trash':
      return PATHS.trash;
    case 'aiQueue':
      return PATHS.aiQueue;
    case 'aiSearch':
      return PATHS.aiSearch;
    case 'jobs':
      return jobsPath(route.kind, route.state);
    case 'settings':
      return `/settings/${encodeURIComponent(route.section)}`;
    case 'device':
      return PATHS.device;
    case 'share': {
      // Rebuilt from the parsed fields, never the raw address (safeNext's
      // rule): a next= target can only ever carry what parseRoute itself
      // would have accepted.
      const params = new URLSearchParams();
      if (route.url) params.set('url', route.url);
      if (route.text) params.set('text', route.text);
      if (route.title) params.set('title', route.title);
      const query = params.toString();
      return query ? `${PATHS.share}?${query}` : PATHS.share;
    }
  }
}

// The pattern of a route, as crash reports name it: never the address, which
// carries ids.
export function patternOf(route: WebRoute): string {
  switch (route.name) {
    case 'library':
    case 'collection':
    case 'post':
    case 'trash':
    case 'aiTags':
    case 'jobs':
    case 'aiQueue':
    case 'aiSearch':
    case 'device':
    case 'login':
    case 'magic':
    case 'reauth':
    case 'share':
      return PATHS[route.name];
    case 'settings':
      return PATHS.settings;
    case 'notFound':
      return NOT_FOUND_PATTERN;
  }
}

// Where to go after signing in, from `?next=`: only a page of this app, and
// rebuilt from its route, so the value is never echoed into the address.
// `/share`'s content lives entirely in its query (`url`, `text`, `title`), so
// unlike every other target this one does read it — but, same as the rest,
// only the three parsed fields come back out (pathOf's `share` case), never
// the raw string.
export function safeNext(next: string | null | undefined): string | null {
  if (!next || !next.startsWith('/') || next.startsWith('//') || next.startsWith('/\\'))
    return null;
  const hashIndex = next.indexOf('#');
  const withoutHash = hashIndex === -1 ? next : next.slice(0, hashIndex);
  const queryIndex = withoutHash.indexOf('?');
  const pathname = queryIndex === -1 ? withoutHash : withoutHash.slice(0, queryIndex);
  const search = queryIndex === -1 ? '' : withoutHash.slice(queryIndex);
  const route = parseRoute(pathname, search);
  if (
    route.name === 'notFound' ||
    route.name === 'login' ||
    route.name === 'magic' ||
    route.name === 'reauth'
  ) {
    return null;
  }
  return pathOf(route);
}

// The sign-in page, remembering the page that was asked for.
export function loginPath(from?: string | null): string {
  const next = safeNext(from);
  return next && next !== '/' ? `${PATHS.login}?next=${encodeURIComponent(next)}` : PATHS.login;
}

// The fragment of an address on the page `path`, taken out of the address at
// once so it does not linger in the history; null on any other page or
// without one.
function takeFragment(
  path: string,
  location: Pick<Location, 'pathname' | 'hash'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  if (!matchPattern(path, location.pathname)) return null;
  const fragment = location.hash.replace(/^#/, '').trim();
  if (location.hash) history.replaceState(null, '', path);
  return fragment || null;
}

// The token of a sign-in link (`/login/magic#<token>`).
export function takeMagicToken(
  location: Pick<Location, 'pathname' | 'hash'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  return takeFragment(PATHS.magic, location, history);
}

// The token of a re-authentication link (`/login/reauth#<token>`).
export function takeReauthToken(
  location: Pick<Location, 'pathname' | 'hash'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  return takeFragment(PATHS.reauth, location, history);
}

// The code of a device's sign-in (`/device#BCDF-GHJK`, the CLI's
// `verificationUriComplete`), decoded; it only fills the form in.
export function takeDeviceCode(
  location: Pick<Location, 'pathname' | 'hash'>,
  history: Pick<History, 'replaceState'>,
): string | null {
  const code = takeFragment(PATHS.device, location, history);
  if (!code) return null;
  try {
    return decodeURIComponent(code).slice(0, 32);
  } catch {
    return null;
  }
}

// History entries the app pushes carry their depth in the app, so `back`
// knows whether the previous entry is one of its own pages.
const DEPTH = 'shelfyDepth';

function depthOf(state: unknown): number {
  const depth = (state as Record<string, unknown> | null)?.[DEPTH];
  return typeof depth === 'number' && depth > 0 ? depth : 0;
}

// The shared UI's Navigation (src/api/navigation.tsx) on the address bar.
// `navigate` and `back` keep their identity; `route` follows the address.
export function WebNavigation({ children }: { children: React.ReactNode }): React.JSX.Element {
  const [location, setLocation] = useLocation();
  const search = useSearch();
  const route = useMemo(() => parseAppRoute(location, search), [location, search]);
  const locationRef = useRef(location);
  locationRef.current = location;

  const navigate = useCallback<Navigation['navigate']>(
    (to, { replace = false } = {}) => {
      const path = pathOf(to);
      if (path === locationRef.current && !replace) return;
      const depth = depthOf(window.history.state);
      setLocation(path, { replace, state: { [DEPTH]: replace ? depth : depth + 1 } });
    },
    [setLocation],
  );

  const back = useCallback<Navigation['back']>(
    (fallback) => {
      if (depthOf(window.history.state) > 0) window.history.back();
      else setLocation(pathOf(fallback), { replace: true, state: { [DEPTH]: 0 } });
    },
    [setLocation],
  );

  const navigation = useMemo<Navigation>(
    () => ({ route, navigate, back }),
    [route, navigate, back],
  );
  return <NavigationProvider navigation={navigation}>{children}</NavigationProvider>;
}
