// UI strings for the mobile bottom navigation (P1-02, under 900px). Mirrors
// the label choices already used in the sidebar (sidebar.ts) so the same
// destination reads the same way in both places.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    library: 'Libreria',
    search: 'Cerca',
    ai: 'AI',
    settings: 'Impostazioni',
  },
  en: {
    library: 'Library',
    search: 'Search',
    ai: 'AI',
    settings: 'Settings',
  },
} satisfies LangMessages;
