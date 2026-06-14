// Strings for the Activity Center: the pure buildActivities() selector, the
// useActivityCore() session-log milestones, and the ActivityCenter UI itself
// (the sidebar strip + popover). Both the hook (src/hooks/useActivity.js) and the
// component (src/components/ActivityCenter.jsx) read this single namespace.
//
// The Italian column reproduces the app's existing copy VERBATIM: buildActivities
// is unit-tested as a plain function (defaults to lang 'it') and tests/setup pins
// the UI to Italian, so these `it` values are asserted character-for-character.
//
// Status / phase labels are kept as FLAT string keys (status_*, phase_*) rather
// than nested objects, so translate() never mistakes them for a { one, other }
// plural shape. Counts (Auto-tag {count}/{total}) are plain interpolations, not
// plurals, for the same reason.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // ── strip / popover header & chrome (ActivityCenter.jsx) ──
    title: 'Attività',
    empty: 'Nessuna attività in background.',
    sectionLive: 'In corso',
    sectionRecent: 'Recenti',
    clearAll: 'Cancella tutto',
    timeNow: 'adesso',
    timeMinAgo: '{n} min fa',
    timeHourAgo: '{n} h fa',
    timeDayAgo: '{n} g fa',

    // ── live item titles (buildActivities) ──
    analysisTitle: 'Auto-tag AI',
    downloadTitle: 'Download media',
    webTitle: 'Aggiunta sito',
    webTitleError: 'Aggiunta sito non riuscita',
    modelTitle: 'Download modello AI',
    syncTitle: 'Sincronizzazione {platform}',
    saveTitle: 'Salvataggio post',
    saveSubtitle: 'Importo i selezionati…',
    binariesTitle: 'Preparazione strumenti',
    sttTitle: 'Download modello vocale',

    // ── live item subtitles / counts (buildActivities) ──
    newPosts: '{n} nuovi post',
    inProgress: 'In corso…',
    hostFallback: 'sito',
    syncAllSaved: 'Tutti i salvati',
    syncCounts: '{scanned} analizzati · {n} nuovi',
    syncLoginTitle: 'Accesso richiesto — {platform}',
    syncLoginSub: 'Apri il browser e accedi per sincronizzare.',
    syncErrorTitle: 'Sincronizzazione {platform} non riuscita',
    syncErrorSub: 'Pagina dei salvati non raggiungibile.',
    syncOpenBrowser: 'Apri browser',

    // ── action labels (buildActivities) ──
    actionResume: 'Riprendi',
    actionPause: 'Pausa',
    actionCancelAll: 'Annulla tutto',
    actionCancel: 'Annulla',
    actionRemove: 'Rimuovi',
    actionRetry: 'Riprova',
    actionStop: 'Interrompi',
    actionClose: 'Chiudi',

    // ── shortLabel formats (buildActivities) ──
    shortAnalysis: 'Auto-tag {count}/{total}',
    shortDownload: 'Download {count}/{total}',
    shortWeb: 'Aggiunta sito',
    shortSave: 'Salvataggio post',
    shortSync: 'Sync {platform}',
    shortModelPct: 'Modello {pct}%',
    shortModel: 'Download modello',
    shortSttPct: 'Voce {pct}%',
    shortStt: 'Modello vocale',
    shortBinariesPct: 'Strumenti {pct}%',
    shortBinaries: 'Preparazione strumenti',
    shortUpdate: 'Aggiornamento',
    pausedSuffix: 'in pausa',

    // ── headline (buildActivities) ──
    headlineIdle: 'Nessuna attività',
    headlineMore: '{primary} · +{n}',

    // ── STATUS_LABEL (flat) ──
    status_queued: 'In coda',
    status_running: 'In corso',
    status_paused: 'In pausa',
    status_done: 'Completato',
    status_error: 'Errore',
    status_available: 'Disponibile',
    status_ready: 'Pronto',
    status_installing: 'Installazione',

    // ── WEB_PHASE_LABEL (flat) ──
    phase_queued: 'In coda',
    phase_discovering: 'Individuazione pagine',
    phase_capturing: 'Cattura screenshot',
    phase_extracting: 'Estrazione contenuti',
    phase_analyzing: 'Analisi AI',
    phase_done: 'Completato',
    phase_error: 'Errore',

    // ── updateItem (buildActivities) ──
    updateAvailable: 'Aggiornamento{v} disponibile',
    updateAvailableSub: 'Si ricompila sul PC (qualche minuto).',
    updateNow: 'Aggiorna ora',
    updateLater: 'Più tardi',
    updateDownloading: 'Scaricamento aggiornamento',
    updateBuilding: 'Compilazione aggiornamento',
    updateBuildingSub: 'Compilo SHELFY{v}…',
    updateInstalling: 'Installazione…',
    updateInstallingSub: 'L’app si riavvierà tra poco.',
    updateReady: 'Aggiornamento{v} pronto',
    updateReadySub: 'Riavvia per installarlo.',
    updateRestartNow: 'Riavvia ora',
    updateManualSub: 'Disponibile da scaricare.',
    updateDownload: 'Scarica',
    updateFailed: 'Aggiornamento non riuscito',
    updateFailedSub: 'Errore durante l’aggiornamento.',
    updateRetry: 'Riprova',
    updateClose: 'Chiudi',

    // ── useActivityCore log milestones ──
    logUpdateReady: 'Aggiornamento {version} pronto',
    logUpdateReadySub: 'Riavvia per installarlo.',
    logUpdateError: 'Aggiornamento non riuscito',
    logBinariesReady: 'Strumenti pronti',
    logBinariesError: 'Preparazione strumenti non riuscita',
    logSttReady: 'Modello vocale pronto',
    logModelReady: 'Modello AI pronto',
    logAnalysisDone: 'Auto-tag completato',
    logAnalysisDoneSub: '{n} post analizzati',
    logDownloadDone: 'Download completati',
    logDownloadDoneSub: '{n} file',
    logWebAdded: 'Sito aggiunto',
    logWebAddedPartial: 'Sito aggiunto (parziale)',
    logWebError: 'Aggiunta sito non riuscita',
    logSaveDone: 'Post salvati',
    logSaveDoneSub: '{n} post aggiunti alla libreria',
    logSyncDone: 'Sincronizzazione {platform} completata',
    logSyncDoneSub: '{n} nuovi post',
    logSyncDoneSkippedSub: '{n} nuovi post · {m} cartelle saltate',
    logSyncStopped: 'Sincronizzazione {platform} interrotta',
  },
  en: {
    // ── strip / popover header & chrome (ActivityCenter.jsx) ──
    title: 'Activity',
    empty: 'No background activity.',
    sectionLive: 'In progress',
    sectionRecent: 'Recent',
    clearAll: 'Clear all',
    timeNow: 'now',
    timeMinAgo: '{n} min ago',
    timeHourAgo: '{n} h ago',
    timeDayAgo: '{n} d ago',

    // ── live item titles (buildActivities) ──
    analysisTitle: 'AI auto-tag',
    downloadTitle: 'Media download',
    webTitle: 'Adding site',
    webTitleError: 'Could not add site',
    modelTitle: 'AI model download',
    syncTitle: 'Syncing {platform}',
    saveTitle: 'Saving posts',
    saveSubtitle: 'Importing the selected ones…',
    binariesTitle: 'Preparing tools',
    sttTitle: 'Voice model download',

    // ── live item subtitles / counts (buildActivities) ──
    newPosts: '{n} new posts',
    inProgress: 'In progress…',
    hostFallback: 'site',
    syncAllSaved: 'All saved posts',
    syncCounts: '{scanned} scanned · {n} new',
    syncLoginTitle: 'Login required — {platform}',
    syncLoginSub: 'Open the browser and log in to sync.',
    syncErrorTitle: '{platform} sync failed',
    syncErrorSub: 'Could not reach the saved page.',
    syncOpenBrowser: 'Open browser',

    // ── action labels (buildActivities) ──
    actionResume: 'Resume',
    actionPause: 'Pause',
    actionCancelAll: 'Cancel all',
    actionCancel: 'Cancel',
    actionRemove: 'Remove',
    actionRetry: 'Retry',
    actionStop: 'Stop',
    actionClose: 'Close',

    // ── shortLabel formats (buildActivities) ──
    shortAnalysis: 'Auto-tag {count}/{total}',
    shortDownload: 'Download {count}/{total}',
    shortWeb: 'Adding site',
    shortSave: 'Saving posts',
    shortSync: 'Sync {platform}',
    shortModelPct: 'Model {pct}%',
    shortModel: 'Model download',
    shortSttPct: 'Voice {pct}%',
    shortStt: 'Voice model',
    shortBinariesPct: 'Tools {pct}%',
    shortBinaries: 'Preparing tools',
    shortUpdate: 'Update',
    pausedSuffix: 'paused',

    // ── headline (buildActivities) ──
    headlineIdle: 'No activity',
    headlineMore: '{primary} · +{n}',

    // ── STATUS_LABEL (flat) ──
    status_queued: 'Queued',
    status_running: 'In progress',
    status_paused: 'Paused',
    status_done: 'Completed',
    status_error: 'Error',
    status_available: 'Available',
    status_ready: 'Ready',
    status_installing: 'Installing',

    // ── WEB_PHASE_LABEL (flat) ──
    phase_queued: 'Queued',
    phase_discovering: 'Discovering pages',
    phase_capturing: 'Capturing screenshots',
    phase_extracting: 'Extracting content',
    phase_analyzing: 'AI analysis',
    phase_done: 'Completed',
    phase_error: 'Error',

    // ── updateItem (buildActivities) ──
    updateAvailable: 'Update{v} available',
    updateAvailableSub: 'It rebuilds on your PC (a few minutes).',
    updateNow: 'Update now',
    updateLater: 'Later',
    updateDownloading: 'Downloading update',
    updateBuilding: 'Building update',
    updateBuildingSub: 'Building SHELFY{v}…',
    updateInstalling: 'Installing…',
    updateInstallingSub: 'The app will restart shortly.',
    updateReady: 'Update{v} ready',
    updateReadySub: 'Restart to install it.',
    updateRestartNow: 'Restart now',
    updateManualSub: 'Available to download.',
    updateDownload: 'Download',
    updateFailed: 'Update failed',
    updateFailedSub: 'An error occurred during the update.',
    updateRetry: 'Retry',
    updateClose: 'Close',

    // ── useActivityCore log milestones ──
    logUpdateReady: 'Update {version} ready',
    logUpdateReadySub: 'Restart to install it.',
    logUpdateError: 'Update failed',
    logBinariesReady: 'Tools ready',
    logBinariesError: 'Could not prepare tools',
    logSttReady: 'Voice model ready',
    logModelReady: 'AI model ready',
    logAnalysisDone: 'Auto-tag completed',
    logAnalysisDoneSub: '{n} posts analyzed',
    logDownloadDone: 'Downloads completed',
    logDownloadDoneSub: '{n} files',
    logWebAdded: 'Site added',
    logWebAddedPartial: 'Site added (partial)',
    logWebError: 'Could not add site',
    logSaveDone: 'Posts saved',
    logSaveDoneSub: '{n} posts added to your library',
    logSyncDone: 'Syncing {platform} completed',
    logSyncDoneSub: '{n} new posts',
    logSyncDoneSkippedSub: '{n} new posts · {m} folders skipped',
    logSyncStopped: 'Syncing {platform} stopped',
  },
} satisfies LangMessages;
