// The catalog prompts and the normalization of their answers, shared by the desktop
// (electron/analyzer.ts re-exports these functions) and the scripts. The Rust core ports
// them in crates/core/src/ai/{catalog,normalize}.rs, and the golden sets under
// shared/golden/ai/catalog/ (scripts/golden/ai-catalog.ts) keep both byte for byte equal.
//
// The texts and the numbers (the caption cut, the hint count, the tag caps) come from the
// "catalog" and "web_catalog" tasks of shared/ai/manifest.json.

import {
  maxTokens,
  responseSchema,
  task,
  userPrompt,
  systemPrompt,
  type ResponseSchema,
  type TaskName,
} from './prompts';

/** Which catalog a post gets: social posts, or websites (web references). */
export type AnalyzeKind = 'social' | 'web';

/** The structured result of a catalog answer (camelCase, ready for `updateAiAnalysis`). */
export interface AnalyzeResult {
  description: string;
  modelUsed?: string;
  tags: string[];
  generalTags: string[];
  specificTags: string[];
  entities: string[];
  keywords: string[];
  saveReason: string;
  language: string;
  /** Web only: the purpose, for `ai_content_type`. */
  contentType?: string;
  /** Web only: the industry, for `ai_category`. */
  category?: string;
}

/**
 * The raw JSON object a catalog answer holds (`video_catalog` or `web_catalog`), before
 * normalization. Every field is best-effort: the model can answer anything.
 */
export interface RawCatalog {
  description?: unknown;
  general_tags?: unknown;
  specific_tags?: unknown;
  entities?: unknown;
  search_keywords?: unknown;
  save_reason?: unknown;
  language?: unknown;
  /** Web schema only. */
  purpose?: unknown;
  /** Web schema only. */
  industry?: unknown;
}

/** A catalog request, provider-neutral: the images go after the user text. */
export interface CatalogRequest {
  system: string;
  user: string;
  schema: ResponseSchema;
  temperature: number;
  maxTokens: number;
}

/** The manifest task of a catalog kind. */
export function catalogTask(kind: AnalyzeKind): TaskName {
  return kind === 'web' ? 'web_catalog' : 'catalog';
}

/**
 * Neutralizes delimiter-like markers (`<<<…>>>`) inside untrusted text before it is placed
 * between the prompt's data markers: a caption or page text holding the literal closing
 * marker (e.g. `<<<END CAPTION>>>`) could otherwise end the data region early and smuggle
 * instructions into the prompt.
 */
export function stripPromptMarkers(text: unknown): string {
  return String(text).replace(/<<<[\s\S]*?>>>/g, ' ');
}

/** The untrusted text of a prompt: markers stripped, trimmed, cut at `max` with "…". */
function untrustedText(text: unknown, max: number): string {
  const clean = typeof text === 'string' ? stripPromptMarkers(text).trim() : '';
  return clean.length > max ? `${clean.slice(0, max)}…` : clean;
}

/** The non-blank strings of `hints`, at most `max`, comma-separated. */
function hintList(hints: unknown, max: number): string {
  const list = Array.isArray(hints)
    ? hints.filter((t): t is string => typeof t === 'string' && !!t.trim()).slice(0, max)
    : [];
  return list.join(', ');
}

function numberOf(value: number | undefined, what: string): number {
  if (value === undefined) throw new Error(`shared/ai: the catalog task has no ${what}`);
  return value;
}

/**
 * The user text of a catalog request. The images are sent as separate parts of the same
 * message; the caption (when present) supplies the facts the pixels cannot convey (tools,
 * names, technique, author), as untrusted data between markers. `frequentTags` is the
 * archive's vocabulary for a social post and the detected tech stack for a website.
 */
export function buildUserPrompt(
  caption: unknown,
  frequentTags: unknown,
  hasFrames = true,
  kind: AnalyzeKind = 'social',
): string {
  if (kind === 'web') return buildWebUserPrompt(caption, frequentTags, hasFrames);
  const spec = task('catalog');
  return userPrompt('catalog', {
    frames: Boolean(hasFrames),
    caption: untrustedText(caption, numberOf(spec.captionMax, 'captionMax')),
    vocabulary: hintList(frequentTags, numberOf(spec.hintsMax, 'hintsMax')),
  });
}

