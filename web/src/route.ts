// The web app's few addresses (P1-04 brings the full routes):
//
// - `/login`: the sign-in page; `?error=invalid_link` explains a failed link.
// - `/login/magic#<token>`: what a sign-in link opens. The token stays in the
//   fragment, so it never reaches a server or its logs.
// - anything else: the app, once signed in.

export const LOGIN_PATH = '/login';
export const MAGIC_PATH = '/login/magic';

export type InitialRoute =
  | { kind: 'magic'; token: string | null }
  | { kind: 'login'; error: string | null }
  | { kind: 'app' };

// Reads the route of the page load. On the sign-in link page it takes the
// token out of the address at once, so it does not linger in the history.
export function readInitialRoute(
  location: Pick<Location, 'pathname' | 'search' | 'hash'>,
  history: Pick<History, 'replaceState'>,
): InitialRoute {
  const path = location.pathname.replace(/\/+$/, '') || '/';
  if (path === MAGIC_PATH) {
    const token = location.hash.replace(/^#/, '').trim();
    if (location.hash) history.replaceState(null, '', MAGIC_PATH);
    return { kind: 'magic', token: token || null };
  }
  if (path === LOGIN_PATH) {
    return { kind: 'login', error: new URLSearchParams(location.search).get('error') };
  }
  return { kind: 'app' };
}
