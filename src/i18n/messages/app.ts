// Top-level App shell strings. Currently just the dev-only "build refreshed"
// bar shown in development. The Italian column reproduces the existing copy
// verbatim (the timestamp is interpolated).
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    devUpdated: 'DEV — ultimo aggiornamento: {time}',
  },
  en: {
    devUpdated: 'DEV — last update: {time}',
  },
} satisfies LangMessages;
