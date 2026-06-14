// UI strings for the Websites view (the web-reference capture pipeline panel):
// the status/phase vocabulary, the phase steppers (capture + AI), per-job queue
// chrome, the live timeline, the detected-data sections (screenshots, palette,
// fonts, tech stack, awards), the AI analysis band, the version/snapshot bar,
// the header counters + toolbar, the search/archive strip, the multi-select
// delete flow and the empty/no-results states. The Italian column reproduces
// the app's existing copy verbatim. Extracted website content + URLs are DATA
// and are NOT translated; brand/product names ("Website Analyzer") stay as-is;
// shared buttons (Annulla, Riprova, Elimina, …) come from the `common`
// namespace.
export default {
  it: {
    // ── Status labels (queue row stage fallback) ──
    statusPending: 'In coda',
    statusQueued: 'In coda',
    statusDiscovering: 'Discovery',
    statusCapturing: 'Cattura',
    statusExtracting: 'Estrazione',
    statusAnalyzing: 'Analisi AI',
    statusDone: 'Completato',
    statusError: 'Errore',
    statusCancelled: 'Annullato',

    // ── Capture phase stepper ──
    phaseQueue: 'Coda',
    phaseDiscovery: 'Discovery',
    phaseCapture: 'Cattura',
    phaseExtraction: 'Estrazione',
    phaseAnalysis: 'Analisi',
    phaseDone: 'Fatto',

    // ── AI phase stepper ──
    aiPhaseQueue: 'Coda',
    aiPhaseExtraction: 'Estrazione',
    aiPhaseAnalysis: 'Analisi',
    aiPhaseDescription: 'Descrizione',
    aiPhaseTags: 'Tag',
    aiPhaseDone: 'Fatto',

    // ── AI analysis band ──
    aiTitle: 'Analisi AI',
    aiModelNotReady: 'Modello AI non pronto — scaricalo dalle Impostazioni.',
    aiQueuedForAnalysis: 'In coda per l’analisi…',
    aiStartsAfterCapture: 'Parte al termine della cattura.',
    aiDescription: 'Descrizione',
    aiTags: 'Tag',
    aiEntities: 'Entità',
    aiSearchAlso: 'Cerca anche: ',

    // ── Analysis meta (archived) ──
    analysisTitle: 'Analisi',
    metaPurpose: 'Scopo',
    metaSector: 'Settore',
    metaLanguage: 'Lingua',
    whySave: 'Perché salvarlo: ',

    // ── Timeline ──
    timelineEmpty: 'Nessun evento ancora. Il dettaglio comparirà appena parte la scansione.',
    behindTheScenes: 'Dietro le quinte',

    // ── Detected-data sections ──
    sectionScreenshots: 'Screenshot',
    screenshotsEmpty: 'Nessuno screenshot ancora.',
    screenshotZoomTitle: '{url} — clic per ingrandire',
    sectionPalette: 'Palette',
    sectionFonts: 'Tipografia',
    sectionTech: 'Tech stack',
    sectionAwards: 'Riconoscimenti',

    // ── Version bar ──
    versions: 'Versioni',
    versionCurrentTitle: 'Versione corrente',
    versionArchivedTitle: 'Versione archiviata',
    versionCurrent: 'Corrente',
    versionDeleteTitle: 'Elimina questa versione',

    // ── Detail header ──
    openReference: 'Apri reference',
    captureDonePartial: 'Cattura completata (alcune pagine saltate)',
    captureDone: 'Cattura completata',
    captureDoneAiContinues: ' — l’analisi AI prosegue qui sotto.',

    // ── Queue row ──
    snapshotVersions: '{n} versioni',

    // ── Header ──
    headerTitle: 'Website Analyzer',
    headerQueued: '{n} in coda',
    headerArchiveTitle: 'Siti analizzati in archivio',
    addSite: 'Aggiungi sito',
    selectMultipleTitle: 'Seleziona più siti per eliminarli',
    selectAction: 'Seleziona',
    clear: 'Pulisci',
    clearTitle: 'Rimuovi dalla lista i job terminati',

    // ── Search strip ──
    searchPlaceholder: 'Cerca tra i siti analizzati…',
    clearSearchTitle: 'Cancella ricerca',
    siteOne: 'sito',
    siteOther: 'siti',

    // ── Selection toolbar ──
    siteSelectedOne: '{n} sito selezionato',
    siteSelectedOther: '{n} siti selezionati',
    shiftRangeHint: 'Shift+clic per selezionare un intervallo',

    // ── Empty / no-results states ──
    emptyTitle: 'Nessun sito analizzato',
    emptyHint:
      'Aggiungi un sito come reference: ne cattureremo gli screenshot, la palette, i font, lo stack tecnologico e i riconoscimenti — vedrai qui ogni passaggio in tempo reale, e l’analisi resterà in archivio.',
    noResults: 'Nessun sito per «{query}».',
    selectSitePrompt: 'Seleziona un sito dall’archivio per vedere il dettaglio.',

    // ── Delete dialog ──
    deleteTitleOne: 'Eliminare {n} sito?',
    deleteTitleOther: 'Eliminare {n} siti?',
    deleteSubtitle: 'Scegli cosa rimuovere. L’operazione non è reversibile.',
    deleteReportTitle: 'Solo il report',
    deleteReportHint: 'Rimuove l’ultima analisi; il sito e le versioni precedenti restano.',
    deleteCompleteTitle: 'Elimina completo',
    deleteCompleteHint: 'Rimuove il sito, tutte le versioni e i file dal disco.',
  },
  en: {
    // ── Status labels ──
    statusPending: 'Queued',
    statusQueued: 'Queued',
    statusDiscovering: 'Discovery',
    statusCapturing: 'Capture',
    statusExtracting: 'Extraction',
    statusAnalyzing: 'AI analysis',
    statusDone: 'Completed',
    statusError: 'Error',
    statusCancelled: 'Cancelled',

    // ── Capture phase stepper ──
    phaseQueue: 'Queue',
    phaseDiscovery: 'Discovery',
    phaseCapture: 'Capture',
    phaseExtraction: 'Extraction',
    phaseAnalysis: 'Analysis',
    phaseDone: 'Done',

    // ── AI phase stepper ──
    aiPhaseQueue: 'Queue',
    aiPhaseExtraction: 'Extraction',
    aiPhaseAnalysis: 'Analysis',
    aiPhaseDescription: 'Description',
    aiPhaseTags: 'Tags',
    aiPhaseDone: 'Done',

    // ── AI analysis band ──
    aiTitle: 'AI analysis',
    aiModelNotReady: 'AI model not ready — download it from Settings.',
    aiQueuedForAnalysis: 'Queued for analysis…',
    aiStartsAfterCapture: 'Starts when the capture finishes.',
    aiDescription: 'Description',
    aiTags: 'Tags',
    aiEntities: 'Entities',
    aiSearchAlso: 'Search also: ',

    // ── Analysis meta (archived) ──
    analysisTitle: 'Analysis',
    metaPurpose: 'Purpose',
    metaSector: 'Sector',
    metaLanguage: 'Language',
    whySave: 'Why save it: ',

    // ── Timeline ──
    timelineEmpty: 'No events yet. The detail will appear as soon as the scan starts.',
    behindTheScenes: 'Behind the scenes',

    // ── Detected-data sections ──
    sectionScreenshots: 'Screenshots',
    screenshotsEmpty: 'No screenshots yet.',
    screenshotZoomTitle: '{url} — click to enlarge',
    sectionPalette: 'Palette',
    sectionFonts: 'Typography',
    sectionTech: 'Tech stack',
    sectionAwards: 'Awards',

    // ── Version bar ──
    versions: 'Versions',
    versionCurrentTitle: 'Current version',
    versionArchivedTitle: 'Archived version',
    versionCurrent: 'Current',
    versionDeleteTitle: 'Delete this version',

    // ── Detail header ──
    openReference: 'Open reference',
    captureDonePartial: 'Capture complete (some pages skipped)',
    captureDone: 'Capture complete',
    captureDoneAiContinues: ' — the AI analysis continues below.',

    // ── Queue row ──
    snapshotVersions: '{n} versions',

    // ── Header ──
    headerTitle: 'Website Analyzer',
    headerQueued: '{n} queued',
    headerArchiveTitle: 'Analysed sites in the archive',
    addSite: 'Add website',
    selectMultipleTitle: 'Select multiple sites to delete them',
    selectAction: 'Select',
    clear: 'Clear',
    clearTitle: 'Remove finished jobs from the list',

    // ── Search strip ──
    searchPlaceholder: 'Search analysed sites…',
    clearSearchTitle: 'Clear search',
    siteOne: 'site',
    siteOther: 'sites',

    // ── Selection toolbar ──
    siteSelectedOne: '{n} site selected',
    siteSelectedOther: '{n} sites selected',
    shiftRangeHint: 'Shift+click to select a range',

    // ── Empty / no-results states ──
    emptyTitle: 'No sites analysed',
    emptyHint:
      'Add a website as a reference: we’ll capture its screenshots, palette, fonts, tech stack and awards — you’ll see every step here in real time, and the analysis will stay in the archive.',
    noResults: 'No sites for «{query}».',
    selectSitePrompt: 'Select a site from the archive to see the detail.',

    // ── Delete dialog ──
    deleteTitleOne: 'Delete {n} site?',
    deleteTitleOther: 'Delete {n} sites?',
    deleteSubtitle: 'Choose what to remove. This action is not reversible.',
    deleteReportTitle: 'Report only',
    deleteReportHint: 'Removes the latest analysis; the site and previous versions remain.',
    deleteCompleteTitle: 'Delete completely',
    deleteCompleteHint: 'Removes the site, all versions and the files from disk.',
  },
};
