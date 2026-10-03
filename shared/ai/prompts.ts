// The AI tasks of shared/ai/manifest.json, as TypeScript reads them: each task's prompts,
// rendered from their template files, its response schema and its sampling settings. The
// desktop (electron/analyzer.ts) and the scripts use this module; the Rust core has the same
// API over the same files (crates/core/src/ai/prompts.rs). See shared/ai/README.md.

import { FILES, MANIFEST } from './index';
import { renderTemplate, type TemplateVars } from './template';

/** A task's `maxTokens` when it depends on the size of the input. */
export interface MaxTokensRule {
  /** Tokens for any input. */
  readonly base: number;
  /** Tokens added per input item (a tag of the group, a tag of the batch). */
  readonly perItem: number;
  /** The ceiling. */
  readonly max: number;
}

/** The normalization caps of a catalog's tag lists. */
export interface CatalogCaps {
  readonly general: number;
  readonly specific: number;
  /** The flat list, general then specific. */
  readonly tags: number;
}

/** One task of the manifest. */
export interface TaskSpec {
  /** What the task is for. */
  readonly about: string;
  /** The system prompt's file. */
  readonly system: string;
  /** The user message's file, when the task has a fixed one. */
  readonly user?: string;
  /** The response schema: its name for the provider, and its file. */
  readonly schema?: { readonly name: string; readonly file: string };
  readonly temperature: number;
  readonly maxTokens: number | MaxTokensRule;
  /** Catalogs: the caption or page text is cut at this many UTF-16 units, then "…". */
  readonly captionMax?: number;
  /** Catalogs: at most this many vocabulary (social) or tech-stack (web) hints. */
  readonly hintsMax?: number;
  /** Catalogs: the caps of the normalized tag lists. */
  readonly caps?: CatalogCaps;
}

export type TaskName = keyof typeof MANIFEST.tasks;

/** The version of the output schemas; web results store it in `ai_schema_version`. */
export const SCHEMA_VERSION: number = MANIFEST.schemaVersion;

const TASKS: Readonly<Record<TaskName, TaskSpec>> = MANIFEST.tasks;

/** A JSON Schema, as a parsed object. */
export type JsonSchema = Record<string, unknown>;

/** A response schema in the shape of OpenAI's `json_schema` (provider-neutral fields). */
export interface ResponseSchema {
  name: string;
  strict: true;
  schema: JsonSchema;
}

/** Every task name, in manifest order. */
export const TASK_NAMES = Object.keys(TASKS) as TaskName[];

/** A task of the manifest. */
export function task(name: TaskName): TaskSpec {
  const spec = TASKS[name];
  if (!spec) throw new Error(`shared/ai: unknown task ${String(name)}`);
  return spec;
}

/** The text of a file the manifest names. */
export function file(name: string): string {
  const text = FILES[name];
  if (text === undefined) throw new Error(`shared/ai: no file ${name}`);
  return text;
}

const schemas = new Map<string, JsonSchema>();

/** A schema file, parsed (once). */
function schemaFile(name: string): JsonSchema {
  let schema = schemas.get(name);
  if (!schema) {
    schema = JSON.parse(file(name)) as JsonSchema;
    schemas.set(name, schema);
  }
  return schema;
}

/** The system prompt of a task, rendered with `vars`. */
export function systemPrompt(name: TaskName, vars: TemplateVars = {}): string {
  return renderTemplate(file(task(name).system), vars);
}

/** The user message of a task, rendered with `vars`. */
export function userPrompt(name: TaskName, vars: TemplateVars = {}): string {
  const user = task(name).user;
  if (!user) throw new Error(`shared/ai: task ${name} has no user message`);
  return renderTemplate(file(user), vars);
}

/** The response schema of a task. */
export function responseSchema(name: TaskName): ResponseSchema {
  const schema = task(name).schema;
  if (!schema) throw new Error(`shared/ai: task ${name} has no response schema`);
  return { name: schema.name, strict: true, schema: schemaFile(schema.file) };
}

/** The response schema as an OpenAI-compatible `response_format`. */
export function responseFormat(name: TaskName): {
  type: 'json_schema';
  json_schema: ResponseSchema;
} {
  return { type: 'json_schema', json_schema: responseSchema(name) };
}

/** The `max_tokens` of a task, for an input of `items` items when the task scales with it. */
export function maxTokens(name: TaskName, items = 0): number {
  const rule = task(name).maxTokens;
  if (typeof rule === 'number') return rule;
  return Math.min(rule.max, items * rule.perItem + rule.base);
}
