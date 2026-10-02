// Gallery search-suggestion chips (web port plan §2.19, AI-41). Maps to
// `POST /search/suggest` (P3-15); the client debounces 600 ms and caches for
// 24h (P3-21 wires this into src/views/Gallery.tsx — untouched by P3-08).
import type { SearchSuggestResult } from '../../../types/electron-api';

export interface AiSuggestApi {
  suggest(query: string): Promise<SearchSuggestResult>;
}
