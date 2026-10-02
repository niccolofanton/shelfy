// AI search: conversational chat plus tag/text/hybrid gallery search (web port
// plan §2.19, App. A's `search:*` row). Maps to `GET /search`, `POST
// /search/chat` (SSE, P3-14) and `GET /me/providers` (P3-09/P3-19).
import type { ChatSearchResult, OkResult, PostSearchResult } from '../../../types/electron-api';

export type SourceScope = 'all' | 'web' | 'social';
export type TagMatchMode = 'and' | 'or';

export interface AiChatMessage {
  role: 'user' | 'assistant';
  content: string;
}

// A configured chat/search provider (desktop: the local model or the owner's
// remote node; web: `GET /me/providers`, P3-09/P3-18).
export interface AiSearchProvider {
  id: string;
  name: string;
  selected: boolean;
  vision?: boolean;
}

export interface AiSearchApi {
  // `GET /search` ranked by tags (P3-05), text (P1-05) or both (P3-07's pools).
  byTags(
    tags: string[],
    mode?: TagMatchMode,
    limit?: number,
    offset?: number,
    source?: SourceScope,
  ): Promise<PostSearchResult>;
  hybrid(
    tags: string[],
    textQuery: string,
    mode?: TagMatchMode,
    limit?: number,
    offset?: number,
    source?: SourceScope,
  ): Promise<PostSearchResult>;
  byText(
    query: string,
    limit?: number,
    offset?: number,
    source?: SourceScope,
  ): Promise<PostSearchResult>;

  // `POST /search/chat`: a cancellable stream with a run id, then a result
  // (the sentinel-parsed tags/keywords, or the deterministic fallback).
  chat(messages: AiChatMessage[], activeTags?: string[]): Promise<ChatSearchResult>;
  cancelChat(): Promise<OkResult>;
  onToken(cb: (payload: unknown) => void): () => void;

  // Re-run the active search when a stored preview lands underneath it
  // (desktop: a filtered `interceptor:newPosts` signal; dropped once previews
  // stream from the CAS instead of a background cache, P1-19).
  onResultsStale(cb: () => void): () => void;

  // The chat provider toggle (AI-43) and the readiness of the model it may
  // need (desktop: the local VLM shares this gate with cataloging; web:
  // `GET /me/providers`, P3-09/P3-18's richer settings page is additive to
  // this — Assumption, see the P3-08 report).
  getProviders(): Promise<AiSearchProvider[]>;
  selectProvider(id: string): Promise<AiSearchProvider[]>;
  getModelStatus(): Promise<unknown>;
  onModelProgress(cb: (progress: unknown) => void): () => void;
  downloadModel(): Promise<unknown>;
}
