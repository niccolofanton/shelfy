import React from 'react';
import { createRoot } from 'react-dom/client';
import '@ui/index.css';
import { I18nProvider } from '@ui/i18n';
import Root from './Root';
import { createAuthApi } from './api/auth';
import { createHttp } from './api/http';
import { createHttpClient } from './api/httpClient';
import { readInitialRoute } from './route';

// Read before anything renders: a sign-in link's token leaves the address bar
// right away.
const route = readInitialRoute(window.location, window.history);
const http = createHttp();
const client = createHttpClient(http);
const auth = createAuthApi(http);

const container = document.getElementById('root');
if (!container) throw new Error('Root element #root not found');
createRoot(container).render(
  <I18nProvider>
    <Root route={route} client={client} auth={auth} http={http} />
  </I18nProvider>,
);
