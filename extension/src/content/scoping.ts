// Content-side scoping state of one document:
// - ScopeTracker: which non-passive capture scope (replay, SSR read, DOM scan) a relayed batch
//   belongs to, from the scope messages the MAIN-world helpers post around their calls;
// - the signed-in Pinterest user (`viewer`), read from the page, so the worker can keep only the
//   user's own boards (plan §2.16).
// Both come from the page and are informational or advisory: the worker decides the scope.

import { isRecord, type CaptureSource, type ScopeMessage } from '../shared/protocol';

const MAX_OPEN_SCOPES = 16;

/**
 * Open non-passive scopes of one document. Window messages are delivered in posting order, so
 * a batch posted between a scope's start and end belongs to it; batches are tagged with the
 * innermost open scope, or `passive` when none is open.
 */
export class ScopeTracker {
  private readonly open = new Map<string, CaptureSource>();

  apply(message: ScopeMessage): void {
    if (message.phase === 'end') this.open.delete(message.id);
    else if (this.open.size < MAX_OPEN_SCOPES) this.open.set(message.id, message.source);
  }

  current(): CaptureSource {
    let innermost: CaptureSource = 'passive';
    for (const source of this.open.values()) innermost = source;
    return innermost;
  }
}

// ── The signed-in Pinterest user ────────────────────────────────────────────

/** Pinterest usernames are letters, digits and underscores; dots and dashes are tolerated. */
const USERNAME = /^[A-Za-z0-9_.-]{1,64}$/;
/** Inline JSON blobs that carry the page context (the same ones the hook's SSR reader scans). */
const CONTEXT_SCRIPTS = 'script[id^="__PWS"], script[type="application/json"]';
/** Pinterest's blobs are large; anything bigger than this is not worth parsing for a username. */
const MAX_SCRIPT_CHARS = 8_000_000;
const MAX_SCRIPTS = 12;
const PROFILE_LINKS =
  '[data-test-id="header-profile"] a[href], a[data-test-id="header-profile"][href]';

function usernameOf(context: unknown): string | null {
  if (!isRecord(context) || !isRecord(context.user)) return null;
  const user = context.user;
  if (user.is_auth === false || user.isAuth === false) return null;
  return typeof user.username === 'string' && USERNAME.test(user.username) ? user.username : null;
}

/** `context.user.username` of a page-context blob, at the places Pinterest puts it. */
export function viewerFromPageData(json: unknown): string | null {
  if (!isRecord(json)) return null;
  const props = isRecord(json.props) ? json.props : null;
  const redux = props && isRecord(props.initialReduxState) ? props.initialReduxState : null;
  for (const context of [json.context, props?.context, redux?.context]) {
    const name = usernameOf(context);
    if (name) return name;
  }
  return null;
}

function viewerFromProfileLink(doc: Document): string | null {
  const link = doc.querySelector<HTMLAnchorElement>(PROFILE_LINKS);
  const href = link?.getAttribute('href') ?? '';
  const match = /^\/([^/?#]+)\/?$/.exec(href);
  return match && USERNAME.test(match[1]) ? match[1] : null;
}

/** The signed-in user of a Pinterest page: the page-context JSON, else the header profile link. */
export function readPinterestViewer(doc: Document): string | null {
  let scanned = 0;
  for (const script of doc.querySelectorAll(CONTEXT_SCRIPTS)) {
    if (++scanned > MAX_SCRIPTS) break;
    const text = script.textContent ?? '';
    if (!text || text.length > MAX_SCRIPT_CHARS || !text.includes('"username"')) continue;
    let json: unknown;
    try {
      json = JSON.parse(text);
    } catch {
      continue;
    }
    const name = viewerFromPageData(json);
    if (name) return name;
  }
  return viewerFromProfileLink(doc);
}

/**
 * Reads the viewer once it is known and caches it for the document (the inline context does not
 * change without a reload). While unknown, it re-reads at most every `retryMs`, because the
 * header link appears only once the page has rendered.
 */
export function createViewerReader(
  doc: Document,
  now: () => number = Date.now,
  retryMs = 5000,
): () => string | null {
  let viewer: string | null = null;
  let lastTry = -Infinity;
  return () => {
    if (viewer || now() - lastTry < retryMs) return viewer;
    // While the document is still parsing, the inline blobs may not exist yet: try again freely.
    if (doc.readyState !== 'loading') lastTry = now();
    viewer = readPinterestViewer(doc);
    return viewer;
  };
}
