import fs from 'fs';
import path from 'path';
import { describe, expect, it } from 'vitest';
import { formatReport, promptDigest } from '../../scripts/ai-eval/score';
import { responseSchema } from '../../shared/ai/prompts';
import {
  schemaProblem,
  scorePost,
  scoreRun,
  termsMatch,
  entityTermsMatch,
  SCORER_VERSION,
  type GoldFile,
  type GoldPost,
} from '../../shared/ai/score';

const FIXTURES = path.resolve(__dirname, '../../shared/ai/fixtures');
const read = <T>(name: string): T =>
  JSON.parse(fs.readFileSync(path.join(FIXTURES, name), 'utf8')) as T;
const gold = read<GoldFile>('gold.json');
const answers = read<Record<string, unknown>>('answers.json');
const invalid =
  read<{ why: string; kind: 'social' | 'web'; answer: unknown }[]>('invalid-answers.json');

describe('termsMatch', () => {
  it('matches forms of the same term', () => {
    expect(termsMatch('Desk Lamps', 'desk lamp')).toBe(true);
    expect(termsMatch('#lighting', 'Lighting')).toBe(true);
    expect(termsMatch('three.js', 'threejs')).toBe(true);
    expect(termsMatch('three js', 'Three.js')).toBe(true);
    expect(termsMatch('full-bleed photography', 'full bleed photography')).toBe(true);
    expect(termsMatch('cities', 'city')).toBe(true);
    expect(termsMatch('glass', 'glass')).toBe(true);
  });

  it('keeps different terms apart', () => {
    expect(termsMatch('spun brass', 'brass')).toBe(false);
    expect(termsMatch('design', 'industrial design')).toBe(false);
    expect(termsMatch('', '')).toBe(false);
  });

  it('folds accents and removes trademark symbols before compatibility normalization', () => {
    expect(termsMatch('São Paulo', 'Sao Paulo')).toBe(true);
    expect(termsMatch('café', 'cafe')).toBe(true);
    expect(termsMatch('Acme™', 'Acme')).toBe(true);
    expect(termsMatch('Acme®', 'Acme')).toBe(true);
    expect(termsMatch('Acme™', 'AcmeTM')).toBe(false);
  });

  it('keeps credited handle boundaries while preserving normal domain synonyms', () => {
    expect(entityTermsMatch('@anna.design', 'anna.design')).toBe(true);
    expect(entityTermsMatch('@anna.design', 'annadesign')).toBe(false);
    expect(entityTermsMatch('@anna.design', '@anna_design')).toBe(false);
    expect(entityTermsMatch('Three.js', 'threejs')).toBe(true);
  });
});

describe('schemaProblem', () => {
  it('passes every fixture answer and fails every invalid one', () => {
    for (const [id, answer] of Object.entries(answers)) {
      const kind = gold.posts[id].kind === 'web' ? 'web_catalog' : 'catalog';
      expect(schemaProblem(responseSchema(kind).schema, answer), id).toBeNull();
    }
    for (const { why, kind, answer } of invalid) {
      const schema = responseSchema(kind === 'web' ? 'web_catalog' : 'catalog').schema;
      expect(schemaProblem(schema, answer), why).not.toBeNull();
    }
  });

  it('names where an answer breaks the schema', () => {
    const schema = responseSchema('web_catalog').schema;
    const answer = { ...(answers['studio-site'] as object), purpose: 'blog' };
    expect(schemaProblem(schema, answer)).toBe('/purpose: not one of the enum');
    expect(schemaProblem(responseSchema('catalog').schema, [])).toBe(
      '/: expected object, got array',
    );
  });

  it('refuses a schema keyword it does not check', () => {
    expect(() => schemaProblem({ type: 'string', pattern: '^a' }, 'a')).toThrow(/pattern/);
    expect(schemaProblem({ type: 'object', minItems: 0, title: 'ok' }, {})).toBeNull();
  });
});

