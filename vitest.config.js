import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';

export default defineConfig({
  plugins: [react()],
  test: {
    globals: true,
    setupFiles: ['tests/setup.ts'],
    // Only the real suite under tests/. Keeps Playwright specs (e2e/) and stale
    // agent-worktree copies (.claude/worktrees/) out of the unit run.
    include: ['tests/**/*.{test,spec}.{js,jsx,ts,tsx}'],
    exclude: ['**/node_modules/**', 'dist/**', 'release/**', '.claude/**', 'e2e/**'],
    environmentMatchGlobs: [
      ['tests/components/**', 'jsdom'],
      ['tests/hooks/**', 'jsdom'],
      ['tests/views/**', 'jsdom'],
    ],
    coverage: {
      provider: 'v8',
      reporter: ['text', 'lcov'],
      include: ['electron/**/*.{js,ts}', 'src/**/*.{js,jsx,ts,tsx}'],
      exclude: [
        'electron/main.js',
        'electron/main.ts',
        'electron/interceptor.js',
        'electron/interceptor.ts',
        'electron/webview-preload.js',
        'electron/webview-preload.ts',
      ],
    },
  },
});
