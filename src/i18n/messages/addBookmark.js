// UI strings for AddBookmarkModal — add a manual bookmark from local files
// (images/videos/pdf/any) with a description and tags. Shared buttons (Annulla,
// Aggiungi, Riprova, Chiudi, Rimuovi) come from the `common` namespace.
export default {
  it: {
    title: 'Nuovo bookmark',
    intro:
      'Aggiungi file dal tuo computer (immagini, video, PDF o altro) con una descrizione e dei tag. Li ritrovi in galleria insieme ai contenuti social e ai siti.',
    dropHint: 'Trascina i file qui o clicca per sfogliare',
    dropMore: 'Trascina altri file o clicca per aggiungerne',
    formats: 'Immagini, video, PDF o qualsiasi file',
    descLabel: 'Descrizione',
    descPlaceholder: 'A cosa ti serve questo bookmark?',
    tagsLabel: 'Tag',
    tagsPlaceholder: 'Aggiungi un tag e premi Invio',
    working: 'Salvataggio…',
    fileCount: { one: '{count} file', other: '{count} file' },
    tooManyFiles: { one: 'Massimo {max} file.', other: 'Massimo {max} file.' },
    fileTooBig: '“{name}” supera il limite di 200 MB.',
    totalTooBig: 'I file selezionati superano il limite complessivo di 500 MB.',
    errorFallback: 'Impossibile salvare il bookmark.',
  },
  en: {
    title: 'New bookmark',
    intro:
      'Add files from your computer (images, videos, PDF or anything) with a description and tags. They show up in the gallery alongside your social content and websites.',
    dropHint: 'Drag files here or click to browse',
    dropMore: 'Drag more files or click to add',
    formats: 'Images, videos, PDF or any file',
    descLabel: 'Description',
    descPlaceholder: 'What is this bookmark for?',
    tagsLabel: 'Tags',
    tagsPlaceholder: 'Add a tag and press Enter',
    working: 'Saving…',
    fileCount: { one: '{count} file', other: '{count} files' },
    tooManyFiles: { one: 'Up to {max} file.', other: 'Up to {max} files.' },
    fileTooBig: '“{name}” exceeds the 200 MB limit.',
    totalTooBig: 'The selected files exceed the 500 MB total limit.',
    errorFallback: 'Could not save the bookmark.',
  },
};
