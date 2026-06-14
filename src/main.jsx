import React from 'react';
import { createRoot } from 'react-dom/client';
import './index.css';
import App from './App';
import { I18nProvider } from './i18n';

const root = createRoot(document.getElementById('root'));
root.render(
  <I18nProvider>
    <App />
  </I18nProvider>,
);
