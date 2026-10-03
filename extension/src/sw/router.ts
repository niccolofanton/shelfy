// The worker's message router. Two doors, each with its sender rule:
//
// - chrome.runtime.onMessage (inside the extension): every route says who may call it — a
//   content script (`content`: a tab of this extension's content scripts; the handler checks
//   the tab further) or an extension page (`page`: the side panel, at a chrome-extension:// URL
//   of this extension). Messages from anyone else are not answered.
// - chrome.runtime.onMessageExternal (the Shelfy SPA, externally_connectable, C9): answered only
//   when `sender.origin` is exactly the Shelfy origin of this build.
//
// Routes register here (`internal`, `external`); later lanes add theirs (P2-13, P2-15, P2-17).

import {
  isRecord,
  parseExternalMessage,
  type ExternalAnswer,
  type ExternalRequest,
  type ExternalType,
  type JsonRecord,
} from '../shared/protocol';

export interface Sender {
  id?: string;
  url?: string;
  origin?: string;
  frameId?: number;
  /** The sender document (Chrome 106+): the sync controller's MAIN-world target (P2-13). */
  documentId?: string;
  tab?: { id?: number; url?: string };
}

export type SenderRole = 'content' | 'page';

/** A message from this extension's content script: in a tab, on a web page. */
export function isContentSender(sender: Sender, extensionId: string): boolean {
  return (
    sender.id === extensionId &&
    typeof sender.tab?.id === 'number' &&
    !(sender.url ?? '').startsWith('chrome-extension://')
  );
}

/**
 * A message from one of this extension's own pages (the side panel, also when opened in a tab):
 * the browser reports its chrome-extension:// URL, which a content script never has.
 */
export function isPageSender(sender: Sender, extensionId: string): boolean {
  return (
    sender.id === extensionId &&
    typeof sender.url === 'string' &&
    sender.url.startsWith(`chrome-extension://${extensionId}/`)
  );
}

/** A message from the Shelfy SPA of this build's origin. */
export function isShelfySender(sender: Sender, origin: string): boolean {
  if (sender.origin !== origin) return false;
  // A page URL, when given, must be on the origin too (belt and braces).
  return sender.url === undefined || sender.url.startsWith(`${origin}/`);
}

export type InternalHandler = (message: JsonRecord, sender: Sender) => Promise<unknown>;
export type ExternalHandler = (request: ExternalRequest, sender: Sender) => Promise<unknown>;

type SendResponse = (response?: unknown) => void;

export class Router {
  private readonly internalRoutes = new Map<
    string,
    { from: SenderRole; handle: InternalHandler }
  >();
  private readonly externalRoutes = new Map<ExternalType, ExternalHandler>();

  constructor(
    private readonly extensionId: string,
    private readonly origin: string,
    private readonly onError: (where: string, err: unknown) => void = () => undefined,
  ) {}

  internal(kind: string, from: SenderRole, handle: InternalHandler): this {
    if (this.internalRoutes.has(kind)) throw new Error(`route ${kind} is already registered`);
    this.internalRoutes.set(kind, { from, handle });
    return this;
  }

  external(type: ExternalType, handle: ExternalHandler): this {
    if (this.externalRoutes.has(type)) throw new Error(`route ${type} is already registered`);
    this.externalRoutes.set(type, handle);
    return this;
  }

  private allowed(role: SenderRole, sender: Sender): boolean {
    return role === 'content'
      ? isContentSender(sender, this.extensionId)
      : isPageSender(sender, this.extensionId);
  }

  /** The chrome.runtime.onMessage listener. */
  readonly onMessage = (message: unknown, sender: Sender, sendResponse: SendResponse): boolean => {
    if (!isRecord(message) || typeof message.kind !== 'string') return false;
    const route = this.internalRoutes.get(message.kind);
    if (!route || !this.allowed(route.from, sender)) return false;
    route.handle(message, sender).then(
      (answer) => sendResponse(answer),
      (err: unknown) => {
        this.onError(message.kind as string, err);
        sendResponse({ ok: false, code: 'internal' });
      },
    );
    return true;
  };

  /** The chrome.runtime.onMessageExternal listener. */
  readonly onMessageExternal = (
    message: unknown,
    sender: Sender,
    sendResponse: SendResponse,
  ): boolean => {
    if (!isShelfySender(sender, this.origin)) return false;
    const request = parseExternalMessage(message);
    if (!request) {
      sendResponse({ ok: false, code: 'bad_request' } satisfies ExternalAnswer);
      return false;
    }
    const handle = this.externalRoutes.get(request.type);
    if (!handle) {
      sendResponse({ ok: false, code: 'unsupported' } satisfies ExternalAnswer);
      return false;
    }
    handle(request, sender).then(
      (answer) => sendResponse(answer),
      (err: unknown) => {
        this.onError(request.type, err);
        sendResponse({ ok: false, code: 'internal' } satisfies ExternalAnswer);
      },
    );
    return true;
  };
}
