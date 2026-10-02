import React from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App';
import { I18nProvider } from './i18n';
import { ShelfyProvider } from './api/ShelfyProvider';
import { getElectronClient } from './api/electronClient';

const container = document.getElementById('root');
if (!container) throw new Error('Root element #root not found');
const root = createRoot(container);
root.render(
  <I18nProvider>
    <ShelfyProvider client={getElectronClient()}>
      <App />
    </ShelfyProvider>
  </I18nProvider>,
);
