// Generic, surface-agnostic error strings used by data hooks whose failures are
// shown to the user (gallery load, etc.). Feature-specific errors live in their
// own namespace.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    loadPosts: 'Caricamento dei post non riuscito.',
  },
  en: {
    loadPosts: 'Could not load posts.',
  },
} satisfies LangMessages;
