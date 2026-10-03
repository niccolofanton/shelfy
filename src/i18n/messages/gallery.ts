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
    // View mode toggle (grid ↔ infinite canvas)
    viewGrid: 'Griglia',
    viewCanvas: 'Canvas',
    viewGridTitle: 'Passa alla vista a griglia (ordinata per data)',
    viewCanvasTitle: 'Passa al canvas infinito (esplora liberamente)',
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
    // Density control (filter sheet's View section, narrow)
    columns: 'Colonne',
    // One-time selection hints (GAL-12)
    tipRange: 'Shift-clic per selezionare un intervallo',
    tipLongPress: 'Suggerimento: tieni premuto un post per selezionarlo',
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
    // Assign to a folder
    addToSource: 'Aggiungi a cartella',
    noSources: 'Nessuna cartella. Creane una qui sotto.',
    createNewSource: 'Nuova cartella',
    // Remove from the current folder (only while viewing one, §1.2 #12)
    removeFromSource: 'Rimuovi da questa cartella',
    // Cleanup + destructive (two-step)
    clearDescriptions: 'Cancella descrizioni AI',
    clearDescriptionsConfirm: 'Conferma: cancella descrizioni ({n})',
    clearTags: 'Rimuovi tag AI',
    clearTagsConfirm: 'Conferma: rimuovi tag AI ({n})',
    deletePosts: 'Elimina post selezionati',
    deletePostsConfirm: 'Conferma: elimina {n} post',
    deleteHint: 'Rimuove i post dalla libreria e i file scaricati. Azione irreversibile.',
    // P1-14: the web's delete moves posts to the trash (Cestino), undoable.
    deleteHintTrash: 'Sposta i post nel Cestino. Puoi annullare o ripristinarli da lì.',
    // Inline feedback (toasts)
    fbAssignError: 'Errore aggiunta alla cartella',
    fbRemovedFromSource: '{n} rimossi dalla cartella',
    fbRemoveFromSourceError: 'Errore nella rimozione dalla cartella',
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
    // P1-11/P1-14: a selection over 500 posts runs as a background job.
    fbDeleteQueued: '{n} post in eliminazione',
    fbSelectionDone: '{n} selezionati',
    fbSelectionError: 'Errore selezione',
    fbPostDeleted: 'Post eliminato',
    // Undo (P1-14: a delete's restore handle, while it is still offered)
    undo: 'Annulla',
    fbUndone: 'Eliminazione annullata',
    fbUndoError: 'Errore nell’annullare l’eliminazione',
    // Background-job progress banner (P1-11/P1-14, minimal — see Gallery's
    // pendingJob doc comment)
    jobRunning: 'Elaborazione in corso…',
    jobProgress: 'Elaborazione in corso… {pct}%',
    // Empty states — one per situation (GAL-2)
    emptySearchTitle: 'Nessun post per “{q}”',
    emptySearchClear: 'Cancella ricerca',
    emptyFiltersTitle: 'Nessun post con questi filtri',
    emptyFiltersReset: 'Reimposta i filtri',
    emptyFolderTitle: 'Questa cartella è vuota',
    emptyFolderBody: 'Seleziona dei post nella libreria, poi scegli Aggiungi a cartella.',
    // Empty library — web (extension/share) vs desktop (import/capture, kept)
    emptyLibraryTitle: 'La libreria è vuota',
    emptyLibraryWebBody:
      'Salva post da Instagram, X e Pinterest con l’estensione del browser o il foglio di condivisione.',
    emptyLibrarySetup: 'Configura l’estensione',
    emptyHint: 'Importa un file JSON o cattura post dalla scheda Browser.',
  },
  en: {
    viewGrid: 'Grid',
    viewCanvas: 'Canvas',
    viewGridTitle: 'Switch to grid view (sorted by date)',
    viewCanvasTitle: 'Switch to the infinite canvas (free roam)',
    sortNewest: 'Newest',
    sortOldest: 'Oldest',
    sortToggleTitle: 'Change sort by date',
    refreshTitle: 'Refresh the gallery',
    syncSourceTitle: 'Sync this source from its connector',
    syncStopTitle: 'Stop syncing',
    select: 'Select',
    selectTitle: 'Select multiple posts for bulk actions',
    suggestedFilters: 'Suggested filters',
    columns: 'Columns',
    tipRange: 'Shift-click to select a range',
    tipLongPress: 'Tip: long-press a post to select it',
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
    addToSource: 'Add to folder',
    noSources: 'No folders yet. Create one below.',
    createNewSource: 'New folder',
    // Remove from the current folder (only while viewing one, §1.2 #12)
    removeFromSource: 'Remove from this folder',
    clearDescriptions: 'Clear AI descriptions',
    clearDescriptionsConfirm: 'Confirm: clear descriptions ({n})',
    clearTags: 'Remove AI tags',
    clearTagsConfirm: 'Confirm: remove AI tags ({n})',
    deletePosts: 'Delete selected posts',
    deletePostsConfirm: 'Confirm: delete {n} posts',
    deleteHint:
      'Removes the posts from the library and the downloaded files. This action is irreversible.',
    // P1-14: the web's delete moves posts to the trash, undoable.
    deleteHintTrash: 'Moves the posts to the Trash. You can undo, or restore them from there.',
    fbAssignError: 'Error adding to folder',
    fbRemovedFromSource: '{n} removed from the folder',
    fbRemoveFromSourceError: 'Error removing from the folder',
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
    // P1-11/P1-14: a selection over 500 posts runs as a background job.
    fbDeleteQueued: '{n} posts being deleted',
    fbSelectionDone: '{n} selected',
    fbSelectionError: 'Selection error',
    fbPostDeleted: 'Post deleted',
    // Undo (P1-14: a delete's restore handle, while it is still offered)
    undo: 'Undo',
    fbUndone: 'Delete undone',
    fbUndoError: 'Error undoing the delete',
    // Background-job progress banner (P1-11/P1-14, minimal — see Gallery's
    // pendingJob doc comment)
    jobRunning: 'Processing…',
    jobProgress: 'Processing… {pct}%',
    emptySearchTitle: 'No posts match “{q}”',
    emptySearchClear: 'Clear search',
    emptyFiltersTitle: 'No posts match these filters',
    emptyFiltersReset: 'Reset filters',
    emptyFolderTitle: 'This folder is empty',
    emptyFolderBody: 'Select posts in the library, then choose Add to folder.',
    emptyLibraryTitle: 'Your library is empty',
    emptyLibraryWebBody:
      'Save posts from Instagram, X and Pinterest with the browser extension or the Share sheet.',
    emptyLibrarySetup: 'Set up the extension',
    emptyHint: 'Import a JSON file or capture posts via the Browser tab.',
  },
} satisfies LangMessages;
