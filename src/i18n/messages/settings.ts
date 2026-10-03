// UI strings for the Settings view (src/views/Settings.jsx). The Italian column
// reproduces the app's existing copy verbatim (char-for-char, including … ’ and
// accents); the English column is a natural translation. Brand/product names
// (Instagram, X / Twitter, Pinterest, SHELFY, yt-dlp, ffmpeg, llama.cpp,
// whisper.cpp, CUDA, Vulkan, Metal) and language-neutral units (GB, %) are not
// translated. Shared terms (Annulla, Conferma, Chiudi, Scarica, …) live in the
// `common` namespace and are reused via useT('common').
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

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

    // AI search-suggestions toggle (AI section)
    aiSuggestTitle: 'Suggerimenti di ricerca AI',
    aiSuggestDesc:
      'Mentre cerchi, un modello locale propone tag correlati da aggiungere ai filtri. Disattivato di default: avvia il modello a ogni ricerca, quindi è pesante.',
    remoteTitle: 'Provider AI remoti',
    remoteDesc:
      'Configura endpoint OpenAI compatibili, modelli e chiavi. Le chiavi restano nel Portachiavi macOS; le richieste HTTP richiedono un peer Tailscale attivo.',
    remoteAdd: 'Aggiungi provider',
    remoteRemove: 'Rimuovi',
    remoteAvailable: 'Disponibile',
    remoteUnavailable: 'Non disponibile',
    remoteName: 'Nome',
    remoteId: 'ID provider',
    remoteBaseUrl: 'Base URL',
    remoteModelId: 'Model ID esatto',
    remoteSecretName: 'Nome segreto',
    remotePiKey: 'ID chiave Pi (alternativa)',
    remoteVision: 'Supporta immagini e visione',
    remoteKey: 'Chiave API',
    remoteKeySet: 'configurata',
    remoteKeyMissing: 'assente',
    remoteKeyPlaceholder: 'Lascia vuoto per mantenere la chiave attuale',
    remoteDefault: 'Predefinito per chat e ricerca',
    remoteVisionDefault: 'Provider per la visione',
    remoteLocal: 'Locale',
    remoteNone: 'Nessuno',
    remoteSave: 'Salva provider',
    remoteSaving: 'Salvataggio…',
    remoteSaved: 'Provider salvati.',
    remoteLoadError: 'Impossibile caricare i provider.',
    remoteSaveError: 'Impossibile salvare i provider.',
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

    // ── The web: an account on a server (src/views/settings/) ──────────────────
    pageSubtitleAccount: 'Il tuo account, la lingua, lo spazio e le note legali.',
    pageSubtitleAccountAi:
      'Il tuo account, le preferenze AI, la lingua, lo spazio e le note legali.',
    sectionAccount: 'Account',
    sectionConnections: 'Connessioni',
    sectionStorage: 'Spazio',
    versionPillWeb: 'server {server} · web {build}',
    versionTitleWeb: 'Versione del server {server} · build dell’app web {build} (UTC)',

    // Account → profile
    profileTitle: 'Profilo',
    emailLabel: 'Email',
    emailManaged: 'L’indirizzo di accesso è gestito da chi amministra il server.',
    roleOwner: 'Proprietario',
    roleMember: 'Membro',
    signOut: 'Esci',
    signingOut: 'Uscita…',

    // Account → passkeys
    passkeysTitle: 'Passkey',
    passkeysDesc:
      'Accedi senza password, con l’impronta, il volto o il PIN del dispositivo. Le passkey restano sui tuoi dispositivi: il server conserva solo la loro chiave pubblica.',
    passkeysEmpty: 'Nessuna passkey. Aggiungine una per accedere senza link.',
    passkeyUnnamed: 'Passkey senza nome',
    passkeyAdded: 'Aggiunta il {date}',
    passkeyLastUsed: 'usata il {date}',
    passkeyNeverUsed: 'mai usata',
    passkeyAdd: 'Aggiungi una passkey',
    passkeyPreparing: 'Preparazione…',
    passkeyLabel: 'Nome',
    passkeyCreate: 'Crea la passkey',
    passkeyCreating: 'In attesa del dispositivo…',
    passkeyCreated: 'Passkey aggiunta.',
    passkeysOff: 'Le passkey non sono attive su questo server.',
    passkeysUnsupported:
      'Questo browser non supporta le passkey: aggiungile da un altro dispositivo.',

    // Account → sessions
    sessionsTitle: 'Sessioni',
    sessionsDesc: 'I browser in cui hai effettuato l’accesso. Esci da quelli che non riconosci.',
    sessionThisBrowser: 'Questo browser',
    sessionSignedIn: 'Accesso il {date}',
    sessionLastSeen: 'attiva il {date}',
    sessionEnd: 'Esci',
    sessionsEndOthers: 'Esci da tutte le altre sessioni',
    deviceOn: '{browser} su {os}',
    unknownDevice: 'Dispositivo sconosciuto',

    // Account → API tokens
    tokensTitle: 'Token API',
    tokensDesc:
      'Danno accesso alla tua libreria all’estensione del browser, al Comando rapido di iOS e allo strumento di migrazione. Revoca quelli che non usi.',
    tokensEmpty: 'Nessun token.',
    tokenKind_extension: 'Estensione del browser',
    tokenKind_shortcut: 'Comando rapido iOS',
    tokenKind_migrate: 'Strumento di migrazione',
    tokenKind_library: 'Client della libreria',
    tokenCreated: 'Creato il {date}',
    tokenLastUsed: 'usato il {date}',
    tokenNeverUsed: 'mai usato',
    tokenExpires: 'scade il {date}',
    tokenExpiryLabel: 'Validità',
    tokenExpiryDays: '{days} giorni',
    tokenRevoke: 'Revoca',
    tokenNew: 'Nuovo token',
    tokenKindLabel: 'Per',
    tokenLabel: 'Nome',
    tokenCreate: 'Crea',
    tokenValueTitle: 'Copia il token ora',
    tokenValueWarning:
      'È l’unica volta che lo vedi: il server ne conserva solo l’impronta. Chi lo possiede può usare la tua libreria, quindi non condividerlo.',
    tokenCopy: 'Copia',
    tokenCopied: 'Copiato',

    // Storage
    storageTitle: 'Spazio usato',
    storageUsed: '{used} usati',
    storageOfQuota: '{used} di {quota}',
    storageNoLimit: 'Nessun limite',
    storageBarLabel: 'Spazio usato da media e database',
    storageMedia: 'Media',
    storageDatabase: 'Database',
    storageCounted: 'Conteggio del {date}',
    storageCounting: 'Conteggio in corso…',
    archiveTitle: 'Cosa archiviare',
    archiveDesc: 'Scegli quali file dei tuoi post conserva il server.',
    archiveThumbnail: 'Copertine',
    archiveThumbnailDesc: 'Copertine dei post e anteprime dei video',
    archiveImage: 'Immagini',
    archiveImageDesc: 'Le immagini dei post, a piena risoluzione',
    archiveVideo: 'Video',
    archiveVideoDesc: 'I video, quando li salvi',

    // Legal
    legalDescWeb:
      'Le regole d’uso di Shelfy: cosa archivia e di cosa sei responsabile per i contenuti che salvi.',
    privacyTitle: 'Informativa sulla privacy',
    privacyDesc: 'Quali dati conserva questo server, dove, per quanto tempo e chi li tratta.',
    privacyAccepted: 'Accettata il {date} · versione {version}',
    privacyNotAccepted: 'Non ancora accettata · versione corrente {version}',
    privacyRead: 'Leggi l’informativa',
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

    // AI search-suggestions toggle (AI section)
    aiSuggestTitle: 'AI search suggestions',
    aiSuggestDesc:
      'While you search, a local model proposes related tags to add to the filters. Off by default: it spins up the model on every search, so it’s heavy.',
    remoteTitle: 'Remote AI providers',
    remoteDesc:
      'Configure OpenAI-compatible endpoints, models and keys. Keys stay in the macOS Keychain; HTTP requests require an active Tailscale peer.',
    remoteAdd: 'Add provider',
    remoteRemove: 'Remove',
    remoteAvailable: 'Available',
    remoteUnavailable: 'Unavailable',
    remoteName: 'Name',
    remoteId: 'Provider ID',
    remoteBaseUrl: 'Base URL',
    remoteModelId: 'Exact model ID',
    remoteSecretName: 'Secret name',
    remotePiKey: 'Pi key ID (alternative)',
    remoteVision: 'Supports images and vision',
    remoteKey: 'API key',
    remoteKeySet: 'configured',
    remoteKeyMissing: 'missing',
    remoteKeyPlaceholder: 'Leave blank to keep the current key',
    remoteDefault: 'Default for chat and search',
    remoteVisionDefault: 'Vision provider',
    remoteLocal: 'Local',
    remoteNone: 'None',
    remoteSave: 'Save providers',
    remoteSaving: 'Saving…',
    remoteSaved: 'Providers saved.',
    remoteLoadError: 'Could not load providers.',
    remoteSaveError: 'Could not save providers.',
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

    // ── The web: an account on a server (src/views/settings/) ──────────────────
    pageSubtitleAccount: 'Your account, language, storage and legal notices.',
    pageSubtitleAccountAi: 'Your account, AI preferences, language, storage and legal notices.',
    sectionAccount: 'Account',
    sectionConnections: 'Connections',
    sectionStorage: 'Storage',
    versionPillWeb: 'server {server} · web {build}',
    versionTitleWeb: 'Server version {server} · web app build {build} (UTC)',

    // Account → profile
    profileTitle: 'Profile',
    emailLabel: 'Email',
    emailManaged: 'The sign-in address is managed by the server’s operator.',
    roleOwner: 'Owner',
    roleMember: 'Member',
    signOut: 'Sign out',
    signingOut: 'Signing out…',

    // Account → passkeys
    passkeysTitle: 'Passkeys',
    passkeysDesc:
      'Sign in without a password, with your device’s fingerprint, face or PIN. Passkeys stay on your devices: the server keeps only their public key.',
    passkeysEmpty: 'No passkeys yet. Add one to sign in without a link.',
    passkeyUnnamed: 'Unnamed passkey',
    passkeyAdded: 'Added {date}',
    passkeyLastUsed: 'last used {date}',
    passkeyNeverUsed: 'never used',
    passkeyAdd: 'Add a passkey',
    passkeyPreparing: 'Preparing…',
    passkeyLabel: 'Name',
    passkeyCreate: 'Create passkey',
    passkeyCreating: 'Waiting for your device…',
    passkeyCreated: 'Passkey added.',
    passkeysOff: 'Passkeys are off on this server.',
    passkeysUnsupported: 'This browser does not support passkeys: add them from another device.',

    // Account → sessions
    sessionsTitle: 'Sessions',
    sessionsDesc: 'The browsers where you are signed in. Sign out of any you don’t recognize.',
    sessionThisBrowser: 'This browser',
    sessionSignedIn: 'Signed in {date}',
    sessionLastSeen: 'active {date}',
    sessionEnd: 'Sign out',
    sessionsEndOthers: 'Sign out all other sessions',
    deviceOn: '{browser} on {os}',
    unknownDevice: 'Unknown device',

    // Account → API tokens
    tokensTitle: 'API tokens',
    tokensDesc:
      'They give the browser extension, the iOS Shortcut and the migration tool access to your library. Revoke the ones you don’t use.',
    tokensEmpty: 'No tokens.',
    tokenKind_extension: 'Browser extension',
    tokenKind_shortcut: 'iOS Shortcut',
    tokenKind_migrate: 'Migration tool',
    tokenKind_library: 'Library client',
    tokenCreated: 'Created {date}',
    tokenLastUsed: 'last used {date}',
    tokenNeverUsed: 'never used',
    tokenExpires: 'expires {date}',
    tokenExpiryLabel: 'Valid for',
    tokenExpiryDays: '{days} days',
    tokenRevoke: 'Revoke',
    tokenNew: 'New token',
    tokenKindLabel: 'For',
    tokenLabel: 'Name',
    tokenCreate: 'Create',
    tokenValueTitle: 'Copy the token now',
    tokenValueWarning:
      'This is the only time you’ll see it: the server keeps only its fingerprint. Anyone holding it can use your library, so don’t share it.',
    tokenCopy: 'Copy',
    tokenCopied: 'Copied',

    // Storage
    storageTitle: 'Space used',
    storageUsed: '{used} used',
    storageOfQuota: '{used} of {quota}',
    storageNoLimit: 'No limit',
    storageBarLabel: 'Space used by media and the database',
    storageMedia: 'Media',
    storageDatabase: 'Database',
    storageCounted: 'Counted {date}',
    storageCounting: 'Counting…',
    archiveTitle: 'What to archive',
    archiveDesc: 'Choose which files of your posts the server keeps.',
    archiveThumbnail: 'Covers',
    archiveThumbnailDesc: 'Post covers and video posters',
    archiveImage: 'Images',
    archiveImageDesc: 'The posts’ images, full size',
    archiveVideo: 'Videos',
    archiveVideoDesc: 'Videos, when you keep them',

    // Legal
    legalDescWeb:
      'The terms of using Shelfy: what it stores, and what you are responsible for in the content you save.',
    privacyTitle: 'Privacy notice',
    privacyDesc: 'What this server keeps, where, for how long, and who processes it.',
    privacyAccepted: 'Accepted on {date} · version {version}',
    privacyNotAccepted: 'Not yet accepted · current version {version}',
    privacyRead: 'Read the notice',
  },
} satisfies LangMessages;
