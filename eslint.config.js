import js from '@eslint/js';
import globals from 'globals';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import prettier from 'eslint-config-prettier';

/**
 * Flat ESLint config for Shelfy.
 *
 * The codebase spans several runtimes, so rules are scoped per area:
 *  - electron/**        main process + webview scripts (CommonJS, Node + browser)
 *  - src/**             React renderer (ESM, browser)
 *  - scripts/*.cjs|mjs  build/eval tooling (Node)
 *  - tests/**           Vitest (jsdom)
 *  - e2e/**             Playwright (handled by tsc for types)
 *
 * Prettier owns formatting; this config focuses on correctness (real bugs,
 * React hook rules, undefined references), so `npm run lint` stays meaningful
 * without drowning in style noise.
 */
export default [
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

  // Electron main process + injected webview scripts (CommonJS, Node + browser)
  {
    files: ['electron/**/*.js'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'commonjs',
      globals: { ...globals.node, ...globals.browser },
    },
  },

  // React renderer
  {
    files: ['src/**/*.{js,jsx}'],
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

  // ESM tooling + root config files
  {
    files: ['scripts/**/*.mjs', '*.config.js', 'eslint.config.js'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: { ...globals.node },
    },
  },

  // Cloudflare Worker (feedback relay) — ESM, runtime Workers (fetch/Response…)
  {
    files: ['workers/**/*.js'],
    languageOptions: {
      ecmaVersion: 2023,
      sourceType: 'module',
      globals: { ...globals.worker, ...globals.node },
    },
  },

  // AudioWorklet processor — runs in AudioWorkletGlobalScope
  {
    files: ['src/lib/dictation/pcm-worklet.js'],
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
    files: ['tests/**/*.{js,jsx}'],
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
];
