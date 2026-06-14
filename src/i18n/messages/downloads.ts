// Strings for the Downloads view: header, controls, per-job status, stats, empty.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Download',
    downloadAll: 'Scarica tutti',
    downloadMissing: 'Scarica mancanti',
    pauseResumeTitlePaused: 'Riprendi i download in coda',
    pauseResumeTitleActive: 'Metti in pausa la coda dei download',
    clearFinished: 'Pulisci completati',
    clearFinishedTitle: 'Rimuovi dalla lista i download completati e annullati',
    clearQueue: 'Svuota coda',
    clearQueueTitle: 'Annulla i download in corso e svuota la lista',
    progressSummary: {
      one: '{done} / {total} post scaricato',
      other: '{done} / {total} post scaricati',
    },
    cancelJob: 'Annulla questo download',
    retryJob: 'Riprova questo download',
    empty: 'Nessun download attivo. Usa i pulsanti qui sopra per iniziare a scaricare i contenuti.',
    statTotal: 'Totale',
    statThumbnails: 'Con anteprime',
    statImages: 'Con immagini',
    statVideos: 'Con video',
    // Per-asset labels (shown next to each job).
    assetThumbnail: 'anteprima',
    assetImage: 'immagine',
    assetVideo: 'video',
    // Per-job status labels.
    statusPending: 'in coda',
    statusDownloading: 'scaricamento',
    statusDone: 'completato',
    statusError: 'errore',
    statusCancelled: 'annullato',
  },
  en: {
    title: 'Downloads',
    downloadAll: 'Download All',
    downloadMissing: 'Download Missing',
    pauseResumeTitlePaused: 'Resume queued downloads',
    pauseResumeTitleActive: 'Pause the download queue',
    clearFinished: 'Clear finished',
    clearFinishedTitle: 'Remove finished and cancelled downloads from the list',
    clearQueue: 'Clear queue',
    clearQueueTitle: 'Cancel in-progress downloads and empty the list',
    progressSummary: {
      one: '{done} / {total} post downloaded',
      other: '{done} / {total} posts downloaded',
    },
    cancelJob: 'Cancel this download',
    retryJob: 'Retry this download',
    empty: 'No active downloads. Use the buttons above to start downloading assets.',
    statTotal: 'Total',
    statThumbnails: 'With thumbnails',
    statImages: 'With images',
    statVideos: 'With videos',
    assetThumbnail: 'thumbnail',
    assetImage: 'image',
    assetVideo: 'video',
    statusPending: 'pending',
    statusDownloading: 'downloading',
    statusDone: 'done',
    statusError: 'error',
    statusCancelled: 'cancelled',
  },
} satisfies LangMessages;
