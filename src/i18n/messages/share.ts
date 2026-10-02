// `/share` (web port plan §2.17, §2.19 Routes; P2-07): the page Android's
// share sheet, the iOS Shortcut and the bookmarklet open. Also the service
// worker's one user-facing prompt (web/src/main.tsx), which belongs here
// rather than in a new namespace of its own.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Salva un link',
    saving: 'Salvataggio in corso…',
    saved: 'Salvato nella tua libreria.',
    alreadySaved: 'Era già nella tua libreria.',
    open: 'Apri',
    retry: 'Riprova',
    noLink: 'Nessun link trovato in questa condivisione.',
    updatePrompt: 'È disponibile una nuova versione di Shelfy. Ricaricare ora?',
  },
  en: {
    title: 'Save a link',
    saving: 'Saving…',
    saved: 'Saved to your library.',
    alreadySaved: 'It was already in your library.',
    open: 'Open',
    retry: 'Try again',
    noLink: 'No link was found in this share.',
    updatePrompt: 'A new version of Shelfy is available. Reload now?',
  },
} satisfies LangMessages;
