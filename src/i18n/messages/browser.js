// UI strings for the in-app social browser / import view (src/views/Browser.jsx):
// the Instagram/X/Pinterest webviews plus the toolbar, sync status bar, selection
// controls and error states. Brand names, URLs, webview-injected page strings and
// platform-API selectors are intentionally NOT translated here.
export default {
  it: {
    // Generic fallback name for a folder/board with no readable title.
    folderFallback: 'Cartella',

    // Errors surfaced by the folder-import flow.
    errNoSelection: 'Nessun post selezionato.',
    errStartImport: "Impossibile avviare l'importazione.",
    errStartSync: 'Impossibile avviare la sincronizzazione.',
    errSourceSyncActive: 'Sincronizzazione della sorgente in corso su questa piattaforma.',

    // Navigation buttons (titles / aria).
    back: 'Indietro',
    forward: 'Avanti',
    reload: 'Ricarica',

    // Capture / intercept status.
    intercepting: 'Intercettazione',

    // Auto-import button.
    stopSyncTitle: 'Interrompi la sincronizzazione',
    stopSourceSyncTitle: 'Interrompi la sincronizzazione della sorgente in corso',
    scriptsErrorCapture: 'Script di cattura non caricato — riprova',
    scriptsLoadingCapture: 'Caricamento script di cattura…',
    autoImportFolderTitle: 'Auto-importa i contenuti di questa raccolta',
    autoImportAllTitle: 'Auto-importa tutti i post salvati',
    stop: 'Interrompi',
    autoImport: 'Auto-import',

    // Selection mode.
    selectTitle: 'Seleziona i post da importare',
    scriptsErrorSelect: 'Script di selezione non caricato — riprova',
    scriptsLoadingSelect: 'Caricamento script di selezione…',
    select: 'Seleziona',
    selectedCount: 'selezionati',
    importSelectedTitle: 'Importa i post selezionati',
    importSelected: 'Importa selezionati',
    exitSelectionTitle: 'Esci dalla selezione',

    // Capture-script load failure (retry).
    reloadCaptureScriptsTitle: 'Ricarica gli script di cattura',
    captureUnavailable: 'Cattura non disponibile · riprova',

    // Empty / off-saved-page hint.
    navigateToSaved: 'Vai ai post salvati',

    // Captured count (manual scroll).
    captured: { one: '{count} post acquisito', other: '{count} post acquisiti' },

    // Sync status bar.
    fetchingNew: 'Recupero nuovi post…',
    scrollingMore: 'Scorrimento per caricarne altri…',
    scanned: 'analizzati',
    new: 'nuovi',
    existing: '{count} esistenti',
    library: 'libreria',

    // Folder-import modal action labels.
    actionDownload: 'Scarica',
    actionImport: 'Importa',
  },
  en: {
    folderFallback: 'Folder',

    errNoSelection: 'No posts selected.',
    errStartImport: 'Could not start the import.',
    errStartSync: 'Could not start syncing.',
    errSourceSyncActive: 'A source sync is already running on this platform.',

    back: 'Back',
    forward: 'Forward',
    reload: 'Reload',

    intercepting: 'Intercepting',

    stopSyncTitle: 'Stop syncing',
    stopSourceSyncTitle: 'Stop the running source sync',
    scriptsErrorCapture: 'Capture script not loaded — retry',
    scriptsLoadingCapture: 'Loading capture script…',
    autoImportFolderTitle: 'Auto-import the contents of this collection',
    autoImportAllTitle: 'Auto-import all saved posts',
    stop: 'Stop',
    autoImport: 'Auto-import',

    selectTitle: 'Select the posts to import',
    scriptsErrorSelect: 'Selection script not loaded — retry',
    scriptsLoadingSelect: 'Loading selection script…',
    select: 'Select',
    selectedCount: 'selected',
    importSelectedTitle: 'Import the selected posts',
    importSelected: 'Import selected',
    exitSelectionTitle: 'Exit selection',

    reloadCaptureScriptsTitle: 'Reload the capture scripts',
    captureUnavailable: 'Capture unavailable · retry',

    navigateToSaved: 'Navigate to saved posts',

    captured: { one: '{count} post captured', other: '{count} posts captured' },

    fetchingNew: 'Fetching new posts…',
    scrollingMore: 'Scrolling to load more…',
    scanned: 'scanned',
    new: 'new',
    existing: '{count} existing',
    library: 'library',

    actionDownload: 'Download',
    actionImport: 'Import',
  },
};
