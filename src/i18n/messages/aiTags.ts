// UI strings for the Tags Explorer (AiTags) view: the coverage dashboard, the
// merge/rename modal, the cluster & alias review columns, the entity/health
// sections, the filter bar and result actions, plus the toasts surfaced from the
// view and the useAiTags hook. The Italian column reproduces the app's existing
// copy verbatim. AI-generated values (tag names, cluster labels, entity names,
// alias forms) come from the DB and are NOT translated — only the surrounding
// chrome is. Shared button labels (Riprova, Rimuovi, …) come from `common`.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // ── Dashboard ──
    title: 'Tags Explorer',
    uniqueTags: 'tag unici',
    taggedPosts: 'post taggati',
    coverage: 'Coverage',
    analyzedOf: '/ {total} analizzati',
    cancelAnalyze: 'Annulla analisi ({n})',
    cancelAnalyzeTitle:
      "Annulla TUTTA l'analisi in coda (tutti i job, non solo quelli avviati qui)",
    analyzeMissing: 'Analizza {n} non analizzati',

    // ── Merge / rename modal ──
    mergeTitle: 'Unisci / Rinomina tag',
    suggestions: 'Suggerimenti ({n})',
    noMergeSuggestions: 'Nessun suggerimento di merge.',
    totalPosts: '{n} post totali',
    merge: 'Unisci',
    manualRename: 'Rinomina manuale',
    renameFromPlaceholder: 'da',
    renameToPlaceholder: 'a',
    rename: 'Rinomina',

    // ── Empty / error states ──
    noPostsAnalyzed: 'Nessun post analizzato',
    emptyHint: "Avvia l'analisi AI per generare tag ed entità sui tuoi post.",
    analyzeUnanalyzed: 'Analizza non analizzati',

    // ── Clusters section ──
    clusters: 'Cluster',
    acceptAll: 'Accetta tutti ({n})',
    acceptAllClustersTitle: 'Accetta tutti i cluster proposti',
    stop: 'Interrompi',
    stopRegenTitle: 'Interrompi la rigenerazione',
    regenerate: 'Rigenera',
    regenerateTitle: "Rigenera i cluster con l'AI locale",
    manage: 'Gestisci',
    manageTitle: 'Unisci / rinomina tag',
    analyzingGroups: 'Analizzo gruppi {done}/{total}',
    noClusters: "Nessun cluster. Genera suggerimenti con l'AI.",
    regenerateClusters: 'Rigenera cluster',

    // ── Alias / synonym section ──
    aliases: 'Alias / Sinonimi',
    acceptAllAliasesTitle: 'Applica tutti gli alias proposti',
    stopAliasTitle: 'Interrompi la proposta di alias',
    generateProposals: 'Genera proposte',
    generateProposalsTitle: "Genera proposte di alias con l'AI locale",
    aliasHintPre: 'Gli alias sono ',
    aliasHintEm: 'proposte del modello locale',
    aliasHintPost:
      " e possono sbagliare: revisionali prima di applicarli (l'accettazione riscrive i tag dei post).",
    analyzingTags: 'Analizzo tag {done}/{total}',
    noAliases: "Nessun alias proposto. Genera suggerimenti con l'AI.",

    // ── Entity / health sections ──
    entities: 'Entità',
    noEntities: 'Nessuna entità',
    tagsToFix: 'Tag da sistemare',
    untagged: 'Senza tag',
    unanalyzed: 'Non analizzati',
    rareTags: 'Tag rari',
    orphans: 'Orfani',

    // ── Filter bar + results ──
    entityChip: 'entità: {name}',
    clearFilters: 'Pulisci',
    resultsCount: '{n} risultati',
    noFilter: 'Nessun filtro',
    related: 'Correlati',
    createCollection: 'Crea collection da questi',
    copyLinks: 'Copia link',
    exportMarkdown: 'Esporta Markdown',
    pickPrompt: "Seleziona un cluster, un tag o un'entità per esplorare i post.",
    noPostsForFilter: 'Nessun post per questo filtro.',
    switchToOr: 'AND su molti tag raramente trova risultati — passa a OR',

    // ── Cluster card ──
    clusterPostCount: '{n} post',
    clickToRename: 'Clicca per rinominare',
    filterClusterPosts: 'Filtra i post di questo cluster',
    removeFromCluster: 'Rimuovi dal cluster',
    accept: 'Accetta',
    dismiss: 'Scarta',
    preview: 'Anteprima',

    // ── Alias card ──
    aliasPostCount: '{n} post',

    // ── Toasts / dialogs ──
    queuedToAnalyze: 'In coda: {n} post da analizzare',
    analyzeStartError: 'Errore avvio analisi',
    mergedRefs: 'Uniti {n} riferimenti in "{target}"',
    mergeError: 'Errore durante il merge',
    renamedRefs: 'Rinominati {n} riferimenti: "{from}" → "{to}"',
    renameError: 'Errore durante la rinomina',
    proposedClusters: 'Proposti {count} cluster da {candidates} gruppi',
    regenCancelled: 'Rigenerazione annullata',
    modelNotReady: 'Modello non pronto',
    regenError: 'Errore durante la rigenerazione',
    confirmCancelAnalyze:
      "Annullare TUTTA l'analisi in coda? Verranno fermati tutti i job, non solo quelli avviati qui.",
    analyzeCancelled: 'Analisi annullata',
    cancelError: "Errore durante l'annullamento",
    clusterAccepted: 'Cluster accettato',
    clustersAccepted: 'Accettati {n} cluster',
    clusterDismissed: 'Cluster scartato',
    proposedAliases: 'Proposti {n} alias da rivedere',
    aliasProposalCancelled: 'Proposta alias annullata',
    aliasProposalError: 'Errore durante la proposta alias',
    aliasApplied: 'Alias applicato',
    aliasDismissed: 'Alias scartato',
    aliasesApplied: 'Applicati {n} alias',
    noLinksToCopy: 'Nessun link da copiare',
    linksCopied: 'Copiati {n} link',
    copyFailed: 'Impossibile copiare negli appunti',
    noResultsToExport: 'Nessun risultato da esportare',
    exportedToMarkdown: 'Esportati {n} post in Markdown',
    noPostsToCollect: 'Nessun post da raccogliere',
    newCollectionPrompt: 'Nome della nuova collection:',
    collectionCreateError: 'Errore creazione collection',
    collectionCreatedWith: 'Collection "{name}" creata con {n} post',
    collectionCreated: 'Collection "{name}" creata',
    defaultCollectionName: 'Nuova collection',

    // ── useAiTags hook ──
    loadError: 'Impossibile caricare i dati AI',
  },
  en: {
    // ── Dashboard ──
    title: 'Tags Explorer',
    uniqueTags: 'unique tags',
    taggedPosts: 'tagged posts',
    coverage: 'Coverage',
    analyzedOf: '/ {total} analyzed',
    cancelAnalyze: 'Cancel analysis ({n})',
    cancelAnalyzeTitle:
      'Cancel the ENTIRE queued analysis (all jobs, not just the ones started here)',
    analyzeMissing: 'Analyze {n} unanalyzed',

    // ── Merge / rename modal ──
    mergeTitle: 'Merge / Rename tags',
    suggestions: 'Suggestions ({n})',
    noMergeSuggestions: 'No merge suggestions.',
    totalPosts: '{n} total posts',
    merge: 'Merge',
    manualRename: 'Manual rename',
    renameFromPlaceholder: 'from',
    renameToPlaceholder: 'to',
    rename: 'Rename',

    // ── Empty / error states ──
    noPostsAnalyzed: 'No posts analyzed',
    emptyHint: 'Start the AI analysis to generate tags and entities for your posts.',
    analyzeUnanalyzed: 'Analyze unanalyzed',

    // ── Clusters section ──
    clusters: 'Clusters',
    acceptAll: 'Accept all ({n})',
    acceptAllClustersTitle: 'Accept all proposed clusters',
    stop: 'Stop',
    stopRegenTitle: 'Stop the regeneration',
    regenerate: 'Regenerate',
    regenerateTitle: 'Regenerate clusters with the local AI',
    manage: 'Manage',
    manageTitle: 'Merge / rename tags',
    analyzingGroups: 'Analyzing groups {done}/{total}',
    noClusters: 'No clusters. Generate suggestions with AI.',
    regenerateClusters: 'Regenerate clusters',

    // ── Alias / synonym section ──
    aliases: 'Aliases / Synonyms',
    acceptAllAliasesTitle: 'Apply all proposed aliases',
    stopAliasTitle: 'Stop the alias proposal',
    generateProposals: 'Generate proposals',
    generateProposalsTitle: 'Generate alias proposals with the local AI',
    aliasHintPre: 'Aliases are ',
    aliasHintEm: 'local-model proposals',
    aliasHintPost:
      ' and can be wrong: review them before applying (accepting rewrites the posts’ tags).',
    analyzingTags: 'Analyzing tags {done}/{total}',
    noAliases: 'No proposed aliases. Generate suggestions with AI.',

    // ── Entity / health sections ──
    entities: 'Entities',
    noEntities: 'No entities',
    tagsToFix: 'Tags to fix',
    untagged: 'Untagged',
    unanalyzed: 'Unanalyzed',
    rareTags: 'Rare tags',
    orphans: 'Orphans',

    // ── Filter bar + results ──
    entityChip: 'entity: {name}',
    clearFilters: 'Clear',
    resultsCount: '{n} results',
    noFilter: 'No filter',
    related: 'Related',
    createCollection: 'Create collection from these',
    copyLinks: 'Copy links',
    exportMarkdown: 'Export Markdown',
    pickPrompt: 'Select a cluster, a tag or an entity to explore the posts.',
    noPostsForFilter: 'No posts for this filter.',
    switchToOr: 'AND across many tags rarely matches — switch to OR',

    // ── Cluster card ──
    clusterPostCount: '{n} posts',
    clickToRename: 'Click to rename',
    filterClusterPosts: 'Filter the posts in this cluster',
    removeFromCluster: 'Remove from cluster',
    accept: 'Accept',
    dismiss: 'Dismiss',
    preview: 'Preview',

    // ── Alias card ──
    aliasPostCount: '{n} posts',

    // ── Toasts / dialogs ──
    queuedToAnalyze: 'Queued: {n} posts to analyze',
    analyzeStartError: 'Error starting analysis',
    mergedRefs: 'Merged {n} references into "{target}"',
    mergeError: 'Error during merge',
    renamedRefs: 'Renamed {n} references: "{from}" → "{to}"',
    renameError: 'Error during rename',
    proposedClusters: 'Proposed {count} clusters from {candidates} groups',
    regenCancelled: 'Regeneration cancelled',
    modelNotReady: 'Model not ready',
    regenError: 'Error during regeneration',
    confirmCancelAnalyze:
      'Cancel the ENTIRE queued analysis? All jobs will be stopped, not just the ones started here.',
    analyzeCancelled: 'Analysis cancelled',
    cancelError: 'Error during cancellation',
    clusterAccepted: 'Cluster accepted',
    clustersAccepted: 'Accepted {n} clusters',
    clusterDismissed: 'Cluster dismissed',
    proposedAliases: 'Proposed {n} aliases to review',
    aliasProposalCancelled: 'Alias proposal cancelled',
    aliasProposalError: 'Error during alias proposal',
    aliasApplied: 'Alias applied',
    aliasDismissed: 'Alias dismissed',
    aliasesApplied: 'Applied {n} aliases',
    noLinksToCopy: 'No links to copy',
    linksCopied: 'Copied {n} links',
    copyFailed: 'Could not copy to the clipboard',
    noResultsToExport: 'No results to export',
    exportedToMarkdown: 'Exported {n} posts to Markdown',
    noPostsToCollect: 'No posts to collect',
    newCollectionPrompt: 'Name of the new collection:',
    collectionCreateError: 'Error creating collection',
    collectionCreatedWith: 'Collection "{name}" created with {n} posts',
    collectionCreated: 'Collection "{name}" created',
    defaultCollectionName: 'New collection',

    // ── useAiTags hook ──
    loadError: 'Could not load AI data',
  },
} satisfies LangMessages;
