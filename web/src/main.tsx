import React from 'react';
import { createRoot } from 'react-dom/client';
import { buildTime } from 'virtual:build-time';
import '@ui/index.css';
import { I18nProvider } from '@ui/i18n';
import ErrorBoundary from '@ui/components/ErrorBoundary';
import Root from './Root';
import { createAuthApi, type Me } from './api/auth';
import { createErrorReporter } from './api/clientErrors';
import { createHttp } from './api/http';
import { createHttpClient } from './api/httpClient';
import { parseRoute, patternOf, takeDeviceCode, takeMagicToken, takeReauthToken } from './routes';

// Read before anything renders: a link's token leaves the address bar right
// away, and so does a device code.
const fragments = {
  magicToken: takeMagicToken(window.location, window.history),
  reauthToken: takeReauthToken(window.location, window.history),
  deviceCode: takeDeviceCode(window.location, window.history),
};
const http = createHttp();
const reportError = createErrorReporter(http, {
  route: () => patternOf(parseRoute(window.location.pathname)),
  clientVersion: String(buildTime),
});
const auth = createAuthApi(http);
// One client per session, made for the signed-in user (their capabilities and
// account, `GET /me`).
const createClient = (me: Me) => createHttpClient(http, { me, reportError });

const container = document.getElementById('root');
if (!container) throw new Error('Root element #root not found');
createRoot(container).render(
  <I18nProvider>
    {/* The last resort, for the sign-in pages too: App and each of its views
        have their own boundaries. */}
    <ErrorBoundary view="root" onError={reportError} layout="page">
      <Root fragments={fragments} createClient={createClient} auth={auth} http={http} />
    </ErrorBoundary>
  </I18nProvider>,
);
