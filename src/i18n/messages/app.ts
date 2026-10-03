// Top-level App shell strings: the dev-only "build refreshed" bar shown in
// development (the timestamp is interpolated), the offline pill (UX-1, ST-3)
// and the suffix of each page's document title (SH-13).
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    devUpdated: 'DEV — ultimo aggiornamento: {time}',
    offline: 'Sei offline',
    pageTitle: '{page} · Shelfy',
  },
  en: {
    devUpdated: 'DEV — last update: {time}',
    offline: 'You’re offline',
    pageTitle: '{page} · Shelfy',
  },
} satisfies LangMessages;
