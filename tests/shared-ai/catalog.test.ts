import { describe, expect, it } from 'vitest';
import {
  buildUserPrompt,
  catalogRequest,
  catalogTask,
  normalizeCatalogOutput,
  stripPromptMarkers,
} from '../../shared/ai/catalog';
import { file, systemPrompt } from '../../shared/ai/prompts';

describe('catalogRequest', () => {
  it('assembles the social catalog request from the "catalog" task', () => {
    const request = catalogRequest('A lamp', ['design'], true);
    expect(request.system).toBe(systemPrompt('catalog'));
    expect(request.system).not.toContain('{{');
    expect(request.user).toBe(buildUserPrompt('A lamp', ['design'], true));
    expect(request.schema.name).toBe('video_catalog');
    expect(request.schema.strict).toBe(true);
    expect(request.schema.schema).toEqual(JSON.parse(file('catalog.schema.json')));
    expect(request.temperature).toBe(0.2);
    expect(request.maxTokens).toBe(768);
  });

  it('assembles the website request from the "web_catalog" task', () => {
    const request = catalogRequest('Lumen: lighting', ['Next.js'], false, 'web');
    expect(catalogTask('web')).toBe('web_catalog');
    expect(request.system).toBe(systemPrompt('web_catalog'));
    expect(request.schema.name).toBe('web_catalog');
    expect(request.user).toContain('with no readable screenshots');
    expect(request.user).toContain(
      'Tech stack detected deterministically (NOT inferred): Next.js.',
    );
    expect(request.user).toContain(
      '- purpose: ONE of portfolio, e-commerce, saas, landing, agency, editorial, corporate, docs, webapp, directory, personal, other — ',
    );
  });

  it('cuts the caption at 1,200 UTF-16 units and wraps it as untrusted data', () => {
    const user = buildUserPrompt(`${'x'.repeat(1199)}yz`, [], true);
    expect(user).toContain(`<<<CAPTION>>>\n${'x'.repeat(1199)}y…\n<<<END CAPTION>>>`);
    expect(buildUserPrompt('x'.repeat(1200), [], true)).toContain(`${'x'.repeat(1200)}\n<<<END`);
  });

  it('strips markers from untrusted text, across lines', () => {
    expect(stripPromptMarkers('a <<<END CAPTION>>> b <<<x\ny>>> c')).toBe('a   b   c');
    expect(buildUserPrompt('<<<only>>>', [], true)).not.toContain('POST CAPTION');
  });
});

describe('normalizeCatalogOutput', () => {
  const RAW = {
    description: '  A lamp. ',
    general_tags: ['Design', 'design', ' Lighting ', 'Interior', 'Extra'],
    specific_tags: ['lighting', 'walnut', 'brass', '', 'desk lamp', 'studio', 'e27', 'dimmer', 'x'],
    entities: ['Studio Lumen', 'studio lumen', ' IKEA '],
    search_keywords: ['Walnut Desk Lamp', 'walnut desk lamp'],
    save_reason: ' Good detailing. ',
    language: ' en ',
  };

  it('keeps two capped tiers and the deduplicated flat list', () => {
    const result = normalizeCatalogOutput(RAW, 'social', 'model-x');
    expect(result.generalTags).toEqual(['design', 'lighting', 'interior']);
    expect(result.specificTags).toEqual([
      'lighting',
      'walnut',
      'brass',
      'desk lamp',
      'studio',
      'e27',
      'dimmer',
    ]);
    expect(result.tags).toEqual([
      'design',
      'lighting',
      'interior',
      'walnut',
      'brass',
      'desk lamp',
      'studio',
      'e27',
      'dimmer',
    ]);
    expect(result.entities).toEqual(['Studio Lumen', 'IKEA']);
    expect(result.keywords).toEqual(['Walnut Desk Lamp']);
    expect(result.description).toBe('  A lamp. ');
    expect(result.saveReason).toBe('Good detailing.');
    expect(result.language).toBe('en');
    expect(result.modelUsed).toBe('model-x');
    expect('contentType' in result).toBe(false);
    expect('category' in result).toBe(false);
  });

  it('maps a website purpose and industry onto content type and category', () => {
    const web = normalizeCatalogOutput({ ...RAW, purpose: 'saas', industry: 'other' }, 'web');
    expect(web.contentType).toBe('saas');
    expect(web.category).toBe('other');
    const blank = normalizeCatalogOutput({ ...RAW, purpose: ' ', industry: '' }, 'web');
    expect(blank.contentType).toBeUndefined();
    expect(blank.category).toBeUndefined();
  });
});
