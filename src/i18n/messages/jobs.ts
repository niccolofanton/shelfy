// Strings for the Jobs view (P4-09, web-only — replaces the desktop's
// Downloads there, PG18): header, queue controls, per-row state, and the
// label dictionaries for job kinds and job error codes. `common.ts` supplies
// the generic verbs (cancel, retry, pause, resume); this namespace only
// carries jobs-specific wording.
//
// `kind.*` and `error.*` are looked up by src/views/jobs/labels.ts, which
// falls back to a prettified raw code for a kind or an error this build does
// not label yet (a newer server, or a kind P2/P3 register later) — see the
// card's "unknown kinds get a generic label" acceptance bullet. `kind.*`
// lists every kind the job-kind registry documents (crates/server/src/jobs/kinds.rs),
// including ones no lane has implemented yet, so the label is ready the day
// the kind starts running.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Lavori',
    empty: 'Nessun lavoro recente.',
    emptyFiltered: 'Nessun lavoro corrisponde ai filtri scelti.',
    refresh: 'Aggiorna',
    loadMore: 'Carica altri',
    clearFilters: 'Rimuovi filtri',
    stateFilterLabel: 'Stato',
    kindFilterLabel: 'Tipo',
    postLink: 'Apri il post',
    tries: '{attempts}/{max} tentativi',
    cancelJob: 'Annulla questo lavoro',
    retryJob: 'Riprova questo lavoro',
    queuePausedBadge: 'in pausa',
    pauseQueueTitle: 'Metti in pausa questo tipo di lavoro',
    resumeQueueTitle: 'Riprendi questo tipo di lavoro',
    cancelAllTitle: 'Annulla tutti i lavori in coda o in corso di questo tipo',
    clearFinishedTitle:
      'Rimuovi dalla lista i lavori completati, falliti e annullati di questo tipo',
    // Per-job state (also the state filter's chip labels).
    'state.queued': 'In coda',
    'state.running': 'In corso',
    'state.succeeded': 'Completato',
    'state.failed': 'Fallito',
    'state.cancelled': 'Annullato',
    // Job kinds (crates/server/src/jobs/kinds.rs).
    'kind.bulk': 'Azione sui post',
    'kind.purge': 'Svuotamento cestino',
    'kind.usage.recompute': 'Calcolo spazio usato',
    'kind.migrate': 'Migrazione libreria',
    'kind.archive.drain': 'Archiviazione media',
    'kind.link.hydrate': 'Recupero link',
    'kind.ai.drain': 'Catalogazione AI',
    'kind.ai.run': 'Analisi AI',
    'kind.media.video': 'Recupero video',
    'kind.capture.site': 'Cattura sito',
    'kind.import': 'Importazione',
    'kind.export': 'Esportazione',
    'kind.gc': 'Pulizia spazio',
    // Job error codes (crates/server/src/jobs/context.rs `codes`, plus the
    // repository errors a job can surface verbatim).
    'error.lease_expired': 'Ha impiegato troppo tempo ed è stato rimesso in coda.',
    'error.internal': 'Errore interno del server.',
    'error.unavailable': 'Un servizio necessario non era disponibile.',
    'error.invalid_payload': 'I dati del lavoro non erano validi.',
    'error.cancelled': 'Annullato.',
    'error.user_locked': 'La libreria era in manutenzione.',
    'error.not_found': 'Non esiste più.',
    'error.conflict': 'È in conflitto con un’altra modifica.',
    'error.validation_failed': 'Alcuni valori non erano validi.',
    'error.invalid_cursor': 'La lista è cambiata durante il caricamento.',
    'error.quota_exceeded': 'Lo spazio di archiviazione era esaurito.',
  },
  en: {
    title: 'Jobs',
    empty: 'No recent jobs.',
    emptyFiltered: 'No job matches the chosen filters.',
    refresh: 'Refresh',
    loadMore: 'Load more',
    clearFilters: 'Clear filters',
    stateFilterLabel: 'State',
    kindFilterLabel: 'Kind',
    postLink: 'Open the post',
    tries: '{attempts}/{max} tries',
    cancelJob: 'Cancel this job',
    retryJob: 'Retry this job',
    queuePausedBadge: 'paused',
    pauseQueueTitle: 'Pause this kind of job',
    resumeQueueTitle: 'Resume this kind of job',
    cancelAllTitle: 'Cancel every queued or running job of this kind',
    clearFinishedTitle: 'Remove finished, failed and cancelled jobs of this kind from the list',
    'state.queued': 'Queued',
    'state.running': 'Running',
    'state.succeeded': 'Succeeded',
    'state.failed': 'Failed',
    'state.cancelled': 'Cancelled',
    'kind.bulk': 'Bulk action',
    'kind.purge': 'Trash purge',
    'kind.usage.recompute': 'Storage usage',
    'kind.migrate': 'Library migration',
    'kind.archive.drain': 'Media archiving',
    'kind.link.hydrate': 'Link hydration',
    'kind.ai.drain': 'AI cataloging',
    'kind.ai.run': 'AI analysis',
    'kind.media.video': 'Video fetch',
    'kind.capture.site': 'Site capture',
    'kind.import': 'Import',
    'kind.export': 'Export',
    'kind.gc': 'Garbage collection',
    'error.lease_expired': 'It took too long and was queued again.',
    'error.internal': 'A server error.',
    'error.unavailable': 'A dependency was unavailable.',
    'error.invalid_payload': "The job's data was invalid.",
    'error.cancelled': 'Cancelled.',
    'error.user_locked': 'The library was under maintenance.',
    'error.not_found': 'It no longer exists.',
    'error.conflict': 'It clashes with another change.',
    'error.validation_failed': 'Some values were invalid.',
    'error.invalid_cursor': 'The list changed while it was loading.',
    'error.quota_exceeded': 'Storage was full.',
  },
} satisfies LangMessages;
