import { defineConfig, type Plugin } from 'vite';
import react from '@vitejs/plugin-react';
import tailwindcss from 'tailwindcss';
import autoprefixer from 'autoprefixer';

const VIRTUAL_ID = 'virtual:build-time';
const RESOLVED_ID = '\0' + VIRTUAL_ID;

function buildTimePlugin(): Plugin {
  return {
    name: 'build-time',
    resolveId(id) {
      if (id === VIRTUAL_ID) return RESOLVED_ID;
      return undefined;
    },
    load(id) {
      if (id === RESOLVED_ID) {
        return `export const buildTime = ${Date.now()}`;
      }
      return undefined;
    },
    handleHotUpdate({ server }) {
      const mod = server.moduleGraph.getModuleById(RESOLVED_ID);
      if (mod) server.moduleGraph.invalidateModule(mod);
    },
  };
}

export default defineConfig({
  plugins: [react(), buildTimePlugin()],
  // PostCSS pipeline (was postcss.config.js) — inlined so the whole config is TS.
  css: {
    postcss: {
      plugins: [tailwindcss(), autoprefixer()],
    },
  },
  root: '.',
  base: './',
  build: {
    outDir: 'dist',
    emptyOutDir: true,
  },
  server: {
    port: 5173,
    strictPort: true,
  },
});
