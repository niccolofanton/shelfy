// Inner-page selection for a web reference (v2).
//
// The primary source is the RENDERED page's own navigation (header/nav/footer/
// main links collected by JS_PAGE_PROBE): it reflects what the site considers
// important and works on JS-rendered sites. The sitemap (v1 discovery) is only a
// supplement. Every candidate is classified into a page type from its path and
// link text (EN/IT/FR/DE/ES), low-value pages are excluded (legal, auth, cart,
// careers, search, feeds), language variants are dropped, and the final pick is
// diversified (one page per type, up to two case studies).

export type PageType =
  | 'home'
  | 'work'
  | 'case-study'
  | 'product'
  | 'features'
  | 'shop'
  | 'pricing'
  | 'about'
  | 'services'
  | 'team'
  | 'docs'
  | 'blog'
  | 'article'
  | 'contact'
  | 'careers'
  | 'legal'
  | 'auth'
  | 'cart'
  | 'search'
  | 'other';

export interface LinkCandidate {
  href: string;
  text?: string;
  region?: string; // header | nav | footer | aside | main | sitemap
  visible?: boolean;
}

export interface PickedPage {
  url: string;
  pageType: PageType;
  score: number;
  source: 'nav' | 'sitemap';
}

const TYPE_RULES: [PageType, RegExp][] = [
  [
    'legal',
    /\b(privacy|cookie|cookies|terms|termini|condizioni|legal|legale|imprint|impressum|disclaimer|gdpr|policy|note-legali|mentions-legales|datenschutz|aviso-legal|accessibility|accessibilita|sitemap)\b/,
  ],
  [
    'auth',
    /\b(login|log-in|signin|sign-in|signup|sign-up|register|registrati|accedi|account|my-account|profile|logout|password|reset)\b/,
  ],
  ['cart', /\b(cart|carrello|checkout|basket|bag|panier|warenkorb|wishlist)\b/],
  [
    'careers',
    /\b(careers?|jobs?|lavora-con-noi|work-with-us|join-us|join|hiring|carriere|carriere|karriere|empleo|stellen|vacancies|posizioni-aperte|open-positions)\b/,
  ],
  ['search', /\b(search|cerca|recherche|suche|buscar)\b/],
  [
    'contact',
    /\b(contact|contacts|contatti|contattaci|kontakt|contacto|get-in-touch|reach-us|say-hello|hello)\b/,
  ],
  ['pricing', /\b(pricing|prices|prezzi|tariffe|plans|piani|abbonamenti|tarifs|preise|precios)\b/],
  [
    'work',
    /\b(work|works|projects?|progetti|portfolio|lavori|case-studies|cases|showcase|realisations|realizzazioni|referenzen|proyectos|clients|clienti)\b/,
  ],
  [
    'product',
    /\b(product|products|prodotto|prodotti|produits|produkte|productos|platform|piattaforma|solutions?|soluzioni|app|apps)\b/,
  ],
  [
    'features',
    /\b(features?|funzionalita|funzioni|capabilities|how-it-works|come-funziona|integrations?)\b/,
  ],
  [
    'shop',
    /\b(shop|store|negozio|collections?|collezioni|catalog|catalogo|boutique|tienda|category|categoria)\b/,
  ],
  [
    'about',
    /\b(about|about-us|chi-siamo|chisiamo|studio|company|azienda|agency|agenzia|story|storia|mission|a-propos|ueber-uns|uber-uns|nosotros|manifesto|philosophy)\b/,
  ],
  [
    'services',
    /\b(services?|servizi|what-we-do|cosa-facciamo|expertise|capabilities|offering|leistungen|servicios)\b/,
  ],
  ['team', /\b(team|people|persone|squadra|founders|leadership|equipe)\b/],
  [
    'docs',
    /\b(docs|documentation|documentazione|guide|guides|developers|api|help|support|supporto|faq)\b/,
  ],
  [
    'blog',
    /\b(blog|journal|news|notizie|insights|stories|articles|articoli|magazine|press|stampa|updates|changelog|now)\b/,
  ],
];

// Path-segment classification first (stable), then the link text.
export function classifyUrl(url: string, text = ''): PageType {
  let segs: string[] = [];
  try {
    const u = new URL(url);
    segs = u.pathname
      .toLowerCase()
      .split('/')
      .filter(Boolean)
      .filter((s) => !/^[a-z]{2}(?:-[a-z]{2})?$/.test(s)); // drop locale segments
  } catch {
    return 'other';
  }
  if (!segs.length) return 'home';
  const first = segs[0].replace(/\.(html?|php|aspx?)$/, '');
  for (const [type, re] of TYPE_RULES) {
    if (re.test(first)) {
      if (
        segs.length >= 2 &&
        (type === 'work' || type === 'blog' || type === 'product' || type === 'shop')
      ) {
        return type === 'work' ? 'case-study' : type === 'blog' ? 'article' : type;
      }
      return type;
    }
  }
  const t = text
    .toLowerCase()
    .replace(/[^a-z0-9àèéìòù\s-]/g, ' ')
    .trim()
    .replace(/\s+/g, '-');
  if (t) {
    for (const [type, re] of TYPE_RULES) if (re.test(t)) return type;
  }
  return segs.length >= 2 ? 'article' : 'other';
}

