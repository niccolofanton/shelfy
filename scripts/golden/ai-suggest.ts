// The shared prompt functions are the exact calls of desktop expandSearchQuery.
import { systemPrompt, userPrompt, responseSchema, task, maxTokens } from '../../shared/ai/prompts';
import type { GoldenSet } from './lib';

const set: GoldenSet = {
  name: 'ai/suggest/request',
  source: 'electron/analyzer.ts#expandSearchQuery',
  generator: 'scripts/golden/ai-suggest.ts',
  build: () =>
    ['', 'AirPods', '  città architecture 🎧  ', '\uFEFFglass desk lamp\u00A0', '{{query}}'].map(
      (query, i) => ({
        id: `query-${i}`,
        args: [query],
        output: {
          system: systemPrompt('suggest'),
          user: userPrompt('suggest', { query: query.trim() }),
          schema: responseSchema('suggest'),
          temperature: task('suggest').temperature,
          maxTokens: maxTokens('suggest'),
        },
      }),
    ),
};
export default [set];
