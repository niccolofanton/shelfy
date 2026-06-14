// UI strings for the left navigation sidebar (groups, sub-tabs, action rows and
// tooltips). Group names: "Connessioni/Connections" (in-app social browsers that
// import content + add actions) vs "Libreria/Library" (downloads, posts, folders);
// action rows are verb-first ("Aggiungi sito" / "Add website"). Brand/product
// names (Instagram, X / Twitter, Pinterest, SHELFY) are not translated.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    sources: 'Connessioni',
    bookmarks: 'Libreria',
    ai: 'AI',
    downloads: 'Downloads',
    website: 'Aggiungi sito',
    manualBookmark: 'Aggiungi bookmark',
    newFolder: 'Nuova cartella',
    allPosts: 'Tutti i post',
    settings: 'Impostazioni',
    feedback: 'Feedback',
    postsCount: '{n} post',
    // Platform labels (only the non-brand "web" entry is translated)
    web: 'Siti web',
    // AI sub-tabs
    aiqueue: 'Auto-tag',
    aiweb: 'Analisi siti',
    aisearch: 'Chat',
    aitags: 'Esplora tag',
    // Tooltips
    editSource: 'Modifica source',
    collapse: 'Comprimi',
    expand: 'Espandi',
    addSite: 'Aggiungi un sito web come reference',
    addBookmark: 'Aggiungi un bookmark da file locali (immagini, video, PDF)',
    addCollection: 'Crea una nuova cartella per organizzare i bookmark',
  },
  en: {
    sources: 'Connections',
    bookmarks: 'Library',
    ai: 'AI',
    downloads: 'Downloads',
    website: 'Add website',
    manualBookmark: 'Add bookmark',
    newFolder: 'New folder',
    allPosts: 'All posts',
    settings: 'Settings',
    feedback: 'Feedback',
    postsCount: '{n} posts',
    web: 'Websites',
    aiqueue: 'Auto-tag',
    aiweb: 'Website Analyzer',
    aisearch: 'Chat',
    aitags: 'Tags Explorer',
    editSource: 'Edit source',
    collapse: 'Collapse',
    expand: 'Expand',
    addSite: 'Add a website as a reference',
    addBookmark: 'Add a bookmark from local files (images, videos, PDF)',
    addCollection: 'Create a new folder to organize bookmarks',
  },
} satisfies LangMessages;
