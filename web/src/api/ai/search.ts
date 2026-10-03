import type { AiChatMessage, AiSearchApi, SourceScope } from '@ui/api/ai';
import type { ChatSearchResult } from '../../../../types/electron-api';
import type { EventStream } from '../events';
import { ApiError, isApiError, type Http } from '../http';
import { toPost } from '../mapping';
import type { components } from '../schema';
type Schemas = components['schemas'];
const ROOT = '/api/v1/search';
const scope = (source: SourceScope = 'all') => (source === 'web' ? 'sites' : source);
const strings = (v: unknown): v is string[] =>
  Array.isArray(v) && v.length <= 50 && v.every((t) => typeof t === 'string' && t.length <= 500);
const broken = () => new ApiError(502, 'unavailable');
export function chatHistory(messages: AiChatMessage[]): AiChatMessage[] {
  const turns = messages
    .filter((m) => m.role === 'user' || m.role === 'assistant')
    .slice(-8)
    .map((m) => ({ ...m }));
  let units = turns.reduce((n, m) => n + m.content.length, 0);
  while (units > 24_000 && turns.length > 1) units -= turns.shift()!.content.length;
  if (units > 24_000) {
    let start = turns[0].content.length - 24_000;
    const first = turns[0].content.charCodeAt(start);
    if (first >= 0xdc00 && first <= 0xdfff) start += 1;
    turns[0].content = turns[0].content.slice(start);
  }
  return turns;
}

