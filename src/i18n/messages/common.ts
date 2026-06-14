// Shared UI strings reused across many surfaces (buttons, generic states). Use
// these via `const tc = useT('common')` so wording stays consistent everywhere.
// The Italian column reproduces the app's existing copy verbatim (the test suite
// asserts some of it). Feature-specific strings live in their own namespace file.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    cancel: 'Annulla',
    confirm: 'Conferma',
    close: 'Chiudi',
    save: 'Salva',
    delete: 'Elimina',
    edit: 'Modifica',
    retry: 'Riprova',
    remove: 'Rimuovi',
    download: 'Scarica',
    loading: 'Caricamento…',
    search: 'Cerca',
    add: 'Aggiungi',
    back: 'Indietro',
    next: 'Avanti',
    done: 'Fatto',
    all: 'Tutti',
    none: 'Nessuno',
    error: 'Errore',
    pause: 'Pausa',
    resume: 'Riprendi',
    later: 'Più tardi',
    open: 'Apri',
    posts: 'post',
    irreversible: 'Irreversibile',
    deleting: 'Eliminazione…',
    inProgress: 'In corso…',
    genericError: 'Operazione non riuscita. Riprova.',
    // Grid zoom control (shared by Gallery / AI Search / Tags Explorer); the
    // ⌘/Ctrl +/- shortcut hint is appended in code next to these labels.
    gridShrink: 'Riduci dimensione griglia',
    gridEnlarge: 'Aumenta dimensione griglia',
  },
  en: {
    cancel: 'Cancel',
    confirm: 'Confirm',
    close: 'Close',
    save: 'Save',
    delete: 'Delete',
    edit: 'Edit',
    retry: 'Retry',
    remove: 'Remove',
    download: 'Download',
    loading: 'Loading…',
    search: 'Search',
    add: 'Add',
    back: 'Back',
    next: 'Next',
    done: 'Done',
    all: 'All',
    none: 'None',
    error: 'Error',
    pause: 'Pause',
    resume: 'Resume',
    later: 'Later',
    open: 'Open',
    posts: 'posts',
    irreversible: 'Irreversible',
    deleting: 'Deleting…',
    inProgress: 'In progress…',
    genericError: 'Operation failed. Try again.',
    gridShrink: 'Shrink grid size',
    gridEnlarge: 'Enlarge grid size',
  },
} satisfies LangMessages;
