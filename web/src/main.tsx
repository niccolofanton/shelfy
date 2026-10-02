import React from 'react';
import { createRoot } from 'react-dom/client';
import { buildTime } from 'virtual:build-time';
import '@ui/index.css';
import { I18nProvider } from '@ui/i18n';
import ErrorBoundary from '@ui/components/ErrorBoundary';
import Root from './Root';
import { createAuthApi } from './api/auth';
import { createErrorReporter } from './api/clientErrors';
import { createHttp } from './api/http';
import { createHttpClient } from './api/httpClient';
import { parseRoute, patternOf, takeMagicToken } from './routes';

// Read before anything renders: a sign-in link's token leaves the address bar
// right away.
const magicToken = takeMagicToken(window.location, window.history);
const http = createHttp();
const reportError = createErrorReporter(http, {
  route: () => patternOf(parseRoute(window.location.pathname)),
  clientVersion: String(buildTime),
});
const client = createHttpClient(http, { reportError });
const auth = createAuthApi(http);

const container = document.getElementById('root');
if (!container) throw new Error('Root element #root not found');
createRoot(container).render(
  <I18nProvider>
    {/* The last resort, for the sign-in pages too: App and each of its views
        have their own boundaries. */}
    <ErrorBoundary view="root" onError={reportError} layout="page">
      <Root magicToken={magicToken} client={client} auth={auth} http={http} />
    </ErrorBoundary>
  </I18nProvider>,
);
