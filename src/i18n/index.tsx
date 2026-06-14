import React, { createContext, useContext, useState, useEffect, useCallback, useMemo } from 'react';

// ── i18n ──────────────────────────────────────────────────────────────────────
// Lightweight in-house internationalization (no runtime dependency). It governs
// ONLY the app UI — content the user imported and the AI prompts/analysis stay in
// their own languages (the prompts live in electron/ and are intentionally
// English). The chosen language is persisted in localStorage, mirroring the
// app's existing settings pattern (see disclaimer.js / useDownloadPrefs).

// A supported UI language code.
export type Lang = 'it' | 'en';

// Variables passed to a translation: {var} interpolation values and the optional
// `count` that selects a { one, other } plural shape.
export type TranslateVars = Record<string, unknown>;

// The signature of a bound translator returned by useT().
export type Translate = (key: string, vars?: TranslateVars) => string;

// A single translation value: a plain string or a { one, other } plural shape.
type Message = string | { one?: string; other?: string };

// One language's flat, namespaced dictionary ("settings.title" → value).
type Dict = Record<string, Message>;

// The raw shape of a messages/*.js namespace module.
interface MessageModule {
  it?: Record<string, Message>;
  en?: Record<string, Message>;
}

interface LanguageOption {
  code: Lang;
  label: string;
}

export const LANGUAGES: LanguageOption[] = [
  { code: 'it', label: 'Italiano' },
  { code: 'en', label: 'English' },
];
const SUPPORTED: readonly Lang[] = ['it', 'en'];
const DEFAULT_LANG: Lang = 'it';
const STORAGE_KEY = 'app:language';

function isSupported(v: unknown): v is Lang {
  return typeof v === 'string' && (SUPPORTED as readonly string[]).includes(v);
}

// Load every namespace file under messages/ and merge them into a single
// { it: {...}, en: {...} } table, with each key namespaced by its filename
// ("settings.title", "sidebar.sources", …). Vite resolves the glob at build time
// (and under vitest, which is Vite-powered), so dropping a new file in messages/
// wires it up automatically — there is no central registry to keep in sync, which
// also lets many files be authored in parallel without edit conflicts.
const modules = import.meta.glob<MessageModule | { default?: MessageModule }>('./messages/*.js', {
  eager: true,
});
const messages: Record<Lang, Dict> = { it: {}, en: {} };
for (const path of Object.keys(modules)) {
  const ns = path.slice(path.lastIndexOf('/') + 1, -3); // './messages/settings.js' → 'settings'
  const raw = modules[path] as MessageModule | { default?: MessageModule };
  const mod: MessageModule = ('default' in raw && raw.default ? raw.default : raw) as MessageModule;
  for (const lang of SUPPORTED) {
    const dict = mod[lang] || {};
    for (const key of Object.keys(dict)) messages[lang][`${ns}.${key}`] = dict[key];
  }
}

export { messages };

// Resolve the language to start in: a previously-saved choice wins; otherwise we
// honour the host language (Italian / English) and fall back to Italian — the
// language the app originally shipped in.
export function getInitialLang(): Lang {
  try {
    const v = localStorage.getItem(STORAGE_KEY);
    if (isSupported(v)) return v;
  } catch {
    /* storage unavailable */
  }
  try {
    const nav = navigator as Navigator & { userLanguage?: string };
    const n = (navigator.language || nav.userLanguage || '').toLowerCase();
    if (n.startsWith('it')) return 'it';
    if (n.startsWith('en')) return 'en';
  } catch {
    /* no navigator (non-browser env) */
  }
  return DEFAULT_LANG;
}

// BCP-47 tag for Intl/toLocale* date & number formatting in the active language.
export function localeTag(lang: string): string {
  return lang === 'en' ? 'en-US' : 'it-IT';
}

function interpolate(str: string, vars?: TranslateVars): string {
  if (!vars) return str;
  return str.replace(/\{(\w+)\}/g, (m, k: string) => (vars[k] != null ? String(vars[k]) : m));
}

// Pure translation usable outside React (e.g. the buildActivities selector, which
// is unit-tested as a plain function). Resolves a fully-qualified key for `lang`,
// falling back to the other languages, then the key itself. Supports {var}
// interpolation and a simple { one, other } plural shape chosen by vars.count.
export function translate(lang: string, key: string, vars?: TranslateVars): string {
  const dict = messages[lang as Lang] || messages[DEFAULT_LANG];
  let val: Message | undefined = dict[key];
  if (val == null) val = messages.en[key];
  if (val == null) val = messages.it[key];
  if (val == null) return key;
  if (val && typeof val === 'object') {
    const n = vars?.count;
    val = (n === 1 ? val.one : val.other) ?? val.other ?? val.one ?? key;
  }
  return typeof val === 'string' ? interpolate(val, vars) : key;
}

interface I18nContextValue {
  lang: string;
  setLang: (next: string) => void;
}

const I18nContext = createContext<I18nContextValue | null>(null);

export function I18nProvider({ children }: { children: React.ReactNode }): React.ReactElement {
  const [lang, setLangState] = useState<string>(getInitialLang);

  const setLang = useCallback((next: string): void => {
    if (!isSupported(next)) return;
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

  const value = useMemo<I18nContextValue>(() => ({ lang, setLang }), [lang, setLang]);
  return <I18nContext.Provider value={value}>{children}</I18nContext.Provider>;
}

// Works even without a provider: component tests render widgets in isolation, so
// we fall back to the persisted/detected language with a no-op setter rather than
// throwing. The real app always mounts <I18nProvider> at the root (main.jsx).
export function useLang(): I18nContextValue {
  const ctx = useContext(I18nContext);
  if (ctx) return ctx;
  return { lang: getInitialLang(), setLang: () => {} };
}

// useT('settings') → t('title') resolves 'settings.title'.
// useT()          → t('settings.title') resolves the fully-qualified key.
export function useT(namespace?: string): Translate {
  const { lang } = useLang();
  return useCallback<Translate>(
    (key, vars) => translate(lang, namespace ? `${namespace}.${key}` : key, vars),
    [lang, namespace],
  );
}
