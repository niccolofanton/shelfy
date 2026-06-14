// UI strings for the first-run AI onboarding wizard (AiOnboarding): the gate that
// covers the AI tabs until the local pipeline is fully configured. Hero copy,
// detected-hardware chips, the four install items (analysis model, voice model,
// AI engine, tag clustering), the one-click install flow with per-item progress,
// the success screen and the footer escape hatches. Brand/tech names (Whisper,
// llama.cpp, FFmpeg, Metal, CUDA…) and model names are NOT translated.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // ── Hero ──
    title: 'Configura la tua AI locale',
    subtitle:
      'Shelfy analizza immagini, video e voce direttamente sul tuo computer. Bastano pochi minuti: scegli il modello, al resto pensiamo noi.',
    privacyNote: '100% locale · nessun dato lascia il tuo computer',

    // ── Hardware ──
    hwTitle: 'Il tuo computer',
    hwDetecting: 'Rilevamento hardware…',
    hwCpu: 'CPU',
    hwRam: 'RAM',
    hwGpu: 'GPU',
    hwAccel: 'Accelerazione',
    coresUnit: '{n} core',
    gbUnit: '{n} GB',

    // ── Step 1: analysis model ──
    stepModelTitle: 'Modello di analisi',
    stepModelDesc: 'Guarda i tuoi contenuti e genera descrizioni e tag per la ricerca.',
    recommendedForYou: 'Consigliato per il tuo computer',
    showAlternatives: 'Mostra altri modelli ({n})',
    hideAlternatives: 'Nascondi gli altri modelli',
    ramRequirement: '{n} GB RAM',
    ramWarning: 'Oltre la memoria di questo computer',
    alreadyInstalledModel: '{name} è già installato.',

    // ── Step 2: voice model ──
    stepVoiceTitle: 'Dettatura vocale',
    stepVoiceDesc: 'Trascrive la tua voce per cercare parlando, senza digitare.',

    // ── Step 3: engine ──
    stepEngineTitle: 'Motore AI',
    stepEngineDesc: 'I componenti che eseguono i modelli in locale, ottimizzati per {accel}.',

    // ── Step 4: tag clustering ──
    stepEmbTitle: 'Raggruppamento dei tag',
    stepEmbDesc: 'Un modello leggero che riunisce automaticamente i tag simili.',

    // ── Item states ──
    statusInstalled: 'Installato',
    statusToInstall: 'Da scaricare',
    statusWaiting: 'In attesa',
    statusDownloading: '{pct}%',
    statusExtracting: 'Installazione…',
    statusError: 'Errore',

    // ── CTA / install flow ──
    install: 'Scarica e configura tutto',
    installWithSize: 'Scarica e configura tutto · ≈ {size} GB',
    resumeInstall: "Riprendi l'installazione",
    retry: 'Riprova',
    installing: 'Installazione in corso…',
    backgroundNote: "Puoi continuare a usare l'app: i download proseguono in background.",
    changeLater: 'Potrai cambiare i modelli in ogni momento dalle Impostazioni.',
    installError: 'Qualcosa è andato storto: {error}',

    // ── Success ──
    doneTitle: 'Tutto pronto!',
    doneSubtitle: 'La tua AI è configurata e gira interamente sul tuo computer.',
    startAnalyzing: 'Analizza i miei contenuti',
    later: 'Più tardi',

    // ── Footer ──
    skip: 'Salta per ora',
    advancedSettings: 'Impostazioni avanzate',
  },
  en: {
    // ── Hero ──
    title: 'Set up your local AI',
    subtitle:
      'Shelfy analyzes images, video and voice right on your computer. It only takes a few minutes: pick the model, we handle the rest.',
    privacyNote: '100% local · no data ever leaves your computer',

    // ── Hardware ──
    hwTitle: 'Your computer',
    hwDetecting: 'Detecting hardware…',
    hwCpu: 'CPU',
    hwRam: 'RAM',
    hwGpu: 'GPU',
    hwAccel: 'Acceleration',
    coresUnit: '{n} cores',
    gbUnit: '{n} GB',

    // ── Step 1: analysis model ──
    stepModelTitle: 'Analysis model',
    stepModelDesc: 'Looks at your content and generates descriptions and tags for search.',
    recommendedForYou: 'Recommended for your computer',
    showAlternatives: 'Show other models ({n})',
    hideAlternatives: 'Hide the other models',
    ramRequirement: '{n} GB RAM',
    ramWarning: 'Exceeds this computer’s memory',
    alreadyInstalledModel: '{name} is already installed.',

    // ── Step 2: voice model ──
    stepVoiceTitle: 'Voice dictation',
    stepVoiceDesc: 'Transcribes your voice so you can search by speaking, not typing.',

    // ── Step 3: engine ──
    stepEngineTitle: 'AI engine',
    stepEngineDesc: 'The components that run the models locally, optimized for {accel}.',

    // ── Step 4: tag clustering ──
    stepEmbTitle: 'Tag clustering',
    stepEmbDesc: 'A lightweight model that automatically groups similar tags.',

    // ── Item states ──
    statusInstalled: 'Installed',
    statusToInstall: 'To download',
    statusWaiting: 'Waiting',
    statusDownloading: '{pct}%',
    statusExtracting: 'Installing…',
    statusError: 'Error',

    // ── CTA / install flow ──
    install: 'Download and set up everything',
    installWithSize: 'Download and set up everything · ≈ {size} GB',
    resumeInstall: 'Resume installation',
    retry: 'Retry',
    installing: 'Installing…',
    backgroundNote: 'You can keep using the app: downloads continue in the background.',
    changeLater: 'You can change the models at any time from Settings.',
    installError: 'Something went wrong: {error}',

    // ── Success ──
    doneTitle: 'All set!',
    doneSubtitle: 'Your AI is configured and runs entirely on your computer.',
    startAnalyzing: 'Analyze my content',
    later: 'Later',

    // ── Footer ──
    skip: 'Skip for now',
    advancedSettings: 'Advanced settings',
  },
} satisfies LangMessages;