describe('scoreRun', () => {
  const report = scoreRun(gold, answers);

  it('counts answered, missing and invalid posts', () => {
    expect(report.counts).toEqual({ posts: 6, answered: 5, missing: 1, invalid: 0 });
    const missing = report.posts.find((p) => p.id === 'unanswered');
    expect(missing?.status).toBe('missing');
    expect(missing?.composite).toBe(0);
    expect(missing?.fields.specific_tags).toEqual({ score: 0 });
  });

  it('scores each field of an answer', () => {
    const lamp = report.posts.find((p) => p.id === 'lamp')!;
    expect(lamp.fields.general_tags?.score).toBe(1);
    // "walnut" matches its gold alternatives; "spun brass" is not "brass".
    expect(lamp.fields.specific_tags?.missed).toEqual(['brass']);
    expect(lamp.fields.specific_tags?.extra).toEqual(['spun brass', 'lamp shade']);
    expect(lamp.fields.language?.score).toBe(1);
    expect(lamp.fields.purpose).toBeUndefined();
    const site = report.posts.find((p) => p.id === 'studio-site')!;
    expect(site.fields.purpose?.score).toBe(1);
    expect(site.fields.industry?.score).toBe(1);
    expect(site.fields.entities?.score).toBe(1);
  });

  it('scores what the products store: the normalized answer', () => {
    // The stairs answer has 4 general tags; only the first 3 are kept, lowercased.
    const stairs = report.posts.find((p) => p.id === 'stairs')!;
    expect(stairs.fields.general_tags?.extra).toEqual(['photography']);
    expect(stairs.fields.entities?.score).toBe(1);
  });

  it('averages fields and media types', () => {
    expect(report.composite).toBeGreaterThan(0.5);
    expect(report.composite).toBeLessThan(1);
    expect(Object.keys(report.byMediaType).sort()).toEqual([
      'carousel',
      'image',
      'text',
      'video',
      'website',
    ]);
    expect(report.fields.purpose).toBe(1);
  });

  it('versions scores and exposes false positives on empty entity gold', () => {
    const perfect = {
      ...(answers.lamp as Record<string, unknown>),
      entities: [],
    };
    const run = scoreRun(
      { posts: { a: { catalog: { entities: [] } }, b: { catalog: { entities: [] } } } },
      { a: perfect, b: { ...perfect, entities: ['invented studio'] } },
    );
    expect(run.scorerVersion).toBe(SCORER_VERSION);
    expect(run.fields.entities).toBe(0.5);
    expect(run.entities).toEqual({
      expected: 0,
      predicted: 1,
      matched: 0,
      emptyGoldPosts: 2,
      falsePositivePosts: 1,
      precision: 0,
      recall: 0,
      f1: 0,
    });
  });

  it('counts unanswered nonempty entity gold as missed in micro recall', () => {
    const run = scoreRun({ posts: { a: { catalog: { entities: ['real studio'] } } } }, {});
    expect(run.entities.expected).toBe(1);
    expect(run.entities.matched).toBe(0);
    expect(run.entities.recall).toBe(0);
  });
});

describe('scorePost', () => {
  const post: GoldPost = gold.posts.lamp;

  it('parses an answer given as text', () => {
    const scored = scorePost('lamp', post, JSON.stringify(answers.lamp));
    expect(scored.status).toBe('answered');
    expect(scored.composite).toBe(scorePost('lamp', post, answers.lamp).composite);
    expect(scorePost('lamp', post, '{"description":').status).toBe('not_json');
  });

  it('scores an answer off the schema as 0, with the reason', () => {
    const scored = scorePost('lamp', post, invalid[0].answer);
    expect(scored.status).toBe('schema_invalid');
    expect(scored.problem).toBe('/entities: missing');
    expect(scored.composite).toBe(0);
  });

  it('gives a perfect answer a perfect score', () => {
    const perfect = scorePost('typeface', gold.posts.typeface, {
      ...gold.posts.typeface.catalog,
    });
    expect(perfect.composite).toBe(1);
  });
});

describe('the CLI report', () => {
  it('names the prompt version and the worst posts', () => {
    const text = formatReport(scoreRun(gold, answers), 2);
    expect(text).toContain(`catalog ${promptDigest('catalog')}`);
    expect(promptDigest('catalog')).toMatch(/^[0-9a-f]{12}$/);
    expect(promptDigest('catalog')).not.toBe(promptDigest('web_catalog'));
    expect(text).toContain('worst 2:');
    expect(text).toContain('unanswered (image)  missing');
  });
});
