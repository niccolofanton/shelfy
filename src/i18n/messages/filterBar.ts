// UI strings for the always-visible Gallery header strip: the search input,
// the active AI-tag chip, the total post count and the "Filtri" drawer toggle.
// The Italian column reproduces the app's existing copy verbatim (the test
// suite asserts some of it). The post unit comes from the `common` namespace.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    searchPlaceholder: 'Search posts...',
    searchAria: 'Search posts',
    clearSearch: 'Cancella ricerca',
    removeTagFilter: 'Rimuovi filtro tag',
    postsCount: '{n} post',
    filters: 'Filtri',
    filtersTitle: 'Filtra per tipo di media, download e tag AI',
  },
  en: {
    searchPlaceholder: 'Search posts...',
    searchAria: 'Search posts',
    clearSearch: 'Clear search',
    removeTagFilter: 'Remove tag filter',
    postsCount: '{n} posts',
    filters: 'Filters',
    filtersTitle: 'Filter by media type, downloads and AI tags',
  },
} satisfies LangMessages;
