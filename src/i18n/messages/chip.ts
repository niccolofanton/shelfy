// UI chrome for the reusable pill chip (Chip.jsx): only the remove-button tooltip
// is user-facing (the label text is supplied by the caller).
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    remove: 'Rimuovi',
  },
  en: {
    remove: 'Remove',
  },
} satisfies LangMessages;
