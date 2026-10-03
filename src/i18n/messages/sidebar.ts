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
    webSyncOpenPlatform: 'Apri {platform}',
    webSyncNow: 'Sincronizza ora',
    webSyncStop: 'Interrompi',
    webSyncOpen: 'Apri pagina salvati',
    webSyncNever: 'Mai sincronizzato',
    webSyncState_running: 'Sincronizzazione…',
    webSyncState_done: 'Ultima sincronizzazione completata',
    webSyncState_failed: 'Ultima sincronizzazione non riuscita',
    webSyncState_stopped: 'Sincronizzazione interrotta',
    webSyncError: 'Azione di sincronizzazione non riuscita ({code}).',
    sources: 'Connessioni',
    bookmarks: 'Libreria',
    ai: 'AI',
    downloads: 'Downloads',
    trash: 'Cestino',
    jobs: 'Attività',
    website: 'Aggiungi sito',
    manualBookmark: 'Aggiungi file',
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
    editSource: 'Modifica cartella',
    collapse: 'Comprimi',
    expand: 'Espandi',
    addSite: 'Aggiungi un sito web come reference',
    addBookmark: 'Aggiungi un file locale (immagini, video, PDF)',
    addCollection: 'Crea una nuova cartella per organizzare i post',
    // Accessible names (UX-1, SH-5/SH-6): icon-only controls name their row.
    editFolderNamed: 'Modifica la cartella {name}',
    collapseNamed: 'Comprimi {name}',
    expandNamed: 'Espandi {name}',
    // Drawer (P1-02, under 900px; a dialog named "Menu" since UX-1)
    menu: 'Menu',
    openMenu: 'Apri il menu',
    closeMenu: 'Chiudi il menu',
  },
  en: {
    webSyncOpenPlatform: 'Open {platform}',
    webSyncNow: 'Sync now',
    webSyncStop: 'Stop',
    webSyncOpen: 'Open saved page',
    webSyncNever: 'Not synced yet',
    webSyncState_running: 'Syncing…',
    webSyncState_done: 'Last sync completed',
    webSyncState_failed: 'Last sync failed',
    webSyncState_stopped: 'Sync stopped',
    webSyncError: 'Sync action failed ({code}).',
    sources: 'Connections',
    bookmarks: 'Library',
    ai: 'AI',
    downloads: 'Downloads',
    trash: 'Trash',
    jobs: 'Jobs',
    website: 'Add website',
    manualBookmark: 'Add file',
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
    editSource: 'Edit folder',
    collapse: 'Collapse',
    expand: 'Expand',
    addSite: 'Add a website as a reference',
    addBookmark: 'Add a local file (images, videos, PDF)',
    addCollection: 'Create a folder to organize your posts',
    editFolderNamed: 'Edit folder {name}',
    collapseNamed: 'Collapse {name}',
    expandNamed: 'Expand {name}',
    menu: 'Menu',
    openMenu: 'Open menu',
    closeMenu: 'Close menu',
  },
} satisfies LangMessages;
