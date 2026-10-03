// Per-field scores of catalog answers against a gold file: the measure for tuning the
// catalog prompts, schemas and inputs (owner task X1) and for comparing runs. Pure: the CLI
// scripts/ai-eval/score.ts reads the files and prints the report.
//
// The gold file (JSON):
//
//   { "posts": { "<post id>": { "kind": "social" | "web", "mediaType"?: "video",
//                               "catalog": { <the answer a perfect model would give> } } } }
//
// `catalog` has the fields of the task's schema (catalog.schema.json or
// web_catalog.schema.json). In a tag list (general_tags, specific_tags, entities) an item
// may be a list of acceptable alternatives: ["walnut", ["walnut wood", "walnut veneer"]].
// The answers file maps each post id to the model's answer: the parsed JSON object, its raw
// text, or null when the model gave none.
//
// Each answer is checked against the task's schema (an answer off the schema scores 0, as
// the web server would store nothing), normalized like the products do
// (normalizeCatalogOutput: tiers, caps, trimming), then scored field by field:
//
//   general_tags, specific_tags   F1 of the matched terms (one-to-one, alternatives allowed)
//   tags                          F1 of the flat list against general ∪ specific (tier swaps)
//   entities                      F1, case-insensitive; explicit handles retain punctuation
//   search_keywords               soft F1: token overlap of each keyword with its best match
//   description, save_reason      F1 of the content words (a ROUGE-1-like overlap)
//   language                      1 when the primary language subtag matches
//   purpose, industry (web)       1 when equal
//
// Terms match when equal after NFKC, lowercasing, a leading "#" dropped, "-", "_" and "/"
// read as spaces, other symbols dropped and a light plural strip; or equal once spaces are
// removed too ("three.js" = "threejs" = "three js"). The composite is the weighted mean of
// the fields the gold has (WEIGHTS, renormalized).

import { normalizeCatalogOutput, type AnalyzeKind, type RawCatalog } from './catalog';
import { responseSchema, type JsonSchema } from './prompts';

/** A gold term: one form, or acceptable alternatives. */
export type GoldTerm = string | string[];

/** The catalog a perfect model would answer for a post. */
export interface GoldCatalog {
  description?: string;
  general_tags?: GoldTerm[];
  specific_tags?: GoldTerm[];
  entities?: GoldTerm[];
  search_keywords?: string[];
  save_reason?: string;
  language?: string;
  purpose?: string;
  industry?: string;
}

export interface GoldPost {
  kind?: AnalyzeKind;
  mediaType?: string;
  catalog: GoldCatalog;
}

export interface GoldFile {
  posts: Record<string, GoldPost>;
}

/** The fields scored, in report order. */
export const FIELDS = [
  'general_tags',
  'specific_tags',
  'tags',
  'entities',
  'search_keywords',
  'description',
  'save_reason',
  'language',
  'purpose',
  'industry',
] as const;
export type Field = (typeof FIELDS)[number];

/** Scores from different versions must be recomputed before comparison. */
export const SCORER_VERSION = 2;

/** The weight of each field in the composite (renormalized over the fields scored). */
export const WEIGHTS: Readonly<Record<Field, number>> = {
  general_tags: 0.15,
  specific_tags: 0.25,
  tags: 0.1,
  entities: 0.15,
  search_keywords: 0.1,
  description: 0.1,
  save_reason: 0.05,
  language: 0.02,
  purpose: 0.15,
  industry: 0.15,
};

/** The score of one field. */
export interface FieldScore {
  /** 0…1. */
  score: number;
  precision?: number;
  recall?: number;
  /** Gold terms the answer missed (lists only). */
  missed?: string[];
  /** Answer terms the gold does not have (lists only). */
  extra?: string[];
}

/** The scores of one post. */
export interface PostScore {
  id: string;
  kind: AnalyzeKind;
  mediaType?: string;
  /** `answered`, `missing` (no answer), `not_json` or `schema_invalid`. */
  status: 'answered' | 'missing' | 'not_json' | 'schema_invalid';
  /** The first schema problem, for `schema_invalid`. */
  problem?: string;
  fields: Partial<Record<Field, FieldScore>>;
  composite: number;
}

/** The scores of a run. */
export interface ScoreReport {
  scorerVersion: number;
  posts: PostScore[];
  /** The mean of each field over the posts that have it in their gold. */
  fields: Partial<Record<Field, number>>;
  composite: number;
  counts: { posts: number; answered: number; missing: number; invalid: number };
  /** The mean composite per media type, when the gold names them. */
  byMediaType: Record<string, { posts: number; composite: number }>;
  /** Micro counts expose hallucinated entities even when most gold lists are empty. */
  entities: {
    expected: number;
    predicted: number;
    matched: number;
    emptyGoldPosts: number;
    falsePositivePosts: number;
    precision: number;
    recall: number;
    f1: number;
  };
}

