// UI chrome for the full-screen screenshot/image viewer (ImageLightbox.jsx):
// dialog label, navigation controls and error state.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    dialogLabel: 'Anteprima a tutto schermo',
    openPageTitle: 'Apri la pagina nel browser',
    openPage: 'Apri pagina',
    closeEsc: 'Chiudi (Esc)',
    loadFailed: 'Impossibile caricare lo screenshot.',
    prev: 'Precedente (←)',
    next: 'Successivo (→)',
  },
  en: {
    dialogLabel: 'Full-screen preview',
    openPageTitle: 'Open the page in the browser',
    openPage: 'Open page',
    closeEsc: 'Close (Esc)',
    loadFailed: 'Could not load the screenshot.',
    prev: 'Previous (←)',
    next: 'Next (→)',
  },
} satisfies LangMessages;
