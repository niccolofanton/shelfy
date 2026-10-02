// Crash reports of the error boundaries (src/components/ErrorBoundary.tsx) to
// `POST /api/v1/client-errors` (plan §2.19 Robustness, §1.2 #11).
//
// A report has technical fields only: the view, the error's name, message and
// stacks, the route pattern (`/p/:key`, never the URL) and the build. It never
// carries a post, and the server refuses unknown fields. Text is clipped to the
// server's limits and identifiers it would refuse are cleaned, so a report is
// never lost to a 422.
//
// Throttling: at most 10 reports a minute, and the same error once a minute (a
// view that crashes on every render must not flood the log). Sending is fire
// and forget: a report that fails is dropped, never reported in turn.
import type { ViewErrorReport } from '@ui/api/ShelfyClient';
import type { Http } from './http';
import type { components } from './schema';

export type ClientErrorReport = components['schemas']['ClientErrorReport'];

export const CLIENT_ERRORS_PATH = '/api/v1/client-errors';

// The server's limits (crates/server/src/routes/client_errors.rs).
const NAME_CHARS = 100;
const MESSAGE_CHARS = 1_000;
const STACK_CHARS = 8_000;
const VIEW_CHARS = 64;
const ROUTE_CHARS = 200;
const VIEW_RE = /^[A-Za-z0-9._:-]{1,64}$/;
const VERSION_RE = /^[A-Za-z0-9.+\-_]{1,64}$/;

export interface ErrorReporterOptions {
  // The route pattern of the page on screen (`/p/:key`).
  route?: () => string | null | undefined;
  // The web app's build.
  clientVersion?: string;
  // Clock, for `occurredAt` and the throttle.
  now?: () => number;
  // At most `limit` reports per `windowMs`; the same error once per window.
  limit?: number;
  windowMs?: number;
}

// Lone UTF-16 surrogates become U+FFFD: the server's JSON parser refuses them.
function wellFormed(text: string): string {
  return text.replace(
    /[\ud800-\udbff](?![\udc00-\udfff])|(?<![\ud800-\udbff])[\udc00-\udfff]/g,
    '\ufffd',
  );
}

// The first `max` UTF-16 units of `text`, without cutting a surrogate pair:
// never more than `max` characters, which is how the server counts.
function clip(text: string, max: number): string {
  const clean = wellFormed(text);
  if (clean.length <= max) return clean;
  const last = clean.charCodeAt(max - 1);
  return clean.slice(0, last >= 0xd800 && last <= 0xdbff ? max - 1 : max);
}

// The view's name as the server accepts it: letters, digits, `.`, `_`, `-`, `:`.
function viewName(view: string): string {
  if (VIEW_RE.test(view)) return view;
  const clean = view.replace(/[^A-Za-z0-9._:-]+/g, '-').slice(0, VIEW_CHARS);
  return clean || 'unknown';
}

function isRoutePattern(route: string): boolean {
  return (
    route.startsWith('/') &&
    route.length <= ROUTE_CHARS &&
    !/[?#]/.test(route) &&
    !/[\u0000-\u001f\u007f]/.test(route)
  );
}

// The report the server takes, from what a boundary caught.
export function toClientErrorReport(
  report: ViewErrorReport,
  context: { occurredAt: number; route?: string | null; clientVersion?: string },
): ClientErrorReport {
  const { error } = report;
  const isError = error instanceof Error;
  const message = isError ? error.message : typeof error === 'string' ? error : typeof error;
  const body: ClientErrorReport = {
    view: viewName(report.view),
    message: clip(String(message ?? ''), MESSAGE_CHARS),
    occurredAt: Math.floor(context.occurredAt),
  };
  if (isError && error.name) body.name = clip(error.name, NAME_CHARS);
  if (isError && error.stack) body.stack = clip(error.stack, STACK_CHARS);
  if (report.componentStack) body.componentStack = clip(report.componentStack, STACK_CHARS);
  if (context.route && isRoutePattern(context.route)) body.route = context.route;
  if (context.clientVersion && VERSION_RE.test(context.clientVersion)) {
    body.clientVersion = context.clientVersion;
  }
  return body;
}

// The web client's `reportError`.
export function createErrorReporter(
  http: Http,
  options: ErrorReporterOptions = {},
): (report: ViewErrorReport) => void {
  const now = options.now ?? Date.now;
  const limit = options.limit ?? 10;
  const windowMs = options.windowMs ?? 60_000;
  const recent: { at: number; signature: string }[] = [];

  return (report) => {
    let body: ClientErrorReport;
    const at = now();
    try {
      body = toClientErrorReport(report, {
        occurredAt: at,
        route: options.route?.(),
        clientVersion: options.clientVersion,
      });
    } catch {
      return;
    }
    while (recent.length && at - recent[0].at >= windowMs) recent.shift();
    const signature = `${body.view}\n${body.name ?? ''}\n${body.message}`;
    if (recent.length >= limit || recent.some((r) => r.signature === signature)) return;
    recent.push({ at, signature });
    // `keepalive`: the report survives a page that is closing.
    http.send('POST', CLIENT_ERRORS_PATH, body, { keepalive: true }).catch(() => {});
  };
}
