import fs from 'node:fs';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { normalizeCatalogOutput, type RawCatalog } from '../../shared/ai/catalog';
import { promptDigest } from '../../scripts/ai-eval/score';
import { scoreCase, type GroundTruth, type ModelOutput } from '../../scripts/extract-eval/score';

const root = path.resolve('scripts/extract-eval');
const read = <T>(file: string): T =>
  JSON.parse(fs.readFileSync(path.join(root, file), 'utf8')) as T;
const contract = read<{ digest: string; cases: number; composite: number; realRunReport: string }>(
  'web/contract.json',
);
const raw =
  read<(ModelOutput & { id: string; mediaType: string; caption: string; error: null })[]>(
    'web/expected-raw.json',
  );
const gold = read<Record<string, GroundTruth>>('ground-truth.json');
const cases = read<{ posts: { id: string; mediaType: string }[] }>('cases.json');
const canned = read<
  {
    id: string;
    mediaType: string;
    caption: string;
    sourceImages: number;
    images: number;
    answer: RawCatalog;
  }[]
>('web/canned.json');

describe('actual web catalog pipeline evaluation contract', () => {
  it('pins the approved prompt digest to a real-run aggregate report', () => {
    expect(promptDigest('catalog')).toBe(contract.digest);
    expect(fs.readFileSync(contract.realRunReport, 'utf8')).toContain(`\`${contract.digest}\``);
  });
  it('covers every fixed case and scores with the unchanged extraction scorer', () => {
    expect(raw.map((r) => r.id)).toEqual(cases.posts.map((p) => String(p.id)));
    expect(raw).toHaveLength(contract.cases);
    const mean = raw.reduce((sum, r) => sum + scoreCase(gold[r.id], r).composite, 0) / raw.length;
    expect(mean).toBe(contract.composite);
    expect(raw.filter((r) => r.error)).toEqual([]);
  });
  it('pins the raw output after production normalization of canned answers', () => {
    expect(
      canned.map((f) => {
        const normalized = normalizeCatalogOutput(f.answer, 'social');
        return {
          id: f.id,
          mediaType: f.mediaType,
          caption: f.caption.slice(0, 120),
          tags: normalized.tags,
          keywords: normalized.keywords,
          entities: normalized.entities,
          description: normalized.description,
          error: null,
        };
      }),
    ).toEqual(raw);
    expect(canned.every((f) => f.images >= 0 && f.images <= 6)).toBe(true);
    expect(canned.some((f) => f.sourceImages > 6 && f.images === 6)).toBe(true);
    expect(canned.filter((f) => f.mediaType === 'text').every((f) => f.images === 0)).toBe(true);
    expect(canned.every((f) => f.caption.includes('#tool #type #art #design #study'))).toBe(true);
  });
  it('scores the web raw file through the same score-only CLI without starting Electron', () => {
    const result = spawnSync(
      process.execPath,
      [
        '--import=tsx',
        path.join(root, 'run.ts'),
        `--score-raw=${path.join(root, 'web/expected-raw.json')}`,
      ],
      { encoding: 'utf8' },
    );
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain(`meanComposite=${contract.composite.toFixed(3)}`);
    expect(result.stdout).toContain('/20');
  });
});
