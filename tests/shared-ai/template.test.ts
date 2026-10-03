import { describe, expect, it } from 'vitest';
import { renderTemplate, TemplateError } from '../../shared/ai/template';

describe('renderTemplate', () => {
  it('returns plain text without its final line break', () => {
    expect(renderTemplate('one\ntwo\n', {})).toBe('one\ntwo');
    expect(renderTemplate('one\ntwo', {})).toBe('one\ntwo');
    expect(renderTemplate('one\n\n', {})).toBe('one\n');
    expect(renderTemplate('', {})).toBe('');
  });

  it('keeps blank lines and inner spacing as written', () => {
    expect(renderTemplate('a\n\n  b  \n\tc\n', {})).toBe('a\n\n  b  \n\tc');
  });

  it('keeps an #if section for true flags and non-empty texts', () => {
    const t = 'start\n{{#if x}}\nyes\n{{else}}\nno\n{{/if}}\nend\n';
    expect(renderTemplate(t, { x: true })).toBe('start\nyes\nend');
    expect(renderTemplate(t, { x: 'text' })).toBe('start\nyes\nend');
    expect(renderTemplate(t, { x: false })).toBe('start\nno\nend');
    expect(renderTemplate(t, { x: '' })).toBe('start\nno\nend');
  });

  it('inverts #unless, and an else is optional', () => {
    expect(renderTemplate('{{#unless x}}\nshown\n{{/unless}}\n', { x: false })).toBe('shown');
    expect(renderTemplate('{{#unless x}}\nshown\n{{/unless}}\n', { x: true })).toBe('');
    expect(renderTemplate('a\n{{#if x}}\nb\n{{/if}}\nc\n', { x: false })).toBe('a\nc');
  });

  it('nests sections', () => {
    const t = '{{#if a}}\nA\n{{#if b}}\nAB\n{{else}}\nA-not-B\n{{/if}}\n{{else}}\nnot-A\n{{/if}}\n';
    expect(renderTemplate(t, { a: true, b: true })).toBe('A\nAB');
    expect(renderTemplate(t, { a: true, b: false })).toBe('A\nA-not-B');
    expect(renderTemplate(t, { a: false, b: true })).toBe('not-A');
  });

  it('accepts blanks around directives and inside braces', () => {
    const t = '  {{ #if x }}\t\n{{ x }}\n {{ else }}\n-\n{{ /if }}\n';
    expect(renderTemplate(t, { x: 'v' })).toBe('v');
  });

  it('drops comment lines', () => {
    expect(renderTemplate('{{! a note }}\ntext\n{{!another}}\n', {})).toBe('text');
  });

  it('inserts values as they are, never as template syntax', () => {
    const t = 'before {{v}} after\n';
    expect(renderTemplate(t, { v: '{{w}} {{#if w}}\n{{/if}} $& $1' })).toBe(
      'before {{w}} {{#if w}}\n{{/if}} $& $1 after',
    );
    expect(renderTemplate('{{a}}{{b}}\n', { a: '1', b: '2' })).toBe('12');
  });

  it('leaves braces that are not a variable alone', () => {
    expect(renderTemplate('{alias, canonical} {{ 2 }} {x}\n', {})).toBe(
      '{alias, canonical} {{ 2 }} {x}',
    );
  });

  it('rejects unknown variables, in any branch', () => {
    expect(() => renderTemplate('{{missing}}\n', {})).toThrow(TemplateError);
    expect(() => renderTemplate('{{#if missing}}\n{{/if}}\n', {})).toThrow(/unknown variable/);
    expect(() => renderTemplate('{{#if x}}\n{{typo}}\n{{/if}}\n', { x: false })).toThrow(
      /line 2: unknown variable "typo"/,
    );
  });

  it('rejects a flag used as text', () => {
    expect(() => renderTemplate('{{x}}\n', { x: true })).toThrow(/is a flag/);
  });

  it('rejects unbalanced sections', () => {
    expect(() => renderTemplate('{{#if x}}\na\n', { x: true })).toThrow(/line 1: .* not closed/);
    expect(() => renderTemplate('a\n{{/if}}\n', {})).toThrow(/closes no section/);
    expect(() => renderTemplate('{{else}}\n', {})).toThrow(/outside a section/);
    expect(() => renderTemplate('{{#if x}}\n{{else}}\n{{else}}\n{{/if}}\n', { x: true })).toThrow(
      /outside a section/,
    );
    expect(() => renderTemplate('{{#if x}}\n{{/unless}}\n', { x: true })).toThrow(/closes/);
  });

  it('rejects directives that are not alone on their line', () => {
    expect(() => renderTemplate('a {{#if x}}\n{{/if}}\n', { x: true })).toThrow(/alone/);
    expect(() => renderTemplate('{{#if x}} b\n{{/if}}\n', { x: true })).toThrow(/alone/);
    expect(() => renderTemplate('text {{! note }}\n', {})).toThrow(/alone/);
    expect(() => renderTemplate('x {{else}}\n', {})).toThrow(/alone/);
  });
});
