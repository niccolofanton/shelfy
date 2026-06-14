// UI strings for AddSiteModal — paste a URL to enqueue a web reference capture.
// Title, the intro hint, the URL + max-pages inputs (labels/placeholders), the
// "queuing" state and the error/retry screen. The Italian column reproduces the
// app's existing copy verbatim. Brand/example hosts (stripe.com) are not
// translated; shared buttons (Annulla, Aggiungi, Riprova, Chiudi) come from the
// `common` namespace.
// ── Types for this i18n namespace ──────────────────────────────────────────────
// A translatable value is either a plain string or a { one, other } plural shape
// (chosen by vars.count in translate()). Each supported language maps namespaced
// keys to such values. `satisfies` keeps the literal key set while type-checking.
type MessageValue = string | { one: string; other: string };
type LangMessages = { it: Record<string, MessageValue>; en: Record<string, MessageValue> };

export default {
  it: {
    title: 'Aggiungi sito',
    intro:
      'Incolla l’indirizzo di un sito: cattureremo screenshot, palette, font e tag AI, e lo ritroverai in galleria insieme ai contenuti social.',
    addressLabel: 'Indirizzo',
    addressPlaceholder: 'es. stripe.com',
    maxPagesLabel: 'Pagine max',
    maxPagesOptional: '(opzionale)',
    maxPagesPlaceholder: 'auto',
    singlePageLabel: 'Solo questa pagina',
    singlePageHint:
      'Cattura solo l’indirizzo incollato, senza esplorare il sito (es. un articolo o una guida).',
    working: 'Aggiunta in corso…',
    errorFallback: 'Impossibile aggiungere il sito.',
  },
  en: {
    title: 'Add website',
    intro:
      'Paste a website address: we’ll capture screenshots, palette, fonts and AI tags, and you’ll find it in the gallery alongside your social content.',
    addressLabel: 'Address',
    addressPlaceholder: 'e.g. stripe.com',
    maxPagesLabel: 'Max pages',
    maxPagesOptional: '(optional)',
    maxPagesPlaceholder: 'auto',
    singlePageLabel: 'Only this page',
    singlePageHint:
      'Capture just the pasted address, without crawling the site (e.g. an article or guide).',
    working: 'Adding…',
    errorFallback: 'Could not add the website.',
  },
} satisfies LangMessages;