// POST answers are ephemeral: never reconnect or repeat a paid request. UTF-8,
// CR/LF boundaries and SSE comments may span arbitrary network chunks.
export async function readChatEvents(
  response: Response,
  accept: (name: string, value: unknown) => void,
  complete: () => boolean = () => false,
): Promise<void> {
  if (!response.body || !response.headers.get('content-type')?.startsWith('text/event-stream'))
    throw broken();
  const reader = response.body.getReader();
  const decoder = new TextDecoder();
  let buffer = '',
    name = '',
    data: string[] = [],
    frameSize = 0;
  function line(text: string) {
    frameSize += text.length;
    if (frameSize > 256_000) throw broken();
    if (!text) {
      if (data.length) {
        let value: unknown;
        try {
          value = JSON.parse(data.join('\n'));
        } catch {
          throw broken();
        }
        accept(name, value);
      }
      name = '';
      data = [];
      frameSize = 0;
    } else if (!text.startsWith(':')) {
      const colon = text.indexOf(':');
      const field = colon < 0 ? text : text.slice(0, colon);
      let value = colon < 0 ? '' : text.slice(colon + 1);
      if (value.startsWith(' ')) value = value.slice(1);
      if (field === 'event') name = value;
      if (field === 'data') data.push(value);
    }
  }
  try {
    while (true) {
      const { value, done } = await reader.read();
      buffer += done ? decoder.decode() : decoder.decode(value, { stream: true });
      if (buffer.length > 256_000) throw broken();
      let match: RegExpExecArray | null;
      while ((match = /\r\n|\r|\n/.exec(buffer))) {
        if (!done && match[0] === '\r' && match.index === buffer.length - 1) break;
        line(buffer.slice(0, match.index));
        if (complete()) return;
        buffer = buffer.slice(match.index + match[0].length);
      }
      if (done) {
        if (buffer || data.length) throw broken();
        return;
      }
    }
  } finally {
    await reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}

export function createAiSearchApi(http: Http, events: Pick<EventStream, 'on'>): AiSearchApi {
  const tokens = new Set<(payload: unknown) => void>();
  let active: { controller: AbortController; id?: string } | null = null;
  let selected: string | undefined;
  async function search(
    tags: string[],
    q: string,
    mode: 'and' | 'or',
    limit: number,
    offset: number,
    source?: SourceScope,
  ) {
    const params = new URLSearchParams({
      tagMode: mode,
      scope: scope(source),
      limit: String(Math.min(200, Math.max(1, limit))),
    });
    tags.forEach((t) => params.append('tags', t));
    if (q.trim()) params.set('q', q);
    const posts: Shelfy.Post[] = [];
    let total = 0,
      skip = Math.max(0, offset);
    do {
      const page = await http.get<Schemas['SearchPage']>(ROOT, params);
      total = page.total;
      posts.push(...page.items.slice(skip, skip + limit - posts.length).map(toPost));
      skip = Math.max(0, skip - page.items.length);
      if (!page.nextCursor || posts.length >= limit) break;
      params.set('cursor', page.nextCursor);
    } while (posts.length < limit);
    return { posts, total };
  }
  async function providers() {
    const [rows, settings] = await Promise.all([
      http.get<Schemas['ProviderSummary'][]>('/api/v1/me/providers'),
      http.get<Schemas['Settings']>('/api/v1/me/settings'),
    ]);
    const available = rows.filter((p) => p.configured && p.models.text);
    const chosen =
      selected ??
      settings.aiRouting.chat ??
      available.find((p) => p.id === 'operator')?.id ??
      available[0]?.id;
    // The displayed account route is also the request override. Consent remains
    // enforced by the server; reading provider summaries never grants it.
    selected = available.some((p) => p.id === chosen) ? chosen : undefined;
    return available.map((p) => ({ id: p.id, name: p.label, selected: p.id === chosen }));
  }
  const api: AiSearchApi = {
    byTags: (tags, mode = 'or', limit = 60, offset = 0, source) =>
      search(tags, '', mode, limit, offset, source),
    hybrid: (tags, q, mode = 'or', limit = 60, offset = 0, source) =>
      search(tags, q, mode, limit, offset, source),
    byText: (q, limit = 60, offset = 0, source) => search([], q, 'or', limit, offset, source),
    async chat(messages, activeTags = [], options) {
      active?.controller.abort();
      const run = { controller: new AbortController(), id: undefined as string | undefined };
      active = run;
      let text = '',
        result: ChatSearchResult | undefined;
      try {
        const response = await http.send(
          'POST',
          `${ROOT}/chat`,
          {
            messages: chatHistory(messages),
            activeTags,
            scope: scope(options?.source),
            ...(selected ? { providerId: selected } : {}),
          },
          {
            signal: run.controller.signal,
            reauthenticate: false,
            headers: { Accept: 'text/event-stream' },
          },
        );
        await readChatEvents(
          response,
          (name, raw) => {
            if (active !== run || run.controller.signal.aborted)
              throw new DOMException('Cancelled', 'AbortError');
            if (!raw || typeof raw !== 'object' || result) throw broken();
            const value = raw as Record<string, unknown>;
            if (
              name === 'run' &&
              !run.id &&
              typeof value.runId === 'string' &&
              value.runId.length <= 128
            ) {
              run.id = value.runId;
              tokens.forEach((cb) => cb({ start: true, runId: run.id }));
            } else if (name === 'token' && run.id && typeof value.text === 'string') {
              text += value.text;
              if (text.length > 1_000_000) throw broken();
              tokens.forEach((cb) => cb({ runId: run.id, token: value.text }));
            } else if (name === 'result' && run.id) {
              const tags = value.tags as Record<string, unknown> | undefined;
              if (
                !tags ||
                !strings(tags.general) ||
                !strings(tags.specific) ||
                !strings(value.keywords) ||
                !strings(value.remove) ||
                typeof value.modelUsed !== 'boolean'
              )
                throw broken();
              if (
                !value.modelUsed &&
                value.replyCode !== 'suggestions' &&
                value.replyCode !== 'no_matches'
              )
                throw broken();
              result = {
                reply: value.modelUsed ? text : '',
                replyCode: value.modelUsed
                  ? undefined
                  : (value.replyCode as 'suggestions' | 'no_matches'),
                tagsToAdd: [...tags.general, ...tags.specific],
                tagsToRemove: value.remove,
                keywordsToAdd: value.keywords,
                tagGroups: {
                  broad: tags.general,
                  specific: tags.specific,
                  keywords: value.keywords,
                },
                modelUsed: value.modelUsed,
              };
            } else if (name === 'error' && value.code === 'cancelled')
              throw new DOMException('Cancelled', 'AbortError');
            else throw broken();
          },
          () => result !== undefined,
        );
        if (!result) throw broken();
        return result;
      } finally {
        if (active === run) active = null;
        run.controller.abort();
      }
    },
    async cancelChat() {
      const run = active;
      active = null;
      run?.controller.abort();
      if (run?.id) {
        try {
          await http.send('POST', `${ROOT}/chat/${encodeURIComponent(run.id)}/cancel`);
        } catch (err) {
          if (!isApiError(err, 'not_found')) throw err;
        }
      }
      return { ok: true };
    },
    onToken(cb) {
      tokens.add(cb);
      return () => tokens.delete(cb);
    },
    onResultsStale: (cb) => events.on('posts.changed', () => cb()),
    onSessionEnded: (cb) => http.onUnauthorized(cb),
    getProviders: providers,
    async selectProvider(id) {
      const rows = await providers();
      if (!rows.some((p) => p.id === id)) throw new ApiError(422, 'validation_failed');
      selected = id;
      return providers();
    },
    async getModelStatus() {
      return { ready: (await providers()).some((p) => p.selected), downloading: false };
    },
    onModelProgress: (cb) => events.on('provider.status', () => cb({ progress: 1 })),
  };
  http.onUnauthorized(() => {
    active?.controller.abort();
    active = null;
    selected = undefined;
  });
  return api;
}
