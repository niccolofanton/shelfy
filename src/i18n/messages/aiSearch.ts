// UI strings for the AI Search view (chat-driven archive search). Covers the
// chat panel (header, model-status banners, empty state, seed prompts, composer
// placeholders/buttons, dictation hints), the proposed tag/keyword groups, the
// results toolbar (mode/source toggles, result counts, bulk actions) and the
// gallery empty/loading states, plus the toast feedback. The Italian column
// reproduces the app's existing copy verbatim. Brand/product names and the AI
// system prompts (which live in electron/) are NOT translated; shared button
// labels come from the `common` namespace.
//
// NOTE: the SEED_PROMPTS shown as suggestion chips are example queries the user
// can send to the model. They are localized here as UI suggestions (not LLM
// system prompts), so an Italian/English user gets idiomatic examples.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // Chat panel header
    chatTitle: 'Chat',
    newConversation: 'Nuova conversazione',
    newConversationShort: 'Nuova',
    // AI model status banner
    modelDownloading: 'Download del modello in corso…',
    modelUnavailable: 'Modello AI non disponibile.',
    modelTextFallback: 'Puoi comunque scrivere: la ricerca sarà testuale.',
    downloadModel: 'Scarica modello',
    // Voice model status banner
    sttDownloading: 'Scarico il modello vocale ({pct}%)…',
    sttUnavailable: 'Dettatura vocale non disponibile.',
    sttBinaryMissingPre: 'Binario ',
    sttBinaryMissingPost: ' non trovato in ',
    sttModelUnavailable: 'Modello vocale non disponibile.',
    sttDownloadHint: 'Scaricalo per dettare le ricerche a voce.',
    sttDownloadBtn: 'Scarica modello vocale (~488MB)',
    // Chat empty state
    emptyTitle: "Cerca nell'archivio",
    emptyHint:
      "Descrivi un soggetto, uno stile, una tecnica o un'atmosfera — filtrerò l'archivio e suggerirò i tag più pertinenti.",
    // Seed prompt suggestions
    seed1: 'shader GLSL raymarching',
    seed2: 'generative art TouchDesigner',
    seed3: 'particelle organiche',
    seed4: 'tipografia cinetica',
    seed5: 'mood board minimalista',
    seed6: 'identità visiva brand',
    // Proposed tag/keyword groups
    broadTags: 'Tag generali',
    specificTags: 'Tag specifici',
    keywords: 'Parole chiave',
    appliedToFilters: 'Applicato ai filtri',
    applyToFilters: 'Applica ai filtri',
    applyToFiltersTitle: 'Sostituisci i filtri con i tag di questo messaggio',
    // Streaming placeholder
    searching: 'Sto cercando…',
    // Composer
    placeholderTranscribing: 'Trascrivo…',
    placeholderListening: 'In ascolto…',
    placeholderDefault: 'Cerca per soggetto, stile, tecnica…',
    stop: 'Stop',
    stopTitle: 'Interrompi',
    send: 'Invia',
    sendTitle: 'Invia (Invio)',
    newlineHint: 'Shift+↵ per andare a capo',
    // Mic button
    micStop: 'Ferma la dettatura',
    micStart: 'Dettatura vocale',
    micModelUnavailable: 'Modello vocale non disponibile',
    // Results toolbar — source scope
    sourceAll: 'Tutto',
    sourceWeb: 'Siti',
    sourceSocial: 'Social',
    clear: 'Pulisci',
    // Result counts
    noActiveFilter: 'Nessun filtro attivo',
    firstNofTotal: 'primi {n} di {total} risultati',
    nResults: '{total} risultati',
    nOfTotalShown: 'su {n} di {total} mostrati',
    // Result bulk actions
    createCollection: 'Crea collection da questi',
    copyLinks: 'Copia link',
    exportMarkdown: 'Esporta Markdown',
    // Gallery empty / loading states
    galleryEmpty: 'Descrivi cosa cerchi nella chat a sinistra: i post suggeriti compariranno qui.',
    noResults: 'Nessun post per questi filtri.',
    // Toast feedback
    noLinksToCopy: 'Nessun link da copiare',
    linksCopied: 'Copiati {n} link',
    copyFailed: 'Impossibile copiare negli appunti',
    nothingToExport: 'Nessun risultato da esportare',
    exported: 'Esportati {n} post in Markdown',
    nothingToCollect: 'Nessun post da raccogliere',
    collectionError: 'Errore creazione collection',
    collectionCreatedWithPosts: 'Collection "{name}" creata con {n} post',
    collectionCreated: 'Collection "{name}" creata',
    // Chat error (from useAiSearch)
    chatError: 'Si è verificato un errore durante la ricerca. Riprova.',
  },
  en: {
    chatTitle: 'Chat',
    newConversation: 'New conversation',
    newConversationShort: 'New',
    modelDownloading: 'Downloading the model…',
    modelUnavailable: 'AI model unavailable.',
    modelTextFallback: 'You can still type: the search will be text-based.',
    downloadModel: 'Download model',
    sttDownloading: 'Downloading the voice model ({pct}%)…',
    sttUnavailable: 'Voice dictation unavailable.',
    sttBinaryMissingPre: 'Binary ',
    sttBinaryMissingPost: ' not found in ',
    sttModelUnavailable: 'Voice model unavailable.',
    sttDownloadHint: 'Download it to dictate your searches.',
    sttDownloadBtn: 'Download voice model (~488MB)',
    emptyTitle: 'Search the archive',
    emptyHint:
      "Describe a subject, a style, a technique or a mood — I'll filter the archive and suggest the most relevant tags.",
    seed1: 'shader GLSL raymarching',
    seed2: 'generative art TouchDesigner',
    seed3: 'organic particles',
    seed4: 'kinetic typography',
    seed5: 'minimalist mood board',
    seed6: 'brand visual identity',
    broadTags: 'General tags',
    specificTags: 'Specific tags',
    keywords: 'Keywords',
    appliedToFilters: 'Applied to filters',
    applyToFilters: 'Apply to filters',
    applyToFiltersTitle: "Replace the filters with this message's tags",
    searching: 'Searching…',
    placeholderTranscribing: 'Transcribing…',
    placeholderListening: 'Listening…',
    placeholderDefault: 'Search by subject, style, technique…',
    stop: 'Stop',
    stopTitle: 'Stop',
    send: 'Send',
    sendTitle: 'Send (Enter)',
    newlineHint: 'Shift+↵ for a new line',
    micStop: 'Stop dictation',
    micStart: 'Voice dictation',
    micModelUnavailable: 'Voice model unavailable',
    sourceAll: 'All',
    sourceWeb: 'Sites',
    sourceSocial: 'Social',
    clear: 'Clear',
    noActiveFilter: 'No active filter',
    firstNofTotal: 'first {n} of {total} results',
    nResults: '{total} results',
    nOfTotalShown: '{n} of {total} shown',
    createCollection: 'Create collection from these',
    copyLinks: 'Copy links',
    exportMarkdown: 'Export Markdown',
    galleryEmpty:
      'Describe what you are looking for in the chat on the left: the suggested posts will appear here.',
    noResults: 'No posts for these filters.',
    noLinksToCopy: 'No link to copy',
    linksCopied: 'Copied {n} links',
    copyFailed: 'Could not copy to the clipboard',
    nothingToExport: 'No result to export',
    exported: 'Exported {n} posts to Markdown',
    nothingToCollect: 'No post to collect',
    collectionError: 'Error creating collection',
    collectionCreatedWithPosts: 'Collection "{name}" created with {n} posts',
    collectionCreated: 'Collection "{name}" created',
    chatError: 'An error occurred during the search. Try again.',
  },
} satisfies LangMessages;
