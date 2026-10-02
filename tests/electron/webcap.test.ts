import { describe, it, expect, vi } from 'vitest';

vi.mock('electron', () => ({
  app: { getPath: () => '/tmp', isPackaged: false, getLocale: () => 'it-IT' },
}));

const discover = await import('../../electron/webcap/discover');
const meta = await import('../../electron/webcap/metadata');
const catalog = await import('../../electron/webcap/ai-catalog');

// A minimal CapturedPage for the pure metadata functions.
function page(over: Record<string, unknown> = {}): never {
  return {
    requestedUrl: 'https://studio.example/',
    url: 'https://studio.example/',
    pageType: 'home',
    status: 200,
    headers: {},
    title: 'Home | Studio Example',
    hero: null,
    bands: [],
    footer: null,
    sections: [],
    video: null,
    heightCss: 4000,
    capped: false,
    jacked: false,
    probe: {},
    network: [],
    qc: { status: 'ok', reason: '' },
    consent: { cmp: null, result: null },
    overlaysRemoved: 0,
    reveal: null,
    timings: {},
    ...over,
  } as never;
}

describe('page classification and selection', () => {
  it('classifies paths in several languages, case studies and locale prefixes', () => {
    expect(discover.classifyUrl('https://x.com/')).toBe('home');
    expect(discover.classifyUrl('https://x.com/it/')).toBe('home');
    expect(discover.classifyUrl('https://x.com/progetti')).toBe('work');
    expect(discover.classifyUrl('https://x.com/work/acme-rebrand')).toBe('case-study');
    expect(discover.classifyUrl('https://x.com/chi-siamo')).toBe('about');
    expect(discover.classifyUrl('https://x.com/en/pricing')).toBe('pricing');
    expect(discover.classifyUrl('https://x.com/privacy-policy')).toBe('legal');
    expect(discover.classifyUrl('https://x.com/lavora-con-noi')).toBe('careers');
    expect(discover.classifyUrl('https://x.com/blog/hello-world')).toBe('article');
  });

  it('prefers navigation pages, excludes low-value ones and language variants, diversifies types', () => {
    const picked = discover.pickPages(
      'https://studio.example/',
      [
        { href: 'https://studio.example/work', text: 'Work', region: 'header' },
        { href: 'https://studio.example/work/a', text: 'A', region: 'main' },
        { href: 'https://studio.example/work/b', text: 'B', region: 'main' },
        { href: 'https://studio.example/work/c', text: 'C', region: 'main' },
        { href: 'https://studio.example/about', text: 'Studio', region: 'nav' },
        { href: 'https://studio.example/contact', text: 'Contact', region: 'footer' },
        { href: 'https://studio.example/privacy', text: 'Privacy', region: 'footer' },
        { href: 'https://studio.example/careers', text: 'Jobs', region: 'footer' },
        { href: 'https://studio.example/de/work', text: 'Arbeit', region: 'nav' },
        { href: 'https://other.example/work', text: 'Elsewhere', region: 'main' },
        { href: 'https://studio.example/feed', text: 'RSS', region: 'footer' },
      ],
      { max: 5 },
    );
    const urls = picked.map((p) => p.url);
    expect(urls[0]).toBe('https://studio.example/work');
    expect(picked.filter((p) => p.pageType === 'case-study')).toHaveLength(2);
    expect(urls).toContain('https://studio.example/about');
    expect(urls).not.toContain('https://studio.example/privacy');
    expect(urls).not.toContain('https://studio.example/careers');
    expect(urls).not.toContain('https://studio.example/de/work');
    expect(urls.some((u) => u.startsWith('https://other.example'))).toBe(false);
  });

  it('only takes a contact page when nothing better is left', () => {
    const picked = discover.pickPages(
      'https://a.example/',
      [
        { href: 'https://a.example/contact', text: 'Contact', region: 'nav' },
        { href: 'https://a.example/services', text: 'Services', region: 'nav' },
      ],
      { max: 1 },
    );
    expect(picked.map((p) => p.pageType)).toEqual(['services']);
  });
});

