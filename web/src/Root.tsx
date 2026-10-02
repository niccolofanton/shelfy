import React, { useCallback, useEffect, useState } from 'react';
import App from '@ui/App';
import Logo from '@ui/components/Logo';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import type { AuthApi } from './api/auth';
import type { Http } from './api/http';
import LoginScreen from './auth/LoginScreen';
import MagicLinkScreen from './auth/MagicLinkScreen';
import { LOGIN_PATH, readInitialRoute, type InitialRoute } from './route';

type Session = 'checking' | 'signedOut' | 'signedIn';

// Sign-in versus app. The session is the server's (an HttpOnly cookie): the
// app asks `GET /me` once, and any 401 later sends the user back here.
export default function Root({
  route: initialRoute,
  client,
  auth,
  http,
}: {
  route: InitialRoute;
  client: ShelfyClient;
  auth: AuthApi;
  http: Http;
}): React.JSX.Element {
  const [route, setRoute] = useState<InitialRoute>(initialRoute);
  const [session, setSession] = useState<Session>('checking');

  useEffect(() => http.onUnauthorized(() => setSession('signedOut')), [http]);

  // A link opened over the link page changes only the fragment, which does
  // not reload the page: take its token the same way.
  useEffect(() => {
    const onHashChange = (): void => {
      const next = readInitialRoute(window.location, window.history);
      if (next.kind === 'magic' && next.token) setRoute(next);
    };
    window.addEventListener('hashchange', onHashChange);
    return () => window.removeEventListener('hashchange', onHashChange);
  }, []);

  useEffect(() => {
    if (route.kind === 'magic' || session !== 'checking') return undefined;
    let alive = true;
    auth
      .me()
      .then((me) => alive && setSession(me ? 'signedIn' : 'signedOut'))
      // Server unreachable: the sign-in page says so when it loads the methods.
      .catch(() => alive && setSession('signedOut'));
    return () => {
      alive = false;
    };
  }, [auth, route.kind, session]);

  // Keep the address in step: /login while signed out, the app's path once in.
  useEffect(() => {
    if (route.kind === 'magic') return;
    const onLogin = window.location.pathname.startsWith(LOGIN_PATH);
    if (session === 'signedIn' && onLogin) window.history.replaceState(null, '', '/');
    if (session === 'signedOut' && !onLogin) window.history.replaceState(null, '', LOGIN_PATH);
  }, [route.kind, session]);

  const handleSignedIn = useCallback(() => {
    window.history.replaceState(null, '', '/');
    setRoute({ kind: 'app' });
    setSession('checking');
  }, []);

  if (route.kind === 'magic') {
    return (
      <MagicLinkScreen
        key={route.token ?? ''}
        token={route.token}
        auth={auth}
        onSignedIn={handleSignedIn}
      />
    );
  }
  if (session === 'checking') {
    return (
      <div className="flex h-full w-full items-center justify-center bg-[#0f0f0f]">
        <Logo size={28} />
      </div>
    );
  }
  if (session === 'signedOut') {
    return <LoginScreen auth={auth} error={route.kind === 'login' ? route.error : null} />;
  }
  return (
    <ShelfyProvider client={client}>
      <App />
    </ShelfyProvider>
  );
}
