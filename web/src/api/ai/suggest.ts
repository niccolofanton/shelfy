import type { AiSuggestApi } from '@ui/api/ai/suggest';
import type { Http } from '../http';

// The server owns the 24-hour vocabulary-aware cache and provider fallback.
export function createSuggestApi(http: Http): AiSuggestApi {
  return {
    async suggest(q, options) {
      return (
        await http.send('POST', '/api/v1/search/suggest', {
          q,
          scope: options?.scope === 'web' ? 'sites' : (options?.scope ?? 'all'),
        })
      ).json();
    },
  };
}