describe('colour and typography metadata', () => {
  it('round-trips sRGB through OKLab and names colours', () => {
    const lab = meta.hexToLab('#533afd')!;
    expect(meta.oklabToHex(lab)).toBe('#533afd');
    expect(meta.colorName('#ffffff')).toBe('white');
    expect(meta.colorName('#07090a')).toBe('black');
    expect(meta.colorName('#533afd')).toMatch(/indigo|blue/);
  });

  it('computes WCAG contrast', () => {
    expect(meta.contrastRatio('#000000', '#ffffff')).toBeCloseTo(21, 0);
    expect(meta.contrastRatio('#777777', '#ffffff')).toBeCloseTo(4.48, 1);
  });

  it('cleans build-artefact font names and drops icon fonts', () => {
    expect(meta.cleanFamily('__GeistSans_3a0388')).toBe('Geist Sans');
    expect(meta.cleanFamily('IBMPlexMono')).toBe('IBM Plex Mono');
    expect(meta.cleanFamily('SourceCodePro')).toBe('Source Code Pro');
    expect(meta.cleanFamily('sohne-var')).toBe('Sohne');
    expect(meta.cleanFamily('__Inter_Fallback_a1b2c3')).toBe('Inter');
    expect(meta.cleanFamily('Inter Placeholder')).toBeNull();
    expect(meta.cleanFamily('"Inter Variable"')).toBe('Inter');
    expect(meta.cleanFamily('Font Awesome 6 Free')).toBeNull();
    expect(meta.cleanFamily('Apple Color Emoji')).toBeNull();
    expect(meta.cleanFamily('sans-serif')).toBeNull();
  });

  it('derives roles, provider and type scale from visible text styles', () => {
    const p = page({
      probe: {
        fontFaces: [
          { family: 'Editorial New', status: 'loaded' },
          { family: 'Neue Montreal', status: 'loaded' },
        ],
        typeStyles: [
          {
            family: '"Editorial New", serif',
            weight: '400',
            style: 'normal',
            size: 96,
            chars: 120,
            minTop: 200,
            tags: { h1: 120 },
            sample: 'We design brands',
          },
          {
            family: '"Neue Montreal", sans-serif',
            weight: '400',
            style: 'normal',
            size: 16,
            chars: 4000,
            minTop: 1200,
            tags: { p: 4000 },
            sample: 'Body copy',
          },
          {
            family: '"Neue Montreal", sans-serif',
            weight: '500',
            style: 'normal',
            size: 24,
            chars: 600,
            minTop: 900,
            tags: { h2: 600 },
            sample: 'Section',
          },
        ],
      },
      network: [
        {
          url: 'https://studio.example/fonts/EditorialNew-Regular.woff2',
          type: 'font',
          status: 200,
          mime: 'font/woff2',
          bytes: 1,
        },
        {
          url: 'https://fonts.gstatic.com/s/neuemontreal/v1/x.woff2',
          type: 'font',
          status: 200,
          mime: 'font/woff2',
          bytes: 1,
        },
      ],
    });
    const t = meta.computeTypography([p], 'studio.example');
    const editorial = t.fonts.find((f) => f.family === 'Editorial New')!;
    const neue = t.fonts.find((f) => f.family === 'Neue Montreal')!;
    expect(editorial.role).toBe('display');
    expect(editorial.classification).toBe('serif');
    expect(editorial.provider).toBe('self-hosted');
    expect(neue.roles).toContain('body');
    expect(neue.provider).toBe('google');
    expect(t.baseSize).toBe(16);
  });
});

describe('tech, awards, identity', () => {
  it('detects technologies from markers, request URLs, headers and generator tags', () => {
    const p = page({
      headers: { server: 'Vercel' },
      probe: {
        markers: { next: '14.2.3', gsap: '3.12.5', lenis: true, three: '160' },
        head: { generator: ['Webflow'] },
      },
      network: [
        {
          url: 'https://cdn.prod.website-files.com/x/js/webflow.js',
          type: 'script',
          status: 200,
          mime: '',
          bytes: 0,
        },
      ],
    });
    const names = Object.fromEntries(meta.computeTech([p]).map((t) => [t.name, t.version]));
    expect(names['Next.js']).toBe('14.2.3');
    expect(names['GSAP']).toBe('3.12.5');
    expect('Lenis' in names).toBe(true);
    expect(names['Three.js']).toBe('160');
    expect('Webflow' in names).toBe(true);
    expect('Vercel' in names).toBe(true);
  });

  it('accepts only award entries that belong to the site itself', () => {
    const p = page({
      probe: {
        awardLinks: [
          {
            href: 'https://www.awwwards.com/sites/studio-example',
            text: 'Site of the Day',
            region: 'footer',
            fixed: false,
            img: 'sotd.svg',
          },
          {
            href: 'https://www.awwwards.com/sites/some-client-project',
            text: '',
            region: 'main',
            fixed: false,
            img: '',
          },
          {
            href: 'https://www.awwwards.com/studio-example/',
            text: 'Our profile',
            region: 'footer',
            fixed: false,
            img: '',
          },
        ],
      },
    });
    const awards = meta.computeAwards([p], 'studio.example', 'Studio Example');
    expect(awards).toHaveLength(1);
    expect(awards[0]).toMatchObject({
      platform: 'awwwards',
      level: 'site-of-the-day',
      evidence: 'badge',
    });
    expect(meta.awardTags(awards).tags).toContain('awwwards-site-of-the-day');
  });

  it('parses JSON-LD types (IRI form, @graph) and the organization', () => {
    const r = meta.parseJsonLd([
      JSON.stringify({
        '@context': 'https://schema.org',
        '@graph': [
          {
            '@type': 'https://schema.org/Corporation',
            name: 'Acme',
            sameAs: ['https://x.com/acme'],
          },
          { '@type': 'WebSite' },
        ],
      }),
      '{not json',
    ]);
    expect(r.types).toEqual(['Corporation', 'WebSite']);
    expect(r.organization).toMatchObject({ name: 'Acme', sameAs: ['https://x.com/acme'] });
  });

  it('cleans page titles', () => {
    expect(meta.cleanTitle('Home | Studio Example', 'Studio Example')).toBe('Studio Example');
    expect(meta.cleanTitle('Work — Studio Example', 'Studio Example')).toBe('Work');
  });
});

