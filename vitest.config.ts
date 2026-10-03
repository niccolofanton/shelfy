import { fileURLToPath } from 'node:url';
import type { Plugin } from 'vite';
import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

// `virtual:build-time` (the renderer's dev bar), which the Vite configs provide;
// lets suites render the whole App.
function buildTimeStub(): Plugin {
  const id = 'virtual:build-time';
  return {
    name: 'build-time-stub',
    resolveId: (source) => (source === id ? '\0' + id : undefined),
    load: (source) => (source === '\0' + id ? 'export const buildTime = 0' : undefined),
  };
}

export default defineConfig({
  plugins: [react(), buildTimeStub()],
  // The web app (web/) imports the renderer as `@ui/…`, as in web/vite.config.ts.
  resolve: { alias: { '@ui': fileURLToPath(new URL('./src', import.meta.url)) } },
  test: {
    globals: true,
    setupFiles: ['tests/setup.ts'],
    // Only the real suites under tests/, extension/tests/ (web port MV3 extension) and
    // web/tests/ (web app). Keeps Playwright specs (e2e/) and stale agent-worktree copies
    // (.claude/worktrees/) out of the unit run.
    include: [
      'tests/**/*.{test,spec}.{js,jsx,ts,tsx}',
      'extension/tests/**/*.test.ts',
      'web/tests/**/*.test.{ts,tsx}',
      'capture/tests/**/*.test.ts',
    ],
    exclude: ['**/node_modules/**', 'dist/**', 'release/**', '.claude/**', 'e2e/**'],
    environmentMatchGlobs: [
      ['tests/api/**', 'jsdom'],
      ['tests/components/**', 'jsdom'],
      ['tests/hooks/**', 'jsdom'],
      ['tests/views/**', 'jsdom'],
      ['web/tests/**', 'jsdom'],
    ],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'lcov'],
      include: ['electron/**/*.ts', 'src/**/*.{ts,tsx}', 'web/src/**/*.{ts,tsx}'],
      exclude: ['electron/main.ts', 'electron/interceptor.ts', 'electron/webview-preload.ts'],
    },
  },
});
