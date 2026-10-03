// The AI seam (web port plan §2.19, App. A): one interface per area, each
// gated by its own ShelfyCapabilities flag (aiQueue, aiTags, aiChat,
// aiSuggest, dictation — `ai` itself reads as "any of them", P3-08 Assumption
// G3-21). Views and hooks reach these only through `useShelfy().ai`, never
// `window.electronAPI` directly.
import type { AiDictationApi } from './dictation';
import type { AiQueueApi } from './queue';
import type { AiSearchApi } from './search';
import type { AiSuggestApi } from './suggest';
import type { AiTagsApi } from './tags';
import type { WebAiQueueApi } from './webQueue';

export type { AiQueueProgress, AiModelProgress, AiModelStatus, AiQueueApi } from './queue';
export type { AiClusterProgress, AiAliasProgress, AiTagsApi } from './tags';
export type {
  AiChatMessage,
  AiSearchProvider,
  SourceScope,
  TagMatchMode,
  AiSearchApi,
} from './search';
export type { AiSuggestApi } from './suggest';
export type { AiDictationApi } from './dictation';

export interface AiApi {
  webQueue?: WebAiQueueApi;
  queue?: AiQueueApi;
  tags?: AiTagsApi;
  search?: AiSearchApi;
  suggest?: AiSuggestApi;
  dictation?: AiDictationApi;
}
