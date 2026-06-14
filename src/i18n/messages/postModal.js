// UI chrome for the post detail dialog (PostModal.jsx): media controls, web
// metadata panel, AI categorization panel, user note / manual tags, collection
// assignment, download/delete actions, tooltips and empty/error states.
//
// NOTE: AI-generated values (description, tags, entities, keywords, save-reason
// TEXT) and the captured content itself are rendered as-is and are NOT
// translated here — only the surrounding labels are.
export default {
  it: {
    // ── Media-type labels (the kind of asset) ──────────────────────────────
    mediaImage: 'Immagine',
    mediaImages: 'Immagini',
    mediaCarousel: 'Carosello',
    mediaVideo: 'Video',
    mediaText: 'Testo',
    mediaWebsite: 'Sito web',
    mediaFile: 'File',

    // ── Palette swatch ─────────────────────────────────────────────────────
    copyHex: 'Copia {hex}',
    copyColor: 'Copia colore {hex}',
    copied: 'Copiato',

    // ── Web metadata panel ─────────────────────────────────────────────────
    siteMetadata: 'Metadati sito',
    openSite: 'Apri sito',
    openInWebsites: 'Apri in Website Analyzer',
    openInWebsitesTitle:
      'Mostra questo sito nel pannello Website Analyzer (cattura + analisi in tempo reale)',
    reanalyze: 'Rianalizza',
    reanalyzeTitle: 'Ricattura e rianalizza questo sito nel pannello Website Analyzer',
    palette: 'Palette',
    fonts: 'Font',
    technologies: 'Tecnologie',
    awards: 'Riconoscimenti',
    award: 'Award',
    openAwardProfile: 'Apri profilo award',

    // ── Web page labels ────────────────────────────────────────────────────
    home: 'Home',
    page: 'Pagina {n}',

    // ── AI analyze status ──────────────────────────────────────────────────
    statusPending: 'In coda…',
    statusExtracting: 'Estrazione fotogrammi…',
    statusAnalyzing: 'Analisi del video…',
    processing: 'Elaborazione…',

    // ── AI section ─────────────────────────────────────────────────────────
    aiSection: 'Categorizzazione AI',

    // ── User layer (manual tags + note) ────────────────────────────────────
    yourTags: 'I tuoi tag',
    filterByTag: 'Filtra per questo tag',
    removeTag: 'Rimuovi tag',
    addTagShort: 'Aggiungi…',
    addYourTag: 'Aggiungi un tuo tag…',
    note: 'Nota',
    writeNote: 'Scrivi una nota personale…',
    editNote: 'Modifica nota',
    addNote: 'Aggiungi una nota personale',

    // ── Model download ─────────────────────────────────────────────────────
    downloadingModel: 'Scarico il modello AI ({percent}%)…',

    // ── AI edit form ───────────────────────────────────────────────────────
    description: 'Descrizione',
    tags: 'Tag',
    addTag: 'Aggiungi tag…',
    whySave: 'Perché salvarlo',

    // ── AI result actions ──────────────────────────────────────────────────
    editAi: 'Modifica categorizzazione AI',
    moreActions: 'Altre azioni',
    deleteDescription: 'Elimina descrizione',
    confirmDeleteDescription: 'Conferma: elimina descrizione',
    removeAiTags: 'Rimuovi tag AI',
    confirmRemoveAiTags: 'Conferma: rimuovi tag AI',
    entities: 'Entità',
    searchAlso: 'Cerca anche: ',
    whySavePrefix: 'Perché salvarlo: ',
    regenerate: 'Rigenera analisi',

    // ── AI analyze (initial) ───────────────────────────────────────────────
    errorPrefix: 'Errore: {error}',
    analyzeVideo: 'Analizza video',
    downloadModelAndAnalyze: 'Scarica modello AI (~3 GB) e analizza',

    // ── Close-with-unsaved-edits confirm ───────────────────────────────────
    unsavedConfirm: 'Hai modifiche non salvate alla categorizzazione AI. Chiudere e perderle?',

    // ── Header ─────────────────────────────────────────────────────────────
    website: 'Sito web',
    manualBookmark: 'Bookmark',
    platformX: 'X / Twitter',
    unknownAuthor: 'unknown',
    post: 'Post',
    viewingLocal: 'Stai vedendo il file scaricato in locale',
    local: 'Locale',

    // ── Add to source (collection) ─────────────────────────────────────────
    addToSource: 'Aggiungi a una source',
    addTo: 'Aggiungi a',
    noSources: 'Nessuna source. Creane una qui sotto.',
    createNewSource: 'Crea nuova source',

    // ── Actions menu ───────────────────────────────────────────────────────
    openFile: 'Apri file',
    openOriginal: 'Apri originale',
    noDownloadableFiles: 'Questo post non ha file scaricabili',
    downloadFilesTitle: 'Scarica i file di questo post in locale',
    nothingToDownload: 'Niente da scaricare',
    downloadFailed: 'Download non riuscito',
    actionFailed: 'Operazione non riuscita, riprova',
    queued: 'In coda…',
    downloadLocal: 'Scarica in locale',
    clickAgainToConfirm: 'Clicca di nuovo per confermare',
    deleteLocalFilesTitle: 'Elimina i file locali (mantiene il record nel DB)',
    confirmDeletion: 'Conferma eliminazione',
    deleteLocalFiles: 'Elimina file locali',
    deletePostTitle: 'Elimina il post dalla libreria (record + file)',
    confirmDeletePost: 'Conferma: elimina post',
    deletePost: 'Elimina post',
    close: 'Chiudi',

    // ── Navigation ─────────────────────────────────────────────────────────
    prevPost: 'Post precedente',
    nextPost: 'Post successivo',

    // ── Media / slides ─────────────────────────────────────────────────────
    zoomFullscreen: 'Ingrandisci a tutto schermo',
    clickToZoomFullscreen: 'Clic per ingrandire a tutto schermo',
    zoom: 'Ingrandisci',
    clickToZoom: 'Clic per ingrandire',
    prevPage: 'Pagina precedente',
    nextPage: 'Pagina successiva',
    goToPage: 'Vai alla pagina {n}',
    prevImage: 'Immagine precedente',
    nextImage: 'Immagine successiva',
    goToImage: 'Vai all’immagine {n}',

    // ── Facts byline ───────────────────────────────────────────────────────
    pages: 'pagine',
    items: 'elementi',
    openLocal: 'Apri {label} in locale',
    assetThumbnail: 'thumbnail',
    assetImage: 'immagine',
    assetVideo: 'video',
  },
  en: {
    // ── Media-type labels ──────────────────────────────────────────────────
    mediaImage: 'Image',
    mediaImages: 'Images',
    mediaCarousel: 'Carousel',
    mediaVideo: 'Video',
    mediaText: 'Text',
    mediaWebsite: 'Website',
    mediaFile: 'File',

    // ── Palette swatch ─────────────────────────────────────────────────────
    copyHex: 'Copy {hex}',
    copyColor: 'Copy color {hex}',
    copied: 'Copied',

    // ── Web metadata panel ─────────────────────────────────────────────────
    siteMetadata: 'Site metadata',
    openSite: 'Open site',
    openInWebsites: 'Open in Website Analyzer',
    openInWebsitesTitle: 'Show this site in the Website Analyzer panel (live capture + analysis)',
    reanalyze: 'Reanalyze',
    reanalyzeTitle: 'Recapture and reanalyze this site in the Website Analyzer panel',
    palette: 'Palette',
    fonts: 'Fonts',
    technologies: 'Technologies',
    awards: 'Awards',
    award: 'Award',
    openAwardProfile: 'Open award profile',

    // ── Web page labels ────────────────────────────────────────────────────
    home: 'Home',
    page: 'Page {n}',

    // ── AI analyze status ──────────────────────────────────────────────────
    statusPending: 'Queued…',
    statusExtracting: 'Extracting frames…',
    statusAnalyzing: 'Analyzing video…',
    processing: 'Processing…',

    // ── AI section ─────────────────────────────────────────────────────────
    aiSection: 'AI categorization',

    // ── User layer (manual tags + note) ────────────────────────────────────
    yourTags: 'Your tags',
    filterByTag: 'Filter by this tag',
    removeTag: 'Remove tag',
    addTagShort: 'Add…',
    addYourTag: 'Add your own tag…',
    note: 'Note',
    writeNote: 'Write a personal note…',
    editNote: 'Edit note',
    addNote: 'Add a personal note',

    // ── Model download ─────────────────────────────────────────────────────
    downloadingModel: 'Downloading the AI model ({percent}%)…',

    // ── AI edit form ───────────────────────────────────────────────────────
    description: 'Description',
    tags: 'Tags',
    addTag: 'Add tag…',
    whySave: 'Why save it',

    // ── AI result actions ──────────────────────────────────────────────────
    editAi: 'Edit AI categorization',
    moreActions: 'More actions',
    deleteDescription: 'Delete description',
    confirmDeleteDescription: 'Confirm: delete description',
    removeAiTags: 'Remove AI tags',
    confirmRemoveAiTags: 'Confirm: remove AI tags',
    entities: 'Entities',
    searchAlso: 'Search also: ',
    whySavePrefix: 'Why save it: ',
    regenerate: 'Regenerate analysis',

    // ── AI analyze (initial) ───────────────────────────────────────────────
    errorPrefix: 'Error: {error}',
    analyzeVideo: 'Analyze video',
    downloadModelAndAnalyze: 'Download AI model (~3 GB) and analyze',

    // ── Close-with-unsaved-edits confirm ───────────────────────────────────
    unsavedConfirm: 'You have unsaved changes to the AI categorization. Close and lose them?',

    // ── Header ─────────────────────────────────────────────────────────────
    website: 'Website',
    manualBookmark: 'Bookmark',
    platformX: 'X / Twitter',
    unknownAuthor: 'unknown',
    post: 'Post',
    viewingLocal: 'You are viewing the locally downloaded file',
    local: 'Local',

    // ── Add to source (collection) ─────────────────────────────────────────
    addToSource: 'Add to a source',
    addTo: 'Add to',
    noSources: 'No sources. Create one below.',
    createNewSource: 'Create new source',

    // ── Actions menu ───────────────────────────────────────────────────────
    openFile: 'Open file',
    openOriginal: 'Open original',
    noDownloadableFiles: 'This post has no downloadable files',
    downloadFilesTitle: 'Download this post’s files locally',
    nothingToDownload: 'Nothing to download',
    downloadFailed: 'Download failed',
    actionFailed: 'Action failed, try again',
    queued: 'Queued…',
    downloadLocal: 'Download locally',
    clickAgainToConfirm: 'Click again to confirm',
    deleteLocalFilesTitle: 'Delete the local files (keeps the DB record)',
    confirmDeletion: 'Confirm deletion',
    deleteLocalFiles: 'Delete local files',
    deletePostTitle: 'Delete the post from the library (record + files)',
    confirmDeletePost: 'Confirm: delete post',
    deletePost: 'Delete post',
    close: 'Close',

    // ── Navigation ─────────────────────────────────────────────────────────
    prevPost: 'Previous post',
    nextPost: 'Next post',

    // ── Media / slides ─────────────────────────────────────────────────────
    zoomFullscreen: 'Zoom to full screen',
    clickToZoomFullscreen: 'Click to zoom to full screen',
    zoom: 'Zoom',
    clickToZoom: 'Click to zoom',
    prevPage: 'Previous page',
    nextPage: 'Next page',
    goToPage: 'Go to page {n}',
    prevImage: 'Previous image',
    nextImage: 'Next image',
    goToImage: 'Go to image {n}',

    // ── Facts byline ───────────────────────────────────────────────────────
    pages: 'pages',
    items: 'items',
    openLocal: 'Open {label} locally',
    assetThumbnail: 'thumbnail',
    assetImage: 'image',
    assetVideo: 'video',
  },
};
