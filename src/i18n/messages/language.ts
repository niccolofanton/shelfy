// Strings for the language picker (Settings → Language section) and its section
// title. Owned by the i18n core, not by a per-view agent.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    section: 'Lingua',
    title: 'Lingua dell’interfaccia',
    desc: 'Cambia la lingua dei testi dell’app. Non modifica i contenuti che hai salvato né la lingua delle analisi AI.',
    // The web: the choice is saved to the account.
    descAccount: 'La scelta resta nel tuo account e ti segue in ogni browser.',
    saveFailed: 'Non è stato possibile salvare la lingua nel tuo account. Riprova.',
  },
  en: {
    section: 'Language',
    title: 'Interface language',
    desc: 'Changes the language of the app’s text. It does not affect your saved content or the language of AI analysis.',
    descAccount: 'Your choice is saved to your account and follows you to every browser.',
    saveFailed: 'The language could not be saved to your account. Try again.',
  },
} satisfies LangMessages;
