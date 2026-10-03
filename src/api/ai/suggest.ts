// Gallery search-suggestion chips (web port plan §2.19, AI-41). Maps to
// `POST /search/suggest`: Gallery debounces 600 ms; the server owns its
// scope/vocabulary-aware 24-hour cache. Desktop retains its IPC transport.
import type { SourceScope } from './search';
import type { SearchSuggestResult } from '../../../types/electron-api';

export interface AiSuggestApi {
  suggest(query: string, options?: { scope?: SourceScope }): Promise<SearchSuggestResult>;
}
