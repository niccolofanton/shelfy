// UI strings for the right-hand filters drawer inside the Gallery page: the
// header, the reset/close controls, the Bookmarks source mirror and the media /
// download / AI-tag segmented filters. The Italian column reproduces the app's
// existing copy verbatim. Brand/product names (Instagram, X / Twitter,
// Pinterest) are not translated.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // Header
    title: 'Filtri',
    reset: 'Reimposta',
    closeTitle: 'Chiudi i filtri',
    // Sources section
    bookmarks: 'Bookmarks',
    allPosts: 'Tutti i post',
    web: 'Siti web',
    // Media-type filter
    mediaType: 'Tipo media',
    mediaAll: 'All',
    mediaVideo: 'Video',
    mediaImage: 'Image',
    mediaCarousel: 'Carousel',
    // Download-status filter
    downloadStatus: 'Stato download',
    downloadAll: 'All',
    downloadDownloaded: 'Scaricati',
    downloadLinkOnly: 'Solo link',
    // AI-tags filter
    aiTags: 'Tag AI',
    aiAll: 'All',
    aiTagged: 'Con tag AI',
    aiUntagged: 'Senza tag AI',
  },
  en: {
    title: 'Filters',
    reset: 'Reset',
    closeTitle: 'Close filters',
    bookmarks: 'Bookmarks',
    allPosts: 'All posts',
    web: 'Websites',
    mediaType: 'Media type',
    mediaAll: 'All',
    mediaVideo: 'Video',
    mediaImage: 'Image',
    mediaCarousel: 'Carousel',
    downloadStatus: 'Download status',
    downloadAll: 'All',
    downloadDownloaded: 'Downloaded',
    downloadLinkOnly: 'Link only',
    aiTags: 'AI tags',
    aiAll: 'All',
    aiTagged: 'With AI tags',
    aiUntagged: 'Without AI tags',
  },
} satisfies LangMessages;