// ── Terms ───────────────────────────────────────────────────────────────────

const STOPWORDS = new Set(
  (
    'a an and are as at be but by for from has have in into is it its of on or that the this ' +
    'to was were will with about over under than then there these those which who whom why how'
  ).split(' '),
);

/** Strips a plural ending from a word of more than 3 letters (lamps → lamp, cities → city). */
function stem(word: string): string {
  if (word.length <= 3) return word;
  if (word.endsWith('ies') && word.length > 4) return `${word.slice(0, -3)}y`;
  if (/(?:ss|sh|ch|x|z)es$/.test(word)) return word.slice(0, -2);
  if (word.endsWith('s') && !word.endsWith('ss')) return word.slice(0, -1);
  return word;
}

/** The words of a term: NFKC, lowercase, "#" dropped, separators read as spaces. */
function folded(text: string): string {
  return (
    text
      // NFKC turns a trademark into literal "TM"; remove it before folding.
      .replace(/[™®℠]/gu, '')
      .normalize('NFKC')
      .normalize('NFD')
      .replace(/[\u0300-\u036f]/g, '')
      .toLowerCase()
  );
}

function words(text: string): string[] {
  return folded(text)
    .replace(/^#+/, '')
    .replace(/[-_/]+/g, ' ')
    .replace(/[^\p{L}\p{N}\s]+/gu, '')
    .split(/\s+/)
    .filter(Boolean);
}

/** The comparable forms of a term: its stemmed words, and its words run together, stemmed. */
function forms(term: string): [string, string] {
  const list = words(term);
  return [list.map(stem).join(' '), stem(list.join(''))];
}

/** Whether two terms name the same thing. */
export function termsMatch(a: string, b: string): boolean {
  const [wa, ca] = forms(a);
  const [wb, cb] = forms(b);
  return !!wa && (wa === wb || ca === cb);
}

/** An explicitly credited handle keeps its dot/underscore identity. */
export function entityTermsMatch(a: string, b: string): boolean {
  const left = folded(a).trim();
  const right = folded(b).trim();
  if (left.startsWith('@') || right.startsWith('@')) {
    const key = (value: string): string => value.replace(/^@/, '');
    return !!key(left) && key(left) === key(right);
  }
  return termsMatch(a, b);
}

/** The content words of a text: words of 3+ letters (or any number), no stopwords, stemmed. */
function contentWords(text: string): string[] {
  return words(text)
    .filter((w) => (w.length >= 3 || /^\p{N}+$/u.test(w)) && !STOPWORDS.has(w))
    .map(stem);
}

const f1 = (precision: number, recall: number): number =>
  precision + recall === 0 ? 0 : (2 * precision * recall) / (precision + recall);

const round = (n: number): number => Math.round(n * 1000) / 1000;

/** F1 of two bags of words. */
function bagF1(a: string[], b: string[]): number {
  if (!a.length && !b.length) return 1;
  if (!a.length || !b.length) return 0;
  const counts = new Map<string, number>();
  for (const w of b) counts.set(w, (counts.get(w) ?? 0) + 1);
  let common = 0;
  for (const w of a) {
    const n = counts.get(w) ?? 0;
    if (n > 0) {
      common++;
      counts.set(w, n - 1);
    }
  }
  return f1(common / a.length, common / b.length);
}

/** Set scores of a list: one-to-one matching, gold items may hold alternatives. */
function listScore(
  answer: string[],
  gold: GoldTerm[],
  match: (a: string, b: string) => boolean = termsMatch,
): FieldScore {
  const alternatives = gold.map((g) => (Array.isArray(g) ? g : [g]));
  const used = new Set<number>();
  const matchedGold = new Set<number>();
  alternatives.forEach((alts, gi) => {
    const hit = answer.findIndex((a, ai) => !used.has(ai) && alts.some((g) => match(a, g)));
    if (hit >= 0) {
      used.add(hit);
      matchedGold.add(gi);
    }
  });
  const precision = answer.length ? used.size / answer.length : gold.length ? 0 : 1;
  const recall = gold.length ? matchedGold.size / gold.length : answer.length ? 0 : 1;
  return {
    score: round(f1(precision, recall)),
    precision: round(precision),
    recall: round(recall),
    missed: alternatives.filter((_, gi) => !matchedGold.has(gi)).map((alts) => alts[0]),
    extra: answer.filter((_, ai) => !used.has(ai)),
  };
}

/** Soft F1 of keyword lists: each keyword's best token overlap with the other side. */
function keywordScore(answer: string[], gold: string[]): FieldScore {
  if (!answer.length || !gold.length) {
    const score = !answer.length && !gold.length ? 1 : 0;
    return { score, precision: score, recall: score };
  }
  const bags = (list: string[]): string[][] => list.map(contentWords);
  const a = bags(answer);
  const g = bags(gold);
  const best = (from: string[][], to: string[][]): number =>
    from.reduce((sum, x) => sum + Math.max(...to.map((y) => bagF1(x, y))), 0) / from.length;
  const precision = best(a, g);
  const recall = best(g, a);
  return {
    score: round(f1(precision, recall)),
    precision: round(precision),
    recall: round(recall),
  };
}

function primaryLanguage(tag: string): string {
  return tag.trim().toLowerCase().split(/[-_]/)[0] ?? '';
}

// ── Schema check ────────────────────────────────────────────────────────────

const ANNOTATIONS = new Set(['title', 'description', '$comment', 'examples', 'default']);
const CHECKED = new Set([
  'type',
  'properties',
  'required',
  'additionalProperties',
  'items',
  'enum',
  'minItems',
  'maxItems',
  'minLength',
  'maxLength',
]);

function typeOf(value: unknown): string {
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'array';
  if (typeof value === 'number') return Number.isInteger(value) ? 'integer' : 'number';
  return typeof value;
}

/**
 * The first place where `value` breaks `schema`, or null. Covers the keywords the catalog
 * schemas use (type, properties, required, additionalProperties, items, enum, the item and
 * length bounds) and refuses any other, so a new schema keyword cannot pass unchecked.
 */
export function schemaProblem(schema: JsonSchema, value: unknown, path = ''): string | null {
  for (const key of Object.keys(schema)) {
    if (!CHECKED.has(key) && !ANNOTATIONS.has(key)) {
      throw new Error(`shared/ai/score.ts: schema keyword "${key}" is not supported`);
    }
  }
  const at = path || '/';
  const type = schema.type as string | undefined;
  const actual = typeOf(value);
  if (type && !(actual === type || (type === 'number' && actual === 'integer'))) {
    return `${at}: expected ${type}, got ${actual}`;
  }
  if (Array.isArray(schema.enum) && !schema.enum.includes(value)) {
    return `${at}: not one of the enum`;
  }
  if (typeof value === 'string') {
    const length = [...value].length;
    if (typeof schema.minLength === 'number' && length < schema.minLength)
      return `${at}: too short`;
    if (typeof schema.maxLength === 'number' && length > schema.maxLength) return `${at}: too long`;
  }
  if (Array.isArray(value)) {
    if (typeof schema.minItems === 'number' && value.length < schema.minItems) {
      return `${at}: fewer than ${schema.minItems} items`;
    }
    if (typeof schema.maxItems === 'number' && value.length > schema.maxItems) {
      return `${at}: more than ${schema.maxItems} items`;
    }
    const items = schema.items as JsonSchema | undefined;
    if (items) {
      for (let i = 0; i < value.length; i++) {
        const problem = schemaProblem(items, value[i], `${path}/${i}`);
        if (problem) return problem;
      }
    }
  }
  if (actual === 'object') {
    const object = value as Record<string, unknown>;
    const properties = (schema.properties ?? {}) as Record<string, JsonSchema>;
    for (const name of (schema.required as string[] | undefined) ?? []) {
      if (!(name in object)) return `${path}/${name}: missing`;
    }
    for (const [name, item] of Object.entries(object)) {
      const property = properties[name];
      if (!property) {
        if (schema.additionalProperties === false) return `${path}/${name}: not in the schema`;
        continue;
      }
      const problem = schemaProblem(property, item, `${path}/${name}`);
      if (problem) return problem;
    }
  }
  return null;
}

// ── Scoring ─────────────────────────────────────────────────────────────────

function emptyScores(gold: GoldCatalog, kind: AnalyzeKind): Partial<Record<Field, FieldScore>> {
  const out: Partial<Record<Field, FieldScore>> = {};
  for (const field of scoredFields(gold, kind)) out[field] = { score: 0 };
  return out;
}

/** The fields a gold catalog is scored on. */
function scoredFields(gold: GoldCatalog, kind: AnalyzeKind): Field[] {
  return FIELDS.filter((field) => {
    if (field === 'tags') return !!(gold.general_tags || gold.specific_tags);
    if (field === 'purpose' || field === 'industry') return kind === 'web' && gold[field] != null;
    return gold[field] != null;
  });
}

function composite(fields: Partial<Record<Field, FieldScore>>): number {
  let sum = 0;
  let weights = 0;
  for (const field of FIELDS) {
    const score = fields[field];
    if (!score) continue;
    sum += WEIGHTS[field] * score.score;
    weights += WEIGHTS[field];
  }
  return weights ? round(sum / weights) : 0;
}

/** Scores one answer against its gold catalog. */
export function scorePost(id: string, gold: GoldPost, answer: unknown): PostScore {
  const kind: AnalyzeKind = gold.kind ?? 'social';
  const base = { id, kind, mediaType: gold.mediaType };
  const fail = (status: PostScore['status'], problem?: string): PostScore => ({
    ...base,
    status,
    ...(problem ? { problem } : {}),
    fields: emptyScores(gold.catalog, kind),
    composite: 0,
  });
  if (answer === null || answer === undefined) return fail('missing');
  let parsed = answer;
  if (typeof answer === 'string') {
    try {
      parsed = JSON.parse(answer);
    } catch {
      return fail('not_json');
    }
  }
  const schema = responseSchema(kind === 'web' ? 'web_catalog' : 'catalog').schema;
  const problem = schemaProblem(schema, parsed);
  if (problem) return fail('schema_invalid', problem);

  const result = normalizeCatalogOutput(parsed as RawCatalog, kind);
  const g = gold.catalog;
  const fields: Partial<Record<Field, FieldScore>> = {};
  for (const field of scoredFields(g, kind)) {
    switch (field) {
      case 'general_tags':
        fields[field] = listScore(result.generalTags, g.general_tags ?? []);
        break;
      case 'specific_tags':
        fields[field] = listScore(result.specificTags, g.specific_tags ?? []);
        break;
      case 'tags':
        fields[field] = listScore(result.tags, [
          ...(g.general_tags ?? []),
          ...(g.specific_tags ?? []),
        ]);
        break;
      case 'entities':
        fields[field] = listScore(result.entities, g.entities ?? [], entityTermsMatch);
        break;
      case 'search_keywords':
        fields[field] = keywordScore(result.keywords, g.search_keywords ?? []);
        break;
      case 'description':
        fields[field] = {
          score: round(bagF1(contentWords(result.description), contentWords(g.description ?? ''))),
        };
        break;
      case 'save_reason':
        fields[field] = {
          score: round(bagF1(contentWords(result.saveReason), contentWords(g.save_reason ?? ''))),
        };
        break;
      case 'language':
        fields[field] = {
          score: primaryLanguage(result.language) === primaryLanguage(g.language ?? '') ? 1 : 0,
        };
        break;
      case 'purpose':
        fields[field] = { score: result.contentType === g.purpose ? 1 : 0 };
        break;
      case 'industry':
        fields[field] = { score: result.category === g.industry ? 1 : 0 };
        break;
    }
  }
  return { ...base, status: 'answered', fields, composite: composite(fields) };
}

/** Scores every post of the gold file; posts without an answer score 0. */
export function scoreRun(gold: GoldFile, answers: Readonly<Record<string, unknown>>): ScoreReport {
  const posts = Object.entries(gold.posts).map(([id, post]) => scorePost(id, post, answers[id]));
  const fields: Partial<Record<Field, number>> = {};
  for (const field of FIELDS) {
    const scores = posts.flatMap((p) => (p.fields[field] ? [p.fields[field]!.score] : []));
    if (scores.length) fields[field] = round(scores.reduce((a, b) => a + b, 0) / scores.length);
  }
  const mean = (list: PostScore[]): number =>
    list.length ? round(list.reduce((a, p) => a + p.composite, 0) / list.length) : 0;
  const byMediaType: ScoreReport['byMediaType'] = {};
  for (const type of new Set(posts.flatMap((p) => (p.mediaType ? [p.mediaType] : [])))) {
    const list = posts.filter((p) => p.mediaType === type);
    byMediaType[type] = { posts: list.length, composite: mean(list) };
  }
  let expected = 0;
  let predicted = 0;
  let matched = 0;
  let emptyGoldPosts = 0;
  let falsePositivePosts = 0;
  for (const post of posts) {
    const entities = gold.posts[post.id].catalog.entities;
    if (entities === undefined) continue;
    expected += entities.length;
    if (entities.length === 0) emptyGoldPosts++;
    if (post.status !== 'answered') continue;
    const field = post.fields.entities;
    const hits = entities.length - (field?.missed?.length ?? entities.length);
    const extras = field?.extra?.length ?? 0;
    matched += hits;
    predicted += hits + extras;
    if (entities.length === 0 && extras > 0) falsePositivePosts++;
  }
  const precision = predicted ? matched / predicted : expected ? 0 : 1;
  const recall = expected ? matched / expected : predicted ? 0 : 1;
  return {
    scorerVersion: SCORER_VERSION,
    posts,
    fields,
    composite: mean(posts),
    counts: {
      posts: posts.length,
      answered: posts.filter((p) => p.status === 'answered').length,
      missing: posts.filter((p) => p.status === 'missing').length,
      invalid: posts.filter((p) => p.status === 'not_json' || p.status === 'schema_invalid').length,
    },
    byMediaType,
    entities: {
      expected,
      predicted,
      matched,
      emptyGoldPosts,
      falsePositivePosts,
      precision: round(precision),
      recall: round(recall),
      f1: round(f1(precision, recall)),
    },
  };
}
