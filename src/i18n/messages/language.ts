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
  },
  en: {
    section: 'Language',
    title: 'Interface language',
    desc: 'Changes the language of the app’s text. It does not affect your saved content or the language of AI analysis.',
  },
} satisfies LangMessages;