/**
 * The user text of a website catalog request: the same untrusted-text bounding as the social
 * prompt, with the authority flipped: purpose and industry come from the PAGE TEXT, the
 * aesthetic and UX tags from the screenshots. `frequentTags` carries the detected tech stack.
 */
export function buildWebUserPrompt(
  caption: unknown,
  frequentTags: unknown,
  hasFrames = true,
): string {
  const spec = task('web_catalog');
  const properties = responseSchema('web_catalog').schema.properties as Record<
    string,
    { enum?: string[] }
  >;
  return userPrompt('web_catalog', {
    frames: Boolean(hasFrames),
    caption: untrustedText(caption, numberOf(spec.captionMax, 'captionMax')),
    tech: hintList(frequentTags, numberOf(spec.hintsMax, 'hintsMax')),
    purposes: (properties.purpose.enum ?? []).join(', '),
    industries: (properties.industry.enum ?? []).join(', '),
  });
}

/** A complete catalog request: prompts, response schema and sampling. */
export function catalogRequest(
  caption: unknown,
  frequentTags: unknown,
  hasFrames: boolean,
  kind: AnalyzeKind = 'social',
): CatalogRequest {
  const name = catalogTask(kind);
  return {
    system: systemPrompt(name),
    user: buildUserPrompt(caption, frequentTags, hasFrames, kind),
    schema: responseSchema(name),
    temperature: task(name).temperature,
    maxTokens: maxTokens(name),
  };
}

/**
 * Normalizes a string array: trim, drop empties, dedupe (case-insensitively), at most `cap`.
 * Lowercases unless `keepCase` (entities and keywords keep their casing).
 */
export function cleanStringArray(
  arr: unknown,
  { keepCase = false, cap = Infinity }: { keepCase?: boolean; cap?: number } = {},
): string[] {
  if (!Array.isArray(arr)) return [];
  const out: string[] = [];
  const seen = new Set<string>();
  for (const v of arr) {
    if (typeof v !== 'string') continue;
    const trimmed = v.trim();
    if (!trimmed) continue;
    const norm = keepCase ? trimmed : trimmed.toLowerCase();
    const dedupKey = norm.toLowerCase();
    if (seen.has(dedupKey)) continue;
    seen.add(dedupKey);
    out.push(norm);
    if (out.length >= cap) break;
  }
  return out;
}

/**
 * A catalog answer as the archive stores it: two tiers of tags (general: themes, specific:
 * details) and the flat list of both, deduplicated and capped; the other fields trimmed (the
 * description as it is). A website's closed-enum purpose and industry map onto
 * `contentType` and `category` (the raw slugs); a social post leaves them out, so those
 * columns stay untouched.
 */
export function normalizeCatalogOutput(
  parsed: RawCatalog,
  kind: AnalyzeKind = 'social',
  modelUsed?: string,
): AnalyzeResult {
  const caps = task(catalogTask(kind)).caps;
  if (!caps) throw new Error('shared/ai: the catalog task has no caps');
  const generalTags = cleanStringArray(parsed.general_tags, { cap: caps.general });
  const specificTags = cleanStringArray(parsed.specific_tags, { cap: caps.specific });
  const tags = cleanStringArray([...generalTags, ...specificTags], { cap: caps.tags });

  const result: AnalyzeResult = {
    description: typeof parsed.description === 'string' ? parsed.description : '',
    tags,
    generalTags,
    specificTags,
    entities: cleanStringArray(parsed.entities, { keepCase: true }),
    keywords: cleanStringArray(parsed.search_keywords, { keepCase: true }),
    saveReason: typeof parsed.save_reason === 'string' ? parsed.save_reason.trim() : '',
    language: typeof parsed.language === 'string' ? parsed.language.trim() : '',
    modelUsed,
  };

  if (kind === 'web') {
    const purpose = typeof parsed.purpose === 'string' ? parsed.purpose.trim() : '';
    const industry = typeof parsed.industry === 'string' ? parsed.industry.trim() : '';
    result.contentType = purpose || undefined; // → ai_content_type
    result.category = industry || undefined; // → ai_category
  }

  return result;
}
