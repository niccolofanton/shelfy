import React, { useCallback, useEffect, useMemo, useState } from 'react';
import { Link, useLocation, useSearch } from 'wouter';
import App from '@ui/App';
import Logo from '@ui/components/Logo';
import { ShelfyProvider } from '@ui/api/ShelfyProvider';
import type { ShelfyClient } from '@ui/api/ShelfyClient';
import { useT } from '@ui/i18n';
import type { AuthApi } from './api/auth';
import type { Http } from './api/http';
import AuthLayout, { Notice, PRIMARY_BUTTON } from './auth/AuthLayout';
import LoginScreen from './auth/LoginScreen';
import MagicLinkScreen from './auth/MagicLinkScreen';
import { WebNavigation, loginPath, parseRoute, takeMagicToken } from './routes';

type Session = 'checking' | 'signedOut' | 'signedIn';

// Pages by address (./routes.tsx) and sign-in versus app. The session is the
// server's (an HttpOnly cookie): the app asks `GET /me` once, and any 401
// later sends the user back to the sign-in page, which remembers the page
// they were on (`?next=`).
export default function Root({
  magicToken = null,
  client,
  auth,
  http,
}: {
  // The token of the sign-in link this page load opened, already taken out
  // of the address (takeMagicToken).
  magicToken?: string | null;
  client: ShelfyClient;
  auth: AuthApi;
  http: Http;
}): React.JSX.Element {
  const [path, navigate] = useLocation();
  const search = useSearch();
  const route = useMemo(() => parseRoute(path, search), [path, search]);
  const [token, setToken] = useState<string | null>(magicToken);
  const [session, setSession] = useState<Session>('checking');

  useEffect(() => http.onUnauthorized(() => setSession('signedOut')), [http]);

  // A link opened over the link page changes only the fragment, which does
  // not reload the page: take its token the same way.
  useEffect(() => {
    const onHashChange = (): void => {
      const next = takeMagicToken(window.location, window.history);
      if (next) setToken(next);
    };
    window.addEventListener('hashchange', onHashChange);
    return () => window.removeEventListener('hashchange', onHashChange);
  }, []);

  const onLinkPage = route.name === 'magic';
  useEffect(() => {
    if (onLinkPage || session !== 'checking') return undefined;
    let alive = true;
    auth
      .me()
      .then((me) => alive && setSession(me ? 'signedIn' : 'signedOut'))
      // Server unreachable: the sign-in page says so when it loads the methods.
      .catch(() => alive && setSession('signedOut'));
    return () => {
      alive = false;
    };
  }, [auth, onLinkPage, session]);

  // Keep the address in step: the sign-in page while signed out, never once in.
  useEffect(() => {
    if (route.name === 'magic') return;
    if (session === 'signedIn' && route.name === 'login') {
      navigate(route.next ?? '/', { replace: true });
    } else if (session === 'signedOut' && route.name !== 'login') {
      navigate(loginPath(path), { replace: true });
    }
  }, [navigate, path, route, session]);

  const handleSignedIn = useCallback(() => {
    navigate('/', { replace: true });
    setSession('checking');
  }, [navigate]);

  if (route.name === 'magic') {
    return (
      <MagicLinkScreen key={token ?? ''} token={token} auth={auth} onSignedIn={handleSignedIn} />
    );
  }
  if (session === 'signedOut') {
    return <LoginScreen auth={auth} error={route.name === 'login' ? route.error : null} />;
  }
  // Checking, or signed in on the sign-in page on the way to `next`.
  if (session === 'checking' || route.name === 'login') {
    return (
      <div className="flex h-full w-full items-center justify-center bg-[#0f0f0f]">
        <Logo size={28} />
      </div>
    );
  }
  if (route.name === 'device') return <DevicePage />;
  return (
    <ShelfyProvider client={client}>
      <WebNavigation>
        <App />
      </WebNavigation>
    </ShelfyProvider>
  );
}

// `/device`: a device's sign-in code (the migration CLI's) is approved here
// once the account API has the device flow (P1-17, P1-20).
function DevicePage(): React.JSX.Element {
  const t = useT('errors');
  return (
    <AuthLayout title={t('unavailableTitle')}>
      <div className="space-y-4">
        <Notice tone="info" testId="device-unavailable">
          {t('unavailable')}
        </Notice>
        <Link href="/" className={PRIMARY_BUTTON} data-testid="device-back">
          {t('backToLibrary')}
        </Link>
      </div>
    </AuthLayout>
  );
}
