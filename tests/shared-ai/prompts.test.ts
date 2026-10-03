import { describe, expect, it } from 'vitest';
import { FILES, MANIFEST } from '../../shared/ai/index';
import {
  file,
  maxTokens,
  responseFormat,
  responseSchema,
  SCHEMA_VERSION,
  systemPrompt,
  task,
  TASK_NAMES,
  userPrompt,
  type TaskName,
} from '../../shared/ai/prompts';
import type { TemplateVars } from '../../shared/ai/template';

// The variables each task's templates take (shared/ai/README.md), with every flag and list
// both set and empty, so every branch of every template renders.
const SENTINELS = {
  generalOpen: '[[GENERAL]]',
  generalClose: '[[/GENERAL]]',
  specificOpen: '[[SPECIFIC]]',
  specificClose: '[[/SPECIFIC]]',
  keywordsOpen: '[[KEYWORDS]]',
  keywordsClose: '[[/KEYWORDS]]',
  removeOpen: '[[REMOVE]]',
  removeClose: '[[/REMOVE]]',
};
const VARIABLES: Record<TaskName, { system: TemplateVars[]; user: TemplateVars[] }> = {
  catalog: {
    system: [{}],
    user: [
      { frames: true, caption: 'c', vocabulary: 'a, b' },
      { frames: false, caption: '', vocabulary: '' },
    ],
  },
  web_catalog: {
    system: [{}],
    user: [
      { frames: true, caption: 'c', tech: 'Next.js', purposes: 'p', industries: 'i' },
      { frames: false, caption: '', tech: '', purposes: 'p', industries: 'i' },
    ],
  },
  qc: { system: [{}], user: [{}] },
  chat: {
    system: [
      { broad: 'a', specific: 'b', active: 'c', perTierCap: '15', maxKeywords: '6', ...SENTINELS },
      { broad: '', specific: '', active: '', perTierCap: '15', maxKeywords: '6', ...SENTINELS },
    ],
    user: [],
  },
  suggest: { system: [{}], user: [{ query: 'q' }] },
  cluster_refine: { system: [{}], user: [{ tags: '- a\n- b (c)' }, { tags: '' }] },
  aliases: { system: [{}], user: [{ candidates: 'a', vocabulary: 'b' }] },
  web_design: {
    system: [{}],
    user: [
      { frames: true, ground: 'Measured fonts and palette', digest: 'Page content' },
      { frames: false, ground: '', digest: '' },
    ],
  },
};

describe('shared/ai', () => {
  it('is at output schema version 2', () => {
    expect(SCHEMA_VERSION).toBe(2);
    expect(MANIFEST.schemaVersion).toBe(2);
  });

  it('renders every template of every task, in every branch', () => {
    expect(TASK_NAMES).toEqual(Object.keys(VARIABLES));
    for (const name of TASK_NAMES) {
      for (const vars of VARIABLES[name].system) {
        const text = systemPrompt(name, vars);
        expect(text.length).toBeGreaterThan(40);
        expect(text).not.toMatch(/\{\{/);
      }
      if (task(name).user) {
        expect(VARIABLES[name].user.length).toBeGreaterThan(0);
        for (const vars of VARIABLES[name].user) {
          expect(userPrompt(name, vars)).not.toMatch(/\{\{/);
        }
      } else {
        expect(() => userPrompt(name, {})).toThrow(/no user message/);
      }
    }
  });

  it('holds no llama.cpp-only knob in any file', () => {
    for (const [name, text] of Object.entries(FILES)) {
      expect(text, name).not.toMatch(
        /dry_(multiplier|base|allowed|penalty)|cache_prompt|chat_template_kwargs|enable_thinking|repeat_penalty|n_predict/i,
      );
    }
    expect(JSON.stringify(MANIFEST)).not.toMatch(/dry|cache_prompt|chat_template|thinking/i);
  });

  it('builds the response formats the desktop sends, key order included', () => {
    for (const name of TASK_NAMES) {
      const spec = task(name);
      if (!spec.schema) {
        expect(() => responseSchema(name)).toThrow(/no response schema/);
        continue;
      }
      const format = responseFormat(name);
      expect(Object.keys(format)).toEqual(['type', 'json_schema']);
      expect(Object.keys(format.json_schema)).toEqual(['name', 'strict', 'schema']);
      expect(format.json_schema.name).toBe(spec.schema.name);
      expect(format.json_schema.schema).toEqual(JSON.parse(file(spec.schema.file)));
      expect(format.json_schema.schema.type).toBe('object');
      expect(format.json_schema.schema.additionalProperties).toBe(false);
    }
  });

  it('offers "other" in the web purpose and industry enums (schema v2)', () => {
    const { properties } = responseSchema('web_catalog').schema as {
      properties: Record<string, { enum: string[] }>;
    };
    expect(properties.purpose.enum).toContain('other');
    expect(properties.industry.enum).toContain('other');
  });

  it('sizes max_tokens by the input where the task says so', () => {
    expect(maxTokens('catalog')).toBe(768);
    expect(maxTokens('chat')).toBe(256);
    expect(maxTokens('cluster_refine', 3)).toBe(280);
    expect(maxTokens('cluster_refine', 1000)).toBe(2048);
    expect(maxTokens('aliases', 40)).toBe(1216);
    expect(maxTokens('aliases', 0)).toBe(256);
  });

  it('refuses unknown files and tasks', () => {
    expect(() => file('nope.md')).toThrow(/no file/);
    expect(() => task('nope' as TaskName)).toThrow(/unknown task/);
  });
});
