// Strings for the create/edit/delete collection ("source") modal.
// The Italian column reproduces the app's existing copy verbatim.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    titleEdit: 'Modifica source',
    titleNew: 'Nuova source',
    previewPlaceholder: 'Anteprima source',
    nameLabel: 'Nome',
    namePlaceholder: 'Es. Ricette, Viaggi, Ispirazioni…',
    duplicate: 'Esiste già una cartella con questo nome.',
    colorLabel: 'Colore',
    customColor: 'Colore personalizzato',
    saveError: 'Salvataggio non riuscito. Riprova.',
    deleteError: 'Eliminazione non riuscita. Riprova.',
    deletePartial: {
      one: 'Eliminata, ma 1 file non è stato rimosso dal disco.',
      other: 'Eliminata, ma {count} file non sono stati rimossi dal disco.',
    },
    deleteVerb: 'Elimina',
    deletePlatformDesc:
      'Rimuove solo il raggruppamento locale. Questa cartella è sincronizzata e riapparirà al prossimo sync.',
    deleteDesc: "Scegli cosa rimuovere. L'operazione non è reversibile.",
    whatToRemove: 'Cosa rimuovere',
    onlyLabel: 'Solo l’etichetta',
    postsRemain: {
      one: 'Il post resta nell’archivio.',
      other: 'I {count} post restano nell’archivio.',
    },
    noLinkedPosts: 'Nessun post collegato.',
    labelAndPosts: {
      one: 'Etichetta e 1 post',
      other: 'Etichetta e i {count} post',
    },
    labelAndPostsDesc: 'Rimuove i post e i file scaricati dall’intero archivio.',
    deletingPosts: 'Elimino…',
    deleteLabelAndPosts: {
      one: 'Elimina etichetta e 1 post',
      other: 'Elimina etichetta e {count} post',
    },
    deleteLabel: 'Elimina etichetta',
    deleteSourceTitle: 'Elimina source',
    create: 'Crea',
    loadError: 'Caricamento delle source non riuscito.',
  },
  en: {
    titleEdit: 'Edit source',
    titleNew: 'New source',
    previewPlaceholder: 'Source preview',
    nameLabel: 'Name',
    namePlaceholder: 'E.g. Recipes, Travel, Inspiration…',
    duplicate: 'A folder with this name already exists.',
    colorLabel: 'Color',
    customColor: 'Custom color',
    saveError: 'Save failed. Try again.',
    deleteError: 'Delete failed. Try again.',
    deletePartial: {
      one: 'Deleted, but 1 file could not be removed from disk.',
      other: 'Deleted, but {count} files could not be removed from disk.',
    },
    deleteVerb: 'Delete',
    deletePlatformDesc:
      'Removes only the local grouping. This folder is synced and will reappear on the next sync.',
    deleteDesc: 'Choose what to remove. This action cannot be undone.',
    whatToRemove: 'What to remove',
    onlyLabel: 'Label only',
    postsRemain: {
      one: 'The post stays in the archive.',
      other: 'The {count} posts stay in the archive.',
    },
    noLinkedPosts: 'No linked posts.',
    labelAndPosts: {
      one: 'Label and 1 post',
      other: 'Label and the {count} posts',
    },
    labelAndPostsDesc: 'Removes the posts and downloaded files from the entire archive.',
    deletingPosts: 'Deleting…',
    deleteLabelAndPosts: {
      one: 'Delete label and 1 post',
      other: 'Delete label and {count} posts',
    },
    deleteLabel: 'Delete label',
    deleteSourceTitle: 'Delete source',
    create: 'Create',
    loadError: 'Could not load sources.',
  },
} satisfies LangMessages;
