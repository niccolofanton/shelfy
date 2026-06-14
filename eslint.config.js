import js from '@eslint/js';
import globals from 'globals';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import prettier from 'eslint-config-prettier';
import tseslint from 'typescript-eslint';

/**
 * Flat ESLint config for Shelfy.
 *
 * The codebase spans several runtimes, so rules are scoped per area:
 *  - electron/**        main process + webview scripts (CommonJS .js / ESM .ts, Node + browser)
 *  - src/**             React renderer (ESM, browser)
 *  - scripts/*.cjs|mjs|ts  build/eval tooling (Node)
 *  - tests/**           Vitest (jsdom)
 *  - e2e/**             Playwright (handled by tsc for types)
 *
 * TypeScript files (.ts/.tsx) get the typescript-eslint parser + recommended
 * rules; the per-area blocks below only contribute globals (and the CommonJS
 * sourceType for the legacy .js side), so the two module systems coexist while
 * the migration is in flight.
 *
 * Prettier owns formatting; this config focuses on correctness (real bugs,
 * React hook rules, undefined references), so `npm run lint` stays meaningful
 * without drowning in style noise.
 */
export default tseslint.config(
  {
    ignores: [
      'node_modules/**',
      '.claude/**',
      'dist/**',
      'dist-electron/**',
      'release/**',
      'out/**',
      'coverage/**',
      'e2e-report/**',
      'playwright-report/**',
      'test-results/**',
      'bin/**',
      'build/ms-playwright/**',
      '**/*.workflow.js',
      '.audit-*',
      '.vlm/**',
      '**/.scratch/**',
      '**/.scratch-alias/**',
    ],
  },

  js.configs.recommended,

  // Shared rule baseline
  {
    rules: {
      'no-unused-vars': [
        'warn',
        { args: 'none', ignoreRestSiblings: true, varsIgnorePattern: '^_' },
      ],
      'no-empty': ['warn', { allowEmptyCatch: true }],
      'no-constant-condition': ['error', { checkLoops: false }],
      'no-control-regex': 'off',
      // Several text-sanitization regexes intentionally embed NBSP / zero-width /
      // directional marks to strip them; only flag stray whitespace in real code.
      'no-irregular-whitespace': [
        'error',
        { skipStrings: true, skipComments: true, skipRegExps: true, skipTemplates: true },
      ],
    },
  },

  // TypeScript: parser + recommended rules for every .ts/.tsx file.
  {
    files: ['**/*.{ts,tsx}'],
    extends: [tseslint.configs.recommended],
    languageOptions: {
      sourceType: 'module',
      parserOptions: { ecmaFeatures: { jsx: true } },
    },
    rules: {
      // TS owns unused detection; the base rule double-reports on typed code.
      'no-unused-vars': 'off',
      '@typescript-eslint/no-unused-vars': [
        'warn',
        {
          args: 'none',
          ignoreRestSiblings: true,
          varsIgnorePattern: '^_',
          argsIgnorePattern: '^_',
        },
      ],
      '@typescript-eslint/no-explicit-any': 'warn',
      // Lazy/conditional requires of heavy native deps (ffmpeg-static,
      // playwright-core) stay synchronous `require()` by design.
      '@typescript-eslint/no-require-imports': 'off',
      '@typescript-eslint/ban-ts-comment': 'warn',
      '@typescript-eslint/no-empty-object-type': 'off',
    },
  },

  // Electron main process + injected webview scripts (Node + browser globals).
  // Legacy .js is CommonJS; converted .ts is ESM (transpiled to CJS by esbuild).
  {
    files: ['electron/**/*.{js,ts}'],
    languageOptions: {
      ecmaVersion: 2023,
      globals: { ...globals.node, ...globals.browser },
    },
  },
  {
    files: ['electron/**/*.js'],
    languageOptions: { sourceType: 'commonjs' },
  },

  // React renderer
  {
    files: ['src/**/*.{js,jsx,ts,tsx}'],
    plugins: { react, 'react-hooks': reactHooks },
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      parserOptions: { ecmaFeatures: { jsx: true } },
      globals: { ...globals.browser },
    },
    settings: { react: { version: 'detect' } },
    rules: {
      ...react.configs.recommended.rules,
      ...reactHooks.configs.recommended.rules,
      'react/prop-types': 'off',
      'react/react-in-jsx-scope': 'off',
      // The <webview> custom element carries attributes the react plugin can't model.
      'react/no-unknown-property': 'off',
      // Cosmetic-only; Prettier/markup review handles copy, not the linter.
      'react/no-unescaped-entities': 'off',
      'react-hooks/exhaustive-deps': 'warn',
    },
  },

  // CommonJS tooling scripts
  {
    files: ['scripts/**/*.cjs', '__mocks__/**/*.{js,cjs}', 'build/**/*.cjs'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'commonjs',
      globals: { ...globals.node },
    },
  },

  // ESM/TS tooling + root config files
  {
    files: [
      'scripts/**/*.{mjs,ts}',
      'build/**/*.{mjs,ts}',
      '__mocks__/**/*.ts',
      '*.config.{js,ts}',
      'eslint.config.{js,ts}',
    ],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: { ...globals.node },
    },
  },

  // Cloudflare Worker (feedback relay) — ESM, runtime Workers (fetch/Response…)
  {
    files: ['workers/**/*.{js,ts}'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: { ...globals.worker, ...globals.node },
    },
  },

  // AudioWorklet processor — runs in AudioWorkletGlobalScope
  {
    files: ['src/lib/dictation/pcm-worklet.{js,ts}'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: {
        AudioWorkletProcessor: 'readonly',
        registerProcessor: 'readonly',
        sampleRate: 'readonly',
        currentTime: 'readonly',
        currentFrame: 'readonly',
      },
    },
  },

  // Vitest unit tests
  {
    files: ['tests/**/*.{js,jsx,ts,tsx}'],
    plugins: { react, 'react-hooks': reactHooks },
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      parserOptions: { ecmaFeatures: { jsx: true } },
      globals: { ...globals.browser, ...globals.node, ...globals.vitest },
    },
    settings: { react: { version: 'detect' } },
    rules: {
      ...react.configs.recommended.rules,
      'react/prop-types': 'off',
      'react/react-in-jsx-scope': 'off',
    },
  },

  // Prettier last: disable all formatting rules
  prettier,
);
