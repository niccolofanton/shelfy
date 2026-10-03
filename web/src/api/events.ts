// The realtime stream of the web app: `GET /api/v1/events` (plan §2.10, D7),
// read with an EventSource that every subscriber of the tab shares. It opens
// with the first subscriber and closes after the last one.
//
// Reconnecting. On any error the EventSource is closed and a new one opens
// after a backoff: 1 s, doubling up to 30 s, each wait drawn between half and
// all of it (jitter), so tabs that lost the server together do not come back
// together. The backoff resets once a stream says `hello`, and coming back
// online reconnects at once. The browser's own reconnect is never used: it
// has no backoff and gives up for good on an HTTP error.
//
// Resuming. The new EventSource asks for the events after the last id this
// stream received (`?lastEventId=`). The server replays them after `hello`,
// or sends `resync` when it no longer has them: subscribers then reload
// everything they show. `hello` of a resumed stream has no id, so the
// position never moves back.
//
// Signing out. EventSource hides the HTTP status: a stream refused with 401
// looks like a network error. `onConnectFailed` runs whenever a connection
// fails before it opened; the web client checks the session there, so an
// expired one signs the app out instead of retrying forever.
import type { components } from './schema';

type Schemas = components['schemas'];

// Every event of the stream with its payload, from the OpenAPI document.
export type ServerEvent = Schemas['ServerEvent'];
export type ServerEventName = ServerEvent['event'];
export type ServerEventData<N extends ServerEventName> = Extract<ServerEvent, { event: N }>['data'];

export const EVENTS_PATH = '/api/v1/events';

// The event names the stream listens for: every name of `ServerEvent`. A name
// the generated union gains fails the typecheck here until it is listed.
const EVENT_NAMES = Object.keys({
  hello: true,
  resync: true,
  'posts.changed': true,
  'stats.changed': true,
  'job.updated': true,
  notification: true,
  'extension.status': true,
  'provider.status': true,
} satisfies Record<ServerEventName, true>) as ServerEventName[];

// The part of EventSource the stream uses; tests pass a fake.
export interface EventSourceLike {
  onopen: ((event: Event) => void) | null;
  onerror: ((event: Event) => void) | null;
  addEventListener(type: string, listener: (event: MessageEvent) => void): void;
  close(): void;
}
export type EventSourceConstructor = new (url: string) => EventSourceLike;

export interface EventStreamOptions {
  // The stream's URL. Default: `/api/v1/events`, every topic.
  url?: string;
  // The EventSource to use; default: the browser's. Without one (an old
  // browser, a test without a fake) the stream never connects.
  EventSource?: EventSourceConstructor | null;
  // The first reconnect wait and the longest one, in ms.
  minDelayMs?: number;
  maxDelayMs?: number;
  // Source of the jitter (tests make it deterministic).
  random?: () => number;
  // Called when a connection fails before it opened (see "Signing out").
  onConnectFailed?: () => void;
  // Where `online` is listened for; default: `window`, when there is one.
  target?: Pick<EventTarget, 'addEventListener' | 'removeEventListener'> | null;
}

// `closed`: no subscriber. `connecting`: an EventSource is opening.
// `open`: it is connected. `waiting`: a reconnect is scheduled.
export type EventStreamState = 'closed' | 'connecting' | 'open' | 'waiting';

export interface EventStream {
  // Subscribes to one event of the stream; returns the unsubscribe function.
  on<N extends ServerEventName>(name: N, listener: (data: ServerEventData<N>) => void): () => void;
  readonly state: EventStreamState;
  // The id of the last event received: where a new connection resumes.
  readonly lastEventId: string | null;
}

export const MIN_RECONNECT_MS = 1_000;
export const MAX_RECONNECT_MS = 30_000;

// The wait before reconnect attempt `attempt` (0 for the first): the
// exponential delay, capped, then drawn between half and all of it.
export function reconnectDelay(
  attempt: number,
  random: () => number = Math.random,
  minDelayMs = MIN_RECONNECT_MS,
  maxDelayMs = MAX_RECONNECT_MS,
): number {
  const ceiling = Math.min(maxDelayMs, minDelayMs * 2 ** Math.min(attempt, 30));
  return Math.round(ceiling / 2 + random() * (ceiling / 2));
}

