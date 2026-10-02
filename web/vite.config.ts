// Vite config of the web app (Shelfy Web). It builds the desktop renderer in
// ../src (imported as `@ui/…`) behind the HTTP ShelfyClient, into web/dist.
//
//   pnpm run web:dev    dev server; /api and /media go to SHELFY_API_URL
//   pnpm run web:build  production build into web/dist
//
// The browser must reach the app on the server's SHELFY_PUBLIC_URL origin (the
// CSRF check compares Origin with it), so in dev that is this server's URL.
import { fileURLToPath } from 'node:url';
import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from 'tailwindcss';
import autoprefixer from 'autoprefixer';
import tailwindConfig from '../tailwind.config';

const webDir = fileURLToPath(new URL('.', import.meta.url));
const uiDir = fileURLToPath(new URL('../src', import.meta.url));

// The shelfy-server the dev server proxies to.
const apiTarget = process.env.SHELFY_API_URL || 'http://127.0.0.1:8080';
const port = Number(process.env.SHELFY_WEB_PORT || 5174);
const proxy = { '/api': apiTarget, '/media': apiTarget };

// `virtual:build-time`, which the renderer's dev bar imports; same module as
// the desktop's (vite.config.ts).
const VIRTUAL_ID = 'virtual:build-time';
const RESOLVED_ID = '\0' + VIRTUAL_ID;
function buildTimePlugin(): Plugin {
  return {
    name: 'build-time',
    resolveId: (id) => (id === VIRTUAL_ID ? RESOLVED_ID : undefined),
    load: (id) => (id === RESOLVED_ID ? `export const buildTime = ${Date.now()}` : undefined),
  };
}

export default defineConfig({
  root: webDir,
  base: '/',
  plugins: [react(), buildTimePlugin()],
  resolve: { alias: { '@ui': uiDir } },
  css: {
    postcss: {
      plugins: [
        tailwindcss({
          ...tailwindConfig,
          content: [`${webDir}index.html`, `${webDir}src/**/*.{ts,tsx}`, `${uiDir}/**/*.{ts,tsx}`],
        }),
        autoprefixer(),
      ],
    },
  },
  build: { outDir: 'dist', emptyOutDir: true },
  server: { port, strictPort: true, proxy },
  preview: { port, strictPort: true, proxy },
});
