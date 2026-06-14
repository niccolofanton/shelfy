// UI strings for the Auto-tag queue view (AiTagsQueue): platform/media-type
// badges, the per-job status labels, the 4-phase pipeline stepper (Coda →
// Estrazione → Analisi → Fatto, here surfaced as a six-segment track), the
// header controls, the timing/counts cards, the in-flight row chrome and the
// empty state. The Italian column reproduces the app's existing copy verbatim.
// Brand names (Instagram, Pinterest, Twitter) and model ids are NOT translated;
// neutral abbreviations on the platform badge (IG/PIN/WEB/TW) stay as-is.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // ── Platform badge titles ──
    platformWeb: 'Web reference',

    // ── Media-type labels ──
    mediaVideo: 'Video',
    mediaCarousel: 'Carosello',
    mediaImages: 'Immagini',
    mediaImage: 'Immagine',
    mediaText: 'Testo',

    // ── Status labels ──
    statusPending: 'In coda',
    statusExtracting: 'Estrazione',
    statusAnalyzing: 'Analisi',
    statusDone: 'Completato',
    statusError: 'Errore',
    statusCancelled: 'Annullato',

    // ── Phase stepper (keep the 4 conceptual phases) ──
    phaseQueue: 'Coda',
    phaseProcessing: 'Processing',
    phaseAnalysis: 'Analisi',
    phaseDescription: 'Descrizione',
    phaseTags: 'Tags',
    phaseDone: 'Fatto',

    // ── Job row chrome ──
    preparingFrames: 'Preparazione dei fotogrammi…',
    generatingDescription: 'Generazione descrizione…',
    cancelJob: 'Annulla',
    retryJob: 'Riprova',

    // ── Header ──
    heading: 'Auto-tag',
    analyzeMissing: 'Analizza mancanti',
    analyzeMissingReadyTitle: 'Genera i tag per tutti i post non ancora analizzati',
    analyzeMissingNotReadyTitle: 'Scarica prima un modello dalle Impostazioni',
    resume: 'Riprendi',
    pause: 'Pausa',
    resumeTitle: 'Riprendi le analisi in coda',
    pauseTitle: 'Metti in pausa la coda delle analisi',
    clearCompleted: 'Pulisci completati',
    clearCompletedTitle: 'Rimuovi dalla lista le analisi completate (libera memoria)',
    cancelAll: 'Annulla tutto',
    cancelAllTitle: 'Annulla le analisi in corso e svuota la lista',
    modelNotReady: 'Modello AI non pronto — scaricalo dalle Impostazioni per generare i tag.',
    progressCount: '{done} / {total} analizzati',
    paused: 'In pausa',

    // ── Timing cards ──
    lastTag: 'Ultimo tag',
    avgTime: 'Tempo medio',
    eta: 'Stima alla fine',
    estimating: 'in stima…',

    // ── Counts bar ──
    inQueue: 'In coda',
    completed: 'Completati',
    errors: 'Errori',

    // ── Empty state ──
    empty:
      'Nessuna analisi in coda. Usa "Analizza mancanti" per generare i tag dei post scaricati.',
  },
  en: {
    // ── Platform badge titles ──
    platformWeb: 'Web reference',

    // ── Media-type labels ──
    mediaVideo: 'Video',
    mediaCarousel: 'Carousel',
    mediaImages: 'Images',
    mediaImage: 'Image',
    mediaText: 'Text',

    // ── Status labels ──
    statusPending: 'Queued',
    statusExtracting: 'Extracting',
    statusAnalyzing: 'Analyzing',
    statusDone: 'Completed',
    statusError: 'Error',
    statusCancelled: 'Cancelled',

    // ── Phase stepper ──
    phaseQueue: 'Queue',
    phaseProcessing: 'Processing',
    phaseAnalysis: 'Analysis',
    phaseDescription: 'Description',
    phaseTags: 'Tags',
    phaseDone: 'Done',

    // ── Job row chrome ──
    preparingFrames: 'Preparing the frames…',
    generatingDescription: 'Generating description…',
    cancelJob: 'Cancel',
    retryJob: 'Retry',

    // ── Header ──
    heading: 'Auto-tag',
    analyzeMissing: 'Analyze missing',
    analyzeMissingReadyTitle: 'Generate tags for every post not yet analyzed',
    analyzeMissingNotReadyTitle: 'Download a model from Settings first',
    resume: 'Resume',
    pause: 'Pause',
    resumeTitle: 'Resume the queued analyses',
    pauseTitle: 'Pause the analysis queue',
    clearCompleted: 'Clear completed',
    clearCompletedTitle: 'Remove completed analyses from the list (frees memory)',
    cancelAll: 'Cancel all',
    cancelAllTitle: 'Cancel the running analyses and empty the list',
    modelNotReady: 'AI model not ready — download it from Settings to generate tags.',
    progressCount: '{done} / {total} analyzed',
    paused: 'Paused',

    // ── Timing cards ──
    lastTag: 'Last tag',
    avgTime: 'Average time',
    eta: 'Estimated finish',
    estimating: 'estimating…',

    // ── Counts bar ──
    inQueue: 'Queued',
    completed: 'Completed',
    errors: 'Errors',

    // ── Empty state ──
    empty: 'No analyses queued. Use "Analyze missing" to generate tags for the downloaded posts.',
  },
} satisfies LangMessages;
