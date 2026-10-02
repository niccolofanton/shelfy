// UI strings for the global "remote AI node unreachable" banner
// (src/components/RemoteAiBanner.tsx). {name} is the configured node's name.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Nodo AI non raggiungibile',
    body: '{name} non risponde. Le analisi AI restano in coda finché il nodo non torna disponibile.',
    useLocal: 'Usa modelli locali',
    useLocalHint: 'Fino al riavvio dell’app',
    downloadLocal: 'Scarica modello locale',
    downloading: 'Download modello locale… {pct}%',
    retry: 'Riprova',
    checking: 'Verifica…',
  },
  en: {
    title: 'AI node unreachable',
    body: '{name} is not responding. AI analyses stay queued until the node is back.',
    useLocal: 'Use local models',
    useLocalHint: 'Until the app restarts',
    downloadLocal: 'Download local model',
    downloading: 'Downloading local model… {pct}%',
    retry: 'Retry',
    checking: 'Checking…',
  },
} satisfies LangMessages;