const EXCLUDED: ReadonlySet<PageType> = new Set(['legal', 'auth', 'cart', 'careers', 'search']);

const TYPE_SCORE: Record<PageType, number> = {
  home: 0,
  work: 100,
  'case-study': 92,
  product: 88,
  features: 84,
  shop: 80,
  pricing: 76,
  about: 72,
  services: 70,
  docs: 58,
  team: 56,
  blog: 50,
  article: 44,
  other: 40,
  contact: 12,
  careers: 0,
  legal: 0,
  auth: 0,
  cart: 0,
  search: 0,
};

const REGION_BONUS: Record<string, number> = {
  header: 16,
  nav: 16,
  main: 6,
  aside: 0,
  footer: -6,
  sitemap: -12,
};

const ASSET_EXT =
  /\.(?:jpe?g|png|gif|webp|avif|svg|ico|css|js|mjs|json|xml|pdf|zip|gz|mp4|webm|mov|mp3|wav|woff2?|ttf|otf|txt|csv|rss|atom)$/i;
const LOCALE_SEG = /^[a-z]{2}(?:-[a-z]{2})?$/i;

function canonical(url: string): string | null {
  try {
    const u = new URL(url);
    if (u.protocol !== 'http:' && u.protocol !== 'https:') return null;
    u.hash = '';
    for (const k of Array.from(u.searchParams.keys())) {
      if (/^utm_|^(gclid|fbclid|ref|mc_cid|mc_eid|_ga|igshid)$/i.test(k)) u.searchParams.delete(k);
    }
    let p = u.pathname.replace(/\/{2,}/g, '/');
    if (p.length > 1) p = p.replace(/\/+$/, '');
    u.pathname = p || '/';
    u.hostname = u.hostname.toLowerCase();
    return u.toString();
  } catch {
    return null;
  }
}

function hostKey(h: string): string {
  return h.toLowerCase().replace(/^www\./, '');
}

function localeOf(url: string): string | null {
  try {
    const seg = new URL(url).pathname.split('/').filter(Boolean)[0] || '';
    return LOCALE_SEG.test(seg) ? seg.toLowerCase() : null;
  } catch {
    return null;
  }
}

export function pickPages(
  primaryUrl: string,
  links: LinkCandidate[],
  {
    max,
    hreflang = [],
    sitemap = [],
  }: { max: number; hreflang?: { lang?: string; href?: string }[]; sitemap?: string[] },
): PickedPage[] {
  if (max <= 0) return [];
  const primary = canonical(primaryUrl);
  if (!primary) return [];
  const base = new URL(primary);
  const primaryLocale = localeOf(primary);
  const alternates = new Set(
    hreflang.map((h) => canonical(h.href || '')).filter((x): x is string => !!x && x !== primary),
  );
  const all: LinkCandidate[] = [
    ...links,
    ...sitemap.map((href) => ({ href, region: 'sitemap', visible: true })),
  ];
  const best = new Map<string, PickedPage & { text: string }>();
  for (const l of all) {
    const c = canonical(l.href);
    if (!c || c === primary || alternates.has(c)) continue;
    const u = new URL(c);
    if (hostKey(u.hostname) !== hostKey(base.hostname)) continue;
    if (ASSET_EXT.test(u.pathname)) continue;
    if (/\/(feed|wp-json|xmlrpc\.php|wp-admin|cdn-cgi|amp)(\/|$)/i.test(u.pathname)) continue;
    if (u.search && u.searchParams.toString().length > 40) continue;
    const loc = localeOf(c);
    if (loc !== primaryLocale && (loc || primaryLocale)) continue; // another language variant
    const depth = u.pathname.split('/').filter(Boolean).length - (loc ? 1 : 0);
    if (depth > 4) continue;
    const type = classifyUrl(c, l.text || '');
    if (EXCLUDED.has(type) || type === 'home') continue;
    let score =
      TYPE_SCORE[type] + (REGION_BONUS[l.region || 'main'] ?? 0) + (l.visible === false ? -10 : 0);
    score -= Math.max(0, depth - 2) * 6;
    const prev = best.get(c);
    if (!prev || score > prev.score) {
      best.set(c, {
        url: c,
        pageType: type,
        score,
        source: l.region === 'sitemap' ? 'sitemap' : 'nav',
        text: l.text || '',
      });
    }
  }
  const ranked = Array.from(best.values()).sort((a, b) => b.score - a.score);
  const perType = new Map<PageType, number>();
  const out: PickedPage[] = [];
  const cap = (t: PageType): number => (t === 'case-study' ? 2 : 1);
  for (const r of ranked) {
    if (out.length >= max) break;
    const n = perType.get(r.pageType) || 0;
    if (n >= cap(r.pageType)) continue;
    // A contact page only when nothing better is left.
    if (
      r.pageType === 'contact' &&
      ranked.some(
        (x) =>
          x.pageType !== 'contact' &&
          !out.some((o) => o.url === x.url) &&
          (perType.get(x.pageType) || 0) < cap(x.pageType),
      )
    )
      continue;
    perType.set(r.pageType, n + 1);
    out.push({ url: r.url, pageType: r.pageType, score: r.score, source: r.source });
  }
  return out;
}