// `url` with the resume point as `lastEventId`.
function resumeUrl(url: string, lastEventId: string | null): string {
  if (!lastEventId) return url;
  return `${url}${url.includes('?') ? '&' : '?'}lastEventId=${encodeURIComponent(lastEventId)}`;
}

function browserEventSource(): EventSourceConstructor | null {
  return typeof EventSource === 'function' ? EventSource : null;
}

export function createEventStream(options: EventStreamOptions = {}): EventStream {
  const url = options.url ?? EVENTS_PATH;
  const Source = options.EventSource === undefined ? browserEventSource() : options.EventSource;
  const random = options.random ?? Math.random;
  const minDelay = options.minDelayMs ?? MIN_RECONNECT_MS;
  const maxDelay = options.maxDelayMs ?? MAX_RECONNECT_MS;
  const target =
    options.target === undefined ? (typeof window === 'undefined' ? null : window) : options.target;

  const listeners = new Map<ServerEventName, Set<(data: unknown) => void>>();
  let subscribers = 0;
  let source: EventSourceLike | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let attempt = 0;
  let state: EventStreamState = 'closed';
  let lastEventId: string | null = null;

  function dispatch(name: ServerEventName, event: MessageEvent): void {
    // Events without an `id:` (a resumed `hello`) keep the previous position.
    if (typeof event.lastEventId === 'string' && event.lastEventId) lastEventId = event.lastEventId;
    let data: unknown;
    try {
      data = JSON.parse(String(event.data));
    } catch {
      console.error(`[events] ${name}: the payload is not JSON`);
      return;
    }
    if (name === 'hello') attempt = 0;
    for (const listener of [...(listeners.get(name) ?? [])]) {
      try {
        listener(data);
      } catch (err) {
        console.error(`[events] a ${name} listener failed:`, err);
      }
    }
  }

  function schedule(): void {
    state = 'waiting';
    const delay = reconnectDelay(attempt, random, minDelay, maxDelay);
    attempt += 1;
    timer = setTimeout(connect, delay);
  }

  function connect(): void {
    timer = null;
    if (!Source || subscribers === 0) return;
    state = 'connecting';
    let es: EventSourceLike;
    try {
      es = new Source(resumeUrl(url, lastEventId));
    } catch (err) {
      console.error('[events] cannot open the stream:', err);
      schedule();
      return;
    }
    source = es;
    let opened = false;
    es.onopen = () => {
      if (source !== es) return;
      opened = true;
      state = 'open';
    };
    es.onerror = () => {
      if (source !== es) return;
      es.close();
      source = null;
      if (!opened) options.onConnectFailed?.();
      // The failure check may have closed the stream (a signed-out app
      // unsubscribes everything).
      if (subscribers > 0 && !timer) schedule();
    };
    for (const name of EVENT_NAMES) {
      es.addEventListener(name, (event) => {
        if (source === es) dispatch(name, event);
      });
    }
  }

  function close(): void {
    if (timer) clearTimeout(timer);
    timer = null;
    source?.close();
    source = null;
    attempt = 0;
    state = 'closed';
    target?.removeEventListener('online', onOnline);
  }

  // Back online: reconnect now instead of at the end of a long wait.
  function onOnline(): void {
    if (state !== 'waiting') return;
    if (timer) clearTimeout(timer);
    attempt = 0;
    connect();
  }

  return {
    on(name, listener) {
      let set = listeners.get(name);
      if (!set) listeners.set(name, (set = new Set()));
      // A wrapper per subscription: the same function may subscribe twice.
      const entry = (data: unknown): void => listener(data as ServerEventData<typeof name>);
      set.add(entry);
      subscribers += 1;
      if (subscribers === 1) {
        target?.addEventListener('online', onOnline);
        connect();
      }
      let subscribed = true;
      return () => {
        if (!subscribed) return;
        subscribed = false;
        set.delete(entry);
        subscribers -= 1;
        if (subscribers === 0) close();
      };
    },
    get state() {
      return state;
    },
    get lastEventId() {
      return lastEventId;
    },
  };
}
