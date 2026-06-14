// UI chrome for the post grid card (PostCard.jsx): media-type / offline / AI
// badges and tooltips shown over each thumbnail. Card date formatting follows the
// active locale via localeTag; no user content is translated here.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    savedOffline: 'Salvato offline',
    linkOnly: 'Solo link — nessun file locale',
    awards: 'Riconoscimenti',
    aiGenerated: "Tag e descrizione generati dall'AI",
    website: 'Sito web',
    manualBookmark: 'Bookmark',
    unknownAuthor: 'unknown',
    post: 'Post',
    deselectPost: 'Deseleziona post',
    selectPost: 'Seleziona post',
  },
  en: {
    savedOffline: 'Saved offline',
    linkOnly: 'Link only — no local file',
    awards: 'Awards',
    aiGenerated: 'AI-generated tags and description',
    website: 'Website',
    manualBookmark: 'Bookmark',
    unknownAuthor: 'unknown',
    post: 'Post',
    deselectPost: 'Deselect post',
    selectPost: 'Select post',
  },
} satisfies LangMessages;
