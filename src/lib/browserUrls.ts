// URL patterns + parsing/validation helpers for the in-app Browser's webview
// tabs (Instagram / X / Pinterest). Pure functions only — no React, no Electron.

// The three webview tabs these helpers key off. Mirrors BrowserTab/SyncPlatform
// declared in the Browser view and sync hooks.
export type BrowserTab = 'instagram' | 'twitter' | 'pinterest';

export interface BrowserTabDef {
  id: BrowserTab;
  url: string;
}

export const TABS: BrowserTabDef[] = [
  { id: 'instagram', url: 'https://www.instagram.com/' },
  { id: 'twitter', url: 'https://x.com/i/bookmarks' },
  { id: 'pinterest', url: 'https://www.pinterest.com/' },
];

export const IG_SAVED_URL_KEY = 'ig-saved-url';
export const PIN_BOARD_URL_KEY = 'pin-board-url';

// Pinterest top-level paths that look like /<seg>/<seg>/ but are NOT user boards
// (search, business hub, idea/today feeds, settings, …). Excluded from the
// board-page regexes below so they don't false-match and trigger a dead-end sync.
// Kept here as the single source of truth for SAVED_PATTERNS.pinterest +
// PIN_BOARD_RE (mirror it in webview-select.js onListingPage).
const PIN_RESERVED_ROOTS =
  'pin|search|business|ideas|today|categories|news|settings|notifications|discover|pin-builder';
export const IG_LOGIN_PATTERN = /instagram\.com\/accounts\/(login|signup|emailsignup)/;

// Logged-out landing pages per platform. A background source-sync that ends up
// on one of these knows the session expired and must surface a "login required"
// error instead of silently capturing nothing. X serves bookmarks behind
// /i/flow/login (SPA redirect); Pinterest uses /login on every ccTLD.
export const LOGIN_PATTERNS: Record<BrowserTab, RegExp> = {
  instagram: IG_LOGIN_PATTERN,
  twitter: /(?:x|twitter)\.com\/(?:i\/flow\/)?login/,
  pinterest: /pinterest\.[a-z.]+\/login/,
};

export const SAVED_PATTERNS: Record<BrowserTab, RegExp> = {
  instagram: /instagram\.com\/(?:[^/?#]+\/)?saved(?:\/|$)/,
  twitter: /x\.com\/i\/bookmarks/,
  // A Pinterest board page: /<user>/<board-slug>/ (and board sections), but NOT a
  // pin detail (/pin/<id>/), a reserved root (search/business/…) nor a profile
  // tab (/<user>/_boards|_saved|_created/).
  pinterest: new RegExp(
    `pinterest\\.[a-z.]+\\/(?!(?:${PIN_RESERVED_ROOTS})\\/)[^/?#]+\\/(?!_)[^/?#]+`,
  ),
};

// A post/pin DETAIL opened from the saved grid: IG /p|reel|tv/<shortcode>/ or
// Pinterest /pin/<id>/. It's an in-page nav that keeps the listing mounted behind
// a modal, so it must NOT be treated as "left the saved page" — neither the sync
// auto-stop nor the pre-sync buffer reset should fire for it.
export const isPostDetail = (u: string): boolean =>
  /\/(?:p|reel|tv)\/[^/]/.test(u) || /\/pin\/\d+/.test(u);

// A saved-COLLECTION (folder) URL looks like
//   /<username>/saved/<folder-slug>/<numeric-folder-id>/
// while the full list is /<username>/saved/all-posts/ (no numeric id) and the
// folder index is /<username>/saved/. The numeric id is what we persist on the
// tag (rename-safe) so re-importing the same folder reuses its tag.
const IG_FOLDER_RE = /instagram\.com\/[^/?#]+\/saved\/([^/?#]+)\/(\d+)/;

export interface IgFolder {
  slug: string;
  folderId: string;
}

export function parseIgFolder(url: string | null | undefined): IgFolder | null {
  const m = IG_FOLDER_RE.exec(url || '');
  if (!m || m[1] === 'all-posts') return null;
  return { slug: m[1], folderId: m[2] };
}

// A Pinterest board URL is /<user>/<board-slug>/. Pinterest only exposes the
// slug (no numeric board id in the URL), so we persist `<user>/<slug>` as the
// collection's externalId. Unlike IG's numeric folder id this is NOT rename-safe
// — renaming a board changes its slug and so creates a new collection.
const PIN_BOARD_RE = new RegExp(
  `pinterest\\.[a-z.]+\\/(?!(?:${PIN_RESERVED_ROOTS})\\/)([^/?#]+)\\/(?!_)([^/?#]+)`,
);

export interface PinBoard {
  user: string;
  slug: string;
  boardId: string;
}

export function parsePinBoard(url: string | null | undefined): PinBoard | null {
  const m = PIN_BOARD_RE.exec(url || '');
  if (!m) return null;
  return { user: m[1], slug: m[2], boardId: `${m[1]}/${m[2]}` };
}

// Human-readable fallback name from a URL slug ("ricette-veloci" → "Ricette Veloci").
// `fallback` is the localized default name used when the slug is empty.
export function deslugify(slug: string | null | undefined, fallback = 'Cartella'): string {
  const s = String(slug || '')
    .replace(/[-_]+/g, ' ')
    .replace(/\s+/g, ' ')
    .trim();
  // Title-case the first letter of each whitespace-separated token. NB: we can't
  // use \b here — JS word boundaries are ASCII-only, so a boundary appears between
  // an ASCII letter and an accented one (é, à, ù, ñ…), wrongly capitalizing
  // mid-word ('città-del-caffè' → 'CittÀ Del CaffÈ'). Matching the token start
  // handles accented and leading-accent words ('città' → 'Città', 'über' → 'Über').
  return s
    ? s.replace(/(^|\s)(\p{L})/gu, (_, sep: string, ch: string) => sep + ch.toUpperCase())
    : fallback;
}

// Hosts the webview is allowed to load. Anything else (other scheme, foreign
// host) is rejected before loadURL/src so a tampered localStorage value or a
// redirect cannot point the privileged webview at an arbitrary origin.
const ALLOWED_HOSTS: Record<BrowserTab, RegExp> = {
  instagram: /(?:^|\.)instagram\.com$/,
  twitter: /(?:^|\.)(?:x\.com|twitter\.com)$/,
  // Pinterest localizes by ccTLD (.com, .it, .co.uk, .com.au, …). Match
  // pinterest.<tld> with an optional second-level country label, anchored to the
  // end so a lookalike like pinterest.com.evil.io is rejected.
  pinterest: /(?:^|\.)pinterest\.[a-z]{2,3}(?:\.[a-z]{2})?$/,
};

export function isAllowedUrl(tab: BrowserTab, url: string | null | undefined): boolean {
  try {
    const u = new URL(String(url));
    if (u.protocol !== 'https:') return false;
    const re = ALLOWED_HOSTS[tab];
    return !!re && re.test(u.hostname);
  } catch {
    return false;
  }
}

// Returns `url` if it is a valid https URL for `tab`, otherwise the tab's
// default. Used to sanitize URLs coming from localStorage before loadURL/src.
export function safeUrl(tab: BrowserTab, url: string | null | undefined, fallback: string): string {
  return isAllowedUrl(tab, url) ? (url as string) : fallback;
}
