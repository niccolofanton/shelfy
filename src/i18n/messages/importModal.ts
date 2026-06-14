// Strings for the JSON import modal (Chrome extension export → app).
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Importa export JSON',
    desc: "Importa i post dall'export JSON dell'estensione Chrome. Sono supportati sia gli export di Instagram sia quelli di Twitter.",
    chooseFile: 'Scegli file',
    importBtn: 'Importa',
    importing: 'Importazione dei post…',
    importedCount: {
      one: 'Importato {count} nuovo post',
      other: 'Importati {count} nuovi post',
    },
    importAnother: 'Importa un altro',
    tryAgain: 'Riprova',
  },
  en: {
    title: 'Import JSON Export',
    desc: 'Import posts from the Chrome extension JSON export. Both Instagram and Twitter exports are supported.',
    chooseFile: 'Choose File',
    importBtn: 'Import',
    importing: 'Importing posts…',
    importedCount: {
      one: 'Imported {count} new post',
      other: 'Imported {count} new posts',
    },
    importAnother: 'Import another',
    tryAgain: 'Try Again',
  },
} satisfies LangMessages;
