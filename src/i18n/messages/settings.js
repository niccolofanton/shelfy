// UI strings for the Settings view (src/views/Settings.jsx). The Italian column
// reproduces the app's existing copy verbatim (char-for-char, including … ’ and
// accents); the English column is a natural translation. Brand/product names
// (Instagram, X / Twitter, Pinterest, SHELFY, yt-dlp, ffmpeg, llama.cpp,
// whisper.cpp, CUDA, Vulkan, Metal) and language-neutral units (GB, %) are not
// translated. Shared terms (Annulla, Conferma, Chiudi, Scarica, …) live in the
// `common` namespace and are reused via useT('common').
export default {
  it: {
    // Page header
    pageTitle: 'Impostazioni',
    pageSubtitle:
      'Configura il modello di analisi AI, le preferenze di download e la gestione dei dati.',

    // Section titles
    sectionAi: 'Intelligenza artificiale',
    sectionData: 'Download e dati',
    sectionUpdates: 'Aggiornamenti',
    sectionDanger: 'Zona pericolosa',
    sectionLegal: 'Note legali',

    // Download types (DOWNLOAD_TYPES)
    typeThumbnailLabel: 'Thumbnails',
    typeThumbnailDesc: 'Anteprime a bassa risoluzione',
    typeImageLabel: 'Images',
    typeImageDesc: 'Immagini a piena risoluzione',
    typeVideoLabel: 'Videos',
    typeVideoDesc: 'File video completi',

    // DangerRow / DeleteControl confirm states
    confirmDelete: 'Conferma eliminazione',
    cancelDelete: 'Annulla eliminazione',
    deleteFromDisk: 'Elimina dal disco',
    deleteNameFromDisk: 'Elimina {name} dal disco',

    // Export modal
    exportTitle: 'Esporta JSON',
    exportedCount: {
      one: 'Esportato {count} post.',
      other: 'Esportati {count} post.',
    },
    exportDescription: 'Scegli le source da includere. Il file è compatibile con l’import.',
    exportFailed: 'Esportazione non riuscita. Riprova.',
    exporting: 'Esportazione…',
    export: 'Esporta',

    // Model state badge
    active: 'Attivo',
    downloaded: 'Scaricato',
    toDownload: 'Da scaricare',

    // Model row
    recommended: 'Consigliato',
    ramUnit: '{n} GB RAM',
    pauseDownload: 'Pausa download',
    cancelDownload: 'Annulla download',
    waitDownloadInProgress: 'Attendi il download in corso',
    resumeDownload: 'Riprendi il download',
    downloadModel: 'Scarica il modello',

    // Model picker (background download note)
    downloadInProgressNote: 'Download in corso… puoi continuare a usare un modello già scaricato.',

    // ModelPicker titles/descriptions (AI section)
    vlmTitle: 'Modello di analisi AI',
    vlmDesc:
      'Gira in locale (llama.cpp): legge i frame dei video e produce tag e descrizioni. Più grande = qualità migliore ma più lento e pesante.',
    sttTitle: 'Modello di trascrizione vocale',
    sttDesc:
      'Gira in locale (whisper.cpp): trascrive la dettatura vocale nella ricerca AI. Più grande = più accurato ma più lento e pesante.',
    embTitle: 'Modello di embedding',
    embDesc:
      'Gira in locale (llama.cpp): raggruppa i tag simili anche quando non co-occorrono, migliorando i cluster. Opzionale e leggerissimo; senza, si usa solo la co-occorrenza.',

    // Concurrency picker
    concurrencyTitle: 'Classificazioni in parallelo',
    concurrencyDesc:
      'Quanti post analizzare contemporaneamente. Valori più alti velocizzano i lotti ma usano più VRAM. Imposta 1 se la GPU ha poca memoria o noti errori.',
    concurrencyAria: 'Numero di classificazioni in parallelo',

    // Update channel picker
    updateChannelTitle: 'Canale aggiornamenti',
    updateChannelStable: 'Stabile',
    updateChannelBeta: 'Beta',
    updateChannelDesc1: ': solo versioni rilasciate. ',
    updateChannelDesc2:
      ': build di test, più recenti ma possono essere instabili (ricevi comunque anche le stabili).',
    installedVersion: 'Versione installata: {version}',
    downloadingVersion: 'Scaricamento {version} — {pct}%',
    buildingVersion: 'Compilazione {version}…',
    updateError: 'Errore durante l’aggiornamento.',
    updateChannelAria: 'Canale aggiornamenti',
    updateReady: 'Aggiornamento {version} pronto da installare',
    updateAvailable: 'Aggiornamento {version} disponibile',
    updateDownloading: 'Scaricamento in corso…',
    updateBuilding: 'Compilazione in corso…',
    updateInstalling: 'Installazione…',
    updateUpToDate: 'Sei sull’ultima versione disponibile.',
    updateNow: 'Aggiorna ora',
    restartAndInstall: 'Riavvia e installa',
    checkForUpdates: 'Controlla aggiornamenti',

    // Runtime binaries card
    llamaVariantCpu: 'CPU (compatibile ovunque)',
    llamaVariantCuda: 'NVIDIA (CUDA)',
    llamaVariantVulkan: 'AMD/Intel (Vulkan)',
    llamaVariantMetal: 'Apple (Metal)',
    runtimeTitle: 'Componenti runtime',
    runtimeDesc:
      'yt-dlp, ffmpeg, motore AI e trascrizione. Non sono inclusi nell’installer: si scaricano una volta e non vengono ri-scaricati ad ogni aggiornamento.',
    runtimeChecking: 'Verifica…',
    runtimeReady: '● Pronti',
    runtimeMissing: '● Mancanti: {missing}',
    variantFallbackWarn:
      'L’accelerazione "{variant}" non si è avviata: passo alla versione CPU (download in corso). Aggiorna i driver e ri-seleziona la GPU per riprovare.',
    variantFailedWarn:
      'Accelerazione {variants} non disponibile su questa macchina: in uso la CPU. Ri-selezionala per riprovare dopo un aggiornamento driver.',
    runtimeVariantAria: 'Variante motore AI',
    redownload: 'Riscarica',
    phaseExtract: 'Estrazione…',
    phaseError: 'Errore: {error}',
    phaseDone: 'Completato',
    phaseDownloadingPct: 'Scaricamento {pct}%',
    phaseDownloading: 'Scaricamento…',

    // Variant labels (PerformanceCard "motore attivo")
    variantCpu: 'CPU',
    variantCuda: 'NVIDIA (CUDA)',
    variantVulkan: 'AMD/Intel (Vulkan)',
    variantMetal: 'Apple (Metal)',

    // Tuning select (auto option)
    tuningAuto: 'Automatico ({effective})',

    // Performance card
    detectingHardware: 'Rilevamento hardware…',
    performanceTitle: 'Prestazioni e hardware',
    modeAuto: 'Automatico',
    modeCustom: 'Personalizzato',
    performanceDescCustom:
      'Sovrascrivi i singoli parametri. Lascia "Automatico" su una voce per il valore consigliato dall’hardware.',
    performanceDescAuto:
      'I motori AI vengono configurati automaticamente in base al tuo hardware. Passa a "Personalizzato" per regolare i parametri a mano.',
    coresUnit: '{cores} core',
    sharedVram: '{vram} GB (condivisa)',
    activeEngine: 'Motore attivo: ',
    recommendedVariantHint: ' · consigliato {variant} (scaricalo da “Componenti runtime”)',
    gpuOffloadLabel: 'Offload su GPU',
    gpuOffloadHintCpu: 'Motore CPU: l’offload non si applica',
    gpuOffloadHint: 'Layer in GPU; “adattivo” = quanti ne entrano nella memoria',
    gpuOffloadAdaptive: 'adattivo',
    gpuOffloadAllLayers: 'Tutti i layer',
    gpuOffloadCpuOnly: 'Solo CPU',
    analysisThreadsLabel: 'Thread analisi (CPU)',
    analysisThreadsHint: 'Thread di calcolo per il modello di analisi',
    microBatchLabel: 'Micro-batch',
    microBatchHint: 'Più alto = prefill più veloce ma più memoria',
    kvCacheLabel: 'Cache KV',
    kvCacheHint: 'q8_0 dimezza la memoria con qualità quasi identica',
    kvCacheF16: 'f16 (qualità)',
    kvCacheQ8: 'q8_0 (memoria)',
    transcriptionThreadsLabel: 'Thread trascrizione',
    transcriptionThreadsHint: 'Thread per la dettatura vocale (whisper)',
    resetToAuto: 'Ripristina tutto ad Automatico',

    // Data actions
    dataTitle: 'Import / Export JSON',
    dataDesc: 'Importa post da un file JSON, oppure esporta i post salvati scegliendo le source.',
    importJSON: 'Importa JSON',
    exportJSON: 'Esporta JSON',

    // Legal card
    legalTitle: 'Avvertenze legali e responsabilità',
    legalDesc1:
      'Shelfy archivia i contenuti che hai salvato nei tuoi account, per uso personale. Sei responsabile del rispetto dei Termini di Servizio delle piattaforme e dei diritti di terzi. Testo completo in ',
    legalAccepted: 'Accettato il {date} · versione {version}',
    legalNotAccepted: 'Non ancora accettato · versione corrente {version}',
    legalReview: 'Rivedi avvertenze',

    // Asset types card
    assetTypesTitle: 'Tipi di asset da scaricare',
    assetTypesDesc: 'Scegli quali asset vengono scaricati con "Download All" e "Download Missing".',

    // Danger zone
    dangerHeading: 'Azioni irreversibili',
    dangerSubheading:
      'Queste operazioni non possono essere annullate. Ognuna richiede una conferma esplicita.',
    dangerAssetsTitle: 'Cancella i file salvati',
    dangerAssetsDesc:
      'Elimina dal disco tutti i file scaricati (thumbnail, immagini, video). I post restano nel database e possono essere riscaricati.',
    dangerAssetsButton: 'Cancella file',
    dangerAssetsDone: 'Tutti i file scaricati sono stati eliminati.',
    dangerAiTitle: 'Cancella descrizioni e tag AI',
    dangerAiDesc:
      'Rimuove da tutti i post le descrizioni, i tag e le altre analisi generate dall’AI. I post restano nella libreria e possono essere rianalizzati.',
    dangerAiButton: 'Cancella analisi AI',
    dangerAiDone: 'Tutte le analisi AI sono state eliminate.',
    dangerDataTitle: 'Cancella tutti i post salvati',
    dangerDataDesc:
      'Rimuove definitivamente tutti i post dal database. I file già scaricati sul disco non vengono eliminati.',
    dangerDataButton: 'Cancella tutti i dati',
    dangerDataDone: 'Tutti i post sono stati eliminati.',
  },
  en: {
    // Page header
    pageTitle: 'Settings',
    pageSubtitle: 'Configure the AI analysis model, download preferences and data management.',

    // Section titles
    sectionAi: 'Artificial intelligence',
    sectionData: 'Downloads and data',
    sectionUpdates: 'Updates',
    sectionDanger: 'Danger zone',
    sectionLegal: 'Legal',

    // Download types (DOWNLOAD_TYPES)
    typeThumbnailLabel: 'Thumbnails',
    typeThumbnailDesc: 'Low-resolution previews',
    typeImageLabel: 'Images',
    typeImageDesc: 'Full-resolution images',
    typeVideoLabel: 'Videos',
    typeVideoDesc: 'Complete video files',

    // DangerRow / DeleteControl confirm states
    confirmDelete: 'Confirm deletion',
    cancelDelete: 'Cancel deletion',
    deleteFromDisk: 'Delete from disk',
    deleteNameFromDisk: 'Delete {name} from disk',

    // Export modal
    exportTitle: 'Export JSON',
    exportedCount: {
      one: 'Exported {count} post.',
      other: 'Exported {count} posts.',
    },
    exportDescription: 'Choose which sources to include. The file is compatible with import.',
    exportFailed: 'Export failed. Try again.',
    exporting: 'Exporting…',
    export: 'Export',

    // Model state badge
    active: 'Active',
    downloaded: 'Downloaded',
    toDownload: 'To download',

    // Model row
    recommended: 'Recommended',
    ramUnit: '{n} GB RAM',
    pauseDownload: 'Pause download',
    cancelDownload: 'Cancel download',
    waitDownloadInProgress: 'Wait for the download in progress',
    resumeDownload: 'Resume the download',
    downloadModel: 'Download the model',

    // Model picker (background download note)
    downloadInProgressNote:
      'Download in progress… you can keep using a model that’s already downloaded.',

    // ModelPicker titles/descriptions (AI section)
    vlmTitle: 'AI analysis model',
    vlmDesc:
      'Runs locally (llama.cpp): reads video frames and produces tags and descriptions. Bigger = better quality but slower and heavier.',
    sttTitle: 'Voice transcription model',
    sttDesc:
      'Runs locally (whisper.cpp): transcribes voice dictation in AI search. Bigger = more accurate but slower and heavier.',
    embTitle: 'Embedding model',
    embDesc:
      'Runs locally (llama.cpp): groups similar tags even when they don’t co-occur, improving clusters. Optional and very lightweight; without it, only co-occurrence is used.',

    // Concurrency picker
    concurrencyTitle: 'Parallel classifications',
    concurrencyDesc:
      'How many posts to analyze at once. Higher values speed up batches but use more VRAM. Set 1 if the GPU has little memory or you notice errors.',
    concurrencyAria: 'Number of parallel classifications',

    // Update channel picker
    updateChannelTitle: 'Update channel',
    updateChannelStable: 'Stable',
    updateChannelBeta: 'Beta',
    updateChannelDesc1: ': released versions only. ',
    updateChannelDesc2:
      ': test builds, more recent but may be unstable (you still receive stable ones too).',
    installedVersion: 'Installed version: {version}',
    downloadingVersion: 'Downloading {version} — {pct}%',
    buildingVersion: 'Building {version}…',
    updateError: 'Error during the update.',
    updateChannelAria: 'Update channel',
    updateReady: 'Update {version} ready to install',
    updateAvailable: 'Update {version} available',
    updateDownloading: 'Downloading…',
    updateBuilding: 'Building…',
    updateInstalling: 'Installing…',
    updateUpToDate: 'You’re on the latest available version.',
    updateNow: 'Update now',
    restartAndInstall: 'Restart and install',
    checkForUpdates: 'Check for updates',

    // Runtime binaries card
    llamaVariantCpu: 'CPU (compatible everywhere)',
    llamaVariantCuda: 'NVIDIA (CUDA)',
    llamaVariantVulkan: 'AMD/Intel (Vulkan)',
    llamaVariantMetal: 'Apple (Metal)',
    runtimeTitle: 'Runtime components',
    runtimeDesc:
      'yt-dlp, ffmpeg, AI engine and transcription. They’re not bundled in the installer: they download once and aren’t re-downloaded on every update.',
    runtimeChecking: 'Checking…',
    runtimeReady: '● Ready',
    runtimeMissing: '● Missing: {missing}',
    variantFallbackWarn:
      'The "{variant}" acceleration didn’t start: switching to the CPU version (download in progress). Update your drivers and re-select the GPU to try again.',
    variantFailedWarn:
      '{variants} acceleration isn’t available on this machine: using the CPU. Re-select it to try again after a driver update.',
    runtimeVariantAria: 'AI engine variant',
    redownload: 'Re-download',
    phaseExtract: 'Extracting…',
    phaseError: 'Error: {error}',
    phaseDone: 'Completed',
    phaseDownloadingPct: 'Downloading {pct}%',
    phaseDownloading: 'Downloading…',

    // Variant labels (PerformanceCard "motore attivo")
    variantCpu: 'CPU',
    variantCuda: 'NVIDIA (CUDA)',
    variantVulkan: 'AMD/Intel (Vulkan)',
    variantMetal: 'Apple (Metal)',

    // Tuning select (auto option)
    tuningAuto: 'Automatic ({effective})',

    // Performance card
    detectingHardware: 'Detecting hardware…',
    performanceTitle: 'Performance and hardware',
    modeAuto: 'Automatic',
    modeCustom: 'Custom',
    performanceDescCustom:
      'Override individual parameters. Leave "Automatic" on an entry for the value recommended by your hardware.',
    performanceDescAuto:
      'AI engines are configured automatically based on your hardware. Switch to "Custom" to adjust the parameters by hand.',
    coresUnit: '{cores} cores',
    sharedVram: '{vram} GB (shared)',
    activeEngine: 'Active engine: ',
    recommendedVariantHint: ' · recommended {variant} (download it from “Runtime components”)',
    gpuOffloadLabel: 'GPU offload',
    gpuOffloadHintCpu: 'CPU engine: offload doesn’t apply',
    gpuOffloadHint: 'Layers on GPU; “adaptive” = as many as fit in memory',
    gpuOffloadAdaptive: 'adaptive',
    gpuOffloadAllLayers: 'All layers',
    gpuOffloadCpuOnly: 'CPU only',
    analysisThreadsLabel: 'Analysis threads (CPU)',
    analysisThreadsHint: 'Compute threads for the analysis model',
    microBatchLabel: 'Micro-batch',
    microBatchHint: 'Higher = faster prefill but more memory',
    kvCacheLabel: 'KV cache',
    kvCacheHint: 'q8_0 halves memory with nearly identical quality',
    kvCacheF16: 'f16 (quality)',
    kvCacheQ8: 'q8_0 (memory)',
    transcriptionThreadsLabel: 'Transcription threads',
    transcriptionThreadsHint: 'Threads for voice dictation (whisper)',
    resetToAuto: 'Reset everything to Automatic',

    // Data actions
    dataTitle: 'Import / Export JSON',
    dataDesc: 'Import posts from a JSON file, or export saved posts by choosing the sources.',
    importJSON: 'Import JSON',
    exportJSON: 'Export JSON',

    // Legal card
    legalTitle: 'Legal notices and liability',
    legalDesc1:
      'Shelfy stores the content you saved in your accounts, for personal use. You are responsible for complying with the platforms’ Terms of Service and third-party rights. Full text in ',
    legalAccepted: 'Accepted on {date} · version {version}',
    legalNotAccepted: 'Not yet accepted · current version {version}',
    legalReview: 'Review notices',

    // Asset types card
    assetTypesTitle: 'Asset types to download',
    assetTypesDesc:
      'Choose which assets are downloaded with "Download All" and "Download Missing".',

    // Danger zone
    dangerHeading: 'Irreversible actions',
    dangerSubheading:
      'These operations cannot be undone. Each one requires an explicit confirmation.',
    dangerAssetsTitle: 'Delete saved files',
    dangerAssetsDesc:
      'Deletes from disk all downloaded files (thumbnails, images, videos). Posts stay in the database and can be re-downloaded.',
    dangerAssetsButton: 'Delete files',
    dangerAssetsDone: 'All downloaded files have been deleted.',
    dangerAiTitle: 'Delete AI descriptions and tags',
    dangerAiDesc:
      'Removes from all posts the descriptions, tags and other AI-generated analysis. Posts stay in the library and can be re-analyzed.',
    dangerAiButton: 'Delete AI analysis',
    dangerAiDone: 'All AI analysis has been deleted.',
    dangerDataTitle: 'Delete all saved posts',
    dangerDataDesc:
      'Permanently removes all posts from the database. Files already downloaded to disk are not deleted.',
    dangerDataButton: 'Delete all data',
    dangerDataDone: 'All posts have been deleted.',
  },
};
