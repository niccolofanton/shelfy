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
    jobs: 'Attività',
    settings: 'Impostazioni',
    // The bar's landmark name (UX-1).
    navLabel: 'Navigazione principale',
  },
  en: {
    library: 'Library',
    search: 'Search',
    ai: 'AI',
    jobs: 'Jobs',
    settings: 'Settings',
    navLabel: 'Main navigation',
  },
} satisfies LangMessages;
