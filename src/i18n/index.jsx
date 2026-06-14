import React, { createContext, useContext, useState, useEffect, useCallback, useMemo } from 'react';

// ── i18n ──────────────────────────────────────────────────────────────────────
// Lightweight in-house internationalization (no runtime dependency). It governs
// ONLY the app UI — content the user imported and the AI prompts/analysis stay in
// their own languages (the prompts live in electron/ and are intentionally
// English). The chosen language is persisted in localStorage, mirroring the
// app's existing settings pattern (see disclaimer.js / useDownloadPrefs).

export const LANGUAGES = [
  { code: 'it', label: 'Italiano' },
  { code: 'en', label: 'English' },
];
const SUPPORTED = ['it', 'en'];
const DEFAULT_LANG = 'it';
const STORAGE_KEY = 'app:language';

// Load every namespace file under messages/ and merge them into a single
// { it: {...}, en: {...} } table, with each key namespaced by its filename
// ("settings.title", "sidebar.sources", …). Vite resolves the glob at build time
// (and under vitest, which is Vite-powered), so dropping a new file in messages/
// wires it up automatically — there is no central registry to keep in sync, which
// also lets many files be authored in parallel without edit conflicts.
const modules = import.meta.glob('./messages/*.js', { eager: true });
const messages = { it: {}, en: {} };
for (const path of Object.keys(modules)) {
  const ns = path.slice(path.lastIndexOf('/') + 1, -3); // './messages/settings.js' → 'settings'
  const mod = modules[path]?.default || modules[path] || {};
  for (const lang of SUPPORTED) {
    const dict = mod[lang] || {};
    for (const key of Object.keys(dict)) messages[lang][`${ns}.${key}`] = dict[key];
  }
}

export { messages };

// Resolve the language to start in: a previously-saved choice wins; otherwise we
// honour the host language (Italian / English) and fall back to Italian — the
// language the app originally shipped in.
export function getInitialLang() {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (SUPPORTED.includes(v)) return v;
  } catch {
    /* storage unavailable */
  }
  try {
    const n = (navigator.language || navigator.userLanguage || '').toLowerCase();
    if (n.startsWith('it')) return 'it';
    if (n.startsWith('en')) return 'en';
  } catch {
    /* no navigator (non-browser env) */
  }
  return DEFAULT_LANG;
}

// BCP-47 tag for Intl/toLocale* date & number formatting in the active language.
export function localeTag(lang) {
  return lang === 'en' ? 'en-US' : 'it-IT';
}

function interpolate(str, vars) {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (m, k) => (vars[k] != null ? String(vars[k]) : m));
}

// Pure translation usable outside React (e.g. the buildActivities selector, which
// is unit-tested as a plain function). Resolves a fully-qualified key for `lang`,
// falling back to the other languages, then the key itself. Supports {var}
// interpolation and a simple { one, other } plural shape chosen by vars.count.
export function translate(lang, key, vars) {
  const dict = messages[lang] || messages[DEFAULT_LANG];
  let val = dict[key];
  if (val == null) val = messages.en[key];
  if (val == null) val = messages.it[key];
  if (val == null) return key;
  if (val && typeof val === 'object') {
    const n = vars?.count;
    val = (n === 1 ? val.one : val.other) ?? val.other ?? val.one ?? key;
  }
  return typeof val === 'string' ? interpolate(val, vars) : key;
}

const I18nContext = createContext(null);

export function I18nProvider({ children }) {
  const [lang, setLangState] = useState(getInitialLang);

  const setLang = useCallback((next) => {
    if (!SUPPORTED.includes(next)) return;
    setLangState(next);
    try {
      localStorage.setItem(STORAGE_KEY, next);
    } catch {
      /* storage unavailable — choice stays in-memory for this session */
    }
  }, []);

  // Keep <html lang> in sync for accessibility / correct hyphenation.
  useEffect(() => {
    try {
      document.documentElement.lang = lang;
    } catch {
      /* no document */
    }
  }, [lang]);

  const value = useMemo(() => ({ lang, setLang }), [lang, setLang]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

// Works even without a provider: component tests render widgets in isolation, so
// we fall back to the persisted/detected language with a no-op setter rather than
// throwing. The real app always mounts <I18nProvider> at the root (main.jsx).
export function useLang() {
  const ctx = useContext(I18nContext);
  if (ctx) return ctx;
  return { lang: getInitialLang(), setLang: () => {} };
}

// useT('settings') → t('title') resolves 'settings.title'.
// useT()          → t('settings.title') resolves the fully-qualified key.
export function useT(namespace) {
  const { lang } = useLang();
  return useCallback(
    (key, vars) => translate(lang, namespace ? `${namespace}.${key}` : key, vars),
    [lang, namespace],
  );
}
