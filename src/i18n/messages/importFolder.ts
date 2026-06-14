// UI strings for the folder/board import modal (src/components/ImportFolderModal.jsx).
// Brand names (Instagram/Pinterest), the user's tag/board names and colour hex
// values are not translated.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    // Generic fallbacks for a destination with no readable name.
    folderFallback: 'Cartella',
    boardFallback: 'Board',
    tagFallback: 'tag',

    // Fallback action verb when the parent doesn't pass one.
    defaultAction: 'Importa',

    // Error surfaced when the import fails.
    importFailed: 'Importazione non riuscita.',

    // Title.
    titleInstagram: 'Importa cartella Instagram',
    titlePinterest: 'Importa board Pinterest',

    // Lead instruction. {name} is the source folder/board name.
    leadBefore: 'Stai per importare i contenuti della cartella',
    leadAfter: '. Dove vuoi salvarli?',

    // Destination: platform only.
    platformOnly: 'Solo {platform}',
    platformOnlyHint: 'Nessun tag, importa e basta',

    // Destination: tag.
    addToTag: 'Aggiungi a un tag cartella',
    addToTagHint: 'Sotto {platform} nel menu laterale',

    // Tag editor.
    existingTag: 'Tag esistente',
    createNew: '➕ Crea nuovo tag…',
    wasNamed: '(era «{name}»)',
    tagPreview: 'Anteprima tag',
    tagName: 'Nome del tag',
    tagNamePlaceholder: 'Nome cartella',
    color: 'Colore',
    customColor: 'Colore personalizzato',

    // Confirm button: action verb + destination, e.g. "Importa in Ricette".
    inDest: '{action} in {dest}',
  },
  en: {
    folderFallback: 'Folder',
    boardFallback: 'Board',
    tagFallback: 'tag',

    defaultAction: 'Import',

    importFailed: 'Import failed.',

    titleInstagram: 'Import Instagram folder',
    titlePinterest: 'Import Pinterest board',

    leadBefore: 'You are about to import the contents of the folder',
    leadAfter: '. Where do you want to save them?',

    platformOnly: '{platform} only',
    platformOnlyHint: 'No tag, just import',

    addToTag: 'Add to a folder tag',
    addToTagHint: 'Under {platform} in the sidebar',

    existingTag: 'Existing tag',
    createNew: '➕ Create new tag…',
    wasNamed: '(was «{name}»)',
    tagPreview: 'Tag preview',
    tagName: 'Tag name',
    tagNamePlaceholder: 'Folder name',
    color: 'Color',
    customColor: 'Custom color',

    inDest: '{action} {dest}',
  },
} satisfies LangMessages;