describe('AI catalog mapping', () => {
  it('keeps only vocabulary values and builds facets, tags and entities', () => {
    const post = {
      id: 'web:1',
      webDomain: 'studio.example',
      webMeta: {
        siteName: 'Studio Example',
        scheme: 'dark',
        lang: 'en',
        tech: [{ name: 'Three.js', category: '3d', confidence: 0.95 }],
        traits: { webgl: true, smoothScroll: 'Lenis' },
        awardEntities: ['Awwwards'],
      },
      webFonts: [{ family: 'Editorial New', classification: 'serif' }],
      webPalette: [{ hex: '#0b0b0b', name: 'black', role: 'background' }],
      webAwards: [{ platform: 'awwwards' }],
    } as unknown as Shelfy.Post;
    const m = catalog.mapCatalog(
      {
        observations: 'Oversized serif headline.',
        site_type: 'portfolio-studio',
        site_type_secondary: 'none',
        industry: 'design-creative',
        audience: 'brands',
        style: ['editorial', 'not-a-style', 'dark-elegant'],
        theme: 'dark',
        color_mood: ['monochrome'],
        density: 'airy',
        layout_patterns: ['oversized wordmark'],
        hero_type: '3d-webgl',
        imagery: ['3d-renders'],
        typography: ['serif editorial'],
        components: ['case study cards'],
        craft: 'exceptional',
        notable_details: ['Serif wordmark bleeding off the right edge'],
        reference_for: ['borrow the bleeding wordmark for a studio home'],
        tags: ['#Bleeding-Wordmark'],
        search_keywords: ['dark serif studio portfolio'],
        summary: 'A studio portfolio.',
        description: 'Editorial New headline on black.',
      },
      post,
      'Test Model',
    );
    expect(m.catalog.style).toEqual(['editorial', 'dark-elegant']);
    expect(m.contentType).toBe('portfolio-studio');
    expect(m.category).toBe('design-creative');
    expect(m.catalog.facets.tech).toEqual(['Three.js']);
    expect(m.catalog.facets.motion).toEqual(expect.arrayContaining(['webgl', 'smooth-scroll']));
    expect(m.catalog.tags).toEqual(['bleeding-wordmark']);
    expect(m.entities).toEqual(
      expect.arrayContaining(['Studio Example', 'Editorial New', 'Three.js', 'Awwwards']),
    );
    expect(m.description).toContain('A studio portfolio.');
    // 'serif editorial' agrees with the measured serif font and is kept.
    expect(m.catalog.typography).toEqual(['serif editorial']);
  });

  it('drops typography labels the measured fonts contradict', () => {
    const m = catalog.mapCatalog(
      { typography: ['monospace accents', 'geometric sans', 'serif editorial'] },
      {
        webFonts: [{ family: 'Aeonik', classification: 'sans' }],
        webMeta: {},
      } as unknown as Shelfy.Post,
      'x',
    );
    expect(m.catalog.typography).toEqual(['geometric sans']);
  });

  it('falls back to safe values for out-of-vocabulary answers', () => {
    const m = catalog.mapCatalog(
      { site_type: 'blogpost', industry: 'spaceships', theme: 'neon' },
      { webMeta: { scheme: 'light' } } as unknown as Shelfy.Post,
      'x',
    );
    expect(m.contentType).toBe('other');
    expect(m.category).toBe('other');
    expect(m.catalog.theme).toBe('light');
  });
});
