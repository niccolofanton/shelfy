// String lookup for the extension's pages, over the app's `extension` message namespace
// (src/i18n/messages/extension.ts). The same rules as the app's translate(): the language's
// value, else English, else Italian, else the key; {var} interpolation; { one, other } plurals
// chosen by vars.count. No React and no import.meta.glob: the bundle carries this one file.

import messages from '../../../src/i18n/messages/extension';

export type Lang = 'en' | 'it';
export type Vars = Record<string, string | number>;
export type Translate = (key: string, vars?: Vars) => string;

type Message = string | { one: string; other: string };
type Dict = Record<string, Message>;

const DICTS: Record<Lang, Dict> = { en: messages.en, it: messages.it };

/** Italian for an Italian browser, English otherwise. */
export function pickLang(languages: readonly string[] | undefined): Lang {
  const first = (languages?.[0] ?? '').toLowerCase();
  return first.startsWith('it') ? 'it' : 'en';
}

function interpolate(text: string, vars?: Vars): string {
  if (!vars) return text;
  return text.replace(/\{(\w+)\}/g, (match, name: string) =>
    vars[name] !== undefined ? String(vars[name]) : match,
  );
}

export function translate(lang: Lang, key: string, vars?: Vars): string {
  const value = DICTS[lang][key] ?? DICTS.en[key] ?? DICTS.it[key];
  if (value === undefined) return key;
  const text = typeof value === 'string' ? value : vars?.count === 1 ? value.one : value.other;
  return interpolate(text, vars);
}

export function createTranslate(lang: Lang): Translate {
  return (key, vars) => translate(lang, key, vars);
}

/** True when `key` exists in both languages (tests and the panel's fallbacks use it). */
export function hasMessage(key: string): boolean {
  return key in DICTS.en && key in DICTS.it;
}
