// Read the actual private desktop builders without changing its public API.
import fs from 'node:fs';
import ts from 'typescript';
import {
  userPrompt,
  systemPrompt,
  responseSchema,
  task,
  maxTokens,
  type TaskName,
} from '../../shared/ai/prompts';
import type { GoldenSet } from './lib';

const source = fs.readFileSync(new URL('../../electron/analyzer.ts', import.meta.url), 'utf8');
const tree = ts.createSourceFile('analyzer.ts', source, ts.ScriptTarget.Latest, true);
function desktop(name: string): (...args: unknown[]) => string {
  const declaration = tree.statements.find(
    (s) => ts.isFunctionDeclaration(s) && s.name?.text === name,
  );
  if (!declaration) throw new Error(`Missing desktop builder ${name}`);
  const js = ts.transpile(declaration.getText(tree), { target: ts.ScriptTarget.ES2022 });
  return new Function('userPrompt', `${js}\nreturn ${name};`)(userPrompt);
}
const refine = desktop('buildRefinePrompt');
const aliases = desktop('buildAliasPrompt');
const request = (name: TaskName, user: string, count: number) => ({
  system: systemPrompt(name),
  user,
  schema: responseSchema(name),
  temperature: task(name).temperature,
  maxTokens: maxTokens(name, count),
});
const v = (norm: string, form = norm) => ({ norm, form, count: 2 });
const refineInputs = [
  { tags: [], neighbors: {} },
  {
    tags: ['lamp', 'brass', 'résumé 東京'],
    neighbors: { lamp: ['brass'], brass: [], 'résumé 東京': ['lamp', 'brass'] },
  },
  { tags: ['literal {{tags}}', 'line\nbreak'], neighbors: { 'literal {{tags}}': ['quote "x"'] } },
  { tags: Array.from({ length: 400 }, (_, i) => `tag ${i}`), neighbors: {} },
];
const aliasInputs = [
  [[], []],
  [
    [v('lamp', 'Lamp'), v('brass', ''), v('', '')],
    [v('light', 'Luce'), v('résumé 東京')],
  ],
  [[v('literal', '{{vocabulary}}\nquote "x"')], [v('lamp')]],
  [
    Array.from({ length: 100 }, (_, i) => v(`candidate ${i}`)),
    Array.from({ length: 300 }, (_, i) => v(`canonical ${i}`)),
  ],
];
const sets: GoldenSet[] = [
  {
    name: 'ai/taxonomy-prompts/refine',
    source: 'electron/analyzer.ts#buildRefinePrompt',
    generator: 'scripts/golden/ai-taxonomy-prompts.ts',
    build: () =>
      refineInputs.map((group, i) => ({
        id: `refine-${i}`,
        args: [group],
        output: request('cluster_refine', refine(group), group.tags.length),
      })),
  },
  {
    name: 'ai/taxonomy-prompts/aliases',
    source: 'electron/analyzer.ts#buildAliasPrompt',
    generator: 'scripts/golden/ai-taxonomy-prompts.ts',
    build: () =>
      aliasInputs.map(([batch, vocab], i) => ({
        id: `aliases-${i}`,
        args: [batch, vocab],
        output: request('aliases', aliases(batch, vocab), batch.length),
      })),
  },
];
export default sets;
