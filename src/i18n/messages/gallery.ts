// UI strings for the Gallery view: the date-sort toggle, the refresh/select
// controls, the select-all toggle next to the selection counter, the
// bulk-actions menu (analyze, download, assign to a
// source, the cleanup actions and the destructive delete), inline feedback
// toasts and the empty state. The total count shown in the unified toolbar
// comes from the `filterBar` namespace (postsCount). The Italian column
// reproduces the app's existing copy verbatim (the test suite asserts some of
// it). Brand/product names are not translated; shared button labels (Scarica,
// Elimina, …) come from the `common` namespace.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // Sort options (date)
    sortNewest: 'Più recenti',
    sortOldest: 'Meno recenti',
    sortToggleTitle: 'Cambia ordinamento per data',
    // Toolbar (browse) — refresh is icon-only, so only its tooltip is needed
    refreshTitle: 'Aggiorna la galleria',
    syncSourceTitle: 'Sincronizza questa source dal connettore',
    syncStopTitle: 'Interrompi la sincronizzazione',
    select: 'Seleziona',
    selectTitle: 'Seleziona più post per azioni in blocco',
    // Suggested filter tags
    suggestedFilters: 'Filtri suggeriti',
    // Toolbar (select mode)
    selectedCount: '{n} selezionati',
    actions: 'Azioni',
    actionsTitle: 'Azioni in blocco',
    exitSelectionTitle: 'Esci dalla selezione',
    // Bulk actions menu — selection
    deselectAll: 'Deseleziona tutti',
    selectAll: 'Seleziona tutti',
    selectAllN: 'Seleziona tutti i {n}',
    selectAllTitle: 'Seleziona tutti i post della vista corrente, filtri inclusi',
    deselectAllTitle: 'Svuota la selezione corrente',
    // Bulk actions menu — primary
    analyze: 'Analizza',
    downloadOnlyMissingHint: 'Scarica solo gli elementi non ancora scaricati',
    // Assign to source
    addToSource: 'Aggiungi a source',
    noSources: 'Nessuna source. Creane una qui sotto.',
    createNewSource: 'Crea nuova source',
    // Cleanup + destructive (two-step)
    clearDescriptions: 'Cancella descrizioni AI',
    clearDescriptionsConfirm: 'Conferma: cancella descrizioni ({n})',
    clearTags: 'Rimuovi tag AI',
    clearTagsConfirm: 'Conferma: rimuovi tag AI ({n})',
    deletePosts: 'Elimina post selezionati',
    deletePostsConfirm: 'Conferma: elimina {n} post',
    deleteHint: 'Rimuove i post dalla libreria e i file scaricati. Azione irreversibile.',
    // Inline feedback (toasts)
    fbAssignError: 'Errore aggiunta alla source',
    fbModelFirst: 'Scarica prima un modello (Impostazioni)',
    fbQueued: '{n} in coda',
    fbQueuedPartial: '{n} in coda · {skipped} da scaricare',
    fbAnalyzeNoneLocal: 'Nessun media locale: scarica prima i {n} selezionati',
    fbAnalyzeError: 'Errore avvio analisi',
    // Suggested-download banner (selection has remote-only media)
    analyzeNeedsDownload: '{n} media non scaricati: vanno scaricati prima di analizzarli',
    analyzeDownloadMissing: 'Scarica i {n} mancanti',
    analyzeSuggestDismiss: 'Ignora',
    fbNoFileTypes: 'Nessun tipo file attivo (Impostazioni)',
    fbDownloading: '{n} in download',
    fbDownloadError: 'Errore avvio download',
    fbDescriptionsCleared: '{n} descrizioni eliminate',
    fbClearDescriptionsError: 'Errore eliminazione descrizioni',
    fbTagsCleared: '{n} post senza tag AI',
    fbClearTagsError: 'Errore rimozione tag AI',
    fbPostsDeleted: '{n} post eliminati',
    fbFilesNotRemoved: '{n} file non rimossi',
    fbDeleteError: 'Errore eliminazione post',
    fbSelectionDone: '{n} selezionati',
    fbSelectionError: 'Errore selezione',
    fbPostDeleted: 'Post eliminato',
    // Empty state
    emptyTitle: 'Nessun post trovato.',
    emptyHint: 'Importa un file JSON o cattura post dalla scheda Browser.',
  },
  en: {
    sortNewest: 'Newest',
    sortOldest: 'Oldest',
    sortToggleTitle: 'Change sort by date',
    refreshTitle: 'Refresh the gallery',
    syncSourceTitle: 'Sync this source from its connector',
    syncStopTitle: 'Stop syncing',
    select: 'Select',
    selectTitle: 'Select multiple posts for bulk actions',
    suggestedFilters: 'Suggested filters',
    selectedCount: '{n} selected',
    actions: 'Actions',
    actionsTitle: 'Bulk actions',
    exitSelectionTitle: 'Exit selection',
    deselectAll: 'Deselect all',
    selectAll: 'Select all',
    selectAllN: 'Select all {n}',
    selectAllTitle: 'Select every post in the current view, filters included',
    deselectAllTitle: 'Clear the current selection',
    analyze: 'Analyze',
    downloadOnlyMissingHint: 'Only downloads items not yet downloaded',
    addToSource: 'Add to source',
    noSources: 'No source yet. Create one below.',
    createNewSource: 'Create new source',
    clearDescriptions: 'Clear AI descriptions',
    clearDescriptionsConfirm: 'Confirm: clear descriptions ({n})',
    clearTags: 'Remove AI tags',
    clearTagsConfirm: 'Confirm: remove AI tags ({n})',
    deletePosts: 'Delete selected posts',
    deletePostsConfirm: 'Confirm: delete {n} posts',
    deleteHint:
      'Removes the posts from the library and the downloaded files. This action is irreversible.',
    fbAssignError: 'Error adding to source',
    fbModelFirst: 'Download a model first (Settings)',
    fbQueued: '{n} queued',
    fbQueuedPartial: '{n} queued · {skipped} need download',
    fbAnalyzeNoneLocal: 'No local media: download the {n} selected first',
    fbAnalyzeError: 'Error starting analysis',
    // Suggested-download banner (selection has remote-only media)
    analyzeNeedsDownload: '{n} media not downloaded: download them before analyzing',
    analyzeDownloadMissing: 'Download the {n} missing',
    analyzeSuggestDismiss: 'Dismiss',
    fbNoFileTypes: 'No file type enabled (Settings)',
    fbDownloading: '{n} downloading',
    fbDownloadError: 'Error starting download',
    fbDescriptionsCleared: '{n} descriptions cleared',
    fbClearDescriptionsError: 'Error clearing descriptions',
    fbTagsCleared: '{n} posts without AI tags',
    fbClearTagsError: 'Error removing AI tags',
    fbPostsDeleted: '{n} posts deleted',
    fbFilesNotRemoved: '{n} files not removed',
    fbDeleteError: 'Error deleting posts',
    fbSelectionDone: '{n} selected',
    fbSelectionError: 'Selection error',
    fbPostDeleted: 'Post deleted',
    emptyTitle: 'No posts found.',
    emptyHint: 'Import a JSON file or capture posts via the Browser tab.',
  },
} satisfies LangMessages;
