import React, { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useLocation, useSearch } from 'wouter';
import App from '@ui/App';
import Logo from '@ui/components/Logo';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import { LANGUAGES, useLang } from '@ui/i18n';
import type { AuthApi, Me } from './api/auth';
import type { Http } from './api/http';
import DevicePage from './auth/DevicePage';
import LoginScreen from './auth/LoginScreen';
import MagicLinkScreen from './auth/MagicLinkScreen';
import { ReauthHost } from './auth/ReauthDialog';
import ReauthLinkScreen from './auth/ReauthLinkScreen';
import {
  WebNavigation,
  loginPath,
  parseRoute,
  takeDeviceCode,
  takeMagicToken,
  takeReauthToken,
} from './routes';

// The session is the server's (an HttpOnly cookie). Signed in, it carries the
// user (`GET /me`) and the ShelfyClient made for them: one per session,
// because the client owns the session's realtime stream.
type Session =
  | { state: 'checking' }
  | { state: 'signedOut' }
  | { state: 'signedIn'; me: Me; client: ShelfyClient };

// What a page load took out of the address before anything rendered (./routes:
// a link's token, a device code).
export interface Fragments {
  magicToken?: string | null;
  reauthToken?: string | null;
  deviceCode?: string | null;
}

// Pages by address (./routes.tsx) and sign-in versus app. The app asks
// `GET /me` once per session; any 401 later sends the user back to the sign-in
// page, which remembers the page they were on (`?next=`). The interface takes
// the account's language (`GET /me/settings`) when it has one, so it follows
// the user from browser to browser.
export default function Root({
  fragments = {},
  createClient,
  auth,
  http,
}: {
  fragments?: Fragments;
  // Makes the session's client for the signed-in user.
  createClient: (me: Me) => ShelfyClient;
  auth: AuthApi;
  http: Http;
}): React.JSX.Element {
  const [path, navigate] = useLocation();
  const search = useSearch();
  const route = useMemo(() => parseRoute(path, search), [path, search]);
  const { setLang } = useLang();
  const setLangRef = useRef(setLang);
  setLangRef.current = setLang;
  const [magicToken, setMagicToken] = useState<string | null>(fragments.magicToken ?? null);
  const [reauthToken, setReauthToken] = useState<string | null>(fragments.reauthToken ?? null);
  const [deviceCode, setDeviceCode] = useState<string | null>(fragments.deviceCode ?? null);
  const [session, setSession] = useState<Session>({ state: 'checking' });

  useEffect(() => http.onUnauthorized(() => setSession({ state: 'signedOut' })), [http]);

  // A link opened over the same page changes only the fragment, which does not
  // reload the page: take its token (or code) the same way.
  useEffect(() => {
    const onHashChange = (): void => {
      const magic = takeMagicToken(window.location, window.history);
      if (magic) setMagicToken(magic);
      const reauth = takeReauthToken(window.location, window.history);
      if (reauth) setReauthToken(reauth);
      const code = takeDeviceCode(window.location, window.history);
      if (code) setDeviceCode(code);
    };
    window.addEventListener('hashchange', onHashChange);
    return () => window.removeEventListener('hashchange', onHashChange);
  }, []);

  // The link pages spend their token without the session check.
  const onLinkPage = route.name === 'magic' || route.name === 'reauth';
  useEffect(() => {
    if (onLinkPage || session.state !== 'checking') return undefined;
    let alive = true;
    Promise.all([auth.me(), auth.settings().catch(() => null)])
      .then(([me, settings]) => {
        if (!alive) return;
        if (!me) {
          setSession({ state: 'signedOut' });
          return;
        }
        const language = settings?.language;
        if (language && LANGUAGES.some((l) => l.code === language)) setLangRef.current(language);
        setSession({ state: 'signedIn', me, client: createClient(me) });
      })
      // Server unreachable: the sign-in page says so when it loads the methods.
      .catch(() => alive && setSession({ state: 'signedOut' }));
    return () => {
      alive = false;
    };
  }, [auth, createClient, onLinkPage, session.state]);

  // Keep the address in step: the sign-in page while signed out, never once in.
  useEffect(() => {
    if (onLinkPage) return;
    if (session.state === 'signedIn' && route.name === 'login') {
      navigate(route.next ?? '/', { replace: true });
    } else if (session.state === 'signedOut' && route.name !== 'login') {
      navigate(loginPath(path), { replace: true });
    }
  }, [navigate, onLinkPage, path, route, session.state]);

  // A sign-in link redeemed: on to the library, as a new session.
  const handleLinkSignedIn = useCallback(() => {
    navigate('/', { replace: true });
    setSession({ state: 'checking' });
  }, [navigate]);

  // A passkey signed in on the sign-in page: check the session again, which
  // goes on to `?next=`.
  const handlePasskeySignedIn = useCallback(() => setSession({ state: 'checking' }), []);

  if (route.name === 'magic') {
    return (
      <MagicLinkScreen
        key={magicToken ?? ''}
        token={magicToken}
        auth={auth}
        onSignedIn={handleLinkSignedIn}
      />
    );
  }
  if (route.name === 'reauth') {
    return <ReauthLinkScreen key={reauthToken ?? ''} token={reauthToken} auth={auth} />;
  }
  if (session.state === 'signedOut') {
    return (
      <LoginScreen
        auth={auth}
        error={route.name === 'login' ? route.error : null}
        onSignedIn={handlePasskeySignedIn}
      />
    );
  }
  // Checking, or signed in on the sign-in page on the way to `next`.
  if (session.state === 'checking' || route.name === 'login') {
    return (
      <div className="flex h-full w-full items-center justify-center bg-[#0f0f0f]">
        <Logo size={28} />
      </div>
    );
  }
  const { me, client } = session;
  return (
    <>
      {route.name === 'device' ? (
        <DevicePage key={deviceCode ?? ''} auth={auth} initialCode={deviceCode} />
      ) : (
        <ShelfyProvider client={client}>
          <WebNavigation>
            <App />
          </WebNavigation>
        </ShelfyProvider>
      )}
      <ReauthHost
        http={http}
        auth={auth}
        methods={{ passkeys: me.capabilities.passkeys, emailLink: me.capabilities.emailLink }}
      />
    </>
  );
}
